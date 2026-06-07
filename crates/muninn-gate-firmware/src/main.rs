#![no_std]
#![no_main]
#![warn(missing_docs)]
//! Thin Muninn Gate firmware entrypoint selected by board Cargo features.

#[cfg(all(feature = "heltec-v4", feature = "bifrost-pros3"))]
compile_error!("select exactly one Muninn Gate board feature");

#[cfg(not(any(feature = "heltec-v4", feature = "bifrost-pros3")))]
compile_error!("select a Muninn Gate board feature, for example `heltec-v4` or `bifrost-pros3`");

#[cfg(any(feature = "heltec-v4", feature = "bifrost-pros3"))]
use esp_backtrace as _;
#[cfg(any(feature = "heltec-v4", feature = "bifrost-pros3"))]
use esp_hal::main;

#[cfg(any(feature = "heltec-v4", feature = "bifrost-pros3"))]
mod app_desc;

/// Firmware entrypoint selected by Cargo features.
#[cfg(feature = "heltec-v4")]
#[main]
fn main() -> !
{
    muninn_gate_board_heltec_v4::WifiLora32V4x::run()
}

/// Firmware entrypoint for the Bifrost ProS3 board variant.
#[cfg(feature = "bifrost-pros3")]
#[main]
fn main() -> !
{
    muninn_gate_board_bifrost_pros3::BifrostProS3Gate::run()
}
