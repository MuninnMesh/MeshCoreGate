//! WiFi LoRa 32 V4.x, ESP32S3 + SX1262 LoRa Node TX power level map.

/// Practical maximum TX power level for Heltec V4 efficiency.
pub const HELTEC_V4_MAX_EFFICIENT_TX_POWER_LEVEL: i8 = 20;

/// Maximum TX power level accepted by the current SX1262 radio path.
pub const SX1262_MAX_TX_POWER_LEVEL: i8 = 22;

/// One Heltec V4 TX power level and output estimate.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct HeltecV4TxPowerLevel
{
    /// Firmware TX power level selected for the radio path.
    pub level:             i8,
    /// Approximate conducted output power in tenths of dBm.
    pub output_dbm_tenths: i16,
    /// Approximate conducted output power in milliwatts.
    pub output_milliwatts: u16,
}

/// Heltec V4 TX power level to conducted-output table.
///
/// The board uses an SX1262 plus external FEM, so the configured TX level is
/// not equal to conducted output power. Values are approximate bench
/// measurements and should not be treated as regulatory certification data.
pub const HELTEC_V4_TX_POWER_LEVELS: &[HeltecV4TxPowerLevel] = &[
    HeltecV4TxPowerLevel {
        level:             1,
        output_dbm_tenths: 70,
        output_milliwatts: 5,
    },
    HeltecV4TxPowerLevel {
        level:             5,
        output_dbm_tenths: 122,
        output_milliwatts: 17,
    },
    HeltecV4TxPowerLevel {
        level:             10,
        output_dbm_tenths: 203,
        output_milliwatts: 107,
    },
    HeltecV4TxPowerLevel {
        level:             12,
        output_dbm_tenths: 225,
        output_milliwatts: 179,
    },
    HeltecV4TxPowerLevel {
        level:             14,
        output_dbm_tenths: 243,
        output_milliwatts: 268,
    },
    HeltecV4TxPowerLevel {
        level:             16,
        output_dbm_tenths: 254,
        output_milliwatts: 349,
    },
    HeltecV4TxPowerLevel {
        level:             18,
        output_dbm_tenths: 272,
        output_milliwatts: 520,
    },
    HeltecV4TxPowerLevel {
        level:             20,
        output_dbm_tenths: 277,
        output_milliwatts: 593,
    },
    HeltecV4TxPowerLevel {
        level:             22,
        output_dbm_tenths: 272,
        output_milliwatts: 520,
    },
];

/// Clamp a requested TX power level to the efficient WiFi LoRa 32 V4.x range.
pub const fn clamp_to_efficient_tx_power_level(requested_level: i8) -> i8
{
    if requested_level < 1 {
        1
    } else if requested_level > HELTEC_V4_MAX_EFFICIENT_TX_POWER_LEVEL {
        HELTEC_V4_MAX_EFFICIENT_TX_POWER_LEVEL
    } else {
        requested_level
    }
}

/// Clamp a requested TX power level to the radio driver's accepted range.
pub const fn clamp_to_driver_tx_power_level(requested_level: i8) -> i8
{
    if requested_level < 1 {
        1
    } else if requested_level > SX1262_MAX_TX_POWER_LEVEL {
        SX1262_MAX_TX_POWER_LEVEL
    } else {
        requested_level
    }
}

/// Return the nearest conducted-output estimate at or below a TX power level.
pub fn conducted_output_at_or_below(level: i8) -> Option<HeltecV4TxPowerLevel>
{
    let mut out = None;
    for point in HELTEC_V4_TX_POWER_LEVELS {
        if point.level <= level {
            out = Some(*point);
        }
    }
    out
}
