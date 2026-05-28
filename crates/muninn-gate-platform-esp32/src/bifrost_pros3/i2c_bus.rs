//! Shared blocking I2C bus for the Bifrost ProS3 STEMMA peripherals.
//!
//! The MAX17048 fuel gauge and the SSD1327 grayscale OLED both sit on the
//! single `I2C0` bus exposed on the STEMMA QT connector. We build the bus
//! once at boot, park it in a `'static` `RefCell` via `StaticCell`, then
//! hand each consumer an `embedded_hal_bus::i2c::RefCellDevice` so the
//! single-threaded blocking driver can be borrowed transaction-by-transaction.
//!
//! Single-threaded only: `RefCellDevice` is unsuitable if multiple
//! interrupts/cores share the bus. The Bifrost bring-up runner is one
//! cooperative loop on a single core, so this is fine; if later phases add a
//! second consumer that runs from an ISR, swap to `CriticalSectionDevice`.

use core::cell::RefCell;

use embedded_hal_bus::i2c::RefCellDevice;
use esp_hal::Blocking;
use esp_hal::i2c::master::{Config as I2cConfig, I2c};
use esp_hal::peripherals::{GPIO8, GPIO9, I2C0};
use esp_hal::time::Rate;
use static_cell::StaticCell;

/// Bus frequency. 400 kHz is "Fast Mode" — both the MAX17048 and the SSD1327
/// are happy here, and it keeps an 8 KiB framebuffer flush short.
pub const I2C_FREQUENCY_KHZ: u32 = 400;

/// Aliased type for an I2C handle borrowed from the shared bus.
pub type SharedI2cDevice = RefCellDevice<'static, I2c<'static, Blocking>>;

/// Errors initializing the shared I2C bus.
#[derive(Debug, Clone, Copy)]
pub enum I2cBusError
{
    /// `esp-hal` rejected the bus config (clock/pin combo).
    Config,
}

/// Bring the bus up once and stash it in a static cell so callers can mint
/// as many [`SharedI2cDevice`]s as they need (e.g. one for the OLED, one for
/// the fuel gauge).
pub fn init(
    i2c: I2C0<'static>,
    sda: GPIO8<'static>,
    scl: GPIO9<'static>,
) -> Result<&'static RefCell<I2c<'static, Blocking>>, I2cBusError>
{
    static BUS: StaticCell<RefCell<I2c<'static, Blocking>>> = StaticCell::new();

    let bus = I2c::new(
        i2c,
        I2cConfig::default().with_frequency(Rate::from_khz(I2C_FREQUENCY_KHZ)),
    )
    .map_err(|_| I2cBusError::Config)?
    .with_sda(sda)
    .with_scl(scl);

    Ok(BUS.init(RefCell::new(bus)))
}

/// Mint a fresh borrowed handle on the shared bus. Cheap; safe to call many
/// times (each call returns a separate `RefCellDevice` that runtime-borrows
/// the bus for the duration of each transaction).
pub fn device(bus: &'static RefCell<I2c<'static, Blocking>>) -> SharedI2cDevice
{
    RefCellDevice::new(bus)
}
