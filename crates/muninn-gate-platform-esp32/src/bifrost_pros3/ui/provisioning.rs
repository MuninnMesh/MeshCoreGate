//! Provisioning screen — shown while the gate has no stored config.
//!
//! Layout:
//!
//! ```text
//!  ┌─────────────────────────────────────────────────┐
//!  │ header (title + battery + WiFi bars)            │
//!  ├─────────────────────────────────────────────────┤
//!  │                                                 │
//!  │           Configuration Required                │  ← top prompt
//!  │                                                 │
//!  │  ─────────────────────────────────────────────  │
//!  │  HomeWifi               -52  ▁▃▅▇               │  ← top 3 visible
//!  │  Cafe-Guest             -71  ▁▃▅░               │     networks
//!  │  IoT-Things             -84  ▁▃░░               │
//!  │  ─────────────────────────────────────────────  │
//!  │                                                 │
//!  │           Connect USB & Configure               │  ← bottom hint (dim)
//!  └─────────────────────────────────────────────────┘
//! ```

use embedded_graphics::draw_target::DrawTarget;
use embedded_graphics::geometry::{Point, Size};
use embedded_graphics::pixelcolor::Gray4;
use embedded_graphics::primitives::{
    Line,
    PrimitiveStyleBuilder,
    Rectangle,
    StyledDrawable,
};
use u8g2_fonts::types::{FontColor, HorizontalAlignment, VerticalPosition};

use super::header;
use super::wifi::draw_ap_list;
use super::{FONT_BODY, FONT_TINY, LUMA_BG, LUMA_DIM, LUMA_TEXT, UiState};

/// Number of APs the provisioning screen shows (the strongest available).
const VISIBLE_APS: usize = 3;

pub fn draw<D, E>(target: &mut D, state: &UiState) -> Result<(), E>
where
    D: DrawTarget<Color = Gray4, Error = E>,
{
    Rectangle::new(Point::new(0, 0), Size::new(128, 128))
        .draw_styled(
            &PrimitiveStyleBuilder::new().fill_color(LUMA_BG).build(),
            target,
        )?;
    header::draw(target, state)?;

    let body_top = header::BODY_TOP_Y;

    // Top prompt — body weight, centered.
    let _ = FONT_BODY.render_aligned(
        "Configuration",
        Point::new(64, body_top + 10),
        VerticalPosition::Baseline,
        HorizontalAlignment::Center,
        FontColor::Transparent(LUMA_TEXT),
        target,
    );
    let _ = FONT_BODY.render_aligned(
        "Required",
        Point::new(64, body_top + 22),
        VerticalPosition::Baseline,
        HorizontalAlignment::Center,
        FontColor::Transparent(LUMA_TEXT),
        target,
    );

    // Subtle dividers framing the network list.
    let divider_style = PrimitiveStyleBuilder::new()
        .stroke_color(LUMA_DIM)
        .stroke_width(1)
        .build();
    Line::new(Point::new(4, body_top + 28), Point::new(123, body_top + 28))
        .draw_styled(&divider_style, target)?;

    // Top 3 APs (3 px breathing room under the divider).
    draw_ap_list(target, state.wifi_aps.as_slice(), body_top + 33, VISIBLE_APS)?;

    Line::new(Point::new(4, body_top + 88), Point::new(123, body_top + 88))
        .draw_styled(&divider_style, target)?;

    // Bottom hint, dim grey — anchored at the very bottom of the panel with
    // tight line spacing so the footer reads as one compact block.
    let _ = FONT_TINY.render_aligned(
        "Connect USB &",
        Point::new(64, body_top + 100),
        VerticalPosition::Baseline,
        HorizontalAlignment::Center,
        FontColor::Transparent(LUMA_DIM),
        target,
    );
    let _ = FONT_TINY.render_aligned(
        "Configure",
        Point::new(64, body_top + 109),
        VerticalPosition::Baseline,
        HorizontalAlignment::Center,
        FontColor::Transparent(LUMA_DIM),
        target,
    );

    Ok(())
}
