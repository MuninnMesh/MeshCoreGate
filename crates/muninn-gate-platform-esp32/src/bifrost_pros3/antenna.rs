//! ProS3[D] 2.4 GHz antenna RF switch (GPIO 11).
//!
//! Hardware: a single RF SP2T switch routes the ESP32-S3 WiFi/BT RF pin to
//! either the on-board 3D PCB antenna or the u.FL connector. The control
//! line is GPIO 11: LOW = onboard, HIGH = external u.FL.
//!
//! Drive the line BEFORE bringing up the radio so the next path-config sees
//! the correct route. The factory default state is the onboard antenna, so
//! we initialize to that and let the firmware opt into external.

use esp_hal::gpio::{Level, Output, OutputConfig};
use esp_hal::peripherals::GPIO11;

/// Which 2.4 GHz antenna path the on-board RF switch is routing to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AntennaPath
{
    /// On-board 3D PCB antenna (safe default).
    Onboard3D,
    /// External antenna via the u.FL connector.
    ExternalUfl,
}

impl AntennaPath
{
    /// Human label suitable for boot logs / UI.
    pub const fn label(self) -> &'static str
    {
        match self {
            Self::Onboard3D => "onboard 3D",
            Self::ExternalUfl => "external u.FL",
        }
    }

    const fn level(self) -> Level
    {
        match self {
            Self::Onboard3D => Level::Low,
            Self::ExternalUfl => Level::High,
        }
    }
}

/// Owned handle to the GPIO 11 antenna RF switch line.
pub struct AntennaSwitch
{
    pin:     Output<'static>,
    current: AntennaPath,
}

impl AntennaSwitch
{
    /// Take ownership of the antenna control line. Defaults to the safe
    /// on-board antenna so the radio cannot be brought up into an unloaded
    /// external connector.
    pub fn new(pin: GPIO11<'static>) -> Self
    {
        let initial = AntennaPath::Onboard3D;
        Self {
            pin:     Output::new(pin, initial.level(), OutputConfig::default()),
            current: initial,
        }
    }

    /// Route the RF switch to the requested antenna path. Idempotent.
    pub fn select(&mut self, path: AntennaPath)
    {
        self.pin.set_level(path.level());
        self.current = path;
    }

    /// Currently selected antenna path.
    pub fn path(&self) -> AntennaPath
    {
        self.current
    }
}
