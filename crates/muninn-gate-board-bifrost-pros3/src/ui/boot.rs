//! Boot splash — full-screen 128x128 monochrome bitmap from
//! `boot_img.png`, packed at 1 bit per pixel in [`crate::boot_logo`].
//!
//! Shown for the 500 ms boot advert window while the radio owner services
//! the queued MeshCore gateway advert. A compact spinner is overlaid only
//! when LoRa is initialized, so the animation corresponds to real radio work.

use embedded_graphics::draw_target::DrawTarget;
use embedded_graphics::geometry::{Point, Size};
use embedded_graphics::pixelcolor::Gray4;
use embedded_graphics::primitives::{PrimitiveStyleBuilder, Rectangle, StyledDrawable};
use embedded_graphics::{Drawable, Pixel};

use super::spinner::{SpinnerStyle, draw_spinner};
use super::{LUMA_ACCENT, LUMA_BG, UiState};
use crate::boot_logo::BOOT_LOGO;

/// Render the boot splash to a freshly-cleared frame.
pub fn draw<D, E>(target: &mut D, state: &UiState) -> Result<(), E>
where
    D: DrawTarget<Color = Gray4, Error = E>,
{
    Rectangle::new(Point::new(0, 0), Size::new(128, 128)).draw_styled(
        &PrimitiveStyleBuilder::new().fill_color(LUMA_BG).build(),
        target,
    )?;

    // 1-bpp bitmap, 16 bytes per row, MSB = leftmost pixel.
    for y in 0..128i32 {
        let row_base = (y as usize) * 16;
        for byte_idx in 0..16i32 {
            let byte = BOOT_LOGO[row_base + byte_idx as usize];
            if byte == 0 {
                continue;
            }
            let x_base = byte_idx * 8;
            for bit in 0..8i32 {
                if byte & (1 << (7 - bit)) != 0 {
                    Pixel(Point::new(x_base + bit, y), LUMA_ACCENT).draw(target)?;
                }
            }
        }
    }
    if state.lora_available {
        Rectangle::new(Point::new(48, 103), Size::new(32, 25)).draw_styled(
            &PrimitiveStyleBuilder::new().fill_color(LUMA_BG).build(),
            target,
        )?;
        draw_spinner(
            target,
            SpinnerStyle::PulseDots,
            Point::new(64, 115),
            state.now_ms,
        )?;
    }
    Ok(())
}
