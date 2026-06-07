//! Seeed Wio-SX1262 radio owner for Bifrost ProS3.
//!
//! This module owns the SX1262's host interface — SPI2 (SCK/MOSI/MISO),
//! `NSS`, `RESET`, `BUSY`, `DIO1`, and the module-level `RF_SW` control —
//! and exposes a cooperative service loop modelled on the Heltec V4
//! `CooperativeRadioOwner` in `muninn_gate_platform_esp32::radio`. The
//! orchestration is mirrored rather than reused because the platform crate's
//! `BoardMeshRadio` type alias is hardwired to the Heltec FEM (`HeltecV4Fem`);
//! the Bifrost board uses the Wio-SX1262's on-module RF switch. The SX1262's
//! `DIO2` drives the TX side internally, while the module's exported `RF_SW`
//! input must be driven HIGH in RX mode and LOW before TX.
//!
//! Pin assignments (see `docs/boards/bifrost_pros3.md`):
//!
//! | Signal       | GPIO |
//! |--------------|------|
//! | SCK          | 12   |
//! | MOSI         | 14   |
//! | MISO         | 13   |
//! | NSS          | 38   |
//! | BUSY         | 39   |
//! | DIO1         | 40   |
//! | RESET        | 41   |
//! | RF_SW/ANT_SW | 21   |
//!
//! SCK/MOSI/MISO are on GPIO 12/13/14, NOT 35/36/37 — the latter are the
//! ProS3 N16R8's octal-PSRAM pins and unusable as general I/O.
//!
//! `RF_SW` is initialized HIGH so the receiver path is enabled during reset
//! and RX. It is driven LOW for TX; leaving it HIGH while transmitting keeps
//! the module biased toward receive mode and makes adverts disappear.
//!
//! Lifecycle:
//!
//! 1. [`RawLoraPeripherals`] is stored on `BoardServices` at boot; nothing touches the SX1262 yet.
//! 2. Once the gateway config has been loaded (so we know the LoRa frequency, SF/BW/CR, sync word,
//!    and TX power), the runtime calls [`BifrostRadioOwner::init`] which constructs SPI2, the
//!    SX126x driver, and the [`MeshRadio`] wrapper. Init runs the SX1262 boot reset, image
//!    calibration, and modulation/packet/TX param commands.
//! 3. Each pass through the main poll loop calls [`BifrostRadioOwner::service`] which (a) starts
//!    continuous RX once [`RX_START_DELAY_MS`] has elapsed, (b) drains any received frame into the
//!    shared RX queue, and (c) pops a queued TX frame and transmits it, respecting an airtime
//!    budget.

use embassy_futures::block_on;
use embedded_hal_bus::spi::ExclusiveDevice;
use esp_hal::Blocking;
use esp_hal::delay::Delay;
use esp_hal::gpio::{Input, InputConfig, Level, Output, OutputConfig, Pull};
use esp_hal::peripherals::{GPIO12, GPIO13, GPIO14, GPIO21, GPIO38, GPIO39, GPIO40, GPIO41, SPI2};
use esp_hal::spi::Mode;
use esp_hal::spi::master::{Config as SpiConfig, Spi};
use esp_hal::time::{Instant, Rate};
use muninn_gate_core::TxPowerMapping;
use muninn_gate_platform_esp32::radio::{
    RADIO_AIRTIME_BUDGET_FACTOR,
    RADIO_REINIT_ERROR_THRESHOLD,
    dequeue_tx_frame,
    enqueue_rx_frame,
    register_gateway_radio_host,
    requeue_tx_frame_front,
};
use muninn_gate_platform_esp32::telemetry_state;
/// SPI clock for the Bifrost Wio-SX1262 bus. Keep the prototype deliberately
/// slow: the current hand-wired board shows patterned MISO read corruption
/// during/after TX at 1 MHz. The PCB path can move back toward the SX1262's
/// normal multi-MHz rates.
const BIFROST_LORA_SPI_HZ: u32 = 250_000;
/// Bifrost starts RX immediately because WiFi association is deliberately
/// delayed until after the boot splash. Entering RX first also exercises the
/// TCXO/PLL path before the first advert TX, which is more reliable on the
/// Wio-SX1262 than transmitting straight out of cold standby.
const BIFROST_RX_START_DELAY_MS: u64 = 0;
/// How many times the owner will preserve and retry the same queued frame
/// inside the active MeshCore response window after TX/reinit trouble.
const TX_FRAME_RETRY_LIMIT: u8 = 8;
/// Small backoff after a TX failure so the hard-reset/reinit path and SX1262
/// BUSY line settle before the frame is tried again.
const TX_RETRY_DELAY_MS: u64 = 250;
/// Same dimming policy as the Bifrost runtime LED writer.
const BIFROST_RGB_DIM_SHIFT: u8 = 4;
use muninn_mesh_radio::{
    MeshRadio,
    MeshRadioConfig,
    MeshRadioError,
    MeshRadioTxStatus,
    RadioClock,
    RadioSwitch,
};
use muninn_mesh_sx126x::{NoAntenna, SX126x};

use crate::status_led::{Rgb, StatusLed, operational_polling_color};

/// Concrete SPI device type for the SX126x — `Spi` bus + NSS output + Delay.
type LoraSpiDevice = ExclusiveDevice<Spi<'static, Blocking>, Output<'static>, Delay>;

/// Concrete `MeshRadio` instantiation for the Bifrost ProS3 / Wio-SX1262.
///
/// The SX126x driver's optional antenna pin is unused. Bifrost controls the
/// Wio module's exported `RF_SW` through [`BifrostRfSwitch`]: HIGH for RX, LOW
/// for TX. The SX1262's `DIO2` remains enabled by the generic config and
/// selects the module's TX path internally.
pub type BifrostMeshRadio = MeshRadio<
    LoraSpiDevice,
    Output<'static>,
    Input<'static>,
    NoAntenna,
    Input<'static>,
    BifrostRfSwitch,
    BifrostRadioClock,
>;

struct BifrostPollLed
{
    led:                  StatusLed,
    entry_green_until_ms: Option<u64>,
    last_color:           Option<Rgb>,
}

pub struct BifrostRadioClock
{
    poll_led: Option<BifrostPollLed>,
}

impl BifrostRadioClock
{
    const fn new() -> Self
    {
        Self { poll_led: None }
    }

    pub fn attach_poll_led(&mut self, led: StatusLed, entry_green_until_ms: Option<u64>)
    {
        self.poll_led = Some(BifrostPollLed {
            led,
            entry_green_until_ms,
            last_color: None,
        });
        self.service_poll_led();
    }

    pub fn detach_poll_led(&mut self) -> Option<StatusLed>
    {
        self.poll_led.take().map(|state| state.led)
    }

    pub fn service_poll_led(&mut self)
    {
        let now_ms = Instant::now().duration_since_epoch().as_millis();
        let Some(state) = self.poll_led.as_mut() else {
            return;
        };
        let color = operational_polling_color(now_ms, state.entry_green_until_ms)
            .dim(BIFROST_RGB_DIM_SHIFT);
        if state.last_color == Some(color) {
            return;
        }
        if state.led.set_color(color).is_err() {
            esp_println::println!(
                "RGB: radio-clock polling set_color failed at now_ms={}; LED disabled",
                now_ms,
            );
            self.poll_led = None;
            return;
        }
        if let Some(state) = self.poll_led.as_mut() {
            state.last_color = Some(color);
        }
    }
}

impl RadioClock for BifrostRadioClock
{
    fn now_ms(&self) -> u64
    {
        Instant::now().duration_since_epoch().as_millis()
    }

    fn delay_ms(&mut self, delay_ms: u32)
    {
        if self.poll_led.is_none() {
            Delay::new().delay_millis(delay_ms);
            return;
        }
        let delay = Delay::new();
        for _ in 0..delay_ms {
            self.service_poll_led();
            delay.delay_millis(1);
        }
        self.service_poll_led();
    }
}

/// Wio-SX1262 exported RF_SW input.
///
/// Seeed's module datasheet describes this pin as receiver-mode enable:
/// HIGH enables RX mode and LOW is used otherwise. TX selection itself is
/// handled by SX1262 DIO2, which the generic radio configuration enables via
/// `SetDio2AsRfSwitchCtrl`.
pub struct BifrostRfSwitch
{
    rf_sw: Output<'static>,
}

impl BifrostRfSwitch
{
    /// Construct the switch pin, initially HIGH for receiver mode.
    fn new(rf_sw: Output<'static>) -> Self
    {
        Self { rf_sw }
    }
}

impl RadioSwitch for BifrostRfSwitch
{
    fn set_rx(&mut self) -> Result<(), MeshRadioError>
    {
        self.rf_sw.set_high();
        Ok(())
    }

    fn set_tx(&mut self) -> Result<(), MeshRadioError>
    {
        self.rf_sw.set_low();
        Ok(())
    }
}

/// Errors returned from [`BifrostRadioOwner::init`].
#[derive(Debug, Clone, Copy)]
pub enum BifrostRadioError
{
    /// esp-hal rejected the SPI bus configuration.
    SpiInit,
    /// `ExclusiveDevice` wrapping the SPI bus + NSS failed.
    SpiDevice,
    /// `MeshRadio::init` failed — typically reset/BUSY didn't release, the
    /// SX1262 didn't respond on SPI, or image calibration failed.
    RadioInit,
}

/// Raw LoRa peripherals stashed at boot and consumed by the runtime once
/// the gateway config is loaded. Keeping them as raw peripherals (rather
/// than partially-constructed driver pieces) defers SPI clock generation
/// until we know the radio config — which keeps the bus quiet during the
/// WiFi/UI bring-up window.
pub struct RawLoraPeripherals
{
    /// SPI2 controller backing the LoRa bus.
    pub spi2:  SPI2<'static>,
    /// SPI clock pin (GPIO 12). Moved off the original GPIO 36 — that
    /// pin is bonded to the ProS3's octal PSRAM (see module docs).
    pub sck:   GPIO12<'static>,
    /// SPI MOSI pin (GPIO 14). Moved off GPIO 35 (octal PSRAM pin).
    pub mosi:  GPIO14<'static>,
    /// SPI MISO pin (GPIO 13). Moved off GPIO 37 (octal PSRAM pin).
    pub miso:  GPIO13<'static>,
    /// SX1262 chip-select pin (GPIO 38).
    pub nss:   GPIO38<'static>,
    /// SX1262 BUSY pin (GPIO 39).
    pub busy:  GPIO39<'static>,
    /// SX1262 DIO1 (IRQ) pin (GPIO 40).
    pub dio1:  GPIO40<'static>,
    /// SX1262 NRST pin (GPIO 41).
    pub reset: GPIO41<'static>,
    /// Wio-SX1262 module RF_SW pin (GPIO 21). HIGH enables the module RX
    /// path; TX drives it LOW while SX1262 DIO2 selects the transmit path.
    pub rf_sw: GPIO21<'static>,
}

/// Cooperative SX1262/MeshCore owner for the Bifrost board. Mirrors the
/// `CooperativeRadioOwner` pattern from `muninn_gate_platform_esp32::radio`:
/// a single owner serviced inside the main poll loop, with delayed RX start
/// and an airtime-budgeted TX dispatcher.
pub struct BifrostRadioOwner
{
    /// Initialized MeshCore radio wrapper around the SX1262 driver.
    radio:              BifrostMeshRadio,
    /// Board mapping for current TX power; published with radio health.
    tx_power:           TxPowerMapping,
    /// Whether continuous RX has been started yet.
    rx_started:         bool,
    /// Wall-clock (ms) at which RX continuous should be enabled — delayed
    /// past boot so WiFi DHCP doesn't fight the radio for the first window.
    rx_start_ms:        u64,
    /// Wall-clock (ms) earliest next TX may begin; advanced by
    /// `last_tx_airtime_ms * RADIO_AIRTIME_BUDGET_FACTOR` after every send.
    next_tx_time_ms:    u64,
    /// Wall-clock (ms) of last health sample.
    last_health_ms:     u64,
    /// Consecutive RX/TX failures; reset on success. When the count reaches
    /// [`RADIO_REINIT_ERROR_THRESHOLD`] the radio is reinitialized in place.
    consecutive_errors: u8,
    /// Retry attempts already spent on the currently front-queued TX frame.
    tx_retry_attempts:  u8,
}

impl BifrostRadioOwner
{
    /// Construct SPI2, the SX126x driver, and the [`MeshRadio`] wrapper
    /// using the gateway-derived radio config. Registers the radio host
    /// callbacks so [`MeshRadio`] internals can read the TX power level.
    pub fn init(
        pins: RawLoraPeripherals,
        radio_config: MeshRadioConfig,
        tx_power: TxPowerMapping,
        now_ms: u64,
    ) -> Result<Self, BifrostRadioError>
    {
        register_gateway_radio_host(tx_power);

        let spi_bus = Spi::new(
            pins.spi2,
            SpiConfig::default()
                .with_frequency(Rate::from_hz(BIFROST_LORA_SPI_HZ))
                .with_mode(Mode::_0),
        )
        .map_err(|_| BifrostRadioError::SpiInit)?
        .with_sck(pins.sck)
        .with_mosi(pins.mosi)
        .with_miso(pins.miso);

        let nss = Output::new(pins.nss, Level::High, OutputConfig::default());
        let reset = Output::new(pins.reset, Level::High, OutputConfig::default());
        // BUSY/DIO1 without internal pulls — the SX1262 actively drives
        // both lines, so a pull would only fight production traffic.
        let busy = Input::new(pins.busy, InputConfig::default().with_pull(Pull::None));
        let dio1 = Input::new(pins.dio1, InputConfig::default().with_pull(Pull::None));
        // RF_SW high enables the module's RX path. It must be pulled low
        // before TX; BifrostRfSwitch owns that transition.
        let rf_sw = Output::new(pins.rf_sw, Level::High, OutputConfig::default());

        let spi_device = ExclusiveDevice::new(spi_bus, nss, Delay::new())
            .map_err(|_| BifrostRadioError::SpiDevice)?;
        let device = SX126x::new_without_ant(spi_device, (reset, busy, dio1));
        let rf_switch = BifrostRfSwitch::new(rf_sw);

        let radio = match block_on(MeshRadio::init(
            device,
            rf_switch,
            BifrostRadioClock::new(),
            radio_config,
        )) {
            Ok(radio) => radio,
            Err(e) => {
                esp_println::println!("LoRa: MeshRadio::init failed at {:?}", e);
                return Err(BifrostRadioError::RadioInit);
            },
        };

        esp_println::println!(
            "LoRa: MeshRadio init OK (standby; RX start delay {}ms)",
            BIFROST_RX_START_DELAY_MS,
        );

        Ok(Self {
            radio,
            tx_power,
            rx_started: false,
            rx_start_ms: now_ms.saturating_add(BIFROST_RX_START_DELAY_MS),
            next_tx_time_ms: now_ms,
            last_health_ms: now_ms,
            consecutive_errors: 0,
            tx_retry_attempts: 0,
        })
    }

    /// Service one bounded radio pass. Mirrors `CooperativeRadioOwner::service`
    /// in the platform crate: starts continuous RX after the configured
    /// delay, drains any received frame into the shared RX queue, and
    /// dispatches a single queued TX frame while respecting the airtime
    /// budget.
    pub fn service(&mut self, now_ms: u64)
    {
        self.radio.clock_mut().service_poll_led();

        if !self.rx_started && now_ms >= self.rx_start_ms {
            match self.radio.start_rx() {
                Ok(()) => {
                    self.rx_started = true;
                    esp_println::println!("LoRa: RX continuous started");
                },
                Err(_) => {
                    esp_println::println!("LoRa: RX continuous start failed");
                },
            }
        }

        // Health sample (RSSI ring, device errors) at 1 Hz. We don't
        // publish these to the platform telemetry store yet — that lands
        // with the polling milestone — but sampling keeps the noise-floor
        // ring fresh for when we do.
        if now_ms.saturating_sub(self.last_health_ms) >= 1_000 {
            self.last_health_ms = now_ms;
            self.radio.sample_health();
            self.publish_health(now_ms);
        }

        if self.rx_started {
            match block_on(self.radio.poll_receive((now_ms / 1_000) as u32)) {
                Ok(Some(frame)) => {
                    self.consecutive_errors = 0;
                    esp_println::println!(
                        "LoRa: RX frame len={} rssi={} snr={} src={:08X} dst={:08X} kind=0x{:02X} \
                         hash={:08X}",
                        frame.raw_len,
                        frame.message.rssi_dbm,
                        frame.message.snr_tenth_db,
                        frame.message.src,
                        frame.message.dst,
                        frame.message.kind,
                        frame.raw_hash,
                    );
                    self.publish_health(now_ms);
                    if enqueue_rx_frame(frame).is_err() {
                        esp_println::println!("LoRa: RX queue full — dropping frame");
                    }
                },
                Ok(None) => {
                    self.consecutive_errors = 0;
                },
                Err(_) => self.note_io_error("RX poll failed"),
            }
        }

        // Gate TX on RX continuous having been entered at least once.
        // Empirically the SX1262 / Wio-SX1262 setup is unreliable when
        // SetTx is called from a pure cold-boot StbyRC — the chip
        // mode reads back as 0x00 and SetTx never transitions us into
        // TX. After at least one RX continuous entry the PLL / TCXO
        // path is exercised and subsequent TX commands work cleanly.
        if self.rx_started
            && now_ms >= self.next_tx_time_ms
            && let Some(frame) = dequeue_tx_frame()
        {
            let payload_len = frame.payload_slice().len();
            self.radio.sample_health();
            let pre_chip_mode = self.radio.rx_stats().chip_mode;
            esp_println::println!(
                "LoRa: TX attempt ({}B payload, pre-TX chip_mode=0x{:02X})",
                payload_len,
                pre_chip_mode,
            );
            match block_on(self.radio.transmit(&frame)) {
                Ok(MeshRadioTxStatus::Sent) => {
                    self.consecutive_errors = 0;
                    self.tx_retry_attempts = 0;
                    let raw_irq = self.radio.last_tx_irq_raw();
                    esp_println::println!(
                        "LoRa: TX sent ({}ms airtime, raw_irq=0x{:04X})",
                        self.radio.last_tx_airtime_ms,
                        raw_irq,
                    );
                    self.next_tx_time_ms = now_ms.saturating_add(
                        u64::from(self.radio.last_tx_airtime_ms)
                            .saturating_mul(u64::from(RADIO_AIRTIME_BUDGET_FACTOR)),
                    );
                },
                Err(e) => {
                    self.radio.sample_health();
                    let stats = self.radio.rx_stats();
                    let raw_irq = self.radio.last_tx_irq_raw();
                    esp_println::println!(
                        "LoRa: TX error {:?} (raw_irq=0x{:04X} post-TX chip_mode=0x{:02X} \
                         dev_err=0x{:04X} last_tx_dbm={:?}) — reinitializing chip",
                        e,
                        raw_irq,
                        stats.chip_mode,
                        stats.device_errors,
                        stats.last_applied_tx_power_dbm,
                    );
                    // Force a hard chip reset + re-init after TX trouble.
                    // The Wio-SX1262 on Bifrost can drop into an
                    // unresponsive state after a TX timeout (GetStatus
                    // returns 0x00 thereafter), and a soft recovery doesn't
                    // bring it back. Preserve the frame for a bounded number
                    // of retries so one bad SPI/IRQ read doesn't waste the
                    // whole MeshCore request window.
                    if block_on(self.radio.reinit()).is_err() {
                        esp_println::println!("LoRa: reinit after TX failure failed");
                        self.rx_started = false;
                    } else {
                        esp_println::println!("LoRa: reinit OK after TX failure");
                        self.rx_started = true;
                    }
                    if self.tx_retry_attempts < TX_FRAME_RETRY_LIMIT {
                        self.tx_retry_attempts = self.tx_retry_attempts.saturating_add(1);
                        match requeue_tx_frame_front(frame) {
                            Ok(()) => {
                                self.next_tx_time_ms = now_ms.saturating_add(TX_RETRY_DELAY_MS);
                                esp_println::println!(
                                    "LoRa: TX frame requeued for retry {}/{}",
                                    self.tx_retry_attempts,
                                    TX_FRAME_RETRY_LIMIT,
                                );
                            },
                            Err(_) => {
                                esp_println::println!(
                                    "LoRa: TX retry requeue failed; dropping frame"
                                );
                                self.tx_retry_attempts = 0;
                            },
                        }
                    } else {
                        esp_println::println!("LoRa: TX retry limit reached; dropping frame");
                        self.tx_retry_attempts = 0;
                    }
                    self.consecutive_errors = 0;
                },
            }
        }

        self.radio.clock_mut().service_poll_led();
    }

    pub fn attach_poll_led(&mut self, led: StatusLed, entry_green_until_ms: Option<u64>)
    {
        self.radio
            .clock_mut()
            .attach_poll_led(led, entry_green_until_ms);
    }

    pub fn detach_poll_led(&mut self) -> Option<StatusLed>
    {
        self.radio.clock_mut().detach_poll_led()
    }

    /// `true` once continuous RX has been started — the runtime uses this
    /// to flip the UI / LED out of "radio not yet listening" state.
    pub fn is_listening(&self) -> bool
    {
        self.rx_started
    }

    fn note_io_error(&mut self, message: &str)
    {
        self.consecutive_errors = self.consecutive_errors.saturating_add(1);
        esp_println::println!(
            "LoRa: {} (consecutive errors {})",
            message,
            self.consecutive_errors,
        );
        if self.consecutive_errors >= RADIO_REINIT_ERROR_THRESHOLD {
            esp_println::println!("LoRa: reinitializing after repeated errors");
            if block_on(self.radio.reinit()).is_err() {
                esp_println::println!("LoRa: reinit failed");
                self.rx_started = false;
            } else {
                self.rx_started = true;
            }
            self.consecutive_errors = 0;
        }
    }

    fn publish_health(&mut self, now_ms: u64)
    {
        let stats = self.radio.rx_stats();
        telemetry_state::update_radio_health(
            now_ms,
            self.tx_power,
            stats,
            self.radio.last_tx_airtime_ms,
        );
    }
}
