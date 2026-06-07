//! Shared single-threaded blocking I²C bus glue.
//!
//! Build an `I2c<'static, Blocking>` somewhere, park it in a [`SharedI2cBus`]
//! via [`install_bus`], then mint per-consumer [`SharedI2cDevice`]s with
//! [`device`]. Each consumer driver gets a transaction-by-transaction
//! borrow on the same underlying bus — perfect for a cooperative
//! single-core loop sharing an OLED + fuel gauge on the same wire.
//!
//! Single-threaded only: [`embedded_hal_bus::i2c::RefCellDevice`] is
//! NOT safe if a second consumer can preempt mid-transaction (e.g. an
//! ISR). For that case, switch to `CriticalSectionDevice` from the same
//! crate — same API, different sync primitive.

use core::cell::RefCell;

use embedded_hal_bus::i2c::RefCellDevice;
use esp_hal::Blocking;
use esp_hal::i2c::master::I2c;
use static_cell::StaticCell;

/// Park the bus here in a `'static` slot so consumers can hold borrowed
/// device handles for the program's lifetime.
pub type SharedI2cBus = RefCell<I2c<'static, Blocking>>;

/// Per-consumer handle on the shared bus. Each consumer driver holds
/// one of these; the underlying bus is borrowed for the duration of
/// each transaction and released on drop.
pub type SharedI2cDevice = RefCellDevice<'static, I2c<'static, Blocking>>;

/// Install a constructed [`I2c`] into a `'static` cell and return a
/// reference suitable for [`device`]. Typical use:
///
/// ```ignore
/// static BUS: StaticCell<SharedI2cBus> = StaticCell::new();
/// let bus = I2c::new(peripherals.I2C0, cfg)?.with_sda(sda).with_scl(scl);
/// let bus_ref = i2c_bus::install_bus(&BUS, bus);
/// let display = SomeDisplay::new(i2c_bus::device(bus_ref));
/// let gauge = SomeGauge::new(i2c_bus::device(bus_ref));
/// ```
pub fn install_bus(
    cell: &'static StaticCell<SharedI2cBus>,
    bus: I2c<'static, Blocking>,
) -> &'static SharedI2cBus
{
    cell.init(RefCell::new(bus))
}

/// Mint a fresh consumer handle on the shared bus. Cheap; safe to call
/// many times (each call returns a separate [`RefCellDevice`] that
/// runtime-borrows the bus for the duration of each transaction).
pub fn device(bus: &'static SharedI2cBus) -> SharedI2cDevice
{
    RefCellDevice::new(bus)
}
