//! SX126x errata and register workarounds.
//!
//! The radio owner applies these helpers during init, before RX, and around TX
//! parameter changes. They are generic over the concrete SPI and GPIO types so
//! board crates stay responsible for all platform wiring.

use embedded_hal::digital::{InputPin, OutputPin};
use embedded_hal::spi::SpiDevice;
use muninn_mesh_sx126x::SX126x;
use muninn_mesh_sx126x::reg::Register;

use super::types::MeshRadioError;

/// SX1262 errata 15.4 IQ polarity setup address.
const REG_IQ_POLARITY_SETUP: u16 = 0x0736;
/// SX1262 bit value for standard IQ polarity.
const IQ_POLARITY_STANDARD_BIT: u8 = 0x04;
/// SX1262 errata TX clamp configuration bitmask.
const TX_CLAMP_CONFIG_MASK: u8 = 0x1E;
/// SX1262 errata 15.1 bit required when bandwidth is not 500 kHz.
const TX_MODULATION_BW_NOT_500KHZ_BIT: u8 = 0x04;
/// SX1262 boosted RX gain register value.
const RX_BOOSTED_GAIN: u8 = 0x96;
/// SX126x retention register address for keeping boosted RX gain.
const REG_RX_GAIN_RETENTION: u16 = 0x029F;
/// Retention bytes that preserve boosted RX gain across standby/RX changes.
const RX_GAIN_RETENTION_BYTES: [u8; 3] = [0x08, 0x89, 0x0A];
/// SX1262 OCP value for the +22 dBm PA path, approximately 140 mA.
const SX1262_OCP_140MA: u8 = 0x38;
/// SX1262 RX sensitivity patch bit used by RadioLib-compatible setups.
const RX_SENSITIVITY_PATCH_BIT: u8 = 0x01;

#[cfg(feature = "stm32wl-lora-agc-jam-recovery")]
/// Undocumented STM32WL LoRa AGC register used by ST's public workaround.
const REG_STM32WL_LORA_AGC_CFG_UNDOCUMENTED: u16 = 0x08A3;

#[cfg(feature = "stm32wl-lora-agc-jam-recovery")]
/// ST AGC workaround mask that keeps the step threshold at one.
const STM32WL_AGC_STEP_THRESHOLD_MASK: u8 = 0x01;

/// SX126x workaround methods used by the radio owner.
pub trait Sx126xWorkarounds
{
    /// Apply all one-time SX126x errata and register workarounds after init.
    fn apply_init_workarounds(&mut self, iq_inverted: bool) -> Result<(), MeshRadioError>;

    /// Apply SX1262 errata 15.4 IQ polarity setup after packet parameter changes.
    fn apply_iq_polarity(&mut self, iq_inverted: bool) -> Result<(), MeshRadioError>;

    /// Apply the SX1262 TX clamp workaround for the high-power PA path.
    fn apply_tx_clamp_config(&mut self) -> Result<(), MeshRadioError>;

    /// Apply SX1262 errata 15.1 for LoRa bandwidths other than 500 kHz.
    fn apply_tx_modulation_patch(&mut self) -> Result<(), MeshRadioError>;

    /// Apply boosted RX gain and retention registers.
    fn apply_rx_boosted_gain(&mut self) -> Result<(), MeshRadioError>;

    /// Apply SX1262 over-current protection for the high-power PA path.
    fn apply_ocp_configuration(&mut self) -> Result<(), MeshRadioError>;

    /// Apply the RX sensitivity register patch.
    fn apply_rx_sensitivity_patch(&mut self) -> Result<(), MeshRadioError>;

    /// Apply the STM32WL LoRa AGC jam-recovery workaround when enabled.
    fn apply_stm32wl_lora_agc_jam_recovery(&mut self) -> Result<(), MeshRadioError>;
}

impl<TSPI, TNRST, TBUSY, TANT, TDIO1, TSPIERR, TPINERR> Sx126xWorkarounds
    for SX126x<TSPI, TNRST, TBUSY, TANT, TDIO1>
where
    TPINERR: core::fmt::Debug,
    TSPI: SpiDevice<Error = TSPIERR>,
    TNRST: OutputPin<Error = TPINERR>,
    TBUSY: InputPin<Error = TPINERR>,
    TANT: OutputPin<Error = TPINERR>,
    TDIO1: InputPin<Error = TPINERR>,
{
    fn apply_init_workarounds(&mut self, iq_inverted: bool) -> Result<(), MeshRadioError>
    {
        self.apply_iq_polarity(iq_inverted)?;
        self.apply_tx_clamp_config()?;
        self.apply_tx_modulation_patch()?;
        self.apply_rx_boosted_gain()?;
        self.apply_ocp_configuration()?;
        self.apply_rx_sensitivity_patch()?;
        Ok(())
    }

    fn apply_iq_polarity(&mut self, iq_inverted: bool) -> Result<(), MeshRadioError>
    {
        self.wait_on_busy().map_err(|_| MeshRadioError::DeviceIo)?;
        let mut val = [0u8; 1];
        self.read_register(REG_IQ_POLARITY_SETUP, &mut val)
            .map_err(|_| MeshRadioError::DeviceIo)?;
        if iq_inverted {
            val[0] &= !IQ_POLARITY_STANDARD_BIT;
        } else {
            val[0] |= IQ_POLARITY_STANDARD_BIT;
        }
        self.wait_on_busy().map_err(|_| MeshRadioError::DeviceIo)?;
        self.write_register(Register::IqPolaritySetup, &[val[0]])
            .map_err(|_| MeshRadioError::DeviceIo)?;
        Ok(())
    }

    fn apply_tx_clamp_config(&mut self) -> Result<(), MeshRadioError>
    {
        self.wait_on_busy().map_err(|_| MeshRadioError::DeviceIo)?;
        let mut val = [0u8; 1];
        self.read_register(Register::TxClampConfig as u16, &mut val)
            .map_err(|_| MeshRadioError::DeviceIo)?;
        self.wait_on_busy().map_err(|_| MeshRadioError::DeviceIo)?;
        self.write_register(Register::TxClampConfig, &[val[0] | TX_CLAMP_CONFIG_MASK])
            .map_err(|_| MeshRadioError::DeviceIo)?;
        Ok(())
    }

    fn apply_tx_modulation_patch(&mut self) -> Result<(), MeshRadioError>
    {
        self.wait_on_busy().map_err(|_| MeshRadioError::DeviceIo)?;
        let mut val = [0u8; 1];
        self.read_register(Register::TxModulation as u16, &mut val)
            .map_err(|_| MeshRadioError::DeviceIo)?;
        self.wait_on_busy().map_err(|_| MeshRadioError::DeviceIo)?;
        self.write_register(
            Register::TxModulation,
            &[val[0] | TX_MODULATION_BW_NOT_500KHZ_BIT],
        )
        .map_err(|_| MeshRadioError::DeviceIo)?;
        Ok(())
    }

    fn apply_rx_boosted_gain(&mut self) -> Result<(), MeshRadioError>
    {
        self.wait_on_busy().map_err(|_| MeshRadioError::DeviceIo)?;
        self.write_register(Register::RxGain, &[RX_BOOSTED_GAIN])
            .map_err(|_| MeshRadioError::DeviceIo)?;
        self.wait_on_busy().map_err(|_| MeshRadioError::DeviceIo)?;
        self.write_register_raw(REG_RX_GAIN_RETENTION, &RX_GAIN_RETENTION_BYTES)
            .map_err(|_| MeshRadioError::DeviceIo)?;
        Ok(())
    }

    fn apply_ocp_configuration(&mut self) -> Result<(), MeshRadioError>
    {
        self.wait_on_busy().map_err(|_| MeshRadioError::DeviceIo)?;
        self.set_ocp_configuration(SX1262_OCP_140MA)
            .map_err(|_| MeshRadioError::DeviceIo)?;
        Ok(())
    }

    fn apply_rx_sensitivity_patch(&mut self) -> Result<(), MeshRadioError>
    {
        self.wait_on_busy().map_err(|_| MeshRadioError::DeviceIo)?;
        let mut val = [0u8; 1];
        self.read_register(Register::RxSensitivityPatch as u16, &mut val)
            .map_err(|_| MeshRadioError::DeviceIo)?;
        let patched = val[0] | RX_SENSITIVITY_PATCH_BIT;
        self.wait_on_busy().map_err(|_| MeshRadioError::DeviceIo)?;
        self.write_register(Register::RxSensitivityPatch, &[patched])
            .map_err(|_| MeshRadioError::DeviceIo)?;
        log::debug!(
            "LORA RX sensitivity patch: {:#04x} -> {:#04x}",
            val[0],
            patched
        );
        Ok(())
    }

    #[cfg(feature = "stm32wl-lora-agc-jam-recovery")]
    fn apply_stm32wl_lora_agc_jam_recovery(&mut self) -> Result<(), MeshRadioError>
    {
        let mut val = [0u8; 1];
        self.read_register(REG_STM32WL_LORA_AGC_CFG_UNDOCUMENTED, &mut val)
            .map_err(|_| MeshRadioError::DeviceIo)?;
        let patched = val[0] & STM32WL_AGC_STEP_THRESHOLD_MASK;
        self.write_register_raw(REG_STM32WL_LORA_AGC_CFG_UNDOCUMENTED, &[patched])
            .map_err(|_| MeshRadioError::DeviceIo)?;
        log::debug!(
            "LORA ST AGC jam workaround: reg 0x08A3 {:#04x} -> {:#04x}",
            val[0],
            patched
        );
        Ok(())
    }

    #[cfg(not(feature = "stm32wl-lora-agc-jam-recovery"))]
    fn apply_stm32wl_lora_agc_jam_recovery(&mut self) -> Result<(), MeshRadioError>
    {
        Ok(())
    }
}
