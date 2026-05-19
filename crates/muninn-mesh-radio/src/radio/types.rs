//! Public radio data types and small platform traits.
//!
//! These definitions form the stable API for the generic Mesh radio wrapper.
//! Concrete SPI, GPIO, timing, and FEM behavior is supplied by platform crates.

use muninn_mesh_meshcore_lib::{
    MESHCORE_DEFAULT_FREQ_HZ,
    MESHCORE_DEFAULT_PREAMBLE_LEN,
    MESHCORE_DEFAULT_SYNC_WORD,
    MeshMessage,
};
use muninn_mesh_sx126x::op::modulation::{LoRaBandWidth, LoRaSpreadFactor, LoraCodingRate};
use muninn_mesh_sx126x::op::rxtx::RampTime;

/// Default TCXO startup delay in milliseconds.
pub const LORA_TCXO_DELAY_MS: u32 = 20;
/// Maximum SX126x LoRa payload length.
pub const MAX_LORA_PAYLOAD_LEN: usize = 255;

/// Errors returned by the generic radio wrapper.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MeshRadioError
{
    /// Radio or peripheral initialization failed.
    InitFailed,
    /// A SPI/GPIO transaction with the SX1262 failed.
    DeviceIo,
    /// The SX1262 BUSY pin remained asserted past the requested timeout.
    BusyTimeout,
    /// A TX operation did not complete before its timeout.
    TxTimeout,
    /// The caller supplied an empty or oversized TX payload.
    InvalidPayload,
}

/// Minimal blocking clock used by the radio owner.
pub trait RadioClock
{
    /// Return monotonic milliseconds since boot.
    fn now_ms(&self) -> u64;

    /// Block the current radio-owner context for the requested milliseconds.
    fn delay_ms(&mut self, delay_ms: u32);
}

/// Board-specific RF front-end path switching.
pub trait RadioSwitch
{
    /// Switch the external RF path to receive mode.
    fn set_rx(&mut self) -> Result<(), MeshRadioError>;

    /// Switch the external RF path to transmit mode.
    fn set_tx(&mut self) -> Result<(), MeshRadioError>;
}

/// No-op RF switch for boards without an external FEM path-control GPIO.
#[derive(Debug, Clone, Copy, Default)]
pub struct NoRadioSwitch;

impl RadioSwitch for NoRadioSwitch
{
    fn set_rx(&mut self) -> Result<(), MeshRadioError>
    {
        Ok(())
    }

    fn set_tx(&mut self) -> Result<(), MeshRadioError>
    {
        Ok(())
    }
}

/// Result status for a TX request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MeshRadioTxStatus
{
    /// The frame was transmitted successfully.
    Sent,
}

/// Received MeshCore frame with parsed message metadata and raw bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MeshRxFrame
{
    /// Parsed MeshCore message metadata for UI and routing layers.
    pub message:   MeshMessage,
    /// Number of valid bytes in [`Self::raw_bytes`].
    pub raw_len:   u8,
    /// FNV-1a hash of the raw received bytes.
    pub raw_hash:  u32,
    /// Raw received LoRa payload bytes.
    pub raw_bytes: [u8; MAX_LORA_PAYLOAD_LEN],
}

impl MeshRxFrame
{
    /// Return the valid raw LoRa payload slice.
    pub fn raw_payload(&self) -> &[u8]
    {
        let len = (self.raw_len as usize).min(self.raw_bytes.len());
        &self.raw_bytes[..len]
    }
}

/// Configuration for MeshCore LoRa operation on the board radio.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MeshRadioConfig
{
    /// LoRa center frequency in hertz.
    pub freq_hz:       u32,
    /// LoRa sync word.
    pub sync_word:     u16,
    /// LoRa spreading factor.
    pub spread_factor: LoRaSpreadFactor,
    /// LoRa signal bandwidth.
    pub bandwidth:     LoRaBandWidth,
    /// LoRa coding rate.
    pub coding_rate:   LoraCodingRate,
    /// LoRa preamble length in symbols.
    pub preamble_len:  u16,
    /// Whether LoRa IQ should be inverted.
    pub iq_inverted:   bool,
    /// SX1262 TX power ramp time.
    pub tx_ramp_time:  RampTime,
    /// Whether the radio driver should enable TCXO control during initialization.
    pub use_tcxo:      bool,
    /// TCXO startup delay in milliseconds.
    pub tcxo_delayms:  u32,
}

impl Default for MeshRadioConfig
{
    fn default() -> Self
    {
        Self {
            freq_hz:       MESHCORE_DEFAULT_FREQ_HZ,
            sync_word:     MESHCORE_DEFAULT_SYNC_WORD,
            spread_factor: LoRaSpreadFactor::SF7,
            bandwidth:     LoRaBandWidth::BW62,
            coding_rate:   LoraCodingRate::CR4_5,
            preamble_len:  MESHCORE_DEFAULT_PREAMBLE_LEN,
            iq_inverted:   false,
            tx_ramp_time:  RampTime::Ramp200u,
            use_tcxo:      true,
            tcxo_delayms:  LORA_TCXO_DELAY_MS,
        }
    }
}

/// Read-only snapshot of radio-side RX/TX observability counters.
///
/// Counters are `u32` and wrap on overflow. `last_applied_tx_power_dbm`
/// is `None` until the first `transmit()` call completes the pre-`set_tx_params`
/// cache write.
#[derive(Debug, Clone, Copy)]
pub struct RxStats
{
    /// Successfully decoded RX frame count.
    pub rx_frames:                 u32,
    /// RX packets rejected by CRC.
    pub rx_crc_error:              u32,
    /// RX timeout IRQ count.
    pub rx_timeout:                u32,
    /// Packets whose `HEADER_ERROR` IRQ bit was set without a following `RX_DONE`.
    pub rx_header_error:           u32,
    /// RSSI of the most recently decoded packet in dBm.
    pub last_rssi_dbm:             i16,
    /// SNR of the most recently decoded packet in tenths of a dB.
    pub last_snr_tenth_db:         i16,
    /// TX power most recently written to the radio, if any.
    pub last_applied_tx_power_dbm: Option<i8>,
    /// Rolling minimum of recent idle-RX `GetRssiInst` samples.
    pub last_noise_floor_dbm:      i16,
    /// Latest raw `GetRssiInst` sample while idle-listening.
    pub last_rssi_inst_dbm:        i16,
    /// Whether [`Self::last_rssi_inst_dbm`] contains a valid sample.
    pub has_rssi_inst:             bool,
    /// Raw chip-mode nibble from `GetStatus`; 0 means "never sampled".
    pub chip_mode:                 u8,
    /// `GetDeviceErrors` raw bitmask observed since the previous `rx_stats()` call.
    pub device_errors:             u16,
}
