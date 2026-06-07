//! "Connecting / Connected / Error" body screen.
//!
//! Renders one of three states based on `UiState::network`:
//!
//! - `Connecting { ssid }` — label "Connecting to" + SSID + spinner
//! - `Connected  { ssid, .. }` — label "Connected to"  + SSID + checkmark/IP
//! - `Error      { reason }`   — bright "Offline" headline + reason
//!
//! Anything else falls back to a dash so the screen never panics on an
//! unexpected phase.

use core::fmt::Write as _;

use embedded_graphics::draw_target::DrawTarget;
use embedded_graphics::geometry::{Point, Size};
use embedded_graphics::pixelcolor::Gray4;
use embedded_graphics::primitives::{PrimitiveStyleBuilder, Rectangle, StyledDrawable};
use heapless::String;
use u8g2_fonts::types::{FontColor, HorizontalAlignment, VerticalPosition};

use super::spinner::{self, SPINNER_CENTER_Y};
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
    header,
};

pub fn draw<D, E>(target: &mut D, state: &UiState) -> Result<(), E>
where
    D: DrawTarget<Color = Gray4, Error = E>,
{
    Rectangle::new(Point::new(0, 0), Size::new(128, 128)).draw_styled(
        &PrimitiveStyleBuilder::new().fill_color(LUMA_BG).build(),
        target,
    )?;
    header::draw(target, state)?;

    let body_top = header::BODY_TOP_Y;

    match &state.network {
        NetworkPhase::Connected { ssid, ipv4, .. } => {
            draw_connected(target, body_top, ssid.as_str(), *ipv4, state.now_ms)?
        },
        NetworkPhase::Error { reason } => draw_error(target, body_top, reason.as_str())?,
        NetworkPhase::Connecting { ssid } => {
            draw_connecting(target, body_top, ssid.as_str(), state.now_ms)?
        },
        _ => draw_connecting(target, body_top, "—", state.now_ms)?,
    }

    Ok(())
}

fn draw_connecting<D, E>(target: &mut D, body_top: i32, ssid: &str, now_ms: u64) -> Result<(), E>
where
    D: DrawTarget<Color = Gray4, Error = E>,
{
    let _ = FONT_BODY.render_aligned(
        "Connecting to",
        Point::new(64, body_top + 22),
        VerticalPosition::Baseline,
        HorizontalAlignment::Center,
        FontColor::Transparent(LUMA_TEXT),
        target,
    );
    let _ = FONT_BODY.render_aligned(
        ssid,
        Point::new(64, body_top + 36),
        VerticalPosition::Baseline,
        HorizontalAlignment::Center,
        FontColor::Transparent(LUMA_ACCENT),
        target,
    );
    spinner::draw_spinner(
        target,
        spinner::DEFAULT_STYLE,
        Point::new(64, SPINNER_CENTER_Y),
        now_ms,
    )?;
    let _ = FONT_TINY.render_aligned(
        "Waiting for AP",
        Point::new(64, 122),
        VerticalPosition::Baseline,
        HorizontalAlignment::Center,
        FontColor::Transparent(LUMA_DIM),
        target,
    );
    Ok(())
}

fn draw_connected<D, E>(
    target: &mut D,
    body_top: i32,
    ssid: &str,
    ipv4: [u8; 4],
    now_ms: u64,
) -> Result<(), E>
where
    D: DrawTarget<Color = Gray4, Error = E>,
{
    let _ = FONT_BODY.render_aligned(
        "Connected to",
        Point::new(64, body_top + 22),
        VerticalPosition::Baseline,
        HorizontalAlignment::Center,
        FontColor::Transparent(LUMA_TEXT),
        target,
    );
    let _ = FONT_BODY.render_aligned(
        ssid,
        Point::new(64, body_top + 36),
        VerticalPosition::Baseline,
        HorizontalAlignment::Center,
        FontColor::Transparent(LUMA_ACCENT),
        target,
    );

    if ipv4 == [0, 0, 0, 0] {
        // Associated to the AP but DHCP hasn't issued a lease yet. Reuse
        // the orbital spinner + a tiny "Acquiring IP" label so the operator
        // doesn't read a contradictory "IP: —" cell.
        spinner::draw_spinner(
            target,
            spinner::DEFAULT_STYLE,
            Point::new(64, SPINNER_CENTER_Y),
            now_ms,
        )?;
        let _ = FONT_TINY.render_aligned(
            "Acquiring IP",
            Point::new(64, 122),
            VerticalPosition::Baseline,
            HorizontalAlignment::Center,
            FontColor::Transparent(LUMA_DIM),
            target,
        );
    } else {
        // Lease in hand — promote the IP to a prominent body line and the
        // footer just says "Online".
        let mut ip_buf: String<24> = String::new();
        let _ = write!(
            &mut ip_buf,
            "{}.{}.{}.{}",
            ipv4[0], ipv4[1], ipv4[2], ipv4[3],
        );
        let _ = FONT_TINY.render_aligned(
            "IP",
            Point::new(64, body_top + 58),
            VerticalPosition::Baseline,
            HorizontalAlignment::Center,
            FontColor::Transparent(LUMA_DIM),
            target,
        );
        let _ = FONT_BODY.render_aligned(
            ip_buf.as_str(),
            Point::new(64, body_top + 74),
            VerticalPosition::Baseline,
            HorizontalAlignment::Center,
            FontColor::Transparent(LUMA_ACCENT),
            target,
        );
        let _ = FONT_TINY.render_aligned(
            "Online",
            Point::new(64, 122),
            VerticalPosition::Baseline,
            HorizontalAlignment::Center,
            FontColor::Transparent(LUMA_DIM),
            target,
        );
    }

    Ok(())
}

fn draw_error<D, E>(target: &mut D, body_top: i32, reason: &str) -> Result<(), E>
where
    D: DrawTarget<Color = Gray4, Error = E>,
{
    let _ = FONT_HEADLINE.render_aligned(
        "Offline",
        Point::new(64, body_top + 32),
        VerticalPosition::Baseline,
        HorizontalAlignment::Center,
        FontColor::Transparent(LUMA_ACCENT),
        target,
    );
    let _ = FONT_BODY.render_aligned(
        reason,
        Point::new(64, body_top + 60),
        VerticalPosition::Baseline,
        HorizontalAlignment::Center,
        FontColor::Transparent(LUMA_TEXT),
        target,
    );
    let _ = FONT_TINY.render_aligned(
        "Re-upload config",
        Point::new(64, 122),
        VerticalPosition::Baseline,
        HorizontalAlignment::Center,
        FontColor::Transparent(LUMA_DIM),
        target,
    );
    Ok(())
}
