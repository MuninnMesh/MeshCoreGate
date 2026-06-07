#![cfg_attr(not(test), no_std)]
#![warn(missing_docs)]
//! Minimal DS3231 RTC driver.
//!
//! The driver stores and reads UTC Unix seconds. The chip itself has no
//! timezone model; callers should apply display offsets outside the RTC.

use embedded_hal::i2c::I2c;

/// DS3231 7-bit I2C address.
pub const ADDR: u8 = 0x68;

const REG_SECONDS: u8 = 0x00;
const REG_CONTROL: u8 = 0x0E;
const REG_STATUS: u8 = 0x0F;
const CONTROL_EOSC: u8 = 0x80;
const STATUS_OSF: u8 = 0x80;
const SECONDS_PER_DAY: i64 = 86_400;

/// Errors returned by the DS3231 driver.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RtcError
{
    /// I2C transaction failed.
    Io,
    /// RTC registers contained an invalid calendar value.
    InvalidDateTime,
    /// The requested Unix time is outside the supported DS3231 range.
    OutOfRange,
}

/// DS3231 driver bound to an `embedded-hal 1.0` I2C device.
pub struct Ds3231<I>
{
    i2c: I,
}

impl<I> Ds3231<I>
where
    I: I2c,
{
    /// Bind to an I2C device. Does not touch the RTC.
    pub fn new(i2c: I) -> Self
    {
        Self { i2c }
    }

    /// Probe the RTC and ensure the oscillator is enabled.
    pub fn init(&mut self) -> Result<(), RtcError>
    {
        let mut control = [0u8; 1];
        self.i2c
            .write_read(ADDR, &[REG_CONTROL], &mut control)
            .map_err(|_| RtcError::Io)?;
        if control[0] & CONTROL_EOSC != 0 {
            self.i2c
                .write(ADDR, &[REG_CONTROL, control[0] & !CONTROL_EOSC])
                .map_err(|_| RtcError::Io)?;
        }
        Ok(())
    }

    /// Read the current RTC time as UTC Unix seconds.
    pub fn read_unix_seconds(&mut self) -> Result<u64, RtcError>
    {
        let mut regs = [0u8; 7];
        self.i2c
            .write_read(ADDR, &[REG_SECONDS], &mut regs)
            .map_err(|_| RtcError::Io)?;

        let second = bcd_to_u8(regs[0] & 0x7F).ok_or(RtcError::InvalidDateTime)?;
        let minute = bcd_to_u8(regs[1] & 0x7F).ok_or(RtcError::InvalidDateTime)?;
        let hour = decode_hour(regs[2]).ok_or(RtcError::InvalidDateTime)?;
        let day = bcd_to_u8(regs[4] & 0x3F).ok_or(RtcError::InvalidDateTime)?;
        let raw_month = regs[5];
        let month = bcd_to_u8(raw_month & 0x1F).ok_or(RtcError::InvalidDateTime)?;
        let mut year = 2000_i32 + i32::from(bcd_to_u8(regs[6]).ok_or(RtcError::InvalidDateTime)?);
        if raw_month & 0x80 != 0 {
            year += 100;
        }

        unix_from_datetime(year, month, day, hour, minute, second).ok_or(RtcError::InvalidDateTime)
    }

    /// Write UTC Unix seconds into the RTC.
    pub fn write_unix_seconds(&mut self, unix_seconds: u64) -> Result<(), RtcError>
    {
        let dt = datetime_from_unix(unix_seconds).ok_or(RtcError::OutOfRange)?;
        if !(2000..=2199).contains(&dt.year) {
            return Err(RtcError::OutOfRange);
        }

        let year_in_century = ((dt.year - 2000) % 100) as u8;
        let century = if dt.year >= 2100 { 0x80 } else { 0x00 };
        let regs = [
            REG_SECONDS,
            u8_to_bcd(dt.second),
            u8_to_bcd(dt.minute),
            u8_to_bcd(dt.hour),
            u8_to_bcd(weekday_1_to_7(unix_seconds)),
            u8_to_bcd(dt.day),
            u8_to_bcd(dt.month) | century,
            u8_to_bcd(year_in_century),
        ];
        self.i2c.write(ADDR, &regs).map_err(|_| RtcError::Io)?;

        let mut status = [0u8; 1];
        if self
            .i2c
            .write_read(ADDR, &[REG_STATUS], &mut status)
            .map_err(|_| RtcError::Io)
            .is_ok()
            && status[0] & STATUS_OSF != 0
        {
            self.i2c
                .write(ADDR, &[REG_STATUS, status[0] & !STATUS_OSF])
                .map_err(|_| RtcError::Io)?;
        }
        self.init()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct DateTime
{
    year:   i32,
    month:  u8,
    day:    u8,
    hour:   u8,
    minute: u8,
    second: u8,
}

fn decode_hour(raw: u8) -> Option<u8>
{
    if raw & 0x40 == 0 {
        let hour = bcd_to_u8(raw & 0x3F)?;
        return (hour < 24).then_some(hour);
    }

    let mut hour = bcd_to_u8(raw & 0x1F)?;
    if hour == 0 || hour > 12 {
        return None;
    }
    let pm = raw & 0x20 != 0;
    if pm {
        if hour != 12 {
            hour = hour.saturating_add(12);
        }
    } else if hour == 12 {
        hour = 0;
    }
    Some(hour)
}

fn bcd_to_u8(raw: u8) -> Option<u8>
{
    let hi = raw >> 4;
    let lo = raw & 0x0F;
    (hi < 10 && lo < 10).then_some(hi * 10 + lo)
}

fn u8_to_bcd(value: u8) -> u8
{
    ((value / 10) << 4) | (value % 10)
}

fn datetime_from_unix(unix_seconds: u64) -> Option<DateTime>
{
    let unix_seconds = i64::try_from(unix_seconds).ok()?;
    let days = unix_seconds.div_euclid(SECONDS_PER_DAY);
    let seconds_of_day = unix_seconds.rem_euclid(SECONDS_PER_DAY);
    let (year, month, day) = civil_from_days(days);
    Some(DateTime {
        year,
        month,
        day,
        hour: (seconds_of_day / 3_600) as u8,
        minute: ((seconds_of_day % 3_600) / 60) as u8,
        second: (seconds_of_day % 60) as u8,
    })
}

fn unix_from_datetime(
    year: i32,
    month: u8,
    day: u8,
    hour: u8,
    minute: u8,
    second: u8,
) -> Option<u64>
{
    if !(1..=12).contains(&month)
        || !(1..=31).contains(&day)
        || hour > 23
        || minute > 59
        || second > 59
    {
        return None;
    }
    let days = days_from_civil(year, month, day)?;
    let seconds = days
        .checked_mul(SECONDS_PER_DAY)?
        .checked_add(i64::from(hour) * 3_600)?
        .checked_add(i64::from(minute) * 60)?
        .checked_add(i64::from(second))?;
    u64::try_from(seconds).ok()
}

fn civil_from_days(days_since_epoch: i64) -> (i32, u8, u8)
{
    let z = days_since_epoch + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let mut year = yoe as i32 + era as i32 * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u8;
    let month = (mp + if mp < 10 { 3 } else { -9 }) as u8;
    if month <= 2 {
        year += 1;
    }
    (year, month, day)
}

fn days_from_civil(year: i32, month: u8, day: u8) -> Option<i64>
{
    let max_day = days_in_month(year, month)?;
    if day == 0 || day > max_day {
        return None;
    }

    let year = i64::from(year) - if month <= 2 { 1 } else { 0 };
    let era = if year >= 0 { year } else { year - 399 } / 400;
    let yoe = year - era * 400;
    let month = i64::from(month);
    let day = i64::from(day);
    let mp = month + if month > 2 { -3 } else { 9 };
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    Some(era * 146_097 + doe - 719_468)
}

fn days_in_month(year: i32, month: u8) -> Option<u8>
{
    Some(match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if is_leap_year(year) => 29,
        2 => 28,
        _ => return None,
    })
}

fn is_leap_year(year: i32) -> bool
{
    (year % 4 == 0 && year % 100 != 0) || year % 400 == 0
}

fn weekday_1_to_7(unix_seconds: u64) -> u8
{
    // 1970-01-01 was Thursday. Use 1=Sunday, 7=Saturday.
    let days = unix_seconds / 86_400;
    (((days + 4) % 7) + 1) as u8
}

#[cfg(test)]
mod tests
{
    use super::{datetime_from_unix, decode_hour, unix_from_datetime, weekday_1_to_7};

    #[test]
    fn converts_unix_to_calendar()
    {
        let dt = datetime_from_unix(1_704_067_200).unwrap();
        assert_eq!((dt.year, dt.month, dt.day), (2024, 1, 1));
        assert_eq!((dt.hour, dt.minute, dt.second), (0, 0, 0));
    }

    #[test]
    fn converts_calendar_to_unix()
    {
        assert_eq!(unix_from_datetime(2024, 1, 1, 0, 0, 0), Some(1_704_067_200),);
        assert_eq!(unix_from_datetime(2023, 2, 29, 0, 0, 0), None);
    }

    #[test]
    fn decodes_12_hour_mode()
    {
        assert_eq!(decode_hour(0x40 | 0x12), Some(0));
        assert_eq!(decode_hour(0x40 | 0x20 | 0x12), Some(12));
        assert_eq!(decode_hour(0x40 | 0x20 | 0x01), Some(13));
    }

    #[test]
    fn weekday_uses_ds3231_range()
    {
        assert_eq!(weekday_1_to_7(0), 5);
        assert_eq!(weekday_1_to_7(86_400), 6);
    }
}
