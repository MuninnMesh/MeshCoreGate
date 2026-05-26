//! ESP32 radio configuration and owner-task scaffolding.

use core::cell::RefCell;
use core::sync::atomic::{AtomicBool, AtomicI32, AtomicU32, Ordering};

use critical_section::Mutex;
use embassy_futures::block_on;
use embassy_time::{Duration, Instant};
use embedded_hal_bus::spi::ExclusiveDevice;
use esp_hal::Blocking;
use esp_hal::analog::adc::{Adc, AdcCalLine, AdcConfig, AdcPin, Attenuation};
use esp_hal::delay::Delay;
use esp_hal::gpio::{Input, InputConfig, Level, Output, OutputConfig, Pull};
use esp_hal::peripherals::{ADC1, GPIO1, GPIO2, GPIO7, GPIO37, GPIO46};
use esp_hal::spi::Mode;
use esp_hal::spi::master::{Config as SpiConfig, Spi};
use esp_hal::time::Rate;
use heapless::Deque;
use muninn_gate_core::config::MAX_TELEMETRY_PRODUCERS;
use muninn_gate_core::{
    DiagnosticLevel,
    DiagnosticSubsystem,
    Error,
    GatewayConfig,
    TxPowerMapping,
};
use muninn_mesh_meshcore_lib::MeshTxFrame;
use muninn_mesh_radio::{
    MeshRadio,
    MeshRadioConfig,
    MeshRadioError,
    MeshRadioTxStatus,
    MeshRxFrame,
    RadioClock,
    RadioHostFns,
    RadioSwitch,
    RxStats,
    register_radio_host,
};
use muninn_mesh_sx126x::SX126x;
use muninn_mesh_sx126x::op::modulation::{LoRaBandWidth, LoRaSpreadFactor, LoraCodingRate};
use muninn_mesh_sx126x::op::rxtx::RampTime;

use crate::platform::Esp32RadioResources;
use crate::{Esp32BoardVariant, diagnostics, telemetry_state};

/// Delay before starting active LoRa RX after platform bring-up.
pub const RADIO_RX_START_DELAY_MS: u64 = 5_000;
/// SPI bus frequency used for the SX126x peripheral.
pub const LORA_SPI_FREQUENCY_HZ: u32 = 8_000_000;
/// Consecutive radio operation failures before reinitialization.
pub const RADIO_REINIT_ERROR_THRESHOLD: u8 = 3;
/// Multiplier applied to TX airtime before the next queued TX.
pub const RADIO_AIRTIME_BUDGET_FACTOR: u32 = 2;
/// Number of queued MeshCore TX frames retained for the radio owner.
pub const RADIO_TX_QUEUE_LEN: usize = 8;
/// Number of received LoRa frames retained for the MeshCore adapter.
pub const RADIO_RX_QUEUE_LEN: usize = 8;
/// Heltec V4.x VBAT divider top resistor in ohms.
pub const HELTEC_V4_BATTERY_DIVIDER_TOP_OHMS: u32 = 390_000;
/// Heltec V4.x VBAT divider bottom resistor in ohms.
pub const HELTEC_V4_BATTERY_DIVIDER_BOTTOM_OHMS: u32 = 100_000;
/// Minimum plausible Li-ion battery voltage reported by the board ADC.
pub const BATTERY_VALID_MIN_MV: u16 = 2_500;
/// Maximum plausible Li-ion battery voltage reported by the board ADC.
pub const BATTERY_VALID_MAX_MV: u16 = 5_000;
/// Time to let the switched high-impedance divider settle before sampling.
pub const BATTERY_ADC_SETTLE_MS: u32 = 2;
/// Initial ADC conversions discarded after enabling the divider.
pub const BATTERY_ADC_DISCARD_SAMPLES: u8 = 4;
/// ADC conversions averaged for one battery voltage reading.
pub const BATTERY_ADC_AVERAGE_SAMPLES: u8 = 16;
/// Selected TX level exposed to the radio host callback.
static SELECTED_TX_POWER_LEVEL: AtomicI32 = AtomicI32::new(0);
/// Whether the radio host callback currently reports TX as active.
static TX_ACTIVE: AtomicBool = AtomicBool::new(false);
/// Whether the radio owner has entered continuous RX service.
static RADIO_READY: AtomicBool = AtomicBool::new(false);
/// Latest board battery voltage sampled by the radio owner task.
static BATTERY_MV: AtomicU32 = AtomicU32::new(0);
/// Frames waiting to be transmitted by the APP CPU radio owner.
static RADIO_TX_QUEUE: Mutex<RefCell<Deque<MeshTxFrame, RADIO_TX_QUEUE_LEN>>> =
    Mutex::new(RefCell::new(Deque::new()));
/// Received frames waiting for the MeshCore adapter.
static RADIO_RX_QUEUE: Mutex<RefCell<Deque<MeshRxFrame, RADIO_RX_QUEUE_LEN>>> =
    Mutex::new(RefCell::new(Deque::new()));

type BatteryAdc = Adc<'static, ADC1<'static>, Blocking>;
type BatteryAdcPin = AdcPin<GPIO1<'static>, ADC1<'static>, AdcCalLine<ADC1<'static>>>;

/// Blocking clock used by the dedicated ESP32 radio owner core.
#[derive(Debug, Clone, Copy)]
pub struct EspRadioClock;

impl RadioClock for EspRadioClock
{
    fn now_ms(&self) -> u64
    {
        Instant::now().as_millis()
    }

    fn delay_ms(&mut self, delay_ms: u32)
    {
        Delay::new().delay_millis(delay_ms);
    }
}

/// WiFi LoRa 32 V4.x external FEM path switch.
pub struct HeltecV4Fem
{
    /// FEM power rail control, held high while the radio is active.
    _power:  Output<'static>,
    /// FEM enable control, held high while the radio is active.
    _enable: Output<'static>,
    /// FEM PA/RX selector.
    pa_mode: Output<'static>,
}

impl HeltecV4Fem
{
    /// Create and enable the board FEM, defaulting to RX mode.
    pub fn new(power: GPIO7<'static>, enable: GPIO2<'static>, pa_mode: GPIO46<'static>) -> Self
    {
        let power = Output::new(power, Level::High, OutputConfig::default());
        let enable = Output::new(enable, Level::High, OutputConfig::default());
        let mut pa_mode = Output::new(pa_mode, Level::Low, OutputConfig::default());
        pa_mode.set_low();
        Self {
            _power: power,
            _enable: enable,
            pa_mode,
        }
    }
}

impl RadioSwitch for HeltecV4Fem
{
    fn set_rx(&mut self) -> Result<(), MeshRadioError>
    {
        self.pa_mode.set_low();
        Ok(())
    }

    fn set_tx(&mut self) -> Result<(), MeshRadioError>
    {
        self.pa_mode.set_high();
        Ok(())
    }
}

/// Error returned by radio command/event queue operations.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RadioQueueError
{
    /// The selected fixed-capacity queue is full.
    Full,
}

/// Build the board radio configuration from gateway configuration.
pub fn mesh_radio_config_from_gateway<V>(
    config: &GatewayConfig<MAX_TELEMETRY_PRODUCERS>,
) -> Result<MeshRadioConfig, Error>
where
    V: Esp32BoardVariant,
{
    let radio = config.radio;
    Ok(MeshRadioConfig {
        freq_hz:       radio.frequency_hz,
        sync_word:     radio.sync_word,
        spread_factor: spread_factor(radio.spreading_factor)?,
        bandwidth:     bandwidth(radio.bandwidth_hz)?,
        coding_rate:   coding_rate(radio.coding_rate)?,
        preamble_len:  radio.preamble_len,
        iq_inverted:   radio.iq_inverted,
        tx_ramp_time:  ramp_time(radio.tx_ramp_time_us)?,
        use_tcxo:      V::RADIO_HARDWARE.tcxo_enabled,
        tcxo_delayms:  V::RADIO_HARDWARE.tcxo_delay_ms,
    })
}

/// Register host callbacks used by the board-level MeshCore radio wrapper.
pub fn register_gateway_radio_host(tx_power: TxPowerMapping)
{
    SELECTED_TX_POWER_LEVEL.store(i32::from(tx_power.selected_level), Ordering::Relaxed);
    register_radio_host(RadioHostFns {
        lora_tx_power_dbm,
        remember_pubkey,
        set_lora_tx_active,
    });
}

/// Return whether the radio host currently reports active TX.
pub fn radio_tx_active() -> bool
{
    TX_ACTIVE.load(Ordering::Relaxed)
}

/// Return whether the radio owner has entered continuous RX service.
pub fn radio_ready() -> bool
{
    RADIO_READY.load(Ordering::Relaxed)
}

/// Queue a MeshCore frame for serialized radio transmission.
pub fn enqueue_tx_frame(frame: MeshTxFrame) -> Result<(), RadioQueueError>
{
    critical_section::with(|cs| {
        RADIO_TX_QUEUE
            .borrow_ref_mut(cs)
            .push_back(frame)
            .map_err(|_| RadioQueueError::Full)
    })
}

/// Return the next received radio frame, when available.
pub fn try_dequeue_rx_frame() -> Option<MeshRxFrame>
{
    critical_section::with(|cs| RADIO_RX_QUEUE.borrow_ref_mut(cs).pop_front())
}

/// Run the APP CPU radio owner task.
pub fn run_radio_owner_task(
    resources: Esp32RadioResources,
    radio_config: MeshRadioConfig,
    tx_power: TxPowerMapping,
) -> !
{
    RADIO_READY.store(false, Ordering::Relaxed);
    register_gateway_radio_host(tx_power);
    block_on(run_radio_owner_loop(resources, radio_config, tx_power))
}

async fn run_radio_owner_loop(
    resources: Esp32RadioResources,
    radio_config: MeshRadioConfig,
    tx_power: TxPowerMapping,
) -> !
{
    let mut battery = BatterySampler::new(
        resources.battery_adc,
        resources.battery_pin,
        resources.battery_enable,
    );
    battery.sample();

    let spi_bus = match Spi::new(
        resources.spi2,
        SpiConfig::default()
            .with_frequency(Rate::from_hz(LORA_SPI_FREQUENCY_HZ))
            .with_mode(Mode::_0),
    ) {
        Ok(spi) => spi
            .with_sck(resources.sck)
            .with_mosi(resources.mosi)
            .with_miso(resources.miso),
        Err(_) => {
            record_radio_event(
                DiagnosticLevel::Error,
                "radio_spi_init_failed",
                "SX126x SPI bus initialization failed",
            );
            loop {
                blocking_delay_ms(1_000);
            }
        },
    };
    let lora_nss = Output::new(resources.nss, Level::High, OutputConfig::default());
    let lora_reset = Output::new(resources.reset, Level::High, OutputConfig::default());
    let lora_busy = Input::new(resources.busy, InputConfig::default().with_pull(Pull::None));
    let lora_dio1 = Input::new(resources.dio1, InputConfig::default().with_pull(Pull::None));
    let lora_ant = Output::new(resources.ant, Level::High, OutputConfig::default());
    let lora_spi = match ExclusiveDevice::new(spi_bus, lora_nss, Delay::new()) {
        Ok(spi) => spi,
        Err(_) => {
            record_radio_event(
                DiagnosticLevel::Error,
                "radio_spi_device_failed",
                "SX126x SPI device initialization failed",
            );
            loop {
                blocking_delay_ms(1_000);
            }
        },
    };
    let device = SX126x::new(lora_spi, (lora_reset, lora_busy, lora_ant, lora_dio1));
    let rf_switch = HeltecV4Fem::new(resources.fem_power, resources.fem_en, resources.fem_pa);

    let mut radio = match MeshRadio::init(device, rf_switch, EspRadioClock, radio_config).await {
        Ok(radio) => {
            record_radio_event(
                DiagnosticLevel::Info,
                "radio_initialized",
                "SX1262 radio initialized on APP CPU",
            );
            radio
        },
        Err(_) => {
            record_radio_event(
                DiagnosticLevel::Error,
                "radio_init_failed",
                "SX1262 radio initialization failed",
            );
            loop {
                blocking_delay_ms(1_000);
            }
        },
    };

    blocking_delay_ms(RADIO_RX_START_DELAY_MS as u32);
    if radio.start_rx().is_err() {
        record_radio_event(
            DiagnosticLevel::Error,
            "radio_rx_start_failed",
            "SX1262 continuous RX start failed",
        );
    } else {
        RADIO_READY.store(true, Ordering::Relaxed);
        record_radio_event(
            DiagnosticLevel::Info,
            "radio_rx_started",
            "SX1262 continuous RX started",
        );
    }

    let mut consecutive_errors = 0_u8;
    let mut last_health = Instant::now();
    let mut next_tx_time = Instant::now();

    loop {
        let now = Instant::now();
        let now_ms = now.as_millis();
        if now.duration_since(last_health) >= Duration::from_secs(1) {
            last_health = now;
            battery.sample();
            radio.sample_health();
            publish_radio_health(now_ms, tx_power, radio.rx_stats(), radio.last_tx_airtime_ms);
        }

        match radio.poll_receive(now.as_secs() as u32).await {
            Ok(Some(frame)) => {
                consecutive_errors = 0;
                let stats = radio.rx_stats();
                publish_radio_health(now_ms, tx_power, stats, radio.last_tx_airtime_ms);
                if enqueue_rx_frame(frame).is_err() {
                    record_radio_event(
                        DiagnosticLevel::Warn,
                        "radio_rx_queue_full",
                        "received LoRa frame dropped because RX queue is full",
                    );
                }
            },
            Ok(None) => {
                consecutive_errors = 0;
            },
            Err(_) => {
                consecutive_errors = consecutive_errors.saturating_add(1);
                record_radio_event(
                    DiagnosticLevel::Warn,
                    "radio_poll_failed",
                    "SX1262 RX poll failed",
                );
                if consecutive_errors >= RADIO_REINIT_ERROR_THRESHOLD {
                    record_radio_event(
                        DiagnosticLevel::Warn,
                        "radio_reinit",
                        "SX1262 radio reinitializing after repeated errors",
                    );
                    if radio.reinit().await.is_err() {
                        record_radio_event(
                            DiagnosticLevel::Error,
                            "radio_reinit_failed",
                            "SX1262 radio reinitialization failed",
                        );
                    }
                    consecutive_errors = 0;
                }
            },
        }

        if Instant::now() >= next_tx_time
            && let Some(frame) = dequeue_tx_frame()
        {
            match radio.transmit(&frame).await {
                Ok(MeshRadioTxStatus::Sent) => {
                    telemetry_state::increment_radio_tx_total();
                    publish_radio_health(
                        Instant::now().as_millis(),
                        tx_power,
                        radio.rx_stats(),
                        radio.last_tx_airtime_ms,
                    );
                    next_tx_time = Instant::now()
                        + Duration::from_millis(
                            u64::from(radio.last_tx_airtime_ms)
                                .saturating_mul(u64::from(RADIO_AIRTIME_BUDGET_FACTOR)),
                        );
                },
                Err(_) => {
                    consecutive_errors = consecutive_errors.saturating_add(1);
                    record_radio_event(
                        DiagnosticLevel::Warn,
                        "radio_tx_failed",
                        "queued MeshCore TX frame failed",
                    );
                    if consecutive_errors >= RADIO_REINIT_ERROR_THRESHOLD {
                        record_radio_event(
                            DiagnosticLevel::Warn,
                            "radio_reinit",
                            "SX1262 radio reinitializing after repeated errors",
                        );
                        if radio.reinit().await.is_err() {
                            record_radio_event(
                                DiagnosticLevel::Error,
                                "radio_reinit_failed",
                                "SX1262 radio reinitialization failed",
                            );
                        }
                        consecutive_errors = 0;
                    }
                },
            }
        }

        blocking_delay_ms(8);
    }
}

/// Delay on the dedicated radio core without requiring an Embassy executor.
fn blocking_delay_ms(delay_ms: u32)
{
    Delay::new().delay_millis(delay_ms);
}

fn publish_radio_health(
    now_ms: u64,
    tx_power: TxPowerMapping,
    stats: RxStats,
    last_tx_airtime_ms: u32,
)
{
    telemetry_state::update_radio_health(now_ms, tx_power, stats, last_tx_airtime_ms);
}

fn record_radio_event(level: DiagnosticLevel, code: &str, message: &str)
{
    let _ = diagnostics::record(
        Instant::now().as_millis(),
        level,
        DiagnosticSubsystem::Radio,
        code,
        message,
    );
}

fn dequeue_tx_frame() -> Option<MeshTxFrame>
{
    critical_section::with(|cs| RADIO_TX_QUEUE.borrow_ref_mut(cs).pop_front())
}

fn enqueue_rx_frame(frame: MeshRxFrame) -> Result<(), RadioQueueError>
{
    critical_section::with(|cs| {
        RADIO_RX_QUEUE
            .borrow_ref_mut(cs)
            .push_back(frame)
            .map_err(|_| RadioQueueError::Full)
    })
}

struct BatterySampler
{
    adc:    BatteryAdc,
    pin:    BatteryAdcPin,
    enable: Output<'static>,
}

impl BatterySampler
{
    fn new(adc: ADC1<'static>, pin: GPIO1<'static>, enable: GPIO37<'static>) -> Self
    {
        let enable = Output::new(enable, Level::Low, OutputConfig::default());
        let mut config = AdcConfig::new();
        let pin = config.enable_pin_with_cal::<GPIO1<'static>, AdcCalLine<ADC1<'static>>>(
            pin,
            Attenuation::_0dB,
        );
        let adc = Adc::new(adc, config);
        Self { adc, pin, enable }
    }

    fn sample(&mut self)
    {
        let adc_mv = self.read_adc_mv();
        let scaled_mv = battery_mv_from_adc_mv(adc_mv);
        let battery_mv = if valid_battery_mv(scaled_mv) {
            u32::from(scaled_mv)
        } else {
            0
        };
        BATTERY_MV.store(battery_mv, Ordering::Relaxed);
    }

    fn read_adc_mv(&mut self) -> u16
    {
        self.enable.set_high();
        Delay::new().delay_millis(BATTERY_ADC_SETTLE_MS);

        for _ in 0..BATTERY_ADC_DISCARD_SAMPLES {
            let _ = self.adc.read_blocking(&mut self.pin);
        }

        let mut sum = 0_u32;
        for _ in 0..BATTERY_ADC_AVERAGE_SAMPLES {
            sum = sum.saturating_add(u32::from(self.adc.read_blocking(&mut self.pin)));
        }

        self.enable.set_low();
        let average = sum / u32::from(BATTERY_ADC_AVERAGE_SAMPLES);
        average.min(u32::from(u16::MAX)) as u16
    }
}

fn battery_mv_from_adc_mv(adc_mv: u16) -> u16
{
    let scaled = u32::from(adc_mv)
        .saturating_mul(HELTEC_V4_BATTERY_DIVIDER_TOP_OHMS + HELTEC_V4_BATTERY_DIVIDER_BOTTOM_OHMS)
        / HELTEC_V4_BATTERY_DIVIDER_BOTTOM_OHMS;
    scaled.min(u32::from(u16::MAX)) as u16
}

fn valid_battery_mv(mv: u16) -> bool
{
    (BATTERY_VALID_MIN_MV..=BATTERY_VALID_MAX_MV).contains(&mv)
}

/// Return the latest board battery voltage in millivolts, or 0 when unknown.
pub fn battery_mv() -> u16
{
    BATTERY_MV.load(Ordering::Relaxed) as u16
}

fn lora_tx_power_dbm() -> i8
{
    SELECTED_TX_POWER_LEVEL.load(Ordering::Relaxed) as i8
}

fn remember_pubkey(_pubkey4: u16, _full_pubkey: [u8; 32], _last_seen: u32) {}

fn set_lora_tx_active(active: bool)
{
    TX_ACTIVE.store(active, Ordering::Relaxed);
}

fn spread_factor(value: u8) -> Result<LoRaSpreadFactor, Error>
{
    LoRaSpreadFactor::try_from(value).map_err(|_| Error::InvalidConfig)
}

fn bandwidth(value_hz: u32) -> Result<LoRaBandWidth, Error>
{
    match value_hz {
        7_810 => Ok(LoRaBandWidth::BW7),
        10_420 => Ok(LoRaBandWidth::BW10),
        15_630 => Ok(LoRaBandWidth::BW15),
        20_830 => Ok(LoRaBandWidth::BW20),
        31_250 => Ok(LoRaBandWidth::BW31),
        41_670 => Ok(LoRaBandWidth::BW41),
        62_500 => Ok(LoRaBandWidth::BW62),
        125_000 => Ok(LoRaBandWidth::BW125),
        250_000 => Ok(LoRaBandWidth::BW250),
        500_000 => Ok(LoRaBandWidth::BW500),
        _ => Err(Error::InvalidConfig),
    }
}

fn coding_rate(value: u8) -> Result<LoraCodingRate, Error>
{
    match value {
        5 => Ok(LoraCodingRate::CR4_5),
        6 => Ok(LoraCodingRate::CR4_6),
        7 => Ok(LoraCodingRate::CR4_7),
        8 => Ok(LoraCodingRate::CR4_8),
        _ => Err(Error::InvalidConfig),
    }
}

fn ramp_time(value_us: u16) -> Result<RampTime, Error>
{
    match value_us {
        10 => Ok(RampTime::Ramp10u),
        20 => Ok(RampTime::Ramp20u),
        40 => Ok(RampTime::Ramp40u),
        80 => Ok(RampTime::Ramp80u),
        200 => Ok(RampTime::Ramp200u),
        800 => Ok(RampTime::Ramp800u),
        1700 => Ok(RampTime::Ramp1700u),
        3400 => Ok(RampTime::Ramp3400u),
        _ => Err(Error::InvalidConfig),
    }
}
