//! Board variant metadata used by the firmware entrypoint and platform layer.

/// Capability set advertised by a concrete firmware variant.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GatewayCapabilities
{
    /// Whether the board can connect to WiFi.
    pub wifi:        bool,
    /// Whether the board can serve HTTP.
    pub http_server: bool,
    /// Whether the board can expose USB or UART serial.
    pub usb_serial:  bool,
    /// Display hardware available on the board.
    pub display:     DisplayVariant,
}

/// Display hardware class used by a board variant.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DisplayVariant
{
    /// No local display.
    None,
    /// Monochrome 128x64 OLED display.
    Oled128x64,
    /// Monochrome 128x128 OLED display.
    Oled128x128,
    /// Board-specific display not represented by a built-in variant.
    Custom(&'static str),
}

/// Result of mapping a configured TX power level to board-specific output.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TxPowerMapping
{
    /// User or configuration requested TX power level.
    pub requested_level:   i8,
    /// Board-selected radio driver TX power level.
    pub selected_level:    i8,
    /// Approximate conducted output power in tenths of dBm, when known.
    pub output_dbm_tenths: Option<i16>,
    /// Approximate conducted output power in milliwatts, when known.
    pub output_milliwatts: Option<u16>,
}

impl TxPowerMapping
{
    /// Create a direct mapping for boards without a known non-linear PA path.
    pub const fn passthrough(requested_level: i8) -> Self
    {
        Self {
            requested_level,
            selected_level: requested_level,
            output_dbm_tenths: None,
            output_milliwatts: None,
        }
    }
}
