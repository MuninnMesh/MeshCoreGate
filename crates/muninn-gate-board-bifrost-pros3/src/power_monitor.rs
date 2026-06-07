//! Bifrost ProS3 `PowerMonitor` impl backed by MAX17048 + VBUS sense.
//!
//! The platform-level `PowerMonitor` trait wants three quantities (SOC %,
//! voltage mV, USB connected). Bifrost backs them with:
//!
//! - **SOC % + voltage mV** — `muninn-driver-max17048` on the shared STEMMA I²C bus (`Max17048`
//!   cached sample, refreshed once per outer tick by `Self::refresh_from_services`).
//! - **USB connected** — GPIO 33 VBUS sense (sampled on each `refresh`).
//!
//! The struct is a snapshot rather than a borrow on `BoardServices` so
//! it composes cleanly with code paths that already hold a `&mut
//! BoardServices` (the heartbeat log, the UI render). Refresh-on-tick is
//! enough granularity for both — the gauge updates internally at ~1 Hz
//! anyway and the OLED frame interval is similar.

use muninn_driver_max17048::BatterySample;
use muninn_gate_platform_esp32::power::PowerMonitor;

use crate::board::BoardServices;

/// Snapshot of Bifrost's power-monitoring inputs. Construct via
/// [`Self::refresh_from_services`] each tick and pass by `&` to
/// consumers.
#[derive(Debug, Clone, Copy, Default)]
pub struct BifrostPowerSnapshot
{
    sample:        Option<BatterySample>,
    usb_connected: bool,
}

impl BifrostPowerSnapshot
{
    /// Sample the fuel gauge + VBUS pin, returning the result without
    /// mutating the snapshot in-place. The caller stashes the returned
    /// value (typically on `UiState` or a local) and feeds it to anyone
    /// who wants a `&dyn PowerMonitor`.
    pub fn refresh_from_services(services: &mut BoardServices) -> Self
    {
        let sample = services
            .battery
            .as_mut()
            .and_then(|gauge| gauge.read().ok());
        Self {
            sample,
            usb_connected: services.usb_connected(),
        }
    }

    /// The cached fuel-gauge sample, if the gauge was reachable this
    /// tick. Exposed so UI code that wants both the typed struct (for
    /// `voltage_mv` etc.) and the trait surface can avoid double work.
    pub fn battery_sample(&self) -> Option<BatterySample>
    {
        self.sample
    }
}

impl PowerMonitor for BifrostPowerSnapshot
{
    fn soc_percent(&self) -> Option<u8>
    {
        self.sample.map(|s| s.soc_percent)
    }

    fn voltage_mv(&self) -> Option<u16>
    {
        // `BatterySample::voltage_mv` is a `u32` to leave headroom for
        // any future double-cell variant, but PowerMonitor exposes it
        // as `u16` (LiPo cells top out around 4.2 V — comfortably
        // inside `u16`).
        self.sample
            .map(|s| s.voltage_mv.min(u16::MAX as u32) as u16)
    }

    fn usb_connected(&self) -> bool
    {
        self.usb_connected
    }
}
