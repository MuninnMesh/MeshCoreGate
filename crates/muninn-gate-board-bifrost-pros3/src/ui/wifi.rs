//! WiFi scan / status screens.

use embedded_graphics::draw_target::DrawTarget;
use embedded_graphics::geometry::{Point, Size};
use embedded_graphics::pixelcolor::Gray4;
use embedded_graphics::primitives::{PrimitiveStyleBuilder, Rectangle, StyledDrawable};
use heapless::String;
use u8g2_fonts::types::{FontColor, HorizontalAlignment, VerticalPosition};

use super::header::{
    WIFI_BAR_COUNT,
    WIFI_BAR_GAP,
    WIFI_BAR_HEIGHT_MAX,
    WIFI_BAR_WIDTH,
    WIFI_BARS_X,
    rssi_to_bars,
};
use super::{
    FONT_BODY,
    FONT_TINY,
    LUMA_ACCENT,
    LUMA_BG,
    LUMA_DIM,
    LUMA_INACTIVE,
    LUMA_TEXT,
    UiState,
    WifiAp,
    header,
};

/// Dedicated WiFi scan screen.
pub fn draw_scan<D, E>(target: &mut D, state: &UiState) -> Result<(), E>
where
    D: DrawTarget<Color = Gray4, Error = E>,
{
    Rectangle::new(Point::new(0, 0), Size::new(128, 128)).draw_styled(
        &PrimitiveStyleBuilder::new().fill_color(LUMA_BG).build(),
        target,
    )?;
    header::draw(target, state)?;

    let body_top = header::BODY_TOP_Y;
    let _ = FONT_TINY.render_aligned(
        "Visible networks",
        Point::new(64, body_top + 8),
        VerticalPosition::Baseline,
        HorizontalAlignment::Center,
        FontColor::Transparent(LUMA_DIM),
        target,
    );

    draw_ap_list(target, state.wifi_aps.as_slice(), body_top + 16, 8)?;
    Ok(())
}

/// Helper used by both the WiFi scan screen and the provisioning screen.
pub fn draw_ap_list<D, E>(
    target: &mut D,
    aps: &[WifiAp],
    y_top: i32,
    max_rows: usize,
) -> Result<(), E>
where
    D: DrawTarget<Color = Gray4, Error = E>,
{
    if aps.is_empty() {
        let _ = FONT_TINY.render_aligned(
            "scanning…",
            Point::new(64, y_top + 12),
            VerticalPosition::Baseline,
            HorizontalAlignment::Center,
            FontColor::Transparent(LUMA_DIM),
            target,
        );
        return Ok(());
    }

    let row_height: i32 = 12;
    // Right edge of the auth label — 10 px gap before the per-row signal
    // bars (which sit at `WIFI_BARS_X`, same column as the header bars so
    // every WiFi strength indicator on screen aligns vertically). The gap
    // is wider than it needs to be because dropping the `ch{N}/` prefix
    // freed up real estate — we use it to let the SSID grow to the right.
    let meta_right_x: i32 = WIFI_BARS_X - 10;

    for (row, ap) in aps.iter().take(max_rows).enumerate() {
        let baseline = y_top + 10 + row as i32 * row_height;
        if baseline > 126 {
            break;
        }

        // SSID truncated so the right-side auth cell stays clear. Auth
        // label is at most `WPA2`/`WPA3` (4 glyphs ≈ 20 px) plus the
        // 10 px gap to bars, so SSID has the rest.
        let mut ssid: String<32> = String::new();
        const MAX_SSID_CHARS: usize = 12;
        let mut taken = 0;
        for ch in ap.ssid.chars() {
            if taken >= MAX_SSID_CHARS {
                let _ = ssid.push('…');
                break;
            }
            if ssid.push(ch).is_err() {
                break;
            }
            taken += 1;
        }
        let _ = FONT_BODY.render_aligned(
            ssid.as_str(),
            Point::new(2, baseline),
            VerticalPosition::Baseline,
            HorizontalAlignment::Left,
            FontColor::Transparent(LUMA_TEXT),
            target,
        );

        // Security only, e.g. "WPA2", "Open", "WPA3". The channel was
        // dropped — it's not useful info for picking a network and ate
        // the SSID column.
        let _ = FONT_TINY.render_aligned(
            ap.auth.short(),
            Point::new(meta_right_x, baseline),
            VerticalPosition::Baseline,
            HorizontalAlignment::Right,
            FontColor::Transparent(LUMA_DIM),
            target,
        );

        draw_bars(
            target,
            Point::new(WIFI_BARS_X, baseline - WIFI_BAR_HEIGHT_MAX),
            rssi_to_bars(ap.rssi),
        )?;
    }
    Ok(())
}

/// Render the 4-bar WiFi strength indicator used in the AP list. Geometry
/// matches the header indicator exactly so every "WiFi strength" visual on
/// the panel reads at the same scale.
fn draw_bars<D, E>(target: &mut D, origin: Point, filled: u8) -> Result<(), E>
where
    D: DrawTarget<Color = Gray4, Error = E>,
{
    let style_inactive = PrimitiveStyleBuilder::new()
        .fill_color(LUMA_INACTIVE)
        .build();
    let style_accent = PrimitiveStyleBuilder::new().fill_color(LUMA_ACCENT).build();
    for bar in 0..WIFI_BAR_COUNT {
        let bar_h = 2 + bar * 2;
        let bar_x = origin.x + bar * WIFI_BAR_GAP;
        let bar_y = origin.y + (WIFI_BAR_HEIGHT_MAX - bar_h);
        let style = if (bar as u8) < filled {
            &style_accent
        } else {
            &style_inactive
        };
        Rectangle::new(
            Point::new(bar_x, bar_y),
            Size::new(WIFI_BAR_WIDTH as u32, bar_h as u32),
        )
        .draw_styled(style, target)?;
    }
    Ok(())
}
