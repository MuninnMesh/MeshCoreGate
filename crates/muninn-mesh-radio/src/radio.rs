//! Generic SX126x radio task support for MeshCore packets.
//!
//! The code in this module owns the SX126x setup sequence, RX/TX state
//! transitions, and lightweight packet metadata extraction needed by the rest
//! of the firmware. Platform crates provide concrete SPI, GPIO, timing, and
//! RF-switch resources.

use core::cmp::min;

use embedded_hal::digital::{InputPin, OutputPin};
use embedded_hal::spi::SpiDevice;
use muninn_mesh_meshcore_lib::{MeshMessage, MeshTxFrame};
use muninn_mesh_sx126x::SX126x;
use muninn_mesh_sx126x::op::init::StandbyConfig;
use muninn_mesh_sx126x::op::irq::IrqMask;
use muninn_mesh_sx126x::op::modulation::{LoRaBandWidth, LoraCodingRate};
use muninn_mesh_sx126x::op::packet::{
    LoRaCrcType,
    LoRaHeaderType,
    LoRaInvertIq,
    LoRaPacketParams,
    PacketParams,
};
use muninn_mesh_sx126x::op::rxtx::{RxTxTimeout, TxParams};

/// SX126x configuration builder helpers.
mod config;
/// Host callback registry.
mod host;
/// MeshCore packet metadata parsing helpers.
mod packet;
/// Public radio API types.
mod types;
/// SX126x errata and board workaround helpers.
mod workarounds;

use config::{build_lora_config, calib_image_freq};
use host::with_radio_host;
pub use host::{RadioHostFns, register_radio_host};
use packet::{decode_packet_meta, fnv1a32};
pub use types::{
    LORA_TCXO_DELAY_MS,
    MAX_LORA_PAYLOAD_LEN,
    MeshRadioConfig,
    MeshRadioError,
    MeshRadioTxStatus,
    MeshRxFrame,
    NoRadioSwitch,
    RadioClock,
    RadioSwitch,
    RxStats,
};
use workarounds::Sx126xWorkarounds;
/// Number of idle RSSI samples retained for the noise-floor estimate.
const NOISE_FLOOR_SAMPLE_COUNT: usize = 60;
/// Initial reset/standby/calibration BUSY timeout.
const RADIO_LONG_BUSY_TIMEOUT_MS: u32 = 1000;
/// Short state-transition BUSY timeout.
const RADIO_SHORT_BUSY_TIMEOUT_MS: u32 = 100;
/// Minimum SX1262 TX command timeout. Longer-airtime LoRa configs extend this
/// dynamically from the computed packet airtime.
const TX_MIN_TIMEOUT_MS: u32 = 6000;
/// Extra SX1262 command-timeout budget beyond the computed LoRa airtime.
const TX_TIMEOUT_MARGIN_MS: u32 = 2000;
/// Absolute guardrail for malformed configs or airtime-estimator mistakes.
const TX_MAX_TIMEOUT_MS: u32 = 60_000;
/// Extra host-side polling budget beyond the SX1262 command timeout.
const TX_SOFT_TIMEOUT_MARGIN_MS: u64 = 750;
/// Delay between TX IRQ polling attempts.
const TX_POLL_INTERVAL_MS: u64 = 2;
/// Valid TX completion IRQ: only TxDone may be set while waiting for SetTx.
const IRQ_TX_DONE_ONLY: u16 = 0x0001;
/// Non-TX IRQ bits that are not valid evidence of TX completion. Timeout
/// is intentionally excluded and handled as a terminal TX failure.
const IRQ_NON_TX_BITS: u16 = 0x01FE;

/// Generic SX126x radio owner.
pub struct MeshRadio<TSPI, TNRST, TBUSY, TANT, TDIO1, TSWITCH, TCLOCK>
where
    TSPI: SpiDevice,
{
    /// Concrete SX1262 device driver.
    device:                    SX126x<TSPI, TNRST, TBUSY, TANT, TDIO1>,
    /// Active Mesh radio configuration.
    config:                    MeshRadioConfig,
    /// Board-specific RF path switch.
    rf_switch:                 TSWITCH,
    /// Blocking clock and delay provider for the radio owner.
    clock:                     TCLOCK,
    /// Successfully decoded RX frame count.
    rx_frames:                 u32,
    /// Total packets with CRC errors.
    rx_crc_error:              u32,
    /// IRQ status read or clear failures.
    rx_irq_error:              u32,
    /// Buffer status or read failures.
    rx_status_error:           u32,
    /// RX timeouts from IRQ status.
    rx_timeout:                u32,
    /// `HEADER_ERROR` IRQ bumps (see `RxStats::rx_header_error`).
    rx_header_error:           u32,
    /// Transmissions that timed out.
    tx_timeout:                u32,
    /// Failures clearing IRQ during TX.
    tx_irq_error:              u32,
    /// Failures restoring RX state after TX.
    tx_restore_error:          u32,
    /// Packet parameters used when restoring continuous RX.
    rx_packet_params:          PacketParams,
    /// Duration of last successful TX in milliseconds for airtime budgeting.
    pub last_tx_airtime_ms:    u32,
    /// RSSI of the last successfully decoded RX frame (dBm), `i16::MIN` until first RX.
    last_rssi_dbm:             i16,
    /// SNR of the last successfully decoded RX frame (tenths of dB), 0 until first RX.
    last_snr_tenth_db:         i16,
    /// dBm value most recently written to `TxParams` (after any
    /// low-battery reduction). `None` until the first `transmit()` call.
    last_applied_tx_power_dbm: Option<i8>,
    /// Rolling 60-entry ring of idle-RX `GetRssiInst` samples, oldest
    /// overwritten. `noise_floor_dbm` is the minimum of the populated
    /// entries — see `sample_health()`.
    noise_floor_ring:          [i16; NOISE_FLOOR_SAMPLE_COUNT],
    /// Next ring index for an idle-RX RSSI sample.
    noise_floor_ring_cursor:   u8,
    /// Number of populated entries in [`Self::noise_floor_ring`].
    noise_floor_ring_len:      u8,
    /// Minimum over `noise_floor_ring` populated entries, recomputed on
    /// each sample. `i16::MIN` when the ring is empty.
    last_noise_floor_dbm:      i16,
    /// Latest raw `GetRssiInst` sample. `i16::MIN` sentinel until first
    /// successful sample.
    last_rssi_inst_dbm:        i16,
    /// Whether [`Self::last_rssi_inst_dbm`] contains a valid sample.
    has_rssi_inst:             bool,
    /// Raw chip-mode nibble from `GetStatus`. 0 until first sample.
    chip_mode:                 u8,
    /// Accumulated `GetDeviceErrors` bits since the last `rx_stats()`
    /// read. Swapped to 0 on copy.
    device_errors:             u16,
    /// Last raw `GetIrqStatus` value sampled at the moment a TX
    /// completed (or timed out). Exposed via [`Self::last_tx_irq_raw`]
    /// for diagnostic logging from the board crate; useful for
    /// distinguishing "real TX done" (irq=0x0001) from "stale or
    /// corrupted irq" (e.g. 0x0101, 0x5151, 0xFFFF) without dragging
    /// a logger dependency into this crate.
    last_tx_irq_raw:           u16,
}

impl<TSPI, TNRST, TBUSY, TANT, TDIO1, TSPIERR, TPINERR, TSWITCH, TCLOCK>
    MeshRadio<TSPI, TNRST, TBUSY, TANT, TDIO1, TSWITCH, TCLOCK>
where
    TPINERR: core::fmt::Debug,
    TSPI: SpiDevice<Error = TSPIERR>,
    TNRST: OutputPin<Error = TPINERR>,
    TBUSY: InputPin<Error = TPINERR>,
    TANT: OutputPin<Error = TPINERR>,
    TDIO1: InputPin<Error = TPINERR>,
    TSWITCH: RadioSwitch,
    TCLOCK: RadioClock,
{
    /// Create and initialize a generic SX126x radio wrapper.
    pub async fn init(
        mut device: SX126x<TSPI, TNRST, TBUSY, TANT, TDIO1>,
        mut rf_switch: TSWITCH,
        mut clock: TCLOCK,
        config: MeshRadioConfig,
    ) -> Result<Self, MeshRadioError>
    {
        rf_switch.set_rx()?;

        // 1. Reset and wait for busy
        log::debug!(
            "LORA init: freq={}Hz sync={:04X} tx_pwr={}dBm tcxo={}",
            config.freq_hz,
            config.sync_word,
            with_radio_host(|host| (host.lora_tx_power_dbm)()),
            config.use_tcxo
        );
        device.reset().map_err(|_| MeshRadioError::InitFailed)?;
        wait_busy_timeout(&mut device, &mut clock, RADIO_LONG_BUSY_TIMEOUT_MS).await?;

        // 2. Set standby (DC-DC and Fallback are not supported by the crate)
        device
            .set_standby(StandbyConfig::StbyRc)
            .map_err(|_| MeshRadioError::InitFailed)?;
        wait_busy_timeout(&mut device, &mut clock, RADIO_LONG_BUSY_TIMEOUT_MS).await?;

        // Enable DC-DC regulator before PA config per datasheet Section 10.1
        device
            .set_regulator_mode_dcdc()
            .map_err(|_| MeshRadioError::InitFailed)?;
        wait_busy_timeout(&mut device, &mut clock, RADIO_SHORT_BUSY_TIMEOUT_MS).await?;

        // 3. Initialize with config (includes SetPacketType, SetModulationParams, etc.)
        let sx_config = build_lora_config(config);
        let rx_packet_params = sx_config.packet_params.ok_or(MeshRadioError::InitFailed)?;

        device
            .init(sx_config)
            .map_err(|_| MeshRadioError::InitFailed)?;

        device
            .apply_init_workarounds(config.iq_inverted)
            .map_err(|_| MeshRadioError::InitFailed)?;

        // 4. Calibrate image dynamically based on frequency
        let calib_freq = calib_image_freq(config.freq_hz);
        device
            .calibrate_image(calib_freq)
            .map_err(|_| MeshRadioError::InitFailed)?;
        wait_busy_timeout(&mut device, &mut clock, RADIO_LONG_BUSY_TIMEOUT_MS).await?;

        // 5. Clear any device-error bits set during calibration. These bits are sticky until
        //    explicitly cleared, and stale calibration markers (RC64K_CALIB_ERR, IMG_CALIB_ERR,
        //    etc.) pollute the first TX's diagnostics — and on the Bifrost / Wio-SX1262 hand-wired
        //    path we observed `dev_err=0x5151` left over from boot, which made every TX look like a
        //    PLL/PA failure even when the chip was fine.
        let _ = device.clear_device_errors();
        wait_busy_timeout(&mut device, &mut clock, RADIO_SHORT_BUSY_TIMEOUT_MS).await?;

        // Radio stays in standby after init to reduce boot power draw.
        // Call start_rx() from the radio task after boot staging completes.
        log::debug!("LORA init OK (standby, RX deferred)");

        Ok(Self {
            device,
            config,
            rf_switch,
            clock,
            rx_frames: 0,
            rx_crc_error: 0,
            rx_irq_error: 0,
            rx_status_error: 0,
            rx_timeout: 0,
            rx_header_error: 0,
            tx_timeout: 0,
            tx_irq_error: 0,
            tx_restore_error: 0,
            rx_packet_params,
            last_tx_airtime_ms: 0,
            last_rssi_dbm: i16::MIN,
            last_snr_tenth_db: 0,
            last_applied_tx_power_dbm: None,
            noise_floor_ring: [0; NOISE_FLOOR_SAMPLE_COUNT],
            noise_floor_ring_cursor: 0,
            noise_floor_ring_len: 0,
            last_noise_floor_dbm: i16::MIN,
            last_rssi_inst_dbm: i16::MIN,
            has_rssi_inst: false,
            chip_mode: 0,
            device_errors: 0,
            last_tx_irq_raw: 0,
        })
    }

    /// Mutable access to the board-specific clock/delay provider. Board
    /// integrations use this for local side effects that must continue
    /// during radio-owned blocking delays.
    pub fn clock_mut(&mut self) -> &mut TCLOCK
    {
        &mut self.clock
    }

    /// Reset and reinitialize the SX1262 using the existing SPI bus/pins.
    /// Called when the radio enters a bad state (e.g. BUSY stuck high after
    /// voltage droop during TX). Returns the radio to continuous RX mode.
    pub async fn reinit(&mut self) -> Result<(), MeshRadioError>
    {
        log::warn!("LORA reinit: resetting SX1262...");
        let config = self.config;

        self.rf_switch.set_rx()?;

        // Hard reset
        self.device
            .reset()
            .map_err(|_| MeshRadioError::InitFailed)?;
        wait_busy_timeout(
            &mut self.device,
            &mut self.clock,
            RADIO_LONG_BUSY_TIMEOUT_MS,
        )
        .await?;

        self.device
            .set_standby(StandbyConfig::StbyRc)
            .map_err(|_| MeshRadioError::InitFailed)?;
        wait_busy_timeout(
            &mut self.device,
            &mut self.clock,
            RADIO_LONG_BUSY_TIMEOUT_MS,
        )
        .await?;

        self.device
            .set_regulator_mode_dcdc()
            .map_err(|_| MeshRadioError::InitFailed)?;
        wait_busy_timeout(
            &mut self.device,
            &mut self.clock,
            RADIO_SHORT_BUSY_TIMEOUT_MS,
        )
        .await?;

        let sx_config = build_lora_config(config);
        self.rx_packet_params = sx_config.packet_params.ok_or(MeshRadioError::InitFailed)?;

        self.device
            .init(sx_config)
            .map_err(|_| MeshRadioError::InitFailed)?;

        self.device
            .apply_init_workarounds(config.iq_inverted)
            .map_err(|_| MeshRadioError::InitFailed)?;

        // Calibrate image
        let calib_freq = calib_image_freq(config.freq_hz);
        self.device
            .calibrate_image(calib_freq)
            .map_err(|_| MeshRadioError::InitFailed)?;
        wait_busy_timeout(
            &mut self.device,
            &mut self.clock,
            RADIO_LONG_BUSY_TIMEOUT_MS,
        )
        .await?;

        // Go straight to RX
        self.start_rx()?;
        log::warn!("LORA reinit OK — continuous RX restored");
        Ok(())
    }

    /// Apply the SX1262 TX modulation sensitivity workaround.
    fn fix_sensitivity(&mut self) -> Result<(), MeshRadioError>
    {
        self.device.apply_tx_modulation_patch()
    }

    /// Apply boosted RX gain to the SX1262.
    fn apply_rx_boosted_gain(&mut self) -> Result<(), MeshRadioError>
    {
        self.device.apply_rx_boosted_gain()
    }

    /// Apply the RX sensitivity register patch.
    fn fix_rx_sensitivity_patch(&mut self) -> Result<(), MeshRadioError>
    {
        self.device.apply_rx_sensitivity_patch()
    }

    /// Apply the STM32WL LoRa AGC jam-recovery workaround when enabled.
    fn apply_stm32wl_lora_agc_jam_recovery(&mut self) -> Result<(), MeshRadioError>
    {
        self.device.apply_stm32wl_lora_agc_jam_recovery()
    }

    /// Apply all RX sensitivity workarounds used before entering RX.
    fn apply_rx_sensitivity_workarounds(&mut self) -> Result<(), MeshRadioError>
    {
        self.fix_sensitivity()?;
        self.apply_rx_boosted_gain()?;
        self.fix_rx_sensitivity_patch()?;
        // ST public SubGHz_Phy workaround for STM32WL integrated Sub-GHz radio.
        //
        // ST defines 0x08A3 as SUBGHZ_AGCCFG, "Sub-GHz radio Agc LoRa register".
        // Their LoRa RX setup writes:
        //
        //     write(0x08A3, read(0x08A3) & 0x01)
        //
        // with the comment:
        // "Set the step threshold value to 1 to avoid to miss low power signal
        // after an interferer jam the chip in LoRa modulaltion"
        //
        // This register/bitfield is not described in the public STM32WL RM or
        // Semtech SX126x datasheet. Keep this as an explicit feature-gated
        // workaround even though it is enabled by default for field testing.
        self.apply_stm32wl_lora_agc_jam_recovery()?;
        Ok(())
    }

    /// Apply the SX1262 IQ polarity errata workaround for the configured IQ mode.
    fn fix_iq_polarity(&mut self) -> Result<(), MeshRadioError>
    {
        self.device.apply_iq_polarity(self.config.iq_inverted)
    }

    /// Most recent raw `GetIrqStatus` value seen at the moment TX
    /// completed (or `0` if no TX has succeeded yet). A real TX done
    /// reads `0x0001` (just the TxDone bit). Patterns like `0x0101`
    /// or `0x5151` indicate corrupted SPI MISO data, not a genuine
    /// completion.
    pub fn last_tx_irq_raw(&self) -> u16
    {
        self.last_tx_irq_raw
    }

    /// Snapshot of RX/TX observability counters for the UI layer.
    ///
    /// Takes `&mut self` because it drains the accumulated
    /// `device_errors` bitmask — swapping it to 0 after the copy so the
    /// next snapshot reflects only errors observed in the new window.
    pub fn rx_stats(&mut self) -> RxStats
    {
        let device_errors = self.device_errors;
        self.device_errors = 0;
        RxStats {
            rx_frames: self.rx_frames,
            rx_crc_error: self.rx_crc_error,
            rx_timeout: self.rx_timeout,
            rx_header_error: self.rx_header_error,
            last_rssi_dbm: self.last_rssi_dbm,
            last_snr_tenth_db: self.last_snr_tenth_db,
            last_applied_tx_power_dbm: self.last_applied_tx_power_dbm,
            last_noise_floor_dbm: self.last_noise_floor_dbm,
            last_rssi_inst_dbm: self.last_rssi_inst_dbm,
            has_rssi_inst: self.has_rssi_inst,
            chip_mode: self.chip_mode,
            device_errors,
        }
    }

    /// Poll four lightweight SX1262 health surfaces in one pass: live
    /// channel RSSI (`GetRssiInst`), chip mode (`GetStatus`), device
    /// errors (`GetDeviceErrors`, then `ClearDeviceErrors` to avoid
    /// latching), and update the rolling noise-floor minimum. Must be
    /// called from the radio-owning task while the device is in
    /// continuous RX — polls are skipped automatically if a prior
    /// `GetStatus` returned `TX` or `FS`. Each inner SPI call is
    /// fallible; a soft failure leaves the corresponding cached value
    /// unchanged.
    pub fn sample_health(&mut self)
    {
        // Status first — lets us bail before running rssi_inst if we
        // are mid-TX (GetRssiInst during TX returns invalid data).
        let chip_mode = match self.device.get_status() {
            Ok(status) => {
                let m = status.chip_mode().map(|cm| cm as u8).unwrap_or(0);
                self.chip_mode = m;
                m
            },
            Err(_) => {
                log::debug!("LORA sample_health: GetStatus failed");
                return;
            },
        };
        // 0x06 = TX, 0x04 = FS. Either means sampling RSSI is unsafe.
        if chip_mode == 0x06 || chip_mode == 0x04 {
            return;
        }

        if let Ok(rssi) = self.device.get_rssi_inst() {
            let sample = round_i16(rssi);
            self.last_rssi_inst_dbm = sample;
            self.has_rssi_inst = true;
            // Push into the 60-slot ring and recompute the rolling min.
            // The ring is a quiet-floor baseline — distinct from the
            // raw live sample above.
            let cap = self.noise_floor_ring.len();
            let cursor = self.noise_floor_ring_cursor as usize;
            self.noise_floor_ring[cursor] = sample;
            self.noise_floor_ring_cursor = ((cursor + 1) % cap) as u8;
            if (self.noise_floor_ring_len as usize) < cap {
                self.noise_floor_ring_len = self.noise_floor_ring_len.saturating_add(1);
            }
            let len = self.noise_floor_ring_len as usize;
            let mut min = sample;
            for v in &self.noise_floor_ring[..len] {
                if *v < min {
                    min = *v;
                }
            }
            self.last_noise_floor_dbm = min;
        }

        if let Ok(errs) = self.device.get_device_errors() {
            let bits = errs.bits();
            if bits != 0 {
                self.device_errors |= bits;
                // Clear the chip-side latch so a transient error does
                // not stick forever. Our own `device_errors` field is
                // drained on `rx_stats()` read.
                let _ = self.device.clear_device_errors();
            }
        }
    }

    /// Enter continuous RX mode. Called from the radio task after boot staging.
    pub fn start_rx(&mut self) -> Result<(), MeshRadioError>
    {
        self.apply_rx_sensitivity_workarounds()?;
        self.fix_iq_polarity()?;
        self.device
            .clear_irq_status(IrqMask::all())
            .map_err(|_| MeshRadioError::DeviceIo)?;
        self.device
            .set_rx(RxTxTimeout::continuous_rx())
            .map_err(|_| MeshRadioError::DeviceIo)?;
        log::debug!("LORA continuous RX active");
        Ok(())
    }

    /// Poll IRQ status and return a received MeshCore frame when one is ready.
    pub async fn poll_receive(
        &mut self,
        timestamp: u32,
    ) -> Result<Option<MeshRxFrame>, MeshRadioError>
    {
        match self.device.try_dio1_is_high() {
            Ok(true) => {},
            Ok(false) => return Ok(None),
            Err(_) => {
                self.rx_irq_error = self.rx_irq_error.wrapping_add(1);
                return Err(MeshRadioError::DeviceIo);
            },
        }

        let irq = match self.device.get_irq_status() {
            Ok(irq) => irq,
            Err(_) => {
                self.rx_irq_error = self.rx_irq_error.wrapping_add(1);
                return Err(MeshRadioError::DeviceIo);
            },
        };

        // Reserved bits (10..=15) set means MISO returned corrupted /
        // floating-high data — treat the read as "nothing yet" rather
        // than fabricating an RX frame from junk.
        if irq.raw() & 0xFC00 != 0 {
            return Ok(None);
        }

        // TxDone cannot be produced by continuous RX. If it appears here,
        // the IRQ read is stale or corrupted; treating the same word as
        // RxDone fabricates packets from whatever happens to be in the RX
        // buffer.
        if irq.tx_done() {
            self.rx_irq_error = self.rx_irq_error.wrapping_add(1);
            let _ = self.device.clear_irq_status(IrqMask::all());
            let _ = self.device.set_rx(RxTxTimeout::continuous_rx());
            return Ok(None);
        }

        if !irq.rx_done() && !irq.timeout() && !irq.crc_err() && !irq.header_error() {
            return Ok(None);
        }

        if irq.crc_err() {
            self.rx_crc_error = self.rx_crc_error.wrapping_add(1);
            log::warn!("LORA RX CRC error (total={})", self.rx_crc_error);
        }
        if irq.timeout() {
            self.rx_timeout = self.rx_timeout.wrapping_add(1);
            log::warn!("LORA RX timeout (total={})", self.rx_timeout);
        }
        // Count header-error only when no RX_DONE followed it. A header
        // error on a packet that still decodes successfully (rare) is
        // not a diagnostic signal and would double-count.
        if irq.header_error() && !irq.rx_done() {
            self.rx_header_error = self.rx_header_error.wrapping_add(1);
            log::warn!("LORA RX header error (total={})", self.rx_header_error);
        }

        let mut out = None;
        if irq.rx_done() {
            if irq.crc_err() {
                // Already logged and incremented above
            } else {
                let packet_status = self.device.get_packet_status().ok();
                if let Ok(rx_status) = self.device.get_rx_buffer_status() {
                    let read_len =
                        min(rx_status.payload_length_rx() as usize, MAX_LORA_PAYLOAD_LEN);
                    let offset = rx_status.rx_start_buffer_pointer();
                    let mut payload = [0u8; MAX_LORA_PAYLOAD_LEN];
                    if self
                        .device
                        .read_buffer(offset, &mut payload[..read_len])
                        .is_ok()
                    {
                        let meta = decode_packet_meta(&payload[..read_len]);
                        let Some(packet_status) = packet_status.as_ref() else {
                            self.rx_status_error = self.rx_status_error.wrapping_add(1);
                            let _ = self.device.clear_irq_status(IrqMask::all());
                            let _ = self.device.set_rx(RxTxTimeout::continuous_rx());
                            return Ok(None);
                        };
                        let rssi_dbm = round_i16(packet_status.rssi_pkt());
                        if rssi_dbm >= -5 {
                            self.rx_status_error = self.rx_status_error.wrapping_add(1);
                            log::warn!(
                                "LORA RX ignored impossible packet status rssi={} len={} offset={}",
                                rssi_dbm,
                                read_len,
                                offset,
                            );
                            let _ = self.device.clear_irq_status(IrqMask::all());
                            let _ = self.device.set_rx(RxTxTimeout::continuous_rx());
                            return Ok(None);
                        }
                        let snr_tenth_db = round_i16(packet_status.snr_pkt() * 10.0);

                        // Cache last-RX link quality for RxStats.
                        self.last_rssi_dbm = rssi_dbm;
                        self.last_snr_tenth_db = snr_tenth_db;

                        let sender_bytes = if meta.sender_len > 0 {
                            let len = (meta.sender_len as usize).min(meta.sender_name.len());
                            &meta.sender_name[..len]
                        } else {
                            &[][..]
                        };
                        let sender_str = core::str::from_utf8(sender_bytes).unwrap_or("?");

                        match meta.kind {
                            0x07 => log::info!(
                                "LORA RxDone: len={} rssi={} snr={} src={:02X} dst={:02X} type={} \
                                 hops={} sender=ANON",
                                read_len,
                                rssi_dbm,
                                snr_tenth_db,
                                meta.src,
                                meta.dst,
                                meta.kind,
                                meta.path_hops,
                            ),
                            0x09 => log::info!(
                                "LORA RxDone: len={} rssi={} snr={} src={:02X} dst={:02X} type={} \
                                 hops={} sender=TRACE",
                                read_len,
                                rssi_dbm,
                                snr_tenth_db,
                                meta.src,
                                meta.dst,
                                meta.kind,
                                meta.path_hops,
                            ),
                            0x04 if meta.has_pubkey4 => log::info!(
                                "LORA RxDone: len={} rssi={} snr={} src={:02X} dst={:02X} type={} \
                                 hops={} sender={:?} pubkey4={:04X}",
                                read_len,
                                rssi_dbm,
                                snr_tenth_db,
                                meta.src,
                                meta.dst,
                                meta.kind,
                                meta.path_hops,
                                sender_str,
                                meta.pubkey4,
                            ),
                            _ if meta.sender_len > 0 => log::info!(
                                "LORA RxDone: len={} rssi={} snr={} src={:02X} dst={:02X} type={} \
                                 hops={} sender={:?}",
                                read_len,
                                rssi_dbm,
                                snr_tenth_db,
                                meta.src,
                                meta.dst,
                                meta.kind,
                                meta.path_hops,
                                sender_str,
                            ),
                            _ => log::info!(
                                "LORA RxDone: len={} rssi={} snr={} src={:02X} dst={:02X} type={} \
                                 hops={}",
                                read_len,
                                rssi_dbm,
                                snr_tenth_db,
                                meta.src,
                                meta.dst,
                                meta.kind,
                                meta.path_hops,
                            ),
                        }

                        self.rx_frames = self.rx_frames.wrapping_add(1);
                        let mut message = MeshMessage::new(
                            timestamp,
                            meta.src,
                            meta.dst,
                            meta.kind,
                            rssi_dbm,
                            snr_tenth_db,
                            read_len as u16,
                        );
                        message.path_hops = meta.path_hops;
                        if meta.path_len > 0 {
                            message.set_path(&meta.path[..meta.path_len as usize]);
                        }
                        message.repeater = if meta.repeater != 0 {
                            meta.repeater
                        } else {
                            meta.src
                        };
                        if meta.sender_len > 0 {
                            let sender_len = (meta.sender_len as usize).min(meta.sender_name.len());
                            message.set_sender_name(&meta.sender_name[..sender_len]);
                        }
                        if meta.has_pubkey4 {
                            message.set_pubkey4(meta.pubkey4);
                        }
                        if meta.has_full_pubkey && meta.has_pubkey4 {
                            with_radio_host(|host| {
                                (host.remember_pubkey)(meta.pubkey4, meta.full_pubkey, timestamp)
                            });
                        }
                        if meta.text_len > 0 {
                            let text_len = (meta.text_len as usize).min(meta.text_preview.len());
                            message.set_text_preview(&meta.text_preview[..text_len]);
                        }
                        if meta.has_position {
                            message.set_position(meta.lat_e7, meta.lon_e7);
                        }
                        if meta.trace_hashes_len > 0 {
                            message.set_trace_hashes(
                                &meta.trace_hashes[..meta.trace_hashes_len as usize],
                                meta.trace_hash_size,
                            );
                        }
                        let mut raw_bytes = [0u8; MAX_LORA_PAYLOAD_LEN];
                        raw_bytes[..read_len].copy_from_slice(&payload[..read_len]);
                        out = Some(MeshRxFrame {
                            message,
                            raw_len: read_len as u8,
                            raw_hash: fnv1a32(&payload[..read_len]),
                            raw_bytes,
                        });
                    } else {
                        self.rx_status_error = self.rx_status_error.wrapping_add(1);
                    }
                } else {
                    self.rx_status_error = self.rx_status_error.wrapping_add(1);
                }
            }
        }

        // Always clear IRQ status and explicitly re-arm the receiver.
        // Some SX1262 versions require this even in continuous mode.
        let _ = self.device.clear_irq_status(IrqMask::all());
        let _ = self.device.set_rx(RxTxTimeout::continuous_rx());

        Ok(out)
    }

    /// Transmit raw payload bytes and return measured airtime in milliseconds.
    async fn transmit_inner(&mut self, payload: &[u8]) -> Result<u32, MeshRadioError>
    {
        let iq_inverted = self.config.iq_inverted;

        // Clear IRQs before TX to prevent race condition with stale TxDone.
        self.device
            .clear_irq_status(IrqMask::all())
            .map_err(|_| MeshRadioError::DeviceIo)?;

        // Standby is the safe state for FIFO writes and packet-param changes.
        // TCXO-backed boards are more reliable if TX setup keeps the external
        // oscillator active instead of bouncing through RC standby.
        let standby_config = if self.config.use_tcxo {
            StandbyConfig::StbyXOSC
        } else {
            StandbyConfig::StbyRc
        };
        let standby_timeout_ms = if self.config.use_tcxo {
            RADIO_LONG_BUSY_TIMEOUT_MS
        } else {
            RADIO_SHORT_BUSY_TIMEOUT_MS
        };
        self.device
            .set_standby(standby_config)
            .map_err(|_| MeshRadioError::DeviceIo)?;
        wait_busy_timeout(&mut self.device, &mut self.clock, standby_timeout_ms).await?;

        // Write payload to buffer at offset 0.
        self.device
            .write_buffer(0x00, payload)
            .map_err(|_| MeshRadioError::DeviceIo)?;

        // Set packet params for TX (payload length = actual length)
        let tx_params = LoRaPacketParams::default()
            .set_preamble_len(self.config.preamble_len)
            .set_header_type(LoRaHeaderType::VarLen)
            .set_payload_len(payload.len() as u8)
            .set_crc_type(LoRaCrcType::CrcOn)
            .set_invert_iq(if iq_inverted {
                LoRaInvertIq::Inverted
            } else {
                LoRaInvertIq::Standard
            })
            .into();

        self.device
            .set_packet_params(tx_params)
            .map_err(|_| MeshRadioError::DeviceIo)?;

        // SX1262 errata 15.1: fix sensitivity register (RadioLib does this before every TX)
        self.fix_sensitivity()?;

        // SX1262 errata 15.4: fix IQ polarity after SetPacketParams
        self.fix_iq_polarity()?;
        wait_busy_timeout(
            &mut self.device,
            &mut self.clock,
            RADIO_SHORT_BUSY_TIMEOUT_MS,
        )
        .await?;

        let estimated_airtime_ms = estimate_lora_airtime_ms(&self.config, payload.len());
        let tx_timeout_ms = estimated_airtime_ms
            .saturating_add(TX_TIMEOUT_MARGIN_MS)
            .clamp(TX_MIN_TIMEOUT_MS, TX_MAX_TIMEOUT_MS);
        let soft_timeout_ms = u64::from(tx_timeout_ms).saturating_add(TX_SOFT_TIMEOUT_MARGIN_MS);

        // Force frequency synthesis before TX on TCXO boards. SetTx can do
        // this transition internally, but making it explicit gives the PLL a
        // clean lock point and avoids the repeated TX-timeout-only IRQ pattern
        // seen on the Bifrost Wio-SX1262 prototype.
        if self.config.use_tcxo {
            self.device.set_fs().map_err(|_| MeshRadioError::DeviceIo)?;
            wait_busy_timeout(
                &mut self.device,
                &mut self.clock,
                RADIO_LONG_BUSY_TIMEOUT_MS,
            )
            .await?;
        }

        // Start transmission with a timeout long enough for the configured
        // SF/BW/payload. A fixed 6s SX1262 timeout aborts long MeshCore
        // adverts at SF12/BW62 before the packet can finish.
        self.device
            .set_tx(RxTxTimeout::from_ms(tx_timeout_ms))
            .map_err(|_| MeshRadioError::DeviceIo)?;

        // Wait for completion (non-blocking yield)
        let start_ms = self.clock.now_ms();
        let first_poll_delay_ms = minimum_tx_elapsed_ms(estimated_airtime_ms)
            .min(u64::from(tx_timeout_ms))
            .min(u64::from(u32::MAX)) as u32;
        if first_poll_delay_ms > 0 {
            self.clock.delay_ms(first_poll_delay_ms);
        }
        loop {
            let irq = self
                .device
                .get_irq_status()
                .map_err(|_| MeshRadioError::DeviceIo)?;

            // A TX success must be a clean TxDone-only word. Corrupted
            // reads are sampled again until a clean terminal IRQ or the
            // software timeout; clearing here can erase a real TxDone
            // before a stable read gets through on a noisy prototype bus.
            let raw = irq.raw();
            let elapsed_ms = self.clock.now_ms().saturating_sub(start_ms);
            self.last_tx_irq_raw = raw;
            if raw & 0xFC00 != 0 {
                self.tx_irq_error = self.tx_irq_error.wrapping_add(1);
                self.clock.delay_ms(TX_POLL_INTERVAL_MS as u32);
                if elapsed_ms > soft_timeout_ms {
                    self.tx_timeout = self.tx_timeout.wrapping_add(1);
                    log::warn!("LORA TX timeout (corrupt-only irqs, last raw={:?})", irq,);
                    return Err(MeshRadioError::TxTimeout);
                }
                continue;
            }
            if irq.tx_done() {
                if !tx_elapsed_after_minimum(estimated_airtime_ms, elapsed_ms) {
                    self.tx_irq_error = self.tx_irq_error.wrapping_add(1);
                    self.clock.delay_ms(TX_POLL_INTERVAL_MS as u32);
                    if elapsed_ms > soft_timeout_ms {
                        self.tx_timeout = self.tx_timeout.wrapping_add(1);
                        log::warn!("LORA TX timeout (early txdone irq)");
                        return Err(MeshRadioError::TxTimeout);
                    }
                    continue;
                }
                let tx_ms = elapsed_ms as u32;
                if raw != IRQ_TX_DONE_ONLY {
                    self.tx_irq_error = self.tx_irq_error.wrapping_add(1);
                    log::warn!(
                        "LORA TX done with extra IRQ bits raw=0x{:04X} after {}ms",
                        raw,
                        elapsed_ms
                    );
                } else {
                    log::debug!("LORA TX done: {} bytes in {}ms", payload.len(), tx_ms);
                }
                if self.device.clear_irq_status(IrqMask::all()).is_err() {
                    self.tx_irq_error = self.tx_irq_error.wrapping_add(1);
                }
                return Ok(tx_ms);
            }
            if irq.timeout() {
                if !tx_elapsed_after_minimum(estimated_airtime_ms, elapsed_ms)
                    && elapsed_ms <= soft_timeout_ms
                {
                    self.tx_irq_error = self.tx_irq_error.wrapping_add(1);
                    self.clock.delay_ms(TX_POLL_INTERVAL_MS as u32);
                    continue;
                }
                self.tx_timeout = self.tx_timeout.wrapping_add(1);
                log::warn!("LORA TX timeout (IRQ) after {}ms", elapsed_ms);
                return Err(MeshRadioError::TxTimeout);
            }
            if raw & (IRQ_TX_DONE_ONLY | IRQ_NON_TX_BITS) != 0 {
                self.tx_irq_error = self.tx_irq_error.wrapping_add(1);
                self.clock.delay_ms(TX_POLL_INTERVAL_MS as u32);
                if elapsed_ms > soft_timeout_ms {
                    self.tx_timeout = self.tx_timeout.wrapping_add(1);
                    log::warn!("LORA TX timeout (invalid tx irq raw=0x{:04X})", raw);
                    return Err(MeshRadioError::TxTimeout);
                }
                continue;
            }
            if elapsed_ms > soft_timeout_ms {
                self.tx_timeout = self.tx_timeout.wrapping_add(1);
                log::warn!("LORA TX timeout (soft) after {}ms", soft_timeout_ms);
                return Err(MeshRadioError::TxTimeout);
            }
            self.clock.delay_ms(TX_POLL_INTERVAL_MS as u32);
        }
    }

    /// Restore RX packet parameters and continuous receive mode after TX.
    async fn restore_rx(&mut self) -> Result<(), MeshRadioError>
    {
        self.device
            .set_standby(StandbyConfig::StbyRc)
            .map_err(|_| MeshRadioError::DeviceIo)?;
        wait_busy_timeout(
            &mut self.device,
            &mut self.clock,
            RADIO_SHORT_BUSY_TIMEOUT_MS,
        )
        .await?;

        self.device
            .set_packet_params(self.rx_packet_params)
            .map_err(|_| MeshRadioError::DeviceIo)?;

        self.apply_rx_sensitivity_workarounds()?;

        // SX1262 errata 15.4: fix IQ polarity after SetPacketParams
        self.fix_iq_polarity()?;

        self.device
            .clear_irq_status(IrqMask::all())
            .map_err(|_| MeshRadioError::DeviceIo)?;
        self.device
            .set_rx(RxTxTimeout::continuous_rx())
            .map_err(|_| MeshRadioError::DeviceIo)?;
        Ok(())
    }

    /// Transmit a MeshCore frame.
    pub async fn transmit(
        &mut self,
        frame: &MeshTxFrame,
    ) -> Result<MeshRadioTxStatus, MeshRadioError>
    {
        let payload = frame.payload_slice();
        self.transmit_payload(payload).await
    }

    async fn transmit_payload(
        &mut self,
        payload: &[u8],
    ) -> Result<MeshRadioTxStatus, MeshRadioError>
    {
        if payload.is_empty() || payload.len() > MAX_LORA_PAYLOAD_LEN {
            return Err(MeshRadioError::InvalidPayload);
        }

        let tx_power = with_radio_host(|host| (host.lora_tx_power_dbm)());

        // Signal LoRa TX active so WiFi upload defers.
        with_radio_host(|host| (host.set_lora_tx_active)(true));

        self.rf_switch.set_tx()?;
        self.clock.delay_ms(2);

        // 2. Put radio in Standby before configuring for TX
        self.device
            .set_standby(if self.config.use_tcxo {
                StandbyConfig::StbyXOSC
            } else {
                StandbyConfig::StbyRc
            })
            .map_err(|_| MeshRadioError::DeviceIo)?;
        wait_busy_timeout(
            &mut self.device,
            &mut self.clock,
            RADIO_LONG_BUSY_TIMEOUT_MS,
        )
        .await?;

        // Cache the configured dBm before the register write so RxStats
        // reflects what the PA was asked to emit on the last TX.
        self.last_applied_tx_power_dbm = Some(tx_power);
        let tx_params = TxParams::default()
            .set_power_dbm(tx_power)
            .set_ramp_time(self.config.tx_ramp_time);
        self.device
            .set_tx_params(tx_params)
            .map_err(|_| MeshRadioError::DeviceIo)?;

        let tx_result = self.transmit_inner(payload).await;

        let rx_switch_res = self.rf_switch.set_rx();

        with_radio_host(|host| (host.set_lora_tx_active)(false));

        let rx_restore_res = self.restore_rx().await;

        // Propagate RX restore error if any
        if rx_restore_res.is_err() {
            self.tx_restore_error = self.tx_restore_error.wrapping_add(1);
            log::warn!(
                "LORA RX restore failed after TX (total={})",
                self.tx_restore_error
            );
        }
        if rx_switch_res.is_err() {
            self.tx_restore_error = self.tx_restore_error.wrapping_add(1);
            log::warn!(
                "LORA RF switch restore failed after TX (total={})",
                self.tx_restore_error
            );
        }

        // Store airtime for budget tracking
        if let Ok(ms) = tx_result {
            self.last_tx_airtime_ms = ms;
        }

        // Now propagate TX error if any
        tx_result?;
        rx_switch_res?;
        rx_restore_res?;

        Ok(MeshRadioTxStatus::Sent)
    }
}

fn tx_elapsed_after_minimum(estimated_airtime_ms: u32, elapsed_ms: u64) -> bool
{
    elapsed_ms >= minimum_tx_elapsed_ms(estimated_airtime_ms)
}

fn minimum_tx_elapsed_ms(estimated_airtime_ms: u32) -> u64
{
    let estimated = u64::from(estimated_airtime_ms.max(1));
    estimated.saturating_mul(3) / 4
}

fn estimate_lora_airtime_ms(config: &MeshRadioConfig, payload_len: usize) -> u32
{
    let sf = config.spread_factor as u32;
    let bw_hz = lora_bandwidth_hz(config.bandwidth);
    let symbol_us = ((1_u64 << sf) * 1_000_000).div_ceil(u64::from(bw_hz));
    let low_data_rate_opt = symbol_us > 16_380;
    let de = u32::from(low_data_rate_opt);
    let cr = lora_coding_rate_index(config.coding_rate);
    let denominator = 4 * (sf.saturating_sub(2 * de)).max(1);
    let numerator = (8_i32 * payload_len as i32) - (4_i32 * sf as i32) + 28 + 16;
    let payload_symbols = if numerator <= 0 {
        8
    } else {
        8 + (numerator as u32).div_ceil(denominator) * (cr + 4)
    };
    let total_symbols_x100 = u64::from(config.preamble_len)
        .saturating_mul(100)
        .saturating_add(425)
        .saturating_add(u64::from(payload_symbols).saturating_mul(100));
    let airtime_us = total_symbols_x100.saturating_mul(symbol_us).div_ceil(100);
    airtime_us.div_ceil(1_000).min(u64::from(u32::MAX)) as u32
}

fn lora_bandwidth_hz(bandwidth: LoRaBandWidth) -> u32
{
    match bandwidth {
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
    }
}

fn lora_coding_rate_index(coding_rate: LoraCodingRate) -> u32
{
    match coding_rate {
        LoraCodingRate::CR4_5 => 1,
        LoraCodingRate::CR4_6 => 2,
        LoraCodingRate::CR4_7 => 3,
        LoraCodingRate::CR4_8 => 4,
    }
}

/// Blocks until the radio's BUSY line goes low or the timeout is reached.
async fn wait_busy_timeout<TSPI, TNRST, TBUSY, TANT, TDIO1, TSPIERR, TPINERR, TCLOCK>(
    device: &mut SX126x<TSPI, TNRST, TBUSY, TANT, TDIO1>,
    clock: &mut TCLOCK,
    timeout_ms: u32,
) -> Result<(), MeshRadioError>
where
    TPINERR: core::fmt::Debug,
    TSPI: SpiDevice<Error = TSPIERR>,
    TNRST: OutputPin<Error = TPINERR>,
    TBUSY: InputPin<Error = TPINERR>,
    TANT: OutputPin<Error = TPINERR>,
    TDIO1: InputPin<Error = TPINERR>,
    TCLOCK: RadioClock,
{
    let start_ms = clock.now_ms();
    while device.try_is_busy().map_err(|_| MeshRadioError::DeviceIo)? {
        if clock.now_ms().saturating_sub(start_ms) > timeout_ms as u64 {
            return Err(MeshRadioError::BusyTimeout);
        }
        clock.delay_ms(1);
    }
    Ok(())
}

/// Round a floating-point radio measurement to the nearest signed integer.
fn round_i16(value: f32) -> i16
{
    if value >= 0.0 {
        (value + 0.5) as i16
    } else {
        (value - 0.5) as i16
    }
}
