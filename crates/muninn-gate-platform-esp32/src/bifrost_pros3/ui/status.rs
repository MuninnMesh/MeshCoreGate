//! Default operating screen: rolling status of the gate node.

use core::fmt::Write as _;

use embedded_graphics::draw_target::DrawTarget;
use embedded_graphics::geometry::Point;
use embedded_graphics::pixelcolor::Gray4;
use embedded_graphics::primitives::{PrimitiveStyleBuilder, Rectangle, StyledDrawable};
use heapless::String;
use u8g2_fonts::types::{FontColor, HorizontalAlignment, VerticalPosition};

use super::header;
use super::{
    FONT_BODY,
    FONT_HEADLINE,
    FONT_TINY,
    LUMA_ACCENT,
    LUMA_BG,
    LUMA_DIM,
    LUMA_TEXT,
    NetworkPhase,
    UiState,
};

pub fn draw<D, E>(target: &mut D, state: &UiState) -> Result<(), E>
where
    D: DrawTarget<Color = Gray4, Error = E>,
{
    clear(target)?;
    header::draw(target, state)?;

    let body_top = header::BODY_TOP_Y;
    draw_phase_headline(target, &state.network, body_top + 18);
    draw_phase_detail(target, &state.network, body_top + 38);
    draw_footer(target, &state.status_line);
    Ok(())
}

fn clear<D, E>(target: &mut D) -> Result<(), E>
where
    D: DrawTarget<Color = Gray4, Error = E>,
{
    Rectangle::new(Point::new(0, 0), embedded_graphics::geometry::Size::new(128, 128))
        .draw_styled(
            &PrimitiveStyleBuilder::new().fill_color(LUMA_BG).build(),
            target,
        )?;
    Ok(())
}

fn draw_phase_headline<D, E>(target: &mut D, phase: &NetworkPhase, baseline_y: i32)
where
    D: DrawTarget<Color = Gray4, Error = E>,
{
    let (label, luma) = match phase {
        NetworkPhase::Unprovisioned => ("Setup", LUMA_ACCENT),
        NetworkPhase::Scanning => ("Scanning", LUMA_TEXT),
        NetworkPhase::Connecting { .. } => ("Joining", LUMA_TEXT),
        NetworkPhase::Connected { .. } => ("Online", LUMA_ACCENT),
        NetworkPhase::Error { .. } => ("Offline", LUMA_TEXT),
    };
    let _ = FONT_HEADLINE.render_aligned(
        label,
        Point::new(64, baseline_y),
        VerticalPosition::Baseline,
        HorizontalAlignment::Center,
        FontColor::Transparent(luma),
        target,
    );
}

fn draw_phase_detail<D, E>(target: &mut D, phase: &NetworkPhase, y: i32)
where
    D: DrawTarget<Color = Gray4, Error = E>,
{
    match phase {
        NetworkPhase::Unprovisioned => {
            render_center(target, "Plug USB", y, LUMA_TEXT);
            render_center_dim(target, "and send config.json", y + 14);
        },
        NetworkPhase::Scanning => {
            render_center(target, "Looking for AP…", y, LUMA_TEXT);
        },
        NetworkPhase::Connecting { ssid } => {
            render_left_dim(target, "SSID", y - 1);
            render_left(target, ssid.as_str(), y + 12, LUMA_TEXT);
        },
        NetworkPhase::Connected { ssid, rssi, ipv4 } => {
            render_left_dim(target, "SSID", y - 1);
            render_left(target, ssid.as_str(), y + 12, LUMA_TEXT);

            render_left_dim(target, "IP", y + 26);
            let mut ip_str: String<24> = String::new();
            let _ = write!(
                &mut ip_str,
                "{}.{}.{}.{}",
                ipv4[0], ipv4[1], ipv4[2], ipv4[3],
            );
            render_left(target, ip_str.as_str(), y + 38, LUMA_TEXT);

            let mut rssi_str: String<16> = String::new();
            let _ = write!(&mut rssi_str, "{} dBm", rssi);
            let _ = FONT_TINY.render_aligned(
                rssi_str.as_str(),
                Point::new(126, y + 38),
                VerticalPosition::Baseline,
                HorizontalAlignment::Right,
                FontColor::Transparent(LUMA_DIM),
                target,
            );
        },
        NetworkPhase::Error { reason } => {
            render_center(target, reason.as_str(), y, LUMA_TEXT);
            render_center_dim(target, "Hold for retry", y + 14);
        },
    }
}

fn draw_footer<D, E>(target: &mut D, line: &str)
where
    D: DrawTarget<Color = Gray4, Error = E>,
{
    let _ = FONT_TINY.render_aligned(
        line,
        Point::new(64, 125),
        VerticalPosition::Baseline,
        HorizontalAlignment::Center,
        FontColor::Transparent(LUMA_DIM),
        target,
    );
}

fn render_center<D, E>(target: &mut D, text: &str, baseline_y: i32, luma: Gray4)
where
    D: DrawTarget<Color = Gray4, Error = E>,
{
    let _ = FONT_BODY.render_aligned(
        text,
        Point::new(64, baseline_y),
        VerticalPosition::Baseline,
        HorizontalAlignment::Center,
        FontColor::Transparent(luma),
        target,
    );
}

fn render_center_dim<D, E>(target: &mut D, text: &str, baseline_y: i32)
where
    D: DrawTarget<Color = Gray4, Error = E>,
{
    let _ = FONT_TINY.render_aligned(
        text,
        Point::new(64, baseline_y),
        VerticalPosition::Baseline,
        HorizontalAlignment::Center,
        FontColor::Transparent(LUMA_DIM),
        target,
    );
}

fn render_left<D, E>(target: &mut D, text: &str, baseline_y: i32, luma: Gray4)
where
    D: DrawTarget<Color = Gray4, Error = E>,
{
    let _ = FONT_BODY.render_aligned(
        text,
        Point::new(4, baseline_y),
        VerticalPosition::Baseline,
        HorizontalAlignment::Left,
        FontColor::Transparent(luma),
        target,
    );
}

fn render_left_dim<D, E>(target: &mut D, text: &str, baseline_y: i32)
where
    D: DrawTarget<Color = Gray4, Error = E>,
{
    let _ = FONT_TINY.render_aligned(
        text,
        Point::new(4, baseline_y),
        VerticalPosition::Baseline,
        HorizontalAlignment::Left,
        FontColor::Transparent(LUMA_DIM),
        target,
    );
}
