#![no_std]
#![no_main]
#![warn(missing_docs)]
//! Thin Muninn Gate firmware entrypoint selected by board Cargo features.

#[cfg(not(feature = "heltec-v4"))]
compile_error!("select a Muninn Gate board feature, for example `heltec-v4`");

#[cfg(feature = "heltec-v4")]
use esp_backtrace as _;
#[cfg(feature = "heltec-v4")]
use esp_hal::main;

#[cfg(feature = "heltec-v4")]
esp_bootloader_esp_idf::esp_app_desc!();

/// Firmware entrypoint selected by Cargo features.
#[cfg(feature = "heltec-v4")]
#[main]
fn main() -> !
{
    muninn_gate_board_heltec_v4::WifiLora32V4x::run()
}
