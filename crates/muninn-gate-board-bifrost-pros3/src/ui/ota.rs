//! Dedicated firmware update screen.

use core::fmt::Write as _;

use embedded_graphics::draw_target::DrawTarget;
use embedded_graphics::geometry::{Point, Size};
use embedded_graphics::pixelcolor::Gray4;
use embedded_graphics::primitives::{PrimitiveStyleBuilder, Rectangle, StyledDrawable};
use heapless::String;
use u8g2_fonts::types::{FontColor, HorizontalAlignment, VerticalPosition};

use super::{
    FONT_BODY,
    FONT_HEADLINE,
    FONT_TINY,
    LUMA_ACCENT,
    LUMA_BG,
    LUMA_DIM,
    LUMA_INACTIVE,
    LUMA_TEXT,
    OtaUpdateStatus,
    UiState,
    header,
    spinner,
};

const PROGRESS_X: i32 = 16;
const PROGRESS_Y: i32 = 98;
const PROGRESS_W: u32 = 96;
const PROGRESS_H: u32 = 8;

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
    let _ = FONT_HEADLINE.render_aligned(
        "Firmware OTA",
        Point::new(64, body_top + 16),
        VerticalPosition::Baseline,
        HorizontalAlignment::Center,
        FontColor::Transparent(LUMA_ACCENT),
        target,
    );

    spinner::draw_spinner(
        target,
        spinner::DEFAULT_STYLE,
        Point::new(64, body_top + 52),
        state.now_ms,
    )?;

    match state.ota_update {
        Some(status) => draw_progress(target, status)?,
        None => draw_waiting(target)?,
    }

    let _ = FONT_TINY.render_aligned(
        "Do not power off",
        Point::new(64, 122),
        VerticalPosition::Baseline,
        HorizontalAlignment::Center,
        FontColor::Transparent(LUMA_DIM),
        target,
    );
    Ok(())
}

fn draw_waiting<D, E>(target: &mut D) -> Result<(), E>
where
    D: DrawTarget<Color = Gray4, Error = E>,
{
    let _ = FONT_BODY.render_aligned(
        "Waiting for image",
        Point::new(64, 91),
        VerticalPosition::Baseline,
        HorizontalAlignment::Center,
        FontColor::Transparent(LUMA_TEXT),
        target,
    );
    Ok(())
}

fn draw_progress<D, E>(target: &mut D, status: OtaUpdateStatus) -> Result<(), E>
where
    D: DrawTarget<Color = Gray4, Error = E>,
{
    let (Some(received), Some(total)) = (status.received_bytes, status.total_bytes) else {
        return draw_waiting(target);
    };
    let total = total.max(1);
    let pct = ((u64::from(received.min(total)) * 100) / u64::from(total)) as u8;

    let mut label: String<24> = String::new();
    let _ = write!(&mut label, "Receiving {}%", pct);
    let _ = FONT_BODY.render_aligned(
        label.as_str(),
        Point::new(64, 91),
        VerticalPosition::Baseline,
        HorizontalAlignment::Center,
        FontColor::Transparent(LUMA_TEXT),
        target,
    );
    draw_progress_bar(target, pct)
}

fn draw_progress_bar<D, E>(target: &mut D, pct: u8) -> Result<(), E>
where
    D: DrawTarget<Color = Gray4, Error = E>,
{
    let outline = PrimitiveStyleBuilder::new()
        .stroke_color(LUMA_DIM)
        .stroke_width(1)
        .build();
    Rectangle::new(
        Point::new(PROGRESS_X, PROGRESS_Y),
        Size::new(PROGRESS_W, PROGRESS_H),
    )
    .draw_styled(&outline, target)?;

    let fill_w = (PROGRESS_W.saturating_sub(2) * u32::from(pct.min(100))) / 100;
    if fill_w > 0 {
        Rectangle::new(
            Point::new(PROGRESS_X + 1, PROGRESS_Y + 1),
            Size::new(fill_w, PROGRESS_H.saturating_sub(2)),
        )
        .draw_styled(
            &PrimitiveStyleBuilder::new()
                .fill_color(if pct >= 100 { LUMA_ACCENT } else { LUMA_TEXT })
                .build(),
            target,
        )?;
    }

    if fill_w < PROGRESS_W.saturating_sub(2) {
        Rectangle::new(
            Point::new(PROGRESS_X + 1 + fill_w as i32, PROGRESS_Y + 1),
            Size::new(
                PROGRESS_W.saturating_sub(2) - fill_w,
                PROGRESS_H.saturating_sub(2),
            ),
        )
        .draw_styled(
            &PrimitiveStyleBuilder::new()
                .fill_color(LUMA_INACTIVE)
                .build(),
            target,
        )?;
    }
    Ok(())
}
