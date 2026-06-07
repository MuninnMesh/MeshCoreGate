#![no_std]
#![warn(missing_docs)]
//! SSD1327 128×128 4-bit grayscale OLED driver over I²C.
//!
//! See [`DATASHEET.md`](../DATASHEET.md) for device-level details.
//!
//! The Adafruit 4741 / 5634 family wires an SSD1327 to a SparkFun Qwiic /
//! STEMMA QT I²C connector with on-board 3V3 LDO + 12 V boost + level
//! shifters. From the host we only need a 3V3 supply and a vanilla I²C
//! transport — everything else is on-board. Default I²C address is `0x3D`
//! (a solderable jumper moves it to `0x3C`).
//!
//! The panel is 128×128 4-bit grayscale (16 luma levels). Each byte in the
//! framebuffer holds two horizontally-adjacent pixels: high nibble = left
//! pixel, low nibble = right pixel. Total framebuffer = 128 × 128 / 2 = 8 KiB,
//! held in a `'static` buffer the caller passes in so the driver itself
//! stays heap-free.
//!
//! Init/command sequence cribbed from the official SSD1327 datasheet and
//! cross-checked against the working `ssd1327-i2c` 0.2 crate (which targets
//! embedded-hal 0.2; this driver targets 1.0 so we can use esp-hal directly).

use embedded_graphics::Pixel;
use embedded_graphics::draw_target::DrawTarget;
use embedded_graphics::geometry::{OriginDimensions, Size};
use embedded_graphics::pixelcolor::{Gray4, GrayColor};
use embedded_hal::i2c::{I2c, Operation};

/// Display panel width in pixels.
pub const WIDTH: u16 = 128;
/// Display panel height in pixels.
pub const HEIGHT: u16 = 128;
/// Bytes needed to hold the full framebuffer at 4 bits per pixel
/// (two horizontally-adjacent pixels per byte).
pub const FRAMEBUFFER_LEN: usize = (WIDTH as usize) * (HEIGHT as usize) / 2;

/// Default Adafruit 4741 / 5634 I2C address.
pub const DEFAULT_ADDR: u8 = 0x3D;

/// Errors surfaced by the SSD1327 driver.
#[derive(Debug, Clone, Copy)]
pub enum DisplayError
{
    /// An I2C transaction failed.
    Bus,
}

/// Owned driver wrapping a single SSD1327 on a shared I2C bus.
pub struct Ssd1327<I>
{
    i2c:  I,
    addr: u8,
    fb:   &'static mut [u8; FRAMEBUFFER_LEN],
}

impl<I> Ssd1327<I>
where
    I: I2c,
{
    /// Bind the driver to an I2C device. Does not talk to the panel; call
    /// [`Self::init`] before drawing.
    pub fn new(i2c: I, addr: u8, framebuffer: &'static mut [u8; FRAMEBUFFER_LEN]) -> Self
    {
        framebuffer.fill(0);
        Self {
            i2c,
            addr,
            fb: framebuffer,
        }
    }

    /// Run the panel through its power-on init sequence and enable display.
    pub fn init(&mut self) -> Result<(), DisplayError>
    {
        // Unlock command interface, blank panel, configure addressing/colour,
        // then re-enable. Values follow the SSD1327 datasheet recommended
        // boot sequence for a 128×128 grayscale module.
        self.send_cmd(&[0xFD, 0x12])?; // command unlock
        self.send_cmd(&[0xAE])?; // display OFF
        self.send_cmd(&[0x15, 0x00, 0x3F])?; // column addr 0..=63 (=> 128 pixels, 2/byte)
        self.send_cmd(&[0x75, 0x00, 0x7F])?; // row addr 0..=127
        self.send_cmd(&[0x81, 0x80])?; // contrast (~50%)
        self.send_cmd(&[0xA0, 0x51])?; // remap: column + nibble + COM split odd/even
        self.send_cmd(&[0xA1, 0x00])?; // display start line 0
        self.send_cmd(&[0xA2, 0x00])?; // display offset 0
        self.send_cmd(&[0xA4])?; // normal display mode (not all-on / not all-off / not inverse)
        self.send_cmd(&[0xA8, 0x7F])?; // multiplex ratio = 128
        self.send_cmd(&[0xB1, 0x51])?; // phase length
        self.send_cmd(&[0xB3, 0x00])?; // front clock divider / oscillator freq
        self.send_cmd(&[0xAB, 0x01])?; // function selection A: internal VDD
        self.send_cmd(&[0xB6, 0x04])?; // second pre-charge period
        self.send_cmd(&[0xBE, 0x0F])?; // VCOMH
        self.send_cmd(&[0xBC, 0x08])?; // pre-charge voltage
        self.send_cmd(&[0xD5, 0x62])?; // function selection B: enable second precharge + ext VSL
        self.send_cmd(&[0xB9])?; // default linear grayscale table
        self.send_cmd(&[0xAF])?; // display ON
        Ok(())
    }

    /// Adjust panel contrast (0..=255). Higher values are brighter and burn
    /// more current — keep low during bring-up.
    #[allow(
        dead_code,
        reason = "exposed for callers that want runtime brightness control"
    )]
    pub fn set_contrast(&mut self, value: u8) -> Result<(), DisplayError>
    {
        self.send_cmd(&[0x81, value])
    }

    /// Push the entire framebuffer to the panel in one I2C transaction.
    ///
    /// Uses `i2c.transaction()` to stream `[0x40, ...framebuffer]` as two
    /// back-to-back `Write` operations: the SSD1327 sees the control byte
    /// followed by 8 KiB of pixel data with no STOP in between. This
    /// amortizes a single I2C START + addr over the whole frame instead
    /// of repeating it 512 times for 16-byte chunks. At 800 kHz the
    /// dominant cost is the 8 KiB payload itself (~80 ms), so the
    /// transaction-vs-chunked savings are smaller than the bus-speed
    /// bump but still help.
    pub fn flush(&mut self) -> Result<(), DisplayError>
    {
        // Re-arm window to the full panel each flush so a previous
        // partial window write can't desync the cursor.
        self.send_cmd(&[0x15, 0x00, 0x3F])?;
        self.send_cmd(&[0x75, 0x00, 0x7F])?;

        self.i2c
            .transaction(
                self.addr,
                &mut [Operation::Write(&[0x40]), Operation::Write(&self.fb[..])],
            )
            .map_err(|_| DisplayError::Bus)
    }

    fn send_cmd(&mut self, payload: &[u8]) -> Result<(), DisplayError>
    {
        // The "command" control byte is 0x00. Wrap each payload byte:
        // 0x00 cmd 0x00 arg 0x00 arg ... isn't actually required — sending
        // 0x00 once and then the byte stream works on this controller and
        // matches the ssd1327-i2c reference implementation.
        let mut buf = [0u8; 8];
        buf[0] = 0x00;
        buf[1..1 + payload.len()].copy_from_slice(payload);
        self.i2c
            .write(self.addr, &buf[..1 + payload.len()])
            .map_err(|_| DisplayError::Bus)
    }
}

impl<I> DrawTarget for Ssd1327<I>
where
    I: I2c,
{
    type Color = Gray4;
    type Error = DisplayError;

    fn draw_iter<P>(&mut self, pixels: P) -> Result<(), Self::Error>
    where
        P: IntoIterator<Item = Pixel<Self::Color>>,
    {
        for Pixel(coord, color) in pixels.into_iter() {
            let x = coord.x;
            let y = coord.y;
            if !(0..WIDTH as i32).contains(&x) || !(0..HEIGHT as i32).contains(&y) {
                continue;
            }
            let byte_index = (y as usize) * (WIDTH as usize / 2) + (x as usize) / 2;
            let luma = color.luma() & 0x0F;
            let byte = &mut self.fb[byte_index];
            if x % 2 == 0 {
                // Even x = upper nibble (left pixel in the byte).
                *byte = (*byte & 0x0F) | (luma << 4);
            } else {
                // Odd x = lower nibble (right pixel).
                *byte = (*byte & 0xF0) | luma;
            }
        }
        Ok(())
    }

    fn clear(&mut self, color: Self::Color) -> Result<(), Self::Error>
    {
        let luma = color.luma() & 0x0F;
        let byte = (luma << 4) | luma;
        self.fb.fill(byte);
        Ok(())
    }
}

impl<I> OriginDimensions for Ssd1327<I>
where
    I: I2c,
{
    fn size(&self) -> Size
    {
        Size::new(WIDTH as u32, HEIGHT as u32)
    }
}
