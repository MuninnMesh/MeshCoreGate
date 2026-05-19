//! SX126x configuration builders for MeshCore LoRa operation.
//!
//! This module translates `MeshRadioConfig` into the lower-level SX126x
//! configuration object consumed by the driver crate.

use muninn_mesh_sx126x::conf::Config as SxConfig;
use muninn_mesh_sx126x::op::calib::{CalibImageFreq, CalibParam};
use muninn_mesh_sx126x::op::irq::{IrqMask, IrqMaskBit};
use muninn_mesh_sx126x::op::modulation::{LoRaBandWidth, LoRaSpreadFactor, LoraModParams};
use muninn_mesh_sx126x::op::packet::{
    LoRaCrcType,
    LoRaHeaderType,
    LoRaInvertIq,
    LoRaPacketParams,
    PacketType,
};
use muninn_mesh_sx126x::op::rxtx::{DeviceSel, PaConfig, TxParams};
use muninn_mesh_sx126x::op::tcxo::{TcxoDelay, TcxoVoltage};

use super::host::with_radio_host;
use super::types::{MAX_LORA_PAYLOAD_LEN, MeshRadioConfig};

/// Return whether low data-rate optimization is needed for this LoRa mode.
fn needs_ldro(sf: LoRaSpreadFactor, bw: LoRaBandWidth) -> bool
{
    let bw_hz: u32 = match bw {
        LoRaBandWidth::BW7 => 7_810,
        LoRaBandWidth::BW10 => 10_420,
        LoRaBandWidth::BW15 => 15_630,
        LoRaBandWidth::BW20 => 20_830,
        LoRaBandWidth::BW31 => 31_250,
        LoRaBandWidth::BW41 => 41_670,
        LoRaBandWidth::BW62 => 62_500,
        LoRaBandWidth::BW125 => 125_000,
        LoRaBandWidth::BW250 => 250_000,
        LoRaBandWidth::BW500 => 500_000,
    };
    let symbol_dur_us = ((1u32 << (sf as u8)) * 1_000_000) / bw_hz;
    symbol_dur_us > 16_380
}

/// Select the SX126x image-calibration range for a center frequency.
pub fn calib_image_freq(freq_hz: u32) -> CalibImageFreq
{
    if freq_hz > 900_000_000 {
        CalibImageFreq::MHz902_928
    } else if freq_hz > 800_000_000 {
        CalibImageFreq::MHz863_870
    } else if freq_hz > 460_000_000 {
        CalibImageFreq::MHz470_510
    } else {
        CalibImageFreq::MHz430_440
    }
}

/// Build the SX126x LoRa configuration from radio-layer settings.
pub fn build_lora_config(config: MeshRadioConfig) -> SxConfig
{
    let mod_params = LoraModParams::default()
        .set_spread_factor(config.spread_factor)
        .set_bandwidth(config.bandwidth)
        .set_coding_rate(config.coding_rate)
        .set_low_dr_opt(needs_ldro(config.spread_factor, config.bandwidth));

    let packet_params = LoRaPacketParams::default()
        .set_preamble_len(config.preamble_len)
        .set_header_type(LoRaHeaderType::VarLen)
        .set_payload_len(MAX_LORA_PAYLOAD_LEN as u8)
        .set_crc_type(LoRaCrcType::CrcOn)
        .set_invert_iq(if config.iq_inverted {
            LoRaInvertIq::Inverted
        } else {
            LoRaInvertIq::Standard
        });

    // RadioLib uses the SX1262 high-power PA path for all output-power levels.
    let pa_config = PaConfig::default()
        .set_pa_duty_cycle(0x04)
        .set_hp_max(0x07)
        .set_device_sel(DeviceSel::SX1262);

    let tx_params = TxParams::default()
        .set_power_dbm(with_radio_host(|host| (host.lora_tx_power_dbm)()))
        .set_ramp_time(config.tx_ramp_time);

    let irq_mask = IrqMask::none()
        .combine(IrqMaskBit::TxDone)
        .combine(IrqMaskBit::RxDone)
        .combine(IrqMaskBit::Timeout)
        .combine(IrqMaskBit::CrcErr);

    // Calculate PLL frequency using integer math: freq * 2^25 / 32_000_000.
    let rf_freq = ((config.freq_hz as u64 * 32768) / 31250) as u32;

    SxConfig {
        packet_type: PacketType::LoRa,
        sync_word: Some(config.sync_word),
        calib_param: CalibParam::all(),
        mod_params: mod_params.into(),
        pa_config,
        packet_params: Some(packet_params.into()),
        tx_params,
        dio1_irq_mask: irq_mask,
        dio2_irq_mask: IrqMask::none(),
        dio3_irq_mask: IrqMask::none(),
        dio2_rf_switch_ctrl: true,
        rf_freq,
        rf_frequency: config.freq_hz,
        tcxo_opts: if config.use_tcxo {
            Some((
                TcxoVoltage::Volt1_8,
                TcxoDelay::from_ms(config.tcxo_delayms),
            ))
        } else {
            None
        },
    }
}
