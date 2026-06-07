//! Status-LED phase machine for the Bifrost ProS3 on-board WS2812.
//!
//! The actual WS2812 wire driver (timing, GRB encoding, RMT setup) lives in
//! the `muninn-driver-ws2812` crate; this module just owns the high-level
//! `LedPhase` enum and the cadence logic that maps WiFi / LoRa / boot
//! state to a color over time. The runner calls [`LedDriver::set_phase`]
//! when the state machine advances and [`LedDriver::flash`] for one-shot
//! events. Producer polling drives its own blue flicker from the blocking
//! poll loop so it stays in step with the OLED row spinner.

pub use muninn_driver_ws2812::{Rgb, StatusLed};

/// Which steady-state pattern the RGB LED should display. Phases are
/// driven by the runner from the WiFi + boot state machine; transient
/// events (e.g. a LoRa update arriving) are layered on top via
/// [`LedDriver::flash`].
///
/// Color choices:
///
/// - [`Self::BootSweep`]     — RED → PURPLE → CYAN, each held for 200 ms (600 ms total), then holds
///   CYAN until the phase advances.
/// - [`Self::Connecting`]    — solid ORANGE while WiFi association is in flight.
/// - [`Self::AcquiringIp`]   — BLUE blinking at 1 Hz (500 ms on / 500 ms off) while DHCP is
///   pending.
/// - [`Self::Online`]        — GREEN for 1 s after entering the operational screen, then off.
/// - [`Self::OtaFlashing`]   — PURPLE flicker while an OTA/update request is active.
/// - [`Self::CriticalError`] — RED blinking at 2.5 Hz (200 ms on / 200 ms off) for actionable
///   operational failures (e.g. LoRa radio not wired). Fast cadence makes it visibly distinct from
///   the slow `AcquiringIp` blink so the operator notices.
/// - [`Self::FatalError`]    — solid RED for unrecoverable errors.
#[allow(
    dead_code,
    reason = "FatalError is queued for the OOM/panic path; not wired yet"
)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LedPhase
{
    /// Single-shot RED → PURPLE → CYAN gradient during the boot splash.
    BootSweep,
    /// Solid ORANGE while WiFi is associating.
    Connecting,
    /// Blinking BLUE (1 Hz) while DHCP is in flight.
    AcquiringIp,
    /// One-second GREEN pulse when the operational/polling screen is
    /// entered, then idle/off. Active producer polling temporarily
    /// overlays a blue flicker from the poll loop.
    Online,
    /// Blinking PURPLE while an operator-triggered firmware update flow
    /// is active.
    OtaFlashing,
    /// Blinking RED (2.5 Hz) for operational failures the operator must
    /// fix (LoRa radio missing, mis-wired peripherals, etc).
    CriticalError,
    /// Solid RED for unrecoverable errors.
    FatalError,
}

/// Transient one-shot effect overlaid on top of the current
/// [`LedPhase`] for [`LedDriver::FLASH_DURATION_MS`] milliseconds.
#[allow(
    dead_code,
    reason = "wired through to the API now; first caller lands with the LoRa polling loop"
)]
#[derive(Debug, Clone, Copy)]
pub enum LedFlash
{
    /// 200 ms GREEN — a LoRa producer update was received successfully.
    LoraOk,
    /// 200 ms ORANGE — a LoRa producer update failed (timeout, CRC, etc.).
    LoraFail,
}

/// Operational entry pulse duration. After this the RGB LED stays dark
/// unless a producer poll is actively in flight.
pub const OPERATIONAL_ENTRY_GREEN_MS: u64 = 1_000;

/// Half-period for the blue polling flicker. 50 ms high + 50 ms low is
/// approximately 10 full blink cycles per second.
pub const POLL_LED_FLICKER_HALF_PERIOD_MS: u64 = 50;

const OPERATIONAL_GREEN: Rgb = Rgb::new(0, 255, 0);
const POLL_BLUE: Rgb = Rgb::new(0, 80, 255);
const OTA_PURPLE: Rgb = Rgb::new(160, 0, 255);

/// State machine for the RGB indicator. The runner calls
/// [`Self::set_phase`] when the WiFi / boot state changes and
/// [`Self::flash`] for transient events; on every tick it asks for the
/// current color via [`Self::current_color`] and writes it through
/// [`StatusLed::set_color`].
pub struct LedDriver
{
    phase:          LedPhase,
    phase_start_ms: u64,
    flash_until_ms: Option<u64>,
    flash_color:    Option<Rgb>,
}

impl LedDriver
{
    /// How long a one-shot [`LedFlash`] holds the LED at its flash color
    /// before the steady-state phase color takes over again.
    pub const FLASH_DURATION_MS: u64 = 200;

    /// Build a fresh driver in [`LedPhase::BootSweep`] at `t = 0`.
    pub const fn new() -> Self
    {
        Self {
            phase:          LedPhase::BootSweep,
            phase_start_ms: 0,
            flash_until_ms: None,
            flash_color:    None,
        }
    }

    /// Switch to a new steady-state phase. If the new phase matches the
    /// current one this is a no-op so animations keep flowing instead of
    /// snapping back to t = 0 every tick.
    pub fn set_phase(&mut self, phase: LedPhase, now_ms: u64)
    {
        if self.phase != phase {
            self.phase = phase;
            self.phase_start_ms = now_ms;
        }
    }

    /// Layer a one-shot flash over the current phase color. The flash
    /// holds for [`Self::FLASH_DURATION_MS`] then the phase color
    /// resumes.
    #[allow(dead_code, reason = "first caller lands with the LoRa polling loop")]
    pub fn flash(&mut self, flash: LedFlash, now_ms: u64)
    {
        let color = match flash {
            LedFlash::LoraOk => Rgb::new(0, 255, 0),
            LedFlash::LoraFail => Rgb::new(255, 120, 0),
        };
        self.flash_color = Some(color);
        self.flash_until_ms = Some(now_ms + Self::FLASH_DURATION_MS);
    }

    /// Pre-dim RGB color for the current `now_ms`. Pre-dim means the
    /// caller still has to apply the project's brightness shift before
    /// writing to the WS2812.
    pub fn current_color(&self, now_ms: u64) -> Rgb
    {
        if let (Some(until), Some(color)) = (self.flash_until_ms, self.flash_color)
            && now_ms < until
        {
            return color;
        }
        let elapsed = now_ms.saturating_sub(self.phase_start_ms);
        match self.phase {
            LedPhase::BootSweep => boot_sweep_color(elapsed),
            LedPhase::Connecting => Rgb::new(255, 120, 0),
            LedPhase::AcquiringIp => {
                if elapsed % 1000 < 500 {
                    Rgb::new(0, 80, 255)
                } else {
                    Rgb::OFF
                }
            },
            LedPhase::Online => {
                if elapsed < OPERATIONAL_ENTRY_GREEN_MS {
                    OPERATIONAL_GREEN
                } else {
                    Rgb::OFF
                }
            },
            LedPhase::OtaFlashing => {
                if elapsed % 200 < 100 {
                    OTA_PURPLE
                } else {
                    Rgb::OFF
                }
            },
            LedPhase::CriticalError => {
                if elapsed % 400 < 200 {
                    Rgb::new(255, 0, 0)
                } else {
                    Rgb::OFF
                }
            },
            LedPhase::FatalError => Rgb::new(255, 0, 0),
        }
    }

    /// Returns the deadline for the operational entry pulse if the
    /// driver is currently in the operational phase.
    pub fn operational_entry_pulse_until_ms(&self) -> Option<u64>
    {
        if self.phase == LedPhase::Online {
            Some(
                self.phase_start_ms
                    .saturating_add(OPERATIONAL_ENTRY_GREEN_MS),
            )
        } else {
            None
        }
    }
}

/// Blue flicker used only while a producer poll is in flight.
pub fn polling_flicker_color(now_ms: u64) -> Rgb
{
    if (now_ms / POLL_LED_FLICKER_HALF_PERIOD_MS) % 2 == 0 {
        POLL_BLUE
    } else {
        Rgb::OFF
    }
}

/// LED color while polling from the operational screen. The one-second
/// entry pulse has priority so the first forced poll does not hide the
/// "entered polling screen" acknowledgement.
pub fn operational_polling_color(now_ms: u64, entry_green_until_ms: Option<u64>) -> Rgb
{
    if entry_green_until_ms.is_some_and(|until| now_ms < until) {
        OPERATIONAL_GREEN
    } else {
        polling_flicker_color(now_ms)
    }
}

impl Default for LedDriver
{
    fn default() -> Self
    {
        Self::new()
    }
}

/// Three-step boot indicator: RED for 200 ms, PURPLE for 200 ms, CYAN
/// for 200 ms, then holds CYAN until the next phase takes over. The
/// 600 ms total fits comfortably inside the 800 ms splash so the user
/// sees the full sequence with the panel still showing the boot logo.
fn boot_sweep_color(elapsed_ms: u64) -> Rgb
{
    const STEP_MS: u64 = 200;
    const RED: Rgb = Rgb::new(255, 0, 0);
    const PURPLE: Rgb = Rgb::new(128, 0, 128);
    const CYAN: Rgb = Rgb::new(0, 255, 255);

    // `<=` boundaries (not `<`) so the first render at `elapsed_ms =
    // SPLASH_TICK_MS` (≈ 200) still lands on RED rather than skipping
    // straight to PURPLE.
    if elapsed_ms <= STEP_MS {
        RED
    } else if elapsed_ms <= STEP_MS * 2 {
        PURPLE
    } else {
        CYAN
    }
}
