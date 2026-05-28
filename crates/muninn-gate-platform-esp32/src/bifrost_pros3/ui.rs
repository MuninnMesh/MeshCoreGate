//! Bifrost Gate UI for the SSD1327 128×128 4-bit grayscale OLED.
//!
//! The UI is organized as a small set of screens that each draw onto an
//! `embedded-graphics` `DrawTarget<Color = Gray4>`. Common chrome (header
//! with title, battery, network indicator) lives in [`header`] and is shared
//! by every screen via [`Header::draw`].
//!
//! Grayscale palette: `Gray4` exposes 16 luma levels (0 = off, 15 = max).
//! Conventions used across screens:
//!
//! - `LUMA_BG`     — panel background (always off so OLED pixels are dark).
//! - `LUMA_DIM`    — secondary text / subdued chrome.
//! - `LUMA_TEXT`   — primary text.
//! - `LUMA_ACCENT` — full white for headlines and active indicators.
//!
//! The driver only flushes on demand (caller invokes `display.flush()` after
//! a render); each screen is internally responsible for clearing its area.

mod header;
mod provisioning;
mod state;
mod status;
mod wifi;

use embedded_graphics::draw_target::DrawTarget;
use embedded_graphics::pixelcolor::Gray4;
use u8g2_fonts::FontRenderer;
use u8g2_fonts::fonts;

pub use self::state::{AuthLabel, MAX_WIFI_APS, NetworkPhase, UiState, WifiAp};

/// Background luma (panel off).
pub const LUMA_BG: Gray4 = Gray4::new(0);
/// Very subtle — inactive WiFi bars and similar "off" indicator chrome.
pub const LUMA_INACTIVE: Gray4 = Gray4::new(2);
/// Dim text / subtle dividers.
pub const LUMA_DIM: Gray4 = Gray4::new(5);
/// Default body text.
pub const LUMA_TEXT: Gray4 = Gray4::new(11);
/// Headline / active indicator luma.
pub const LUMA_ACCENT: Gray4 = Gray4::new(15);

/// ProFont monospace from U8g2 — clean modern fixed-width designed for
/// code/IDE use. `_tf` = transparent, full ASCII charset.
///
/// Sizes: profont10 ≈ 5×10, profont11 ≈ 6×11, profont15 ≈ 8×15,
///        profont17 ≈ 9×17, profont22 ≈ 11×22.

/// Tiny labels — battery percent, dim metadata, dense table rows.
pub const FONT_TINY: FontRenderer =
    FontRenderer::new::<fonts::u8g2_font_profont10_tf>().with_ignore_unknown_chars(true);
/// Small chrome text — gateway title in the header, footer status line.
pub const FONT_CHROME: FontRenderer =
    FontRenderer::new::<fonts::u8g2_font_profont11_tf>().with_ignore_unknown_chars(true);
/// Body / list rows.
pub const FONT_BODY: FontRenderer =
    FontRenderer::new::<fonts::u8g2_font_profont11_tf>().with_ignore_unknown_chars(true);
/// Big headlines — current state ("Online", "Setup", "Joining").
pub const FONT_HEADLINE: FontRenderer =
    FontRenderer::new::<fonts::u8g2_font_profont15_tf>().with_ignore_unknown_chars(true);

/// Top-level screen selector. The runner picks which screen to render based
/// on the current `UiState` (provisioning vs. connected vs. error etc).
///
/// `Status` and `WifiScan` are wired up for the next phase (after config
/// storage and WiFi association land); only `Provisioning` is dispatched in
/// the current bring-up loop, so the other variants live behind `dead_code`.
#[allow(dead_code, reason = "forward-looking variants used once config storage + association land")]
#[derive(Debug, Clone, Copy)]
pub enum Screen
{
    /// Default operating screen: gateway status + battery + network.
    Status,
    /// Pre-provisioning screen: prompt user + show visible WiFi networks.
    Provisioning,
    /// WiFi scan results screen (used while joining or recovering from error).
    WifiScan,
}

/// Dispatch the requested screen onto the draw target. The caller still owns
/// flushing the framebuffer to hardware.
pub fn render<D, E>(target: &mut D, screen: Screen, state: &UiState) -> Result<(), E>
where
    D: DrawTarget<Color = Gray4, Error = E>,
{
    match screen {
        Screen::Status => status::draw(target, state),
        Screen::Provisioning => provisioning::draw(target, state),
        Screen::WifiScan => wifi::draw_scan(target, state),
    }
}
