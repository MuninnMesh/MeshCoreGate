#![no_std]
#![warn(missing_docs)]
//! MAX17048 / MAX17049 LiPo fuel-gauge driver — I²C, single-cell.
//!
//! See [`DATASHEET.md`](../DATASHEET.md) for the device specifics
//! (registers, timing, quirks). This crate is platform-agnostic: it
//! takes any `embedded-hal 1.0` `I2c` device, including
//! `embedded_hal_bus::i2c::RefCellDevice` over a shared bus.
//!
//! Typical bring-up:
//!
//! ```ignore
//! let mut gauge = Max17048::new(shared_i2c_device);
//! gauge.quick_start()?;       // re-derive SOC from current cell voltage
//! delay.delay_millis(1_500);  // datasheet recommends ≥1 s before reading SOC
//! let sample = gauge.read()?; // millivolts + 0..=100 % SOC
//! ```

use embedded_hal::i2c::I2c;

/// MAX17048 / MAX17049 7-bit I²C address (fixed by the part — no
/// configurable address pins).
pub const ADDR: u8 = 0x36;

/// `VCELL` register — 16-bit cell voltage in 78.125 µV/LSB.
pub const REG_VCELL: u8 = 0x02;
/// `SOC` register — 16-bit state-of-charge in 1/256 %/LSB.
pub const REG_SOC: u8 = 0x04;
/// `MODE` register — write `0x4000` to trigger a quick-start.
pub const REG_MODE: u8 = 0x06;
/// `MODE` payload that re-derives SOC from the current cell voltage.
const MODE_QUICK_START: [u8; 2] = [0x40, 0x00];

/// One fuel-gauge sample. Voltage in millivolts; SOC in whole percent
/// clamped to `0..=100`.
#[derive(Debug, Clone, Copy)]
pub struct BatterySample
{
    /// Cell voltage in millivolts.
    pub voltage_mv:  u32,
    /// State-of-charge as whole percent, clamped to `0..=100`.
    pub soc_percent: u8,
}

/// Errors returned by the fuel-gauge driver.
#[derive(Debug, Clone, Copy)]
pub enum BatteryError
{
    /// A bus transaction failed — typically the gauge is missing, the
    /// rail is off, or there is bus contention.
    Io,
}

/// MAX17048 driver bound to an `embedded-hal 1.0` I²C device.
pub struct Max17048<I>
{
    i2c: I,
}

impl<I> Max17048<I>
where
    I: I2c,
{
    /// Bind to a (shared) I²C device. Does not talk to the gauge yet.
    pub fn new(i2c: I) -> Self
    {
        Self { i2c }
    }

    /// Force the gauge to re-derive SOC from the cell voltage it sees
    /// right now. Caller must wait ≥1 s before treating subsequent SOC
    /// reads as settled — see DATASHEET.md.
    pub fn quick_start(&mut self) -> Result<(), BatteryError>
    {
        self.i2c
            .write(ADDR, &[REG_MODE, MODE_QUICK_START[0], MODE_QUICK_START[1]])
            .map_err(|_| BatteryError::Io)
    }

    /// Read VCELL + SOC and return a typed sample.
    pub fn read(&mut self) -> Result<BatterySample, BatteryError>
    {
        let mut buf = [0u8; 2];

        self.i2c
            .write_read(ADDR, &[REG_VCELL], &mut buf)
            .map_err(|_| BatteryError::Io)?;
        let raw_vcell: u32 = ((buf[0] as u32) << 8) | (buf[1] as u32);
        // 78.125 µV per LSB → (raw * 78_125) / 1_000_000 = millivolts.
        let voltage_mv = (raw_vcell * 78_125) / 1_000_000;

        self.i2c
            .write_read(ADDR, &[REG_SOC], &mut buf)
            .map_err(|_| BatteryError::Io)?;
        let raw_soc: u32 = ((buf[0] as u32) << 8) | (buf[1] as u32);
        // 1/256 % per LSB → divide by 256, then clamp to 0..=100.
        let soc_percent = (raw_soc / 256).min(100) as u8;

        Ok(BatterySample {
            voltage_mv,
            soc_percent,
        })
    }
}
