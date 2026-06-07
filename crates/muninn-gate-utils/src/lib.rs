#![no_std]
#![warn(missing_docs)]
//! Cross-board utilities used by Muninn Gate board crates.
//!
//! These are not device drivers (those live under `drivers/`) and not
//! platform services (those live in `muninn-gate-platform-esp32`). The
//! split exists because some helpers are tiny enough that pulling in
//! the platform crate just to use them would be overkill — typically
//! `'static`/`RefCell`/`StaticCell` glue that every board needs once.

pub mod heap;
pub mod i2c_bus;
