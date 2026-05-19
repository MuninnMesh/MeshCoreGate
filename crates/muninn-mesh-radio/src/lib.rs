//! Platform-independent MeshCore radio wrapper.
//!
//! This crate owns SX126x/MeshCore RX/TX orchestration, packet metadata
//! extraction, and radio health counters. Board crates provide concrete SPI,
//! GPIO, timing, and front-end-module switching policy.

#![no_std]
#![deny(missing_docs)]
#![deny(clippy::missing_docs_in_private_items)]

/// Board-level MeshCore radio integration.
pub mod radio;

pub use radio::*;
