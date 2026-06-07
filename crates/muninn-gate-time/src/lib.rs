#![cfg_attr(not(test), no_std)]
#![warn(missing_docs)]
//! Small no-std wall-clock helpers for Muninn Gate firmware.
//!
//! Boards may run without an RTC. In that case [`GatewayTime`] can still be
//! seeded from USB provisioning and advanced from the monotonic boot clock.
//! When a DS3231 or another RTC is available, board code can implement
//! [`RtcTimeSource`] and use the same state object.

/// Central Standard Time offset from UTC, in minutes.
pub const CST_UTC_OFFSET_MINUTES: i16 = -6 * 60;

/// Minimum accepted UTC offset in minutes.
pub const MIN_UTC_OFFSET_MINUTES: i16 = -12 * 60;

/// Maximum accepted UTC offset in minutes.
pub const MAX_UTC_OFFSET_MINUTES: i16 = 14 * 60;

const SECONDS_PER_DAY: i64 = 86_400;

/// Time-related gateway configuration.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TimeSettings
{
    /// Local display offset from UTC in minutes.
    pub utc_offset_minutes: i16,
    /// Optional wall-clock seed supplied by USB provisioning.
    pub unix_time_seconds:  Option<u64>,
}

impl TimeSettings
{
    /// Return true when the time configuration is internally valid.
    pub const fn is_valid(self) -> bool
    {
        self.utc_offset_minutes >= MIN_UTC_OFFSET_MINUTES
            && self.utc_offset_minutes <= MAX_UTC_OFFSET_MINUTES
    }
}

impl Default for TimeSettings
{
    fn default() -> Self
    {
        Self {
            utc_offset_minutes: 0,
            unix_time_seconds:  None,
        }
    }
}

/// Optional hardware RTC boundary used by board-specific integrations.
pub trait RtcTimeSource
{
    /// RTC driver error type.
    type Error;

    /// Read Unix seconds from the RTC.
    fn read_unix_seconds(&mut self) -> Result<u64, Self::Error>;

    /// Write Unix seconds to the RTC.
    fn write_unix_seconds(&mut self, unix_seconds: u64) -> Result<(), Self::Error>;
}

/// Monotonic-clock-backed wall time state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GatewayTime
{
    utc_offset_minutes: i16,
    base_unix_seconds:  Option<u64>,
    base_monotonic_ms:  u64,
}

impl GatewayTime
{
    /// Create a wall clock from config and current monotonic milliseconds.
    pub const fn new(settings: TimeSettings, monotonic_ms: u64) -> Self
    {
        Self {
            utc_offset_minutes: settings.utc_offset_minutes,
            base_unix_seconds:  settings.unix_time_seconds,
            base_monotonic_ms:  monotonic_ms,
        }
    }

    /// Apply new config. A supplied Unix timestamp reseeds wall time; an
    /// omitted timestamp only updates the display offset.
    pub fn apply_settings(&mut self, settings: TimeSettings, monotonic_ms: u64)
    {
        self.utc_offset_minutes = settings.utc_offset_minutes;
        if let Some(unix_time_seconds) = settings.unix_time_seconds {
            self.set_unix_seconds(unix_time_seconds, monotonic_ms);
        }
    }

    /// Seed from a hardware RTC if present.
    pub fn sync_from_rtc<R>(&mut self, rtc: &mut R, monotonic_ms: u64) -> Result<(), R::Error>
    where
        R: RtcTimeSource,
    {
        let unix_seconds = rtc.read_unix_seconds()?;
        self.set_unix_seconds(unix_seconds, monotonic_ms);
        Ok(())
    }

    /// Persist the current wall time to a hardware RTC.
    pub fn write_to_rtc<R>(&self, rtc: &mut R, monotonic_ms: u64) -> Result<(), R::Error>
    where
        R: RtcTimeSource,
    {
        if let Some(unix_seconds) = self.unix_seconds(monotonic_ms) {
            rtc.write_unix_seconds(unix_seconds)?;
        }
        Ok(())
    }

    /// Set wall time using Unix seconds.
    pub fn set_unix_seconds(&mut self, unix_seconds: u64, monotonic_ms: u64)
    {
        self.base_unix_seconds = Some(unix_seconds);
        self.base_monotonic_ms = monotonic_ms;
    }

    /// Return configured UTC offset in minutes.
    pub const fn utc_offset_minutes(self) -> i16
    {
        self.utc_offset_minutes
    }

    /// Return current Unix seconds if wall time has been seeded.
    pub fn unix_seconds(self, monotonic_ms: u64) -> Option<u64>
    {
        let base = self.base_unix_seconds?;
        Some(base.saturating_add(monotonic_ms.saturating_sub(self.base_monotonic_ms) / 1_000))
    }

    /// Return current local date/time if wall time has been seeded.
    pub fn local_datetime(self, monotonic_ms: u64) -> Option<LocalDateTime>
    {
        local_datetime(self.unix_seconds(monotonic_ms)?, self.utc_offset_minutes)
    }
}

impl Default for GatewayTime
{
    fn default() -> Self
    {
        Self::new(TimeSettings::default(), 0)
    }
}

/// Calendar date/time after applying a fixed UTC offset.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LocalDateTime
{
    /// Gregorian year.
    pub year:   i32,
    /// Month number, 1..=12.
    pub month:  u8,
    /// Day of month, 1..=31.
    pub day:    u8,
    /// Hour, 0..=23.
    pub hour:   u8,
    /// Minute, 0..=59.
    pub minute: u8,
    /// Second, 0..=59.
    pub second: u8,
}

impl LocalDateTime
{
    /// Write `HH:MM` into the provided five-byte buffer.
    pub fn write_hh_mm(self, out: &mut [u8; 5])
    {
        write_two_digits(self.hour, &mut out[0..2]);
        out[2] = b':';
        write_two_digits(self.minute, &mut out[3..5]);
    }
}

/// Convert Unix seconds plus a fixed UTC offset into local date/time.
pub fn local_datetime(unix_seconds: u64, utc_offset_minutes: i16) -> Option<LocalDateTime>
{
    let utc_seconds = i64::try_from(unix_seconds).ok()?;
    let local_seconds = utc_seconds.checked_add(i64::from(utc_offset_minutes) * 60)?;
    if local_seconds < 0 {
        return None;
    }

    let days = local_seconds.div_euclid(SECONDS_PER_DAY);
    let seconds_of_day = local_seconds.rem_euclid(SECONDS_PER_DAY);
    let (year, month, day) = civil_from_days(days);
    Some(LocalDateTime {
        year,
        month,
        day,
        hour: (seconds_of_day / 3_600) as u8,
        minute: ((seconds_of_day % 3_600) / 60) as u8,
        second: (seconds_of_day % 60) as u8,
    })
}

fn write_two_digits(value: u8, out: &mut [u8])
{
    out[0] = b'0' + (value / 10);
    out[1] = b'0' + (value % 10);
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

#[cfg(test)]
mod tests
{
    use super::{CST_UTC_OFFSET_MINUTES, GatewayTime, TimeSettings, local_datetime};

    #[test]
    fn converts_unix_epoch_to_civil_time()
    {
        let dt = local_datetime(1_704_067_200, 0).unwrap();

        assert_eq!((dt.year, dt.month, dt.day), (2024, 1, 1));
        assert_eq!((dt.hour, dt.minute, dt.second), (0, 0, 0));
    }

    #[test]
    fn applies_cst_offset()
    {
        let dt = local_datetime(1_704_067_200, CST_UTC_OFFSET_MINUTES).unwrap();

        assert_eq!((dt.year, dt.month, dt.day), (2023, 12, 31));
        assert_eq!((dt.hour, dt.minute, dt.second), (18, 0, 0));
    }

    #[test]
    fn advances_from_monotonic_clock()
    {
        let clock = GatewayTime::new(
            TimeSettings {
                utc_offset_minutes: 0,
                unix_time_seconds:  Some(1_704_067_200),
            },
            10_000,
        );

        assert_eq!(clock.unix_seconds(15_500), Some(1_704_067_205));
    }

    #[test]
    fn formats_hh_mm()
    {
        let mut text = [0; 5];
        local_datetime(1_704_067_200, CST_UTC_OFFSET_MINUTES)
            .unwrap()
            .write_hh_mm(&mut text);

        assert_eq!(&text, b"18:00");
    }
}
