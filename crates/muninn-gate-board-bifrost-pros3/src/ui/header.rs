//! Shared 13-pixel header strip drawn on every Bifrost screen.
//!
//! Visual treatment:
//!
//! - Background filled in `LUMA_ACCENT` with rounded top corners — reads as a bright "title bar"
//!   docked to the top edge of the panel.
//! - All foreground chrome (title text, battery icon, WiFi bars) drawn in `LUMA_BG` so it pops
//!   against the bright background.
//! - Charging indicator: when USB 5 V is detected with a battery present, a small plug glyph sits
//!   next to the battery icon.
//!
//! Layout (128 px wide, 13 px tall):
//!
//! ```text
//!  ┌─────────────────────────┬─────┬──────┬───────┐
//!  │ Bifrost Gate            │ ▓⌁  │  78  │ ▁▃▅▇  │
//!  └─────────────────────────┴─────┴──────┴───────┘
//!   0                      62 87 93        111   128
//! ```

use embedded_graphics::Pixel;
use embedded_graphics::draw_target::DrawTarget;
use embedded_graphics::geometry::{Point, Size};
use embedded_graphics::pixelcolor::Gray4;
use embedded_graphics::primitives::{
    CornerRadii,
    PrimitiveStyleBuilder,
    Rectangle,
    RoundedRectangle,
    StyledDrawable,
};
use heapless::String;
use muninn_driver_max17048::BatterySample;
use u8g2_fonts::types::{FontColor, HorizontalAlignment, VerticalPosition};

use super::{FONT_CHROME, FONT_TINY, LUMA_ACCENT, LUMA_BG, LUMA_DIM, NetworkPhase, UiState};

/// Header height (background block height).
pub const HEADER_HEIGHT: u32 = 13;
/// Y coordinate of the first body row (one px below the header).
pub const BODY_TOP_Y: i32 = 14;

const HEADER_CORNER_RADIUS: u32 = 2;

/// Maximum title length before truncation kicks in. Sized so a typical
/// user-set `config.name` fits comfortably without bumping into the
/// battery icon at x=88.
const TITLE_MAX_CHARS: usize = 13;
/// Maximum title length when the 4-char gateway public-key prefix is present.
const TITLE_MAX_CHARS_WITH_PREFIX: usize = 9;
/// Approximate ProFont11 character advance in pixels.
const TITLE_CHAR_WIDTH: i32 = 6;
/// Space between the gateway key prefix and configured name.
const TITLE_PREFIX_GAP_PX: i32 = 6;
const BATTERY_ICON_X: i32 = 88;
const BATTERY_ICON_Y: i32 = 2;
const BATTERY_ICON_WIDTH: i32 = 4;
const BATTERY_ICON_BODY_HEIGHT: i32 = 7;
const BATTERY_ICON_TIP_HEIGHT: i32 = 1;
/// Right edge of the percent text. Text grows leftward from here.
const BATTERY_PERCENT_RIGHT_X: i32 = 109;
const BATTERY_PERCENT_Y: i32 = 2;
/// Top-left of the horizontal plug glyph. Sits right next to the WiFi
/// bars (which start at `WIFI_BARS_X = 114`) — the percent text is
/// suppressed in the no-battery state so we close the gap to make the
/// indicator obvious. Vertical offset puts the 5-px glyph in the optical
/// middle of the 13-px header.
const PLUG_ICON_X: i32 = 100;
const PLUG_ICON_Y: i32 = 4;
/// Top-left of the compact charging plug glyph drawn when USB is
/// delivering power AND a battery sample is present below the full-charge
/// threshold. Sits between the battery icon (ends at x=92) and the
/// percent text (right-aligned at x=109).
const CHARGE_PLUG_ICON_X: i32 = 94;
const CHARGE_PLUG_ICON_Y: i32 = 3;
/// SoC threshold (percent) above which we assume "no battery, just USB"
/// rather than "battery fully charged + USB". The MAX17048 reads VBAT
/// = ~4.2 V whenever the charger is driving the rail, regardless of
/// whether a battery is actually attached, so we can't distinguish the
/// two cases without an extra sense pin. Treat ≥ 99 % SoC under USB as
/// "USB only" — minor wart: a truly-full charging battery briefly
/// shows the plug instead of the battery icon, which is acceptable.
const PLUG_USB_ONLY_SOC_THRESHOLD: u8 = 99;
/// WiFi bar group left edge. The per-AP signal bars in the network list
/// share this anchor so all WiFi indicators align vertically.
pub const WIFI_BARS_X: i32 = 114;
const WIFI_BARS_Y: i32 = 2;
/// Same bar dimensions as the per-AP signal bars in the network list, so
/// every "WiFi strength" indicator on screen reads the same.
pub const WIFI_BAR_GAP: i32 = 3;
pub const WIFI_BAR_WIDTH: i32 = 2;
pub const WIFI_BAR_HEIGHT_MAX: i32 = 8;
pub const WIFI_BAR_COUNT: i32 = 4;
/// Mid-grey luma for the scanning-state pulse on the bright header bg.
/// Sits between `LUMA_DIM` (inactive) and `LUMA_BG` (active), giving the
/// animation visible motion without strobing.
const LUMA_HEADER_PULSE: Gray4 = Gray4::new(2);

pub fn draw<D, E>(target: &mut D, state: &UiState) -> Result<(), E>
where
    D: DrawTarget<Color = Gray4, Error = E>,
{
    draw_background(target)?;
    draw_title(target, state)?;
    draw_battery(target, state.battery, state.usb_connected)?;
    draw_wifi_bars(target, &state.network, state.now_ms)?;
    Ok(())
}

fn draw_background<D, E>(target: &mut D) -> Result<(), E>
where
    D: DrawTarget<Color = Gray4, Error = E>,
{
    // All four corners rounded — the header reads as a floating "pill" /
    // tab visually separated from the body region below.
    RoundedRectangle::new(
        Rectangle::new(Point::new(0, 0), Size::new(128, HEADER_HEIGHT)),
        CornerRadii {
            top_left:     Size::new(HEADER_CORNER_RADIUS, HEADER_CORNER_RADIUS),
            top_right:    Size::new(HEADER_CORNER_RADIUS, HEADER_CORNER_RADIUS),
            bottom_left:  Size::new(HEADER_CORNER_RADIUS, HEADER_CORNER_RADIUS),
            bottom_right: Size::new(HEADER_CORNER_RADIUS, HEADER_CORNER_RADIUS),
        },
    )
    .draw_styled(
        &PrimitiveStyleBuilder::new().fill_color(LUMA_ACCENT).build(),
        target,
    )?;
    Ok(())
}

fn draw_title<D, E>(target: &mut D, state: &UiState) -> Result<(), E>
where
    D: DrawTarget<Color = Gray4, Error = E>,
{
    let origin = Point::new(2, 2);
    let prefix = state.gateway_pubkey_prefix.as_str();
    if !prefix.is_empty() {
        let _ = FONT_CHROME.render_aligned(
            prefix,
            origin,
            VerticalPosition::Top,
            HorizontalAlignment::Left,
            FontColor::Transparent(LUMA_BG),
            target,
        );

        let title = truncate_plain(state.title.as_str(), TITLE_MAX_CHARS_WITH_PREFIX);
        let title_x = origin.x + (prefix.len() as i32 * TITLE_CHAR_WIDTH) + TITLE_PREFIX_GAP_PX;
        let _ = FONT_CHROME.render_aligned(
            title.as_str(),
            Point::new(title_x, origin.y),
            VerticalPosition::Top,
            HorizontalAlignment::Left,
            FontColor::Transparent(LUMA_BG),
            target,
        );
        return Ok(());
    }

    let truncated = truncate(state.title.as_str(), TITLE_MAX_CHARS);
    let _ = FONT_CHROME.render_aligned(
        truncated.as_str(),
        origin,
        VerticalPosition::Top,
        HorizontalAlignment::Left,
        FontColor::Transparent(LUMA_BG),
        target,
    );
    Ok(())
}

fn draw_battery<D, E>(
    target: &mut D,
    sample: Option<BatterySample>,
    usb_connected: bool,
) -> Result<(), E>
where
    D: DrawTarget<Color = Gray4, Error = E>,
{
    // Three-state decision matrix:
    //
    // - No fuel-gauge reading → almost certainly no battery → plug.
    // - USB on AND SoC ≥ 99 % → assume "USB-only" (charger pegs VBAT at ~4.2 V whether or not a
    //   cell is attached) → plug.
    // - USB on AND SoC < 99 % → battery is present and charging → battery icon + plug marker.
    // - USB off → on-battery → battery icon, no bolt.
    let charging = match sample {
        None => return draw_plug_icon(target, Point::new(PLUG_ICON_X, PLUG_ICON_Y)),
        Some(s) if usb_connected && s.soc_percent >= PLUG_USB_ONLY_SOC_THRESHOLD => {
            return draw_plug_icon(target, Point::new(PLUG_ICON_X, PLUG_ICON_Y));
        },
        Some(_) => usb_connected,
    };
    let s = sample.expect("returned plug earlier when sample is None");

    let icon_x = BATTERY_ICON_X;
    let icon_y = BATTERY_ICON_Y;

    // Tip: 2 px wide, centered on the body.
    let tip_x = icon_x + (BATTERY_ICON_WIDTH - 2) / 2;
    Rectangle::new(
        Point::new(tip_x, icon_y),
        Size::new(2, BATTERY_ICON_TIP_HEIGHT as u32),
    )
    .draw_styled(
        &PrimitiveStyleBuilder::new().fill_color(LUMA_BG).build(),
        target,
    )?;

    // Body outline with rounded bottom corners (matches the chrome).
    RoundedRectangle::new(
        Rectangle::new(
            Point::new(icon_x, icon_y + BATTERY_ICON_TIP_HEIGHT),
            Size::new(BATTERY_ICON_WIDTH as u32, BATTERY_ICON_BODY_HEIGHT as u32),
        ),
        CornerRadii {
            top_left:     Size::new(0, 0),
            top_right:    Size::new(0, 0),
            bottom_left:  Size::new(1, 1),
            bottom_right: Size::new(1, 1),
        },
    )
    .draw_styled(
        &PrimitiveStyleBuilder::new()
            .stroke_color(LUMA_BG)
            .stroke_width(1)
            .fill_color(LUMA_ACCENT)
            .build(),
        target,
    )?;

    // Standard SOC fill, grows upward from the bottom of the body.
    let inner_h = (BATTERY_ICON_BODY_HEIGHT - 2) as u32;
    let fill_h = (inner_h * s.soc_percent.min(100) as u32) / 100;
    if fill_h > 0 {
        let fill_y = icon_y + BATTERY_ICON_TIP_HEIGHT + 1 + (inner_h as i32 - fill_h as i32);
        let inner_w = BATTERY_ICON_WIDTH as u32 - 2;
        Rectangle::new(Point::new(icon_x + 1, fill_y), Size::new(inner_w, fill_h)).draw_styled(
            &PrimitiveStyleBuilder::new().fill_color(LUMA_BG).build(),
            target,
        )?;
    }

    // Tiny percent label, right-aligned so it can never bleed into the WiFi
    // bars regardless of the digit count.
    let mut buf: String<5> = String::new();
    let _ = core::fmt::write(&mut buf, format_args!("{}", s.soc_percent));
    let _ = FONT_TINY.render_aligned(
        buf.as_str(),
        Point::new(BATTERY_PERCENT_RIGHT_X, BATTERY_PERCENT_Y),
        VerticalPosition::Top,
        HorizontalAlignment::Right,
        FontColor::Transparent(LUMA_BG),
        target,
    );

    // Tiny plug between battery icon and percent text when USB is
    // delivering power. Lets the operator see "battery + external power"
    // vs. "battery only" at a glance without parsing the SOC number.
    if charging {
        draw_charging_plug(target, Point::new(CHARGE_PLUG_ICON_X, CHARGE_PLUG_ICON_Y))?;
    }
    Ok(())
}

/// Tiny plug glyph (5 × 7 px) used when USB is supplying power and the
/// battery isn't already full.
/// Drawn in `LUMA_BG` (dark) against the bright `LUMA_ACCENT` header
/// background.
///
/// ```text
/// ##...
/// ##...
/// ####.
/// #..##
/// ####.
/// ##...
/// ##...
/// ```
fn draw_charging_plug<D, E>(target: &mut D, top_left: Point) -> Result<(), E>
where
    D: DrawTarget<Color = Gray4, Error = E>,
{
    const PIXELS: &[(i32, i32)] = &[
        // Twin prongs.
        (0, 0),
        (1, 0),
        (0, 1),
        (1, 1),
        (0, 5),
        (1, 5),
        (0, 6),
        (1, 6),
        // Plug body.
        (0, 2),
        (1, 2),
        (2, 2),
        (3, 2),
        (0, 3),
        (3, 3),
        (4, 3),
        (0, 4),
        (1, 4),
        (2, 4),
        (3, 4),
    ];
    target.draw_iter(
        PIXELS
            .iter()
            .map(|(dx, dy)| Pixel(Point::new(top_left.x + dx, top_left.y + dy), LUMA_BG)),
    )
}

/// Wall-plug glyph used when no battery is detected. 10 × 5 px, drawn in
/// `LUMA_BG` against the bright `LUMA_ACCENT` header background.
///
/// Reads as "AC plug seen from the side" — two prongs sticking out the
/// left, a hollow plug head in the middle, and a short cord trailing off
/// to the right. The horizontal orientation matches the header's
/// landscape strip and tucks neatly between the battery slot (empty in
/// this state) and the WiFi bars.
///
/// ```text
///     ####.    head top
/// #####..#.    top prong + neck + head right wall
/// ....#..###   head sides + cord
/// #####..#.    bottom prong + neck + head right wall
///     ####.    head bottom
/// ```
fn draw_plug_icon<D, E>(target: &mut D, top_left: Point) -> Result<(), E>
where
    D: DrawTarget<Color = Gray4, Error = E>,
{
    const PIXELS: &[(i32, i32)] = &[
        // Top prong + neck + head-left attachment.
        (0, 1),
        (1, 1),
        (2, 1),
        (3, 1),
        (4, 1),
        // Bottom prong + neck + head-left attachment.
        (0, 3),
        (1, 3),
        (2, 3),
        (3, 3),
        (4, 3),
        // Head top/bottom outline (cols 4..=7).
        (4, 0),
        (5, 0),
        (6, 0),
        (7, 0),
        (4, 4),
        (5, 4),
        (6, 4),
        (7, 4),
        // Head left wall middle (rows 1 and 3 are covered by prongs).
        (4, 2),
        // Head right wall.
        (7, 1),
        (7, 2),
        (7, 3),
        // Cord (extends right past the head).
        (8, 2),
        (9, 2),
    ];
    target.draw_iter(
        PIXELS
            .iter()
            .map(|(dx, dy)| Pixel(Point::new(top_left.x + dx, top_left.y + dy), LUMA_BG)),
    )
}

fn draw_wifi_bars<D, E>(target: &mut D, phase: &NetworkPhase, now_ms: u64) -> Result<(), E>
where
    D: DrawTarget<Color = Gray4, Error = E>,
{
    // Disconnected states leave the bar count at zero so every bar renders
    // in `LUMA_DIM` — a faded gray against the bright header background.
    let (filled, animate) = match phase {
        NetworkPhase::Unprovisioned => (0, false),
        NetworkPhase::Scanning => (0, true),
        NetworkPhase::Connecting { .. } => (1, true),
        NetworkPhase::Connected { rssi, .. } => (rssi_to_bars(*rssi), false),
        NetworkPhase::Error { .. } => (0, false),
    };

    let style_inactive = PrimitiveStyleBuilder::new().fill_color(LUMA_DIM).build();
    let style_active = PrimitiveStyleBuilder::new().fill_color(LUMA_BG).build();
    let style_pulse = PrimitiveStyleBuilder::new()
        .fill_color(LUMA_HEADER_PULSE)
        .build();

    for bar in 0..WIFI_BAR_COUNT {
        let bar_h = 2 + bar * 2;
        let bar_x = WIFI_BARS_X + bar * WIFI_BAR_GAP;
        let bar_y = WIFI_BARS_Y + (WIFI_BAR_HEIGHT_MAX - bar_h);
        let active = (bar as u8) < filled;
        let pulse = animate && (((now_ms / 250) as u8) % 4) == bar as u8;
        let style = match (active, pulse) {
            (true, _) => &style_active,
            (false, true) => &style_pulse,
            (false, false) => &style_inactive,
        };
        Rectangle::new(
            Point::new(bar_x, bar_y),
            Size::new(WIFI_BAR_WIDTH as u32, bar_h as u32),
        )
        .draw_styled(style, target)?;
    }
    Ok(())
}

/// Convert a typical WiFi RSSI to a 1..=4 bar count. We only call this for
/// APs we've actually seen (in a scan result or a live association), so the
/// floor is one bar — "no bars" is reserved for the disconnected / no-AP
/// states which set the bar count to zero explicitly.
pub fn rssi_to_bars(rssi: i8) -> u8
{
    match rssi {
        i8::MIN..=-78 => 1,
        -77..=-67 => 2,
        -66..=-55 => 3,
        _ => 4,
    }
}

fn truncate(input: &str, max_chars: usize) -> heapless::String<32>
{
    let mut out: heapless::String<32> = heapless::String::new();
    let mut chars = 0;
    for ch in input.chars() {
        if chars >= max_chars {
            let _ = out.push('…');
            break;
        }
        if out.push(ch).is_err() {
            break;
        }
        chars += 1;
    }
    out
}

fn truncate_plain(input: &str, max_chars: usize) -> heapless::String<32>
{
    let mut out: heapless::String<32> = heapless::String::new();
    for ch in input.chars().take(max_chars) {
        if out.push(ch).is_err() {
            break;
        }
    }
    out
}
