//! Bifrost Gate ProS3 bring-up runner.
//!
//! The runner is isolated from [`crate::run_gateway`] (which is hard-wired
//! to the Heltec V4 GPIO / SX1262 / SSD1306 resource map) so the two boards
//! can evolve independently. Architecture:
//!
//! ```text
//!     run_bifrost_pros3
//!            │
//!            ▼
//!     BoardServices::init  ── composes every peripheral handle
//!            │
//!            ▼
//!     poll_loop  ── battery read → WiFi scan (15s) → UI render → LED
//! ```
//!
//! Each capability lives in its own submodule and exposes a typed owning
//! handle that fails gracefully (returns `None`) if the underlying hardware
//! is missing or misbehaving. The runner never panics on a missing
//! peripheral; the relevant `Option<_>` slot just stays `None`.
//!
//! Submodule map:
//!
//! - [`antenna`]    — ProS3[D] 2.4 GHz RF switch (GPIO 11).
//! - [`battery`]    — MAX17048 LiPo fuel gauge on the shared STEMMA I2C bus.
//! - [`board`]      — `BoardServices` facade that owns every peripheral.
//! - [`display`]    — SSD1327 128×128 4-bit grayscale OLED driver.
//! - [`i2c_bus`]    — Shared blocking I2C bus for STEMMA peripherals.
//! - [`power`]      — LDO2 enable line (STEMMA + RGB rail).
//! - [`status_led`] — WS2812 RGB LED driver via RMT.
//! - [`ui`]         — Grayscale screen renderers (header + provisioning).
//! - [`wifi`]       — esp-wifi station controller, scan-only.

mod antenna;
mod battery;
mod board;
mod display;
mod i2c_bus;
mod power;
mod status_led;
mod ui;
mod wifi;

use core::fmt::Write as _;

use esp_hal::clock::CpuClock;
use esp_hal::delay::Delay;
use muninn_gate_core::GateFirmwareVariant;

use self::battery::BatterySample;
use self::board::BoardServices;
use self::status_led::Rgb;
use self::ui::{NetworkPhase, Screen, UiState};

/// Poll loop cadence. Drives the fuel-gauge read, UI render, and LED
/// refresh. Battery + LED are cheap; the I2C flush of the framebuffer is
/// the dominant cost (~200 ms at 400 kHz I2C).
const POLL_INTERVAL_MS: u32 = 1_000;
/// Print a serial status line every N ticks (≈ once per 5 s at 1 Hz poll).
const STATUS_LOG_PERIOD_TICKS: u32 = 5;
/// Re-scan visible WiFi networks at this cadence.
const WIFI_SCAN_INTERVAL_MS: u64 = 15_000;
/// Right-shift applied per WS2812 channel to keep the LED comfortably dim.
const RGB_DIM_SHIFT: u8 = 4;

/// UI title shown when no user config is loaded yet. The bracketed `???`
/// reads as "device has no identity assigned" and is replaced by
/// `config.name` from the uploaded `config.json` once the USB provisioning
/// flow lands for this board variant.
const PLACEHOLDER_TITLE: &str = "[???]";

/// Run the Bifrost Gate ProS3 firmware forever.
pub fn run_bifrost_pros3<V>() -> !
where
    V: GateFirmwareVariant,
{
    let cpu_config = esp_hal::Config::default().with_cpu_clock(CpuClock::max());
    let peripherals = esp_hal::init(cpu_config);
    let mut services = BoardServices::init(peripherals);

    services.print_banner::<V>();
    services.dim_led_to_off();

    let mut ui_state = UiState::new();
    let _ = ui_state.title.push_str(PLACEHOLDER_TITLE);

    poll_loop(&mut services, &mut ui_state);
}

fn poll_loop(services: &mut BoardServices, ui_state: &mut UiState) -> !
{
    let delay = Delay::new();
    let mut tick: u32 = 0;
    let mut last_scan_ms: Option<u64> = None;

    loop {
        ui_state.now_ms = ui_state.now_ms.saturating_add(POLL_INTERVAL_MS as u64);

        // 1. Battery sample + USB sense.
        let latest_sample = sample_battery(services);
        ui_state.battery = latest_sample;
        ui_state.usb_connected = services.usb_connected();

        // 2. WiFi scan every WIFI_SCAN_INTERVAL_MS.
        maybe_rescan(services, ui_state, &mut last_scan_ms);

        // Until config storage lands we sit in the Unprovisioned phase
        // forever — that's also the only screen we render right now.
        ui_state.network = NetworkPhase::Unprovisioned;

        // 3. Periodic serial heartbeat.
        if tick.is_multiple_of(STATUS_LOG_PERIOD_TICKS) {
            log_status(
                tick,
                latest_sample,
                ui_state.usb_connected,
                services,
                ui_state.wifi_aps.len(),
            );
        }

        // 4. UI render + 5. LED color.
        render_ui(services, ui_state, tick);
        drive_status_led_from_battery(services, latest_sample, tick);

        delay.delay_millis(POLL_INTERVAL_MS);
        tick = tick.wrapping_add(1);
    }
}

fn sample_battery(services: &mut BoardServices) -> Option<BatterySample>
{
    let gauge = services.battery.as_mut()?;
    match gauge.read() {
        Ok(sample) => Some(sample),
        Err(_) => {
            esp_println::println!("Battery: MAX17048 read failed; SOC unknown");
            None
        },
    }
}

fn maybe_rescan(
    services: &mut BoardServices,
    ui_state: &mut UiState,
    last_scan_ms: &mut Option<u64>,
)
{
    let Some(scanner) = services.wifi.as_mut() else {
        return;
    };

    let now = ui_state.now_ms;
    let due = last_scan_ms
        .map(|last| now.saturating_sub(last) >= WIFI_SCAN_INTERVAL_MS)
        .unwrap_or(true);
    if !due {
        return;
    }

    // Hint the user that a scan is in progress (header bars pulse) and
    // push a frame before the blocking scan stalls the loop for ~1-3 s.
    ui_state.network = NetworkPhase::Scanning;
    if let Some(disp) = services.display.as_mut() {
        if ui::render(disp, Screen::Provisioning, ui_state).is_ok() {
            let _ = disp.flush();
        }
    }

    match scanner.scan() {
        Ok(aps) => {
            ui_state.wifi_aps.clear();
            for ap in aps.into_iter() {
                if ui_state.wifi_aps.push(ap).is_err() {
                    break;
                }
            }
            esp_println::println!("WiFi: scan complete, {} APs", ui_state.wifi_aps.len());
        },
        Err(e) => {
            esp_println::println!("WiFi: scan failed: {:?}", e);
        },
    }
    *last_scan_ms = Some(now);
}

fn render_ui(services: &mut BoardServices, ui_state: &UiState, tick: u32)
{
    let Some(disp) = services.display.as_mut() else {
        return;
    };
    if ui::render(disp, Screen::Provisioning, ui_state).is_err() {
        esp_println::println!("Display: render failed at tick={}", tick);
        return;
    }
    if disp.flush().is_err() {
        esp_println::println!("Display: flush failed at tick={}; UI disabled", tick);
        services.display = None;
    }
}

fn drive_status_led_from_battery(
    services: &mut BoardServices,
    battery: Option<BatterySample>,
    tick: u32,
)
{
    let color = status_color(battery).dim(RGB_DIM_SHIFT);
    let Some(mut led) = services.status_led.take() else {
        return;
    };
    match led.set_color(color) {
        Ok(()) => services.status_led = Some(led),
        Err(_) => {
            esp_println::println!("RGB: set_color failed at tick={}; LED disabled", tick);
        },
    }
}

/// Map an optional fuel-gauge sample to an LED color (pre-dim).
///
/// - `None` → a dim "no data" blue distinguishable from the gradient endpoints.
/// - Otherwise → red(0 %) → yellow(50 %) → green(100 %) linear gradient.
fn status_color(sample: Option<BatterySample>) -> Rgb
{
    let Some(sample) = sample else {
        return Rgb::new(0, 0, 60);
    };
    let soc = sample.soc_percent;
    if soc < 50 {
        let g = ((soc as u16 * 255) / 50) as u8;
        Rgb::new(255, g, 0)
    } else {
        let t = (soc - 50) as u16;
        let r = 255u16.saturating_sub((t * 255) / 50) as u8;
        Rgb::new(r, 255, 0)
    }
}

fn log_status(
    tick: u32,
    battery: Option<BatterySample>,
    usb_connected: bool,
    services: &BoardServices,
    ap_count: usize,
)
{
    let (mv, soc) = match battery {
        Some(s) => (s.voltage_mv as i32, s.soc_percent as i32),
        None => (-1, -1),
    };
    let mut line: heapless::String<192> = heapless::String::new();
    let _ = write!(
        &mut line,
        "Bifrost tick={} display={} led={} wifi={} aps={} usb={} battery_mv={} soc={}%",
        tick,
        if services.display.is_some() { "ok" } else { "gone" },
        if services.status_led.is_some() { "ok" } else { "gone" },
        if services.wifi.is_some() { "ok" } else { "gone" },
        ap_count,
        if usb_connected { "on" } else { "off" },
        mv,
        soc,
    );
    esp_println::println!("{}", line.as_str());
}
