//! `BoardServices` — single facade over every Bifrost ProS3 peripheral
//! the bring-up runner cares about.
//!
//! Boot order is deliberate and documented inline:
//!
//! 1. Antenna RF switch — route 2.4 GHz to the chosen path **before** any
//!    radio brings up.
//! 2. LDO2 enable — power the STEMMA + RGB rail (the OLED and fuel gauge
//!    are unaddressable while this is LOW).
//! 3. Shared I2C bus — `I2C0` (SDA = GPIO 8, SCL = GPIO 9), 400 kHz, parked
//!    in a `'static` `RefCell` so multiple drivers can borrow it.
//! 4. MAX17048 fuel gauge — quick-start sent so SOC is anchored to the
//!    live cell voltage.
//! 5. SSD1327 display — init sequence + clear; the framebuffer lives in a
//!    `'static` cell, not on the runner's stack.
//! 6. RGB LED — WS2812 over RMT, set to OFF until the poll loop drives it.
//! 7. WiFi scanner — esp-wifi station mode, scan-only (no association).
//!
//! Each capability is `Option<_>` so a missing/broken peripheral doesn't
//! take down the rest of the bring-up. The runner short-circuits work that
//! depends on a missing capability.

use core::mem::MaybeUninit;

use esp_hal::delay::Delay;
use esp_hal::gpio::{Input, InputConfig};
use esp_hal::peripherals::Peripherals;
use muninn_gate_core::GateFirmwareVariant;
use static_cell::StaticCell;

use super::antenna::{AntennaPath, AntennaSwitch};
use super::battery::Max17048;
use super::display::{self, FRAMEBUFFER_LEN, Ssd1327};
use super::i2c_bus::{self, SharedI2cDevice};
use super::power::Ldo2Rail;
use super::status_led::{Rgb, StatusLed};
use super::wifi::WifiScanner;

/// Antenna routing applied at boot.
///
/// Operators with the external u.FL antenna already attached want the RF
/// switch driven HIGH; this matches the polygon-pros3d reference firmware
/// behaviour. If the gate is deployed with **no** external antenna, flip
/// this to [`AntennaPath::Onboard3D`] (the safe default the antenna
/// module's constructor uses on its own) before flashing — radiating into
/// an unloaded u.FL connector can damage the WiFi front-end.
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

/// Aliased fuel gauge type bound to the shared bus.
pub type BifrostBattery = Max17048<SharedI2cDevice>;
/// Aliased display type bound to the shared bus.
pub type BifrostDisplay = Ssd1327<SharedI2cDevice>;

/// Single owner over every Bifrost peripheral exposed to the runner.
///
/// Each `Option<_>` is `Some(_)` when the underlying peripheral came up
/// cleanly. Failed peripherals leave their slot `None` and the runner
/// degrades gracefully (no display, no LED, etc.).
pub struct BoardServices
{
    /// 2.4 GHz RF switch (GPIO 11).
    pub antenna:    AntennaSwitch,
    /// MAX17048 LiPo fuel gauge on the shared STEMMA I2C bus.
    pub battery:    Option<BifrostBattery>,
    /// SSD1327 grayscale OLED on the shared STEMMA I2C bus.
    pub display:    Option<BifrostDisplay>,
    /// WS2812 RGB LED driven via the RMT peripheral on GPIO 18.
    pub status_led: Option<StatusLed>,
    /// esp-wifi station controller, scan-only for now.
    pub wifi:       Option<WifiScanner>,
    /// USB VBUS sense (GPIO 33). HIGH while 5 V is present on the USB-C
    /// connector — i.e. the board is plugged into a host and the on-board
    /// charger is supplying current to the LiPo.
    vbus_sense:     Input<'static>,
    /// LDO2 enable line — held for its side effect (don't drop until
    /// shutdown, or the STEMMA + RGB rail loses power).
    _ldo2:          Ldo2Rail,
}

impl BoardServices
{
    /// Initialize every ProS3 board service. Consumes the entire HAL
    /// peripheral block.
    pub fn init(peripherals: Peripherals) -> Self
    {
        init_heap();

        // 1. Antenna routing — always before any radio init. See the
        //    `BOOT_ANTENNA_PATH` const for the "external u.FL by default"
        //    rationale.
        let mut antenna = AntennaSwitch::new(peripherals.GPIO11);
        antenna.select(BOOT_ANTENNA_PATH);

        // 2. LDO2 — STEMMA + RGB rail.
        let ldo2 = Ldo2Rail::enabled(peripherals.GPIO17);
        let settle = Delay::new();
        settle.delay_millis(LDO2_SETTLE_MS);

        // 3. Shared I2C bus + 4. fuel gauge + 5. display.
        let i2c_bus =
            match i2c_bus::init(peripherals.I2C0, peripherals.GPIO8, peripherals.GPIO9) {
                Ok(bus) => Some(bus),
                Err(_) => {
                    esp_println::println!(
                        "I2C0: shared bus init failed; battery + display disabled"
                    );
                    None
                },
            };
        let battery = i2c_bus
            .map(i2c_bus::device)
            .map(|dev| init_battery(dev, &settle));
        let display = i2c_bus.and_then(|bus| init_display(i2c_bus::device(bus)));

        // 6. RGB LED.
        let status_led = init_status_led(peripherals.RMT, peripherals.GPIO18);

        // 7. WiFi scanner.
        let wifi = init_wifi(peripherals.TIMG1, peripherals.RNG, peripherals.WIFI);

        // 8. USB 5 V presence — used by the UI to render a charging icon.
        let vbus_sense = Input::new(peripherals.GPIO33, InputConfig::default());

        Self {
            antenna,
            battery,
            display,
            status_led,
            wifi,
            vbus_sense,
            _ldo2: ldo2,
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
            "WiFi: {}",
            if self.wifi.is_some() {
                "esp-wifi STA scan-only (15s interval)"
            } else {
                "unavailable"
            },
        );
        esp_println::println!(
            "Status policy: LED tracks SOC% (red 0% → yellow 50% → green 100%)"
        );
        esp_println::println!("LoRa: not wired (E22P-915M30S placeholder)");
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
    #[unsafe(link_section = ".dram2_uninit")]
    static mut HEAP_DRAM2: MaybeUninit<[u8; BIFROST_DRAM2_HEAP_BYTES]> = MaybeUninit::uninit();
    static mut HEAP_DRAM: MaybeUninit<[u8; BIFROST_DRAM_HEAP_BYTES]> = MaybeUninit::uninit();
    unsafe {
        esp_alloc::HEAP.add_region(esp_alloc::HeapRegion::new(
            core::ptr::addr_of_mut!(HEAP_DRAM2).cast(),
            BIFROST_DRAM2_HEAP_BYTES,
            esp_alloc::MemoryCapability::Internal.into(),
        ));
        esp_alloc::HEAP.add_region(esp_alloc::HeapRegion::new(
            core::ptr::addr_of_mut!(HEAP_DRAM).cast(),
            BIFROST_DRAM_HEAP_BYTES,
            esp_alloc::MemoryCapability::Internal.into(),
        ));
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
            esp_println::println!(
                "WiFi: scanner up (esp-wifi STA mode, external u.FL antenna)"
            );
            Some(w)
        },
        Err(e) => {
            esp_println::println!("WiFi: scanner init failed: {:?}", e);
            None
        },
    }
}

