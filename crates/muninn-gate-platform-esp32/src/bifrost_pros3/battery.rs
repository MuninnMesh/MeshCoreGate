//! MAX17048 LiPo fuel-gauge driver on the ProS3 STEMMA I2C bus.
//!
//! Pinout: SDA = GPIO 8, SCL = GPIO 9 (also routed to the STEMMA QT
//! connector), bus address `0x36`. The fuel gauge is gated by LDO2 — bring
//! up [`super::power::Ldo2Rail`] and let the rail settle (~50 ms) before
//! constructing a [`Max17048`].
//!
//! The bus is shared with the SSD1327 grayscale OLED, so this driver takes
//! a generic `embedded-hal 1.0` `I2c` device — typically an
//! `embedded_hal_bus::i2c::RefCellDevice` over the underlying esp-hal bus.
//!
//! At boot we send a "quick-start" (write `0x4000` to MODE / `0x06`) so the
//! gauge re-derives state-of-charge from the cell voltage it sees right now
//! instead of whatever stale state it powered up with. The datasheet asks
//! for ≥1 s before the next SOC read is trustworthy.

use embedded_hal::i2c::I2c;

const ADDR: u8 = 0x36;
const REG_VCELL: u8 = 0x02;
const REG_SOC: u8 = 0x04;
const REG_MODE: u8 = 0x06;
const MODE_QUICK_START: [u8; 2] = [0x40, 0x00];

/// MAX17048 sample. Voltage is in millivolts; SOC is whole percent
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
    /// A bus transaction failed — likely the gauge is missing, the rail is
    /// off, or there is bus contention.
    Io,
}

/// MAX17048 driver bound to a (shared) I2C device.
pub struct Max17048<I>
{
    i2c: I,
}

impl<I> Max17048<I>
where
    I: I2c,
{
    /// Bind to a (shared) I2C device. Does not talk to the gauge yet.
    pub fn new(i2c: I) -> Self
    {
        Self { i2c }
    }

    /// Force the gauge to re-derive SOC from the cell voltage it sees right
    /// now. Caller must wait ≥1 s before treating subsequent SOC reads as
    /// settled.
    pub fn quick_start(&mut self) -> Result<(), BatteryError>
    {
        self.i2c
            .write(ADDR, &[REG_MODE, MODE_QUICK_START[0], MODE_QUICK_START[1]])
            .map_err(|_| BatteryError::Io)
    }

    /// Read VCELL + SOC and return them as a typed sample.
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
