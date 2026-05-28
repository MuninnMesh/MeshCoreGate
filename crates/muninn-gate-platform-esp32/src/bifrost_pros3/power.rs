//! ProS3 secondary power rail (LDO2 enable on GPIO 17).
//!
//! Hardware: LDO2 is the on-board 3V3 regulator that powers the WS2812 RGB
//! LED, the STEMMA QT connector (and therefore the MAX17048 fuel gauge on
//! the shared I2C bus), and the optional u.FL/STEMMA accessory rail. LDO1
//! always powers the SoC itself.
//!
//! Drive GPIO 17 HIGH to bring LDO2 up. The rail needs a short settle time
//! (~50 ms) before downstream I2C devices are reliably addressable.

use esp_hal::gpio::{Level, Output, OutputConfig};
use esp_hal::peripherals::GPIO17;

/// Owned handle to the LDO2 enable line. Dropping this disables the rail.
pub struct Ldo2Rail
{
    _pin: Output<'static>,
}

impl Ldo2Rail
{
    /// Drive LDO2 enable HIGH so the STEMMA bus and RGB LED receive 3V3.
    pub fn enabled(pin: GPIO17<'static>) -> Self
    {
        Self {
            _pin: Output::new(pin, Level::High, OutputConfig::default()),
        }
    }
}
