//! `BoardServices` — single facade over every Bifrost ProS3 peripheral
//! the bring-up runner cares about.
//!
//! Boot order is deliberate and documented inline:
//!
//! 1. Antenna RF switch — route 2.4 GHz to the chosen path **before** any radio brings up.
//! 2. LDO2 enable — power the STEMMA + RGB rail (the OLED and fuel gauge are unaddressable while
//!    this is LOW).
//! 3. Shared I2C bus — `I2C0` (SDA = GPIO 8, SCL = GPIO 9), 400 kHz, parked in a `'static`
//!    `RefCell` so multiple drivers can borrow it.
//! 4. MAX17048 fuel gauge — quick-start sent so SOC is anchored to the live cell voltage.
//! 5. SSD1327 display — init sequence + clear; the framebuffer lives in a `'static` cell, not on
//!    the runner's stack.
//! 6. RGB LED — WS2812 over RMT, set to OFF until the poll loop drives it.
//! 7. WiFi scanner — esp-wifi station mode, scan-only (no association).
//!
//! Each capability is `Option<_>` so a missing/broken peripheral doesn't
//! take down the rest of the bring-up. The runner short-circuits work that
//! depends on a missing capability.

use core::mem::MaybeUninit;

use esp_hal::delay::Delay;
use esp_hal::gpio::{Input, InputConfig, Level, Output, OutputConfig, Pull};
use esp_hal::i2c::master::{Config as I2cConfig, I2c};
use esp_hal::peripherals::{GPIO15, GPIO16, Peripherals};
use esp_hal::time::Rate;
use esp_hal::usb_serial_jtag::UsbSerialJtag;
use muninn_driver_ds3231::Ds3231;
use muninn_driver_max17048::Max17048;
use muninn_driver_ssd1327::{self as display, FRAMEBUFFER_LEN, Ssd1327};
use muninn_gate_core::{DiagnosticLevel, DiagnosticSubsystem, GateFirmwareVariant};
use muninn_gate_platform_esp32::diagnostics;
use muninn_gate_platform_esp32::http_server::HttpServer;
use muninn_gate_platform_esp32::serial::UsbJsonReceiver;
use muninn_gate_platform_esp32::storage::Esp32GatewayConfig;
use muninn_gate_time::GatewayTime;
use muninn_gate_utils::i2c_bus::{self, SharedI2cBus, SharedI2cDevice};
use static_cell::StaticCell;

use crate::antenna::{AntennaPath, AntennaSwitch};
use crate::lora::{BifrostRadioOwner, RawLoraPeripherals};
use crate::power::Ldo2Rail;
use crate::status_led::{Rgb, StatusLed};
use crate::wifi::WifiScanner;

/// Antenna routing applied at boot.
///
/// Defaults to the on-board 3D antenna — works without any extra hardware
/// and is safe to TX into. Flip to [`AntennaPath::ExternalUfl`] only when
/// an external antenna is physically attached to the u.FL connector;
/// radiating into an unloaded u.FL pad attenuates RX *and* degrades TX
/// enough that WPA2 association fails even when scan results look strong.
const BOOT_ANTENNA_PATH: AntennaPath = AntennaPath::ExternalUfl;

/// Delay (ms) between asserting LDO2 enable and trusting downstream I2C.
const LDO2_SETTLE_MS: u32 = 50;
/// Delay (ms) the MAX17048 datasheet asks for after a quick-start.
const FUEL_GAUGE_QUICK_START_SETTLE_MS: u32 = 1_500;
/// DRAM2 heap segment — sized to host esp-wifi DMA descriptors + driver
/// state alongside the display framebuffer and other allocator clients.
const BIFROST_DRAM2_HEAP_BYTES: usize = 70 * 1024;
/// Regular DRAM heap spillover for small allocations.
const BIFROST_DRAM_HEAP_BYTES: usize = 32 * 1024;
/// Minimum interval between accepted GPIO15 button presses.
const POLL_BUTTON_DEBOUNCE_MS: u64 = 250;
/// Active buzzer enable pulse. Keep short so the feedback stays quiet.
const BUZZER_PULSE_MS: u32 = 7;

/// Aliased fuel gauge type bound to the shared bus.
pub type BifrostBattery = Max17048<SharedI2cDevice>;
/// Aliased display type bound to the shared bus.
pub type BifrostDisplay = Ssd1327<SharedI2cDevice>;
/// Aliased RTC type bound to the shared bus.
pub type BifrostRtc = Ds3231<SharedI2cDevice>;

/// Active-low GPIO15 operator button used to request an immediate producer poll.
pub struct PollButton
{
    pin:             Input<'static>,
    was_pressed:     bool,
    last_pressed_ms: u64,
}

impl PollButton
{
    fn new(pin: GPIO15<'static>) -> Self
    {
        Self {
            pin:             Input::new(pin, InputConfig::default().with_pull(Pull::Up)),
            was_pressed:     false,
            last_pressed_ms: 0,
        }
    }

    /// Return true once for each debounced press.
    pub fn poll_pressed(&mut self, now_ms: u64) -> bool
    {
        let pressed = self.pin.is_low();
        let accepted = pressed
            && !self.was_pressed
            && now_ms.saturating_sub(self.last_pressed_ms) >= POLL_BUTTON_DEBOUNCE_MS;

        self.was_pressed = pressed;
        if accepted {
            self.last_pressed_ms = now_ms;
        }
        accepted
    }
}

/// GPIO16 active-buzzer enable output for operator feedback.
pub struct QuietBuzzer
{
    pin: Output<'static>,
}

impl QuietBuzzer
{
    fn new(pin: GPIO16<'static>) -> Self
    {
        Self {
            pin: Output::new(pin, Level::Low, OutputConfig::default()),
        }
    }

    /// Emit a short enable pulse. Intended for an active buzzer module:
    /// module VCC to 3V3, GND to GND, IO to GPIO16.
    pub fn chirp(&mut self)
    {
        let delay = Delay::new();
        self.pin.set_high();
        delay.delay_millis(BUZZER_PULSE_MS);
        self.pin.set_low();
    }
}

/// Single owner over every Bifrost peripheral exposed to the runner.
///
/// Each `Option<_>` is `Some(_)` when the underlying peripheral came up
/// cleanly. Failed peripherals leave their slot `None` and the runner
/// degrades gracefully (no display, no LED, etc.).
pub struct BoardServices
{
    /// 2.4 GHz RF switch (GPIO 11).
    pub antenna:       AntennaSwitch,
    /// MAX17048 LiPo fuel gauge on the shared STEMMA I2C bus.
    pub battery:       Option<BifrostBattery>,
    /// SSD1327 grayscale OLED on the shared STEMMA I2C bus.
    pub display:       Option<BifrostDisplay>,
    /// Optional DS3231 RTC on the shared STEMMA I2C bus.
    pub rtc:           Option<BifrostRtc>,
    /// WS2812 RGB LED driven via the RMT peripheral on GPIO 18.
    pub status_led:    Option<StatusLed>,
    /// esp-wifi station controller, scan-only for now.
    pub wifi:          Option<WifiScanner>,
    /// GPIO15 active-low push button for on-demand producer polling.
    pub poll_button:   PollButton,
    /// GPIO16 active-buzzer output for accepted button presses.
    pub buzzer:        QuietBuzzer,
    /// Raw LoRa peripherals captured at boot. Consumed by
    /// [`Self::init_lora`] once the gateway config has been loaded so we
    /// know the radio frequency, SF/BW/CR, sync word, and TX power
    /// before bringing SPI2 up. `None` after a successful
    /// `init_lora` (the SX1262 driver now owns the pins).
    pub lora_pending:  Option<RawLoraPeripherals>,
    /// Initialized SX1262 owner — `Some(_)` after a successful
    /// `init_lora`. The runtime threads `lora_owner.is_some()` into
    /// `ui_state.lora_available` so the operational screen renders
    /// producer rows instead of the "radio not wired" warning panel.
    pub lora_owner:    Option<BifrostRadioOwner>,
    /// Shared platform HTTP server. `None` until WiFi associates +
    /// DHCP succeeds, at which point the runtime takes the WifiDevice
    /// off the scanner and hands it to a fresh [`HttpServer`].
    pub http_server:   Option<HttpServer>,
    /// Most recently applied gateway configuration. Held here so the
    /// HTTP server can authenticate requests against the configured
    /// bearer tokens and the runtime can re-render the operational
    /// screen from the latest source of truth.
    pub active_config: Option<Esp32GatewayConfig>,
    /// USB VBUS sense (GPIO 33). HIGH while 5 V is present on the USB-C
    /// connector — i.e. the board is plugged into a host and the on-board
    /// charger is supplying current to the LiPo.
    vbus_sense:        Input<'static>,
    /// Receive side of the native USB-Serial/JTAG endpoint, wrapped in the
    /// shared JSON-document parser. `esp-println` writes to the same
    /// controller via direct register access, so the TX side stays
    /// implicit and we only own the reader here.
    pub usb_rx:        Option<UsbJsonReceiver>,
    /// LDO2 enable line — held for its side effect (don't drop until
    /// shutdown, or the STEMMA + RGB rail loses power).
    _ldo2:             Ldo2Rail,
}

impl BoardServices
{
    /// Initialize every ProS3 board service. Consumes the entire HAL
    /// peripheral block.
    pub fn init(peripherals: Peripherals) -> Self
    {
        init_heap();

        // 1. Antenna routing — always before any radio init. See the `BOOT_ANTENNA_PATH` const for
        //    the "external u.FL by default" rationale.
        let mut antenna = AntennaSwitch::new(peripherals.GPIO11);
        antenna.select(BOOT_ANTENNA_PATH);

        // 2. LDO2 — STEMMA + RGB rail.
        let ldo2 = Ldo2Rail::enabled(peripherals.GPIO17);
        let settle = Delay::new();
        settle.delay_millis(LDO2_SETTLE_MS);

        // 3. Shared I2C bus + 4. fuel gauge + 5. display. Keep the breadboard harness at Fast Mode;
        //    800 kHz flushed faster, but field capture showed occasional SSD1327 transfer failures
        //    while LoRa TX/reinit was also active.
        static SHARED_I2C: StaticCell<SharedI2cBus> = StaticCell::new();
        let i2c_bus = match I2c::new(
            peripherals.I2C0,
            I2cConfig::default().with_frequency(Rate::from_khz(400)),
        ) {
            Ok(bus) => Some(i2c_bus::install_bus(
                &SHARED_I2C,
                bus.with_sda(peripherals.GPIO8).with_scl(peripherals.GPIO9),
            )),
            Err(_) => {
                esp_println::println!("I2C0: shared bus init failed; battery + display disabled");
                None
            },
        };
        let battery = i2c_bus
            .map(i2c_bus::device)
            .map(|dev| init_battery(dev, &settle));
        let rtc = i2c_bus.and_then(|bus| init_rtc(i2c_bus::device(bus)));
        let display = i2c_bus.and_then(|bus| init_display(i2c_bus::device(bus)));

        // 6. RGB LED.
        let status_led = init_status_led(peripherals.RMT, peripherals.GPIO18);

        // 7. WiFi scanner.
        let wifi = init_wifi(peripherals.TIMG1, peripherals.RNG, peripherals.WIFI);

        // 8. Active-low operator button. Wire GPIO15 to GND through a momentary push button;
        //    internal pull-up holds idle HIGH.
        let poll_button = PollButton::new(peripherals.GPIO15);

        // 9. Active buzzer output. Wire buzzer VCC=3V3, GND=GND, IO=GPIO16.
        let buzzer = QuietBuzzer::new(peripherals.GPIO16);

        // 10. USB 5 V presence — used by the UI to render a charging icon.
        let vbus_sense = Input::new(peripherals.GPIO33, InputConfig::default());

        // 11. USB-Serial/JTAG receive side for host provisioning. The write side is implicit:
        //    `esp-println` writes to the same controller via direct register access without owning
        //    the peripheral, and the host tool (`tools/cli.py`) drives the upload handshake.
        let usb_rx = Some(UsbJsonReceiver::new(UsbSerialJtag::new(
            peripherals.USB_DEVICE,
        )));

        // 12. LoRa peripherals captured for later init. The SX1262 bring-up needs the gateway radio
        //     config (frequency, SF/BW/CR, sync word, TX power) which isn't known until
        //     `storage::load_config` (or USB provisioning) hands us a valid config. The runner
        //     calls `init_lora` once that happens.
        let lora_pending = Some(RawLoraPeripherals {
            spi2:  peripherals.SPI2,
            // SCK/MOSI/MISO moved off GPIO 36/35/37 — those are the
            // ProS3's octal-PSRAM data/strobe pins (SPIIO6/SPIIO7/
            // SPIDQS on the N16R8 WROOM-1) and are NOT usable as
            // general I/O; using them gave intermittent then dead SPI
            // reads. GPIO 12/13/14 are confirmed-free pins.
            sck:   peripherals.GPIO12,
            mosi:  peripherals.GPIO14,
            miso:  peripherals.GPIO13,
            nss:   peripherals.GPIO38,
            busy:  peripherals.GPIO39,
            dio1:  peripherals.GPIO40,
            reset: peripherals.GPIO41,
            rf_sw: peripherals.GPIO21,
        });

        Self {
            antenna,
            battery,
            display,
            rtc,
            status_led,
            wifi,
            poll_button,
            buzzer,
            lora_pending,
            lora_owner: None,
            http_server: None,
            active_config: None,
            vbus_sense,
            usb_rx,
            _ldo2: ldo2,
        }
    }

    /// Consume the pending LoRa peripherals and bring the SX1262 up
    /// with the radio settings derived from the gateway config. No-op
    /// if init has already succeeded or the pending peripherals were
    /// never captured.
    pub fn init_lora(
        &mut self,
        radio_config: muninn_mesh_radio::MeshRadioConfig,
        tx_power: muninn_gate_core::TxPowerMapping,
        now_ms: u64,
    )
    {
        if self.lora_owner.is_some() {
            return;
        }
        let Some(pins) = self.lora_pending.take() else {
            return;
        };
        match BifrostRadioOwner::init(pins, radio_config, tx_power, now_ms) {
            Ok(owner) => {
                esp_println::println!(
                    "LoRa: Wio-SX1262 ready (SPI2 SCK=12 MOSI=14 MISO=13 NSS=38 BUSY=39 DIO1=40 \
                     RESET=41 RF_SW=21)"
                );
                self.lora_owner = Some(owner);
            },
            Err(e) => {
                esp_println::println!("LoRa: init failed ({:?})", e);
            },
        }
    }

    /// Seed wall time from an optional RTC at boot.
    pub fn init_wall_time_from_rtc(&mut self, clock: &mut GatewayTime, now_ms: u64) -> bool
    {
        let Some(rtc) = self.rtc.as_mut() else {
            return false;
        };
        match rtc.read_unix_seconds() {
            Ok(unix_seconds) => {
                clock.set_unix_seconds(unix_seconds, now_ms);
                esp_println::println!("RTC: seeded wall time unix={}", unix_seconds);
                record_rtc_diagnostic(
                    now_ms,
                    DiagnosticLevel::Info,
                    "rtc_read_ok",
                    "RTC wall time seeded from DS3231",
                );
                true
            },
            Err(error) => {
                esp_println::println!("RTC: read failed ({:?})", error);
                record_rtc_diagnostic(
                    now_ms,
                    DiagnosticLevel::Warn,
                    "rtc_read_failed",
                    "RTC read failed",
                );
                false
            },
        }
    }

    /// Persist freshly provisioned wall time to an optional RTC.
    pub fn update_rtc_from_wall_time(&mut self, clock: &GatewayTime, now_ms: u64) -> bool
    {
        let Some(rtc) = self.rtc.as_mut() else {
            return false;
        };
        let Some(unix_seconds) = clock.unix_seconds(now_ms) else {
            return false;
        };
        match rtc.write_unix_seconds(unix_seconds) {
            Ok(()) => {
                esp_println::println!("RTC: wrote unix={}", unix_seconds);
                record_rtc_diagnostic(
                    now_ms,
                    DiagnosticLevel::Info,
                    "rtc_write_ok",
                    "RTC wall time written to DS3231",
                );
                true
            },
            Err(error) => {
                esp_println::println!("RTC: write failed ({:?})", error);
                record_rtc_diagnostic(
                    now_ms,
                    DiagnosticLevel::Warn,
                    "rtc_write_failed",
                    "RTC write failed",
                );
                false
            },
        }
    }

    /// `true` when USB 5 V is currently being delivered to the board.
    pub fn usb_connected(&self) -> bool
    {
        self.vbus_sense.is_high()
    }

    /// Print a one-shot capability banner to USB Serial/JTAG. Folds in the
    /// per-peripheral readiness so a single capture tells the operator
    /// what's healthy and what's not.
    pub fn print_banner<V: GateFirmwareVariant>(&self)
    {
        esp_println::println!("Bifrost Gate boot");
        esp_println::println!(
            "Muninn Gate platform=ESP32-S3 variant={} board=\"{}\"",
            V::NAME,
            V::BOARD,
        );
        esp_println::println!(
            "Capabilities: wifi={} http={} usb_serial={} display={:?}",
            V::CAPABILITIES.wifi,
            V::CAPABILITIES.http_server,
            V::CAPABILITIES.usb_serial,
            V::CAPABILITIES.display,
        );
        esp_println::println!("Antenna: {} (GPIO 11)", self.antenna.path().label());
        esp_println::println!("LDO2: enabled (STEMMA + RGB rail, GPIO 17)");
        esp_println::println!(
            "RGB LED: {}",
            if self.status_led.is_some() {
                "WS2812 on GPIO 18 via RMT channel 0"
            } else {
                "init failed"
            },
        );
        esp_println::println!(
            "Battery: {}",
            if self.battery.is_some() {
                "MAX17048 @ 0x36 via shared I2C0 (SDA=GPIO 8, SCL=GPIO 9, 400 kHz)"
            } else {
                "unavailable"
            },
        );
        esp_println::println!(
            "Display: {}",
            if self.display.is_some() {
                "SSD1327 128x128 4-bit grayscale via shared I2C0 @ 0x3D"
            } else {
                "unavailable"
            },
        );
        esp_println::println!(
            "RTC: {}",
            if self.rtc.is_some() {
                "DS3231 @ 0x68 via shared I2C0 (SDA=GPIO 8, SCL=GPIO 9, 400 kHz)"
            } else {
                "unavailable"
            },
        );
        esp_println::println!(
            "WiFi: {}",
            if self.wifi.is_some() {
                "esp-wifi STA scan-only (15s interval)"
            } else {
                "unavailable"
            },
        );
        esp_println::println!("Poll button: GPIO 15 active-low (press = poll now)");
        esp_println::println!("Buzzer: GPIO 16 active-buzzer pulse on accepted button press");
        esp_println::println!("Status policy: LED tracks SOC% (red 0% → yellow 50% → green 100%)");
        esp_println::println!(
            "LoRa: {}",
            if self.lora_owner.is_some() {
                "Wio-SX1262 ready"
            } else if self.lora_pending.is_some() {
                "Wio-SX1262 peripherals captured (init deferred until config loads)"
            } else {
                "init failed"
            },
        );
    }

    /// Turn the RGB LED fully off. Useful at boot before the policy color
    /// takes over so we don't latch whatever state was on the data line.
    pub fn dim_led_to_off(&mut self)
    {
        if let Some(led) = self.status_led.as_mut() {
            let _ = led.set_color(Rgb::OFF);
        }
    }
}

fn init_heap()
{
    // Storage MUST be declared here (not in utils) so the linker can
    // place HEAP_DRAM2 in the `.dram2_uninit` section. utils::heap
    // provides the register-with-esp-alloc ceremony.
    #[unsafe(link_section = ".dram2_uninit")]
    static mut HEAP_DRAM2: MaybeUninit<[u8; BIFROST_DRAM2_HEAP_BYTES]> = MaybeUninit::uninit();
    static mut HEAP_DRAM: MaybeUninit<[u8; BIFROST_DRAM_HEAP_BYTES]> = MaybeUninit::uninit();
    unsafe {
        muninn_gate_utils::heap::register_dram2_region(&mut *core::ptr::addr_of_mut!(HEAP_DRAM2));
        muninn_gate_utils::heap::register_dram_region(&mut *core::ptr::addr_of_mut!(HEAP_DRAM));
    }
}

fn init_battery(dev: SharedI2cDevice, settle: &Delay) -> BifrostBattery
{
    let mut gauge = Max17048::new(dev);
    match gauge.quick_start() {
        Ok(()) => {
            esp_println::println!("Battery: MAX17048 quick-start sent");
            settle.delay_millis(FUEL_GAUGE_QUICK_START_SETTLE_MS);
        },
        Err(_) => {
            esp_println::println!("Battery: MAX17048 quick-start failed (gauge missing?)");
        },
    }
    gauge
}

fn init_rtc(dev: SharedI2cDevice) -> Option<BifrostRtc>
{
    let mut rtc = Ds3231::new(dev);
    match rtc.init() {
        Ok(()) => {
            esp_println::println!("RTC: DS3231 init OK @ 0x68");
            record_rtc_diagnostic(
                0,
                DiagnosticLevel::Info,
                "rtc_init_ok",
                "DS3231 RTC detected at 0x68",
            );
            Some(rtc)
        },
        Err(e) => {
            esp_println::println!("RTC: DS3231 unavailable ({:?})", e);
            record_rtc_diagnostic(
                0,
                DiagnosticLevel::Warn,
                "rtc_init_failed",
                "DS3231 RTC not detected",
            );
            None
        },
    }
}

fn record_rtc_diagnostic(timestamp_ms: u64, level: DiagnosticLevel, code: &str, message: &str)
{
    let _ = diagnostics::record(
        timestamp_ms,
        level,
        DiagnosticSubsystem::Runtime,
        code,
        message,
    );
}

fn init_display(dev: SharedI2cDevice) -> Option<BifrostDisplay>
{
    static DISPLAY_FB: StaticCell<[u8; FRAMEBUFFER_LEN]> = StaticCell::new();
    let fb = DISPLAY_FB.init([0u8; FRAMEBUFFER_LEN]);
    let mut display = Ssd1327::new(dev, display::DEFAULT_ADDR, fb);
    match display.init() {
        Ok(()) => {
            esp_println::println!(
                "Display: SSD1327 init OK @ 0x{:02x} ({}x{} 4-bit grayscale)",
                display::DEFAULT_ADDR,
                display::WIDTH,
                display::HEIGHT,
            );
            Some(display)
        },
        Err(_) => {
            esp_println::println!(
                "Display: SSD1327 init failed @ 0x{:02x}; UI disabled",
                display::DEFAULT_ADDR,
            );
            None
        },
    }
}

fn init_status_led(
    rmt: esp_hal::peripherals::RMT<'static>,
    data: esp_hal::peripherals::GPIO18<'static>,
) -> Option<StatusLed>
{
    match StatusLed::new(rmt, data) {
        Ok(led) => Some(led),
        Err(_) => {
            esp_println::println!("RGB: StatusLed init failed; LED disabled");
            None
        },
    }
}

fn init_wifi(
    timg1: esp_hal::peripherals::TIMG1<'static>,
    rng: esp_hal::peripherals::RNG<'static>,
    wifi: esp_hal::peripherals::WIFI<'static>,
) -> Option<WifiScanner>
{
    match WifiScanner::new(timg1, rng, wifi) {
        Ok(w) => {
            esp_println::println!("WiFi: scanner up (esp-wifi STA mode, external u.FL antenna)");
            Some(w)
        },
        Err(e) => {
            esp_println::println!("WiFi: scanner init failed: {:?}", e);
            None
        },
    }
}
