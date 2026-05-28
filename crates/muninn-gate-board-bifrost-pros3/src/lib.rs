#![no_std]
#![warn(missing_docs)]
//! Bifrost Gate firmware variant: Unexpected Maker ProS3[D] + Adafruit
//! SSD1327 128×128 4-bit grayscale I2C OLED + future EBYTE E22P-915M30S
//! (SX1262, 30 dBm). See `docs/boards/bifrost_pros3.md` for the full
//! hardware + boot story.
//!
//! Display history: the original plan was an Adafruit SSD1351 128×96 RGB
//! SPI panel; while we wait on the EYESPI cable, the SSD1327 grayscale
//! module took its place. If the SPI path is revived, this variant can
//! either swap back or split into two variants — the existing
//! `DisplayVariant::Oled128x96Color` enum value stays in core for that.

use muninn_gate_core::{DisplayVariant, GateFirmwareVariant, GatewayCapabilities, TxPowerMapping};

/// Bifrost Gate ProS3 / ProS3[D] firmware variant.
pub struct BifrostProS3Gate;

impl BifrostProS3Gate
{
    /// Run the Bifrost ProS3 firmware forever.
    pub fn run() -> !
    {
        muninn_gate_platform_esp32::run_bifrost_pros3::<Self>()
    }
}

impl GateFirmwareVariant for BifrostProS3Gate
{
    const BOARD: &'static str = "Bifrost Gate ProS3 + SSD1327 + E22P-915M30S";
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
        TxPowerMapping::passthrough(requested_level)
    }
}
