#![no_std]
#![warn(missing_docs)]
//! Bifrost Gate firmware variant: Unexpected Maker ProS3[D] + Adafruit
//! SSD1327 128×128 4-bit grayscale I2C OLED + Seeed Wio-SX1262 (SX1262,
//! ~22 dBm). See `docs/boards/bifrost_pros3.md` for the full hardware +
//! boot story.
//!
//! This crate owns the **board-specific** pieces — pin map, peripheral
//! composition, UI layout, runtime orchestration. Chip-level services
//! (esp-wifi setup, smoltcp, HTTP server, common drivers) live in the
//! `muninn-gate-platform-esp32` crate and are consumed from here.

extern crate alloc;

mod antenna;
mod board;
mod boot_logo;
mod lora;
mod power;
mod power_monitor;
mod runtime;
mod status_led;
mod ui;
mod wifi;

use muninn_gate_core::{DisplayVariant, GateFirmwareVariant, GatewayCapabilities, TxPowerMapping};
use muninn_gate_platform_esp32::{Esp32BoardVariant, Esp32RadioHardware, TcxoVoltage};

/// Bifrost Gate ProS3 / ProS3[D] firmware variant.
pub struct BifrostProS3Gate;

impl BifrostProS3Gate
{
    /// Run the Bifrost ProS3 firmware forever.
    pub fn run() -> !
    {
        runtime::run::<Self>()
    }
}

impl GateFirmwareVariant for BifrostProS3Gate
{
    const BOARD: &'static str = "Bifrost Gate ProS3 + SSD1327 + Wio-SX1262";
    const CAPABILITIES: GatewayCapabilities = GatewayCapabilities {
        wifi:        true,
        http_server: true,
        usb_serial:  true,
        display:     DisplayVariant::Oled128x128Grayscale,
    };
    const NAME: &'static str = "bifrost-pros3";
    const PLATFORM: &'static str = "esp32s3";

    fn map_tx_power(requested_level: i8) -> TxPowerMapping
    {
        const BIFROST_WAVESHARE_SAFE_TX_POWER_CAP_DBM: i8 = 14;
        let selected_level = if requested_level > BIFROST_WAVESHARE_SAFE_TX_POWER_CAP_DBM {
            BIFROST_WAVESHARE_SAFE_TX_POWER_CAP_DBM
        } else {
            requested_level
        };
        TxPowerMapping {
            requested_level,
            selected_level,
            output_dbm_tenths: Some(i16::from(selected_level) * 10),
            output_milliwatts: None,
        }
    }
}

impl Esp32BoardVariant for BifrostProS3Gate
{
    /// The Wio-SX1262 module wires an on-module 32 MHz TCXO to the
    /// SX1262's XTAL_A pin. The chip MUST be told it's TCXO-driven via
    /// `SetDIO3AsTcxoCtrl` so it skips the crystal start-up oscillator
    /// (which would corrupt the TCXO's clock waveform). Disabling
    /// TCXO control leads to TX failures because the chip can't lock
    /// XOSC. 100 ms delay is generous — Semtech "typical" is 5 ms but
    /// the Bifrost rail takes longer to settle.
    const RADIO_HARDWARE: Esp32RadioHardware = Esp32RadioHardware {
        tcxo_enabled:  true,
        tcxo_voltage:  TcxoVoltage::Volt1_8,
        tcxo_delay_ms: 100,
    };
}
