//! Fixed-capacity text frames used by display page renderers.

use heapless::{String, Vec};
use muninn_gate_core::{DisplayVariant, Error};

/// Text line count for a 128x64 display using a compact 5x7 font.
pub const OLED_128X64_TEXT_LINES: usize = 8;
/// Text line count for a 128x128 display using a compact 5x7 font.
pub const OLED_128X128_TEXT_LINES: usize = 16;
/// Approximate visible text columns for a 128-pixel-wide compact font.
pub const DISPLAY_LINE_CHARS: usize = 21;
/// Internal formatting buffer size used before clipping to the display width.
pub const UI_SCRATCH_CHARS: usize = 96;
/// Maximum producer name characters shown in node rows.
pub const NODE_ROW_NAME_CHARS: usize = 10;

/// Rendered text frame for a fixed-size display layout.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TextFrame<const LINES: usize, const WIDTH: usize>
{
    /// Rendered display lines clipped to the configured display width.
    pub lines: Vec<String<WIDTH>, LINES>,
}

impl<const LINES: usize, const WIDTH: usize> TextFrame<LINES, WIDTH>
{
    /// Create an empty text frame.
    pub const fn new() -> Self
    {
        Self { lines: Vec::new() }
    }

    /// Return true when no more display lines can be added.
    pub fn is_full(&self) -> bool
    {
        self.lines.len() >= LINES
    }

    /// Return the number of unused display lines.
    pub fn remaining_lines(&self) -> usize
    {
        LINES.saturating_sub(self.lines.len())
    }

    /// Add one line, clipping it to the frame width.
    pub fn push_line(&mut self, value: &str) -> Result<(), Error>
    {
        if self.is_full() {
            return Err(Error::Capacity);
        }

        let mut line = String::new();
        copy_truncated(&mut line, value, WIDTH)?;
        self.lines.push(line).map_err(|_| Error::Capacity)
    }
}

impl<const LINES: usize, const WIDTH: usize> Default for TextFrame<LINES, WIDTH>
{
    fn default() -> Self
    {
        Self::new()
    }
}

/// Default text frame for a 128x64 OLED display.
pub type Oled128x64Frame = TextFrame<OLED_128X64_TEXT_LINES, DISPLAY_LINE_CHARS>;

/// Default text frame for a 128x128 OLED display.
pub type Oled128x128Frame = TextFrame<OLED_128X128_TEXT_LINES, DISPLAY_LINE_CHARS>;

/// Local display size supported by the built-in text layouts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DisplaySize
{
    /// Monochrome 128x64 display.
    Oled128x64,
    /// Monochrome 128x128 display.
    Oled128x128,
}

impl DisplaySize
{
    /// Convert a board display variant into a built-in UI display size.
    pub const fn from_variant(variant: DisplayVariant) -> Option<Self>
    {
        match variant {
            DisplayVariant::Oled128x64 => Some(Self::Oled128x64),
            DisplayVariant::Oled128x128 => Some(Self::Oled128x128),
            DisplayVariant::None | DisplayVariant::Custom(_) => None,
        }
    }

    /// Return the default text line count for this display size.
    pub const fn text_lines(self) -> usize
    {
        match self {
            Self::Oled128x64 => OLED_128X64_TEXT_LINES,
            Self::Oled128x128 => OLED_128X128_TEXT_LINES,
        }
    }

    /// Return the default text column count for this display size.
    pub const fn text_columns(self) -> usize
    {
        DISPLAY_LINE_CHARS
    }
}

/// Copy text into a fixed-capacity string without exceeding `max_bytes`.
fn copy_truncated<const N: usize>(
    line: &mut String<N>,
    value: &str,
    max_bytes: usize,
) -> Result<(), Error>
{
    for ch in value.chars() {
        if line.len().saturating_add(ch.len_utf8()) > max_bytes {
            break;
        }
        line.push(ch).map_err(|_| Error::RenderBufferFull)?;
    }
    Ok(())
}
