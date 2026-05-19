//! Generic `no_std` driver primitives for Semtech SX126x radios.
//!
//! The crate exposes register addresses, command parameter encoders, and a
//! blocking SPI device wrapper. Protocol defaults such as network frequency,
//! sync word, packet layout, and board policy belong in higher-level crates.

#![no_std]
#![deny(missing_docs)]
#![deny(clippy::missing_docs_in_private_items)]

/// Device configuration types used by the optional high-level initializer.
pub mod conf;
/// SX126x command parameter and response types.
pub mod op;
/// SX126x register addresses.
pub mod reg;

mod sx;
pub use sx::*;
