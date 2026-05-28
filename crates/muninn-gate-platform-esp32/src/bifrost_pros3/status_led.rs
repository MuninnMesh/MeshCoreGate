//! Driver for the ProS3 on-board WS2812 RGB LED (GPIO 18 via RMT channel 0).
//!
//! WS2812 timing for an 80 MHz RMT source clock with `clk_divider = 1`
//! (12.5 ns per tick). The values below match the working configuration in
//! the `mesh/apps/polygon-pros3d` reference firmware.

use esp_hal::Blocking;
use esp_hal::gpio::Level;
use esp_hal::peripherals::{GPIO18, RMT};
use esp_hal::rmt::{
    Channel,
    ConstChannelAccess,
    PulseCode,
    Rmt,
    Tx,
    TxChannel,
    TxChannelConfig,
    TxChannelCreator,
};
use esp_hal::time::Rate;

// 80 MHz source clock with divider=1 → 12.5 ns per tick.
const T0H_TICKS: u16 = 32; // 400 ns
const T0L_TICKS: u16 = 64; // 800 ns
const T1H_TICKS: u16 = 64; // 800 ns
const T1L_TICKS: u16 = 32; // 400 ns
const BIT_COUNT: usize = 24;
const BUFFER_LEN: usize = BIT_COUNT + 1;

type Ws2812TxChannel = Channel<Blocking, ConstChannelAccess<Tx, 0>>;

/// 24-bit color triple in linear sRGB space, before any brightness scaling.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rgb
{
    /// Red channel intensity.
    pub r: u8,
    /// Green channel intensity.
    pub g: u8,
    /// Blue channel intensity.
    pub b: u8,
}

impl Rgb
{
    /// All channels at zero (LED off).
    pub const OFF: Self = Self::new(0, 0, 0);

    /// Construct from explicit channels.
    pub const fn new(r: u8, g: u8, b: u8) -> Self
    {
        Self { r, g, b }
    }

    /// Apply a per-channel right-shift to lower brightness. `dim(4)` caps each
    /// channel at 15/255, which is bright enough to see from across a room
    /// but not blinding next to a laptop screen.
    pub const fn dim(self, shift: u8) -> Self
    {
        Self {
            r: self.r >> shift,
            g: self.g >> shift,
            b: self.b >> shift,
        }
    }

    /// Pack into the GRB byte order the WS2812 wire format expects, MSB-first.
    pub const fn to_grb(self) -> u32
    {
        ((self.g as u32) << 16) | ((self.r as u32) << 8) | (self.b as u32)
    }
}

/// Errors returned by the RGB driver.
#[derive(Debug, Clone, Copy)]
pub enum StatusLedError
{
    /// RMT peripheral could not be configured.
    Init,
    /// An RMT transmit failed; the channel is dropped and no further updates
    /// are possible until the driver is rebuilt.
    Transmit,
}

/// Blocking WS2812 driver for a single LED on RMT channel 0.
pub struct StatusLed
{
    channel: Option<Ws2812TxChannel>,
    buf:     [u32; BUFFER_LEN],
}

impl StatusLed
{
    /// Configure RMT at 80 MHz and bind the TX channel to the data pin.
    pub fn new(rmt: RMT<'static>, data: GPIO18<'static>) -> Result<Self, StatusLedError>
    {
        let rmt = Rmt::new(rmt, Rate::from_mhz(80)).map_err(|_| StatusLedError::Init)?;
        let channel = rmt
            .channel0
            .configure_tx(data, TxChannelConfig::default().with_clk_divider(1))
            .map_err(|_| StatusLedError::Init)?;
        Ok(Self {
            channel: Some(channel),
            buf:     [0u32; BUFFER_LEN],
        })
    }

    /// Drive the LED to the requested color and block until the WS2812
    /// reset pulse has been emitted.
    pub fn set_color(&mut self, color: Rgb) -> Result<(), StatusLedError>
    {
        let channel = self.channel.take().ok_or(StatusLedError::Transmit)?;
        encode_grb(color.to_grb(), &mut self.buf);
        let transaction =
            TxChannel::transmit(channel, &self.buf).map_err(|_| StatusLedError::Transmit)?;
        let channel = match transaction.wait() {
            Ok(channel) => channel,
            Err((_, channel)) => channel,
        };
        self.channel = Some(channel);
        Ok(())
    }
}

fn encode_grb(grb: u32, buf: &mut [u32; BUFFER_LEN])
{
    for bit_index in 0..BIT_COUNT {
        let bit = (grb >> (23 - bit_index)) & 1;
        buf[bit_index] = if bit == 1 {
            PulseCode::new(Level::High, T1H_TICKS, Level::Low, T1L_TICKS)
        } else {
            PulseCode::new(Level::High, T0H_TICKS, Level::Low, T0L_TICKS)
        };
    }
    buf[BIT_COUNT] = PulseCode::empty();
}
