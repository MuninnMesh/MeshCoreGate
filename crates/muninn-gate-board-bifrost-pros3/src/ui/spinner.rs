//! Pluggable spinner pack — pick a [`SpinnerStyle`] per screen.
//!
//! Each style is a different visual idiom for "working on it":
//!
//! - [`SpinnerStyle::QrShuffle`] — 8 × 8 grid of 2 × 2 cells colored black/grey/white from a
//!   deterministic hash. Reads like a QR code shuffling. Original Bifrost spinner.
//! - [`SpinnerStyle::BrailleDots`] — the classic terminal-spinner 2 × 3 Braille dot grid cycling
//!   through `⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏`.
//! - [`SpinnerStyle::Arc`] — quarter-circle ring segment rotating around a stationary dim outline.
//! - [`SpinnerStyle::Quadrant`] — half-filled circle rotating left/top/right/bottom, like cycling
//!   `◐◓◑◒`.
//! - [`SpinnerStyle::PulseDots`] — three dots with a brightness wave bouncing back and forth
//!   between them, Claude/Codex flavour.
//! - [`SpinnerStyle::BlockHalves`] — single corner cell lit at a time rotating clockwise through
//!   `▘▝▗▖`.
//!
//! Every style fits inside a 20 × 20 px box centered on the caller's
//! `center` point so swapping styles never reflows the surrounding text.
//! Frame cadence is style-specific (see [`SpinnerStyle::frame_interval_ms`])
//! and tuned to look reasonable at both the 5 Hz splash flush rate and
//! the 1 Hz main-loop flush rate.

use embedded_graphics::draw_target::DrawTarget;
use embedded_graphics::geometry::{Point, Size};
use embedded_graphics::pixelcolor::Gray4;
use embedded_graphics::primitives::{
    Circle,
    CornerRadii,
    PrimitiveStyleBuilder,
    Rectangle,
    RoundedRectangle,
    StyledDrawable,
};
use embedded_graphics::{Drawable, Pixel};

use super::{LUMA_ACCENT, LUMA_BG, LUMA_DIM, LUMA_INACTIVE};

/// Y-coordinate the spinner is centered on across every Bifrost screen.
/// Sharing the constant keeps the spinner visually anchored when the
/// user flips between screens.
pub const SPINNER_CENTER_Y: i32 = 80;

/// Default spinner picked by every screen that doesn't override it.
/// Currently Braille dots — the classic terminal spinner is the most
/// readable on the SSD1327 at this size and reuses muscle memory from
/// every CLI tool the operator has touched.
pub const DEFAULT_STYLE: SpinnerStyle = SpinnerStyle::BrailleDots;

/// Which spinner to draw. See module docs for the visual catalogue.
#[allow(
    dead_code,
    reason = "every variant is a published style — pick per screen"
)]
#[derive(Debug, Clone, Copy)]
pub enum SpinnerStyle
{
    /// 8 × 8 grid of black / grey / white cells, hash-driven (QR shuffle).
    QrShuffle,
    /// Classic 10-frame Braille dots terminal spinner.
    BrailleDots,
    /// Quarter-arc rotating around a dim outline ring.
    Arc,
    /// Half-filled circle rotating left/top/right/bottom.
    Quadrant,
    /// Three dots with a brightness wave bouncing between them.
    PulseDots,
    /// Single 8 × 8 quarter-block lit at a time, cycling clockwise.
    BlockHalves,
}

impl SpinnerStyle
{
    /// Milliseconds between frame advances. Picked per-style so each
    /// animation reads at its own pace: 80 ms feels frantic, 500 ms
    /// feels meditative.
    pub const fn frame_interval_ms(self) -> u64
    {
        match self {
            Self::QrShuffle => 500,
            Self::BrailleDots => 100,
            Self::Arc => 150,
            Self::Quadrant => 180,
            Self::PulseDots => 160,
            Self::BlockHalves => 200,
        }
    }
}

/// Compute the current frame index and dispatch to the per-style
/// renderer. The caller owns clearing the surrounding area; each
/// renderer just paints over its own 20 × 20 footprint.
pub fn draw_spinner<D, E>(
    target: &mut D,
    style: SpinnerStyle,
    center: Point,
    now_ms: u64,
) -> Result<(), E>
where
    D: DrawTarget<Color = Gray4, Error = E>,
{
    let frame = (now_ms / style.frame_interval_ms()) as u32;
    match style {
        SpinnerStyle::QrShuffle => draw_qr_shuffle(target, center, frame),
        SpinnerStyle::BrailleDots => draw_braille(target, center, frame),
        SpinnerStyle::Arc => draw_arc(target, center, frame),
        SpinnerStyle::Quadrant => draw_quadrant(target, center, frame),
        SpinnerStyle::PulseDots => draw_pulse_dots(target, center, frame),
        SpinnerStyle::BlockHalves => draw_block_halves(target, center, frame),
    }
}

// ─── QR shuffle ────────────────────────────────────────────────────────

const QR_GRID_CELLS: i32 = 8;
const QR_CELL_PX: i32 = 2;
const QR_BOX_PX: i32 = QR_GRID_CELLS * QR_CELL_PX;
const QR_BORDER_PAD: i32 = 2;
const QR_BORDER_RADIUS: u32 = 2;

fn draw_qr_shuffle<D, E>(target: &mut D, center: Point, frame: u32) -> Result<(), E>
where
    D: DrawTarget<Color = Gray4, Error = E>,
{
    let top_left = Point::new(center.x - QR_BOX_PX / 2, center.y - QR_BOX_PX / 2);
    let border_origin = Point::new(top_left.x - QR_BORDER_PAD, top_left.y - QR_BORDER_PAD);
    let border_size = Size::new(
        (QR_BOX_PX + QR_BORDER_PAD * 2) as u32,
        (QR_BOX_PX + QR_BORDER_PAD * 2) as u32,
    );
    RoundedRectangle::new(
        Rectangle::new(border_origin, border_size),
        CornerRadii::new(Size::new(QR_BORDER_RADIUS, QR_BORDER_RADIUS)),
    )
    .draw_styled(
        &PrimitiveStyleBuilder::new()
            .stroke_color(LUMA_DIM)
            .stroke_width(1)
            .fill_color(LUMA_BG)
            .build(),
        target,
    )?;

    for cy in 0..QR_GRID_CELLS {
        for cx in 0..QR_GRID_CELLS {
            let color = qr_cell_color(cx as u32, cy as u32, frame);
            if color == LUMA_BG {
                continue;
            }
            let style = PrimitiveStyleBuilder::new().fill_color(color).build();
            Rectangle::new(
                Point::new(top_left.x + cx * QR_CELL_PX, top_left.y + cy * QR_CELL_PX),
                Size::new(QR_CELL_PX as u32, QR_CELL_PX as u32),
            )
            .draw_styled(&style, target)?;
        }
    }
    Ok(())
}

/// Pick one of `{LUMA_BG, LUMA_DIM, LUMA_ACCENT}` from a deterministic
/// hash. 8-bucket split (3 black + 2 grey + 3 white) biases toward the
/// extremes so the result reads as a high-contrast QR code.
fn qr_cell_color(cell_x: u32, cell_y: u32, frame: u32) -> Gray4
{
    let h = cell_x
        .wrapping_mul(0x9E37_79B9)
        .wrapping_add(cell_y.wrapping_mul(0x85EB_CA6B))
        .wrapping_add(frame.wrapping_mul(0xC2B2_AE3D));
    match (h >> 13) & 0x7 {
        0..=2 => LUMA_BG,
        3..=4 => LUMA_DIM,
        _ => LUMA_ACCENT,
    }
}

// ─── Braille dots ──────────────────────────────────────────────────────

/// Bitmask per frame, dots numbered 1..=6 by the standard Braille layout:
/// dot 1 = top-left, dot 2 = mid-left, dot 3 = bottom-left, dot 4 =
/// top-right, dot 5 = mid-right, dot 6 = bottom-right. Bit `n-1` set →
/// dot `n` is lit. This is the classic `⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏` 10-frame loop
/// every spinner library reaches for.
const BRAILLE_FRAMES: [u8; 10] = [
    0b00_001011, // ⠋ dots 1,2,4
    0b00_011001, // ⠙ dots 1,4,5
    0b00_111001, // ⠹ dots 1,4,5,6
    0b00_111000, // ⠸ dots 4,5,6
    0b00_111100, // ⠼ dots 3,4,5,6
    0b00_110100, // ⠴ dots 3,5,6
    0b00_100110, // ⠦ dots 2,3,6
    0b00_100111, // ⠧ dots 1,2,3,6
    0b00_000111, // ⠇ dots 1,2,3
    0b00_001111, // ⠏ dots 1,2,3,4
];

/// (column, row) of each Braille dot in a 2 × 3 grid, in the same bit
/// order as [`BRAILLE_FRAMES`].
const BRAILLE_POSITIONS: [(i32, i32); 6] = [
    (0, 0),
    (0, 1),
    (0, 2), // dots 1, 2, 3
    (1, 0),
    (1, 1),
    (1, 2), // dots 4, 5, 6
];

/// Diameter (px) of each Braille dot. 7 with a 9-px pitch gives a
/// 16 × 25 px footprint — square-ish enough to read as a single glyph,
/// big enough that individual dots register at arm's length on the
/// 128 × 128 panel.
const BRAILLE_DOT_DIAMETER: u32 = 7;
const BRAILLE_DOT_PITCH: i32 = 9;

fn draw_braille<D, E>(target: &mut D, center: Point, frame: u32) -> Result<(), E>
where
    D: DrawTarget<Color = Gray4, Error = E>,
{
    let bits = BRAILLE_FRAMES[(frame as usize) % BRAILLE_FRAMES.len()];
    let grid_w = BRAILLE_DOT_PITCH + BRAILLE_DOT_DIAMETER as i32;
    let grid_h = BRAILLE_DOT_PITCH * 2 + BRAILLE_DOT_DIAMETER as i32;
    let top_left = Point::new(center.x - grid_w / 2, center.y - grid_h / 2);

    // Unlit dots use `LUMA_INACTIVE` (luma 2) rather than `LUMA_DIM`
    // (luma 5) so the active animation reads with high contrast against
    // a near-invisible static grid — mirrors the "barely-there" off
    // state of a real Braille display.
    let lit_style = PrimitiveStyleBuilder::new().fill_color(LUMA_ACCENT).build();
    let dim_style = PrimitiveStyleBuilder::new()
        .fill_color(LUMA_INACTIVE)
        .build();
    for (idx, (cx, cy)) in BRAILLE_POSITIONS.iter().enumerate() {
        let x = top_left.x + cx * BRAILLE_DOT_PITCH;
        let y = top_left.y + cy * BRAILLE_DOT_PITCH;
        let lit = (bits >> idx) & 1 != 0;
        Circle::new(Point::new(x, y), BRAILLE_DOT_DIAMETER)
            .draw_styled(if lit { &lit_style } else { &dim_style }, target)?;
    }
    Ok(())
}

// ─── Arc ───────────────────────────────────────────────────────────────

const ARC_RADIUS_OUTER: i32 = 9;
const ARC_RADIUS_INNER: i32 = 6;

fn draw_arc<D, E>(target: &mut D, center: Point, frame: u32) -> Result<(), E>
where
    D: DrawTarget<Color = Gray4, Error = E>,
{
    let phase = frame % 4;
    let r_outer_sq = ARC_RADIUS_OUTER * ARC_RADIUS_OUTER;
    let r_inner_sq = ARC_RADIUS_INNER * ARC_RADIUS_INNER;

    for dy in -ARC_RADIUS_OUTER..=ARC_RADIUS_OUTER {
        for dx in -ARC_RADIUS_OUTER..=ARC_RADIUS_OUTER {
            let d2 = dx * dx + dy * dy;
            if d2 > r_outer_sq || d2 < r_inner_sq {
                continue;
            }
            // Which 90° wedge does this pixel sit in? Using `|dx|` vs
            // `|dy|` to pick the dominant axis keeps wedge boundaries
            // perfectly diagonal without any trig.
            let in_wedge = match phase {
                0 => dy < 0 && dx.abs() <= dy.abs(), // top
                1 => dx > 0 && dy.abs() <= dx,       // right
                2 => dy > 0 && dx.abs() <= dy,       // bottom
                _ => dx < 0 && dy.abs() <= dx.abs(), // left
            };
            let color = if in_wedge { LUMA_ACCENT } else { LUMA_DIM };
            Pixel(Point::new(center.x + dx, center.y + dy), color).draw(target)?;
        }
    }
    Ok(())
}

// ─── Quadrant ──────────────────────────────────────────────────────────

const QUADRANT_RADIUS: i32 = 8;

fn draw_quadrant<D, E>(target: &mut D, center: Point, frame: u32) -> Result<(), E>
where
    D: DrawTarget<Color = Gray4, Error = E>,
{
    let phase = frame % 4;
    let r2 = QUADRANT_RADIUS * QUADRANT_RADIUS;

    for dy in -QUADRANT_RADIUS..=QUADRANT_RADIUS {
        for dx in -QUADRANT_RADIUS..=QUADRANT_RADIUS {
            if dx * dx + dy * dy > r2 {
                continue;
            }
            let bright = match phase {
                0 => dx < 0, // ◐ left half
                1 => dy < 0, // ◓ top half
                2 => dx > 0, // ◑ right half
                _ => dy > 0, // ◒ bottom half
            };
            let color = if bright { LUMA_ACCENT } else { LUMA_DIM };
            Pixel(Point::new(center.x + dx, center.y + dy), color).draw(target)?;
        }
    }
    Ok(())
}

// ─── Pulse dots (Claude / Codex flavour) ───────────────────────────────

const PULSE_DOT_RADIUS: u32 = 3;
const PULSE_DOT_PITCH: i32 = 9;

/// Brightness step for the wave: the *bright* dot is the peak, neighbour
/// dots are mid-grey, far dots are dim. Cycling `bright` 0→1→2→1 gives a
/// 4-frame ping-pong that reads as a wave bouncing between three dots.
fn draw_pulse_dots<D, E>(target: &mut D, center: Point, frame: u32) -> Result<(), E>
where
    D: DrawTarget<Color = Gray4, Error = E>,
{
    let bright_dot: i32 = match frame % 4 {
        0 => 0,
        1 => 1,
        2 => 2,
        _ => 1,
    };
    let y_top = center.y - PULSE_DOT_RADIUS as i32;
    let start_x = center.x - PULSE_DOT_PITCH - PULSE_DOT_RADIUS as i32;

    for i in 0..3i32 {
        let dist = (i - bright_dot).unsigned_abs();
        let luma = match dist {
            0 => 15,
            1 => 8,
            _ => 4,
        };
        let style = PrimitiveStyleBuilder::new()
            .fill_color(Gray4::new(luma))
            .build();
        Circle::new(
            Point::new(start_x + i * PULSE_DOT_PITCH, y_top),
            PULSE_DOT_RADIUS * 2,
        )
        .draw_styled(&style, target)?;
    }
    Ok(())
}

// ─── Block halves (▘▝▗▖) ───────────────────────────────────────────────

const BLOCK_QUARTER_PX: i32 = 8;

fn draw_block_halves<D, E>(target: &mut D, center: Point, frame: u32) -> Result<(), E>
where
    D: DrawTarget<Color = Gray4, Error = E>,
{
    let total = BLOCK_QUARTER_PX * 2;
    let top_left = Point::new(center.x - BLOCK_QUARTER_PX, center.y - BLOCK_QUARTER_PX);

    // Faint outline of the full 16 × 16 frame so the empty quadrants are
    // still visible — without this the spinner looks like a single block
    // teleporting around an invisible 2 × 2 grid.
    Rectangle::new(top_left, Size::new(total as u32, total as u32)).draw_styled(
        &PrimitiveStyleBuilder::new()
            .stroke_color(LUMA_DIM)
            .stroke_width(1)
            .build(),
        target,
    )?;

    // Clockwise rotation: ▘ → ▝ → ▗ → ▖ → ▘ …
    let (qx, qy) = match frame % 4 {
        0 => (0, 0), // ▘ upper-left
        1 => (1, 0), // ▝ upper-right
        2 => (1, 1), // ▗ lower-right
        _ => (0, 1), // ▖ lower-left
    };
    let quad_origin = Point::new(
        top_left.x + qx * BLOCK_QUARTER_PX,
        top_left.y + qy * BLOCK_QUARTER_PX,
    );
    Rectangle::new(
        quad_origin,
        Size::new(BLOCK_QUARTER_PX as u32, BLOCK_QUARTER_PX as u32),
    )
    .draw_styled(
        &PrimitiveStyleBuilder::new().fill_color(LUMA_ACCENT).build(),
        target,
    )?;
    Ok(())
}
