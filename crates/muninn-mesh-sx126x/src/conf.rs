//! Device initialization configuration.

use super::op::*;

/// Parameters consumed by [`crate::SX126x::init`].
pub struct Config
{
    /// Packet type to select before configuring packet-specific parameters.
    pub packet_type:         PacketType,
    /// Optional LoRa sync word written to the LoRa sync-word registers.
    pub sync_word:           Option<u16>,
    /// Calibration blocks to run during initialization.
    pub calib_param:         CalibParam,
    /// Modulation parameters for the selected packet type.
    pub mod_params:          ModParams,
    /// Power-amplifier configuration.
    pub pa_config:           PaConfig,
    /// Optional packet parameters to set during initialization.
    pub packet_params:       Option<PacketParams>,
    /// TX power and ramp parameters.
    pub tx_params:           TxParams,
    /// IRQ mask mapped to DIO1.
    pub dio1_irq_mask:       IrqMask,
    /// IRQ mask mapped to DIO2.
    pub dio2_irq_mask:       IrqMask,
    /// IRQ mask mapped to DIO3.
    pub dio3_irq_mask:       IrqMask,
    /// Enable the radio's DIO2 RF-switch control output during initialization.
    pub dio2_rf_switch_ctrl: bool,
    /// Raw PLL frequency word passed to `SetRfFrequency`.
    pub rf_freq:             u32,
    /// Requested RF frequency in Hz, retained for callers that need it.
    pub rf_frequency:        u32,
    /// Optional DIO3 TCXO control voltage and startup delay.
    pub tcxo_opts:           Option<(TcxoVoltage, TcxoDelay)>,
}
