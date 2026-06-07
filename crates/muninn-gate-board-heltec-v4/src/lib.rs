#![no_std]
#![warn(missing_docs)]
//! WiFi LoRa 32 V4.x, ESP32S3 + SX1262 LoRa Node variant for Muninn Gate.

pub mod tx_power;

use muninn_gate_core::{DisplayVariant, GateFirmwareVariant, GatewayCapabilities, TxPowerMapping};
use muninn_gate_platform_esp32::{Esp32BoardVariant, Esp32RadioHardware, TcxoVoltage};
use muninn_gate_ui::DisplaySize;

/// WiFi LoRa 32 V4.x, ESP32S3 + SX1262 LoRa Node firmware variant.
pub struct WifiLora32V4x;

impl WifiLora32V4x
{
    /// Built-in text display layout used by this board.
    pub const DISPLAY_SIZE: DisplaySize = DisplaySize::Oled128x64;

    /// Run the WiFi LoRa 32 V4.x firmware forever.
    pub fn run() -> !
    {
        muninn_gate_platform_esp32::run_gateway::<Self>()
    }
}

impl GateFirmwareVariant for WifiLora32V4x
{
    const BOARD: &'static str = "WiFi LoRa 32 V4.x, ESP32S3 + SX1262 LoRa Node";
    const CAPABILITIES: GatewayCapabilities = GatewayCapabilities {
        wifi:        true,
        http_server: true,
        usb_serial:  true,
        display:     DisplayVariant::Oled128x64,
    };
    const NAME: &'static str = "heltec-v4";
    const PLATFORM: &'static str = "esp32s3";

    fn map_tx_power(requested_level: i8) -> TxPowerMapping
    {
        let selected_level = tx_power::clamp_to_driver_tx_power_level(requested_level);
        let output = tx_power::conducted_output_at_or_below(selected_level);

        TxPowerMapping {
            requested_level,
            selected_level,
            output_dbm_tenths: output.map(|point| point.output_dbm_tenths),
            output_milliwatts: output.map(|point| point.output_milliwatts),
        }
    }
}

impl Esp32BoardVariant for WifiLora32V4x
{
    const RADIO_HARDWARE: Esp32RadioHardware = Esp32RadioHardware {
        tcxo_enabled:  true,
        tcxo_voltage:  TcxoVoltage::Volt1_8,
        tcxo_delay_ms: 20,
    };
}
