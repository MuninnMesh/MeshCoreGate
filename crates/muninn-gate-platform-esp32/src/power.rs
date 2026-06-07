//! Board-agnostic power-monitoring surface.
//!
//! Boards implement [`PowerMonitor`] to expose battery / USB state via a
//! single shape the platform's runtime, UI, and telemetry code can call
//! without knowing whether the underlying part is a MAX17048 fuel gauge,
//! an INA226, or a divider+ADC. The trait deliberately exposes only the
//! information consumers actually need:
//!
//! - SOC % (so the UI can pick a battery icon)
//! - cell voltage in mV (for serial heartbeat + diagnostics)
//! - USB 5 V presence (so the UI can render "charging" vs "running on battery")
//!
//! Boards typically wrap a chip driver + a VBUS-sense GPIO + a sample
//! cache; the platform code re-asks at its own cadence and never has to
//! know what's underneath.

/// Board-agnostic power monitor. Returns `None` for any quantity the
/// board does not / cannot measure. The board is responsible for any
/// caching; trait calls are cheap reads.
pub trait PowerMonitor
{
    /// State of charge as a whole percent in `0..=100`. `None` means
    /// either no fuel gauge is wired or the part hasn't settled yet
    /// (e.g. MAX17048 quick-start hasn't completed).
    fn soc_percent(&self) -> Option<u8>;

    /// Cell voltage in millivolts. `None` means no measurement is
    /// available (gauge missing, ADC unconfigured, etc.).
    fn voltage_mv(&self) -> Option<u16>;

    /// `true` when USB 5 V is currently being delivered to the board
    /// (i.e. the on-board charger is active and the host is plugged
    /// in). Boards without a VBUS-sense GPIO can always return
    /// `false`.
    fn usb_connected(&self) -> bool;
}
