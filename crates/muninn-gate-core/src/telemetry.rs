//! Telemetry data model and fixed-capacity in-memory store.

use heapless::Vec;

use crate::config::{GatewayConfig, MAX_TELEMETRY_PRODUCERS};
use crate::{Error, TelemetryProducerConfig, TelemetryProducerId, TelemetryStore};

/// Maximum retained MeshCore/LPP telemetry channels per producer.
pub const MAX_TELEMETRY_CHANNELS: usize = 6;
/// MeshCore self-device LPP channel.
pub const MESHCORE_SELF_CHANNEL: u8 = 1;
/// Cayenne LPP analog input data type.
pub const LPP_ANALOG_INPUT: u8 = 2;
/// Cayenne LPP temperature data type.
pub const LPP_TEMPERATURE: u8 = 103;
/// Cayenne LPP relative humidity data type.
pub const LPP_RELATIVE_HUMIDITY: u8 = 104;
/// Cayenne LPP barometric pressure data type.
pub const LPP_BAROMETRIC_PRESSURE: u8 = 115;
/// Cayenne LPP voltage data type.
pub const LPP_VOLTAGE: u8 = 116;
/// Cayenne LPP current data type.
pub const LPP_CURRENT: u8 = 117;
/// Cayenne LPP percentage data type.
pub const LPP_PERCENTAGE: u8 = 120;
/// Cayenne LPP power data type.
pub const LPP_POWER: u8 = 128;

const LPP_DIGITAL_INPUT: u8 = 0;
const LPP_DIGITAL_OUTPUT: u8 = 1;
const LPP_ANALOG_OUTPUT: u8 = 3;
const LPP_GENERIC_SENSOR: u8 = 100;
const LPP_LUMINOSITY: u8 = 101;
const LPP_PRESENCE: u8 = 102;
const LPP_ACCELEROMETER: u8 = 113;
const LPP_FREQUENCY: u8 = 118;
const LPP_ALTITUDE: u8 = 121;
const LPP_CONCENTRATION: u8 = 125;
const LPP_DISTANCE: u8 = 130;
const LPP_ENERGY: u8 = 131;
const LPP_DIRECTION: u8 = 132;
const LPP_UNIXTIME: u8 = 133;
const LPP_GYROMETER: u8 = 134;
const LPP_COLOUR: u8 = 135;
const LPP_GPS: u8 = 136;
const LPP_SWITCH: u8 = 142;

/// Reusable sensor metrics reported by a producer or one producer channel.
///
/// This intentionally tracks only the MeshCore sensor fields Muninn Gate
/// currently bridges: MCU battery/temperature, SHT4x temperature/humidity,
/// BME680 temperature/humidity/pressure/gas, and INA3221 voltage/current/power.
/// Fields stay optional because a given channel reports only a small subset.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct TelemetryMetrics
{
    /// Battery voltage in volts.
    pub battery_voltage:     Option<f32>,
    /// Battery charge percentage.
    pub battery_percent:     Option<f32>,
    /// Generic voltage in volts when the value is not specifically battery or bus voltage.
    pub voltage:             Option<f32>,
    /// Current in amperes.
    pub current_amps:        Option<f32>,
    /// Power in watts.
    pub power_watts:         Option<f32>,
    /// Temperature in Celsius.
    pub temperature_celsius: Option<f32>,
    /// Relative humidity percentage.
    pub humidity_percent:    Option<f32>,
    /// Pressure in pascals.
    pub pressure_pa:         Option<f32>,
    /// Gas sensor resistance in ohms.
    pub gas_resistance_ohms: Option<f32>,
}

/// Last-known values for one MeshCore/LPP telemetry channel.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ProducerTelemetryChannel
{
    /// MeshCore/LPP channel identifier.
    pub channel_id: u8,
    /// Sensor metrics reported on this channel.
    pub metrics:    TelemetryMetrics,
}

impl ProducerTelemetryChannel
{
    /// Create an empty telemetry channel.
    pub const fn new(channel_id: u8) -> Self
    {
        Self {
            channel_id,
            metrics: TelemetryMetrics::new(),
        }
    }
}

impl TelemetryMetrics
{
    /// Create an empty metric set.
    pub const fn new() -> Self
    {
        Self {
            battery_voltage:     None,
            battery_percent:     None,
            voltage:             None,
            current_amps:        None,
            power_watts:         None,
            temperature_celsius: None,
            humidity_percent:    None,
            pressure_pa:         None,
            gas_resistance_ohms: None,
        }
    }
}

/// Last-known telemetry reported by one producer.
///
/// MeshCore telemetry is channel-oriented, so channel entries are the canonical
/// home for decoded sensor values. The producer-level sensor fields below are a
/// compact compatibility/default-display view populated from the first channel
/// that reports each metric type. They let simple serial, Prometheus, and OLED
/// consumers show one value per producer without choosing a channel. Consumers
/// that need precise multi-channel data should use [`Self::channels`] instead.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ProducerTelemetry
{
    /// Producer identifier associated with this telemetry.
    pub producer_id:  TelemetryProducerId,
    /// Timestamp in gateway monotonic milliseconds.
    pub timestamp_ms: u64,
    /// Default producer metric view, copied from the first reporting channel.
    pub metrics:      TelemetryMetrics,
    /// Last received RSSI in dBm.
    pub rssi:         Option<i16>,
    /// Last received SNR in dB.
    pub snr:          Option<f32>,
    /// Producer-reported uptime in milliseconds.
    pub uptime_ms:    Option<u64>,
    /// Channel-specific telemetry values keyed by MeshCore/LPP channel ID.
    pub channels:     [Option<ProducerTelemetryChannel>; MAX_TELEMETRY_CHANNELS],
}

impl ProducerTelemetry
{
    /// Create an empty telemetry record for a producer.
    pub const fn new(producer_id: TelemetryProducerId, timestamp_ms: u64) -> Self
    {
        Self {
            producer_id,
            timestamp_ms,
            metrics: TelemetryMetrics::new(),
            rssi: None,
            snr: None,
            uptime_ms: None,
            channels: [None; MAX_TELEMETRY_CHANNELS],
        }
    }

    /// Return retained channel telemetry in storage order.
    pub fn channels(&self) -> impl Iterator<Item = &ProducerTelemetryChannel>
    {
        self.channels.iter().flatten()
    }

    /// Return one retained channel by MeshCore/LPP channel ID.
    pub fn channel(&self, channel_id: u8) -> Option<&ProducerTelemetryChannel>
    {
        self.channels()
            .find(|channel| channel.channel_id == channel_id)
    }

    /// Update or create one channel.
    pub fn update_channel<F>(&mut self, channel_id: u8, update: F) -> Result<(), Error>
    where
        F: FnOnce(&mut ProducerTelemetryChannel),
    {
        if let Some(channel) = self
            .channels
            .iter_mut()
            .flatten()
            .find(|channel| channel.channel_id == channel_id)
        {
            update(channel);
            return Ok(());
        }

        let slot = self
            .channels
            .iter_mut()
            .find(|channel| channel.is_none())
            .ok_or(Error::Capacity)?;
        let mut channel = ProducerTelemetryChannel::new(channel_id);
        update(&mut channel);
        *slot = Some(channel);
        Ok(())
    }

    /// Store a channel battery voltage and update the producer default value.
    pub fn set_channel_battery_voltage(&mut self, channel_id: u8, value: f32) -> Result<(), Error>
    {
        if self.metrics.battery_voltage.is_none() {
            self.metrics.battery_voltage = Some(value);
        }
        self.update_channel(channel_id, |channel| {
            channel.metrics.battery_voltage = Some(value);
        })
    }

    /// Store a channel battery percentage and update the producer default value.
    pub fn set_channel_battery_percent(&mut self, channel_id: u8, value: f32) -> Result<(), Error>
    {
        if self.metrics.battery_percent.is_none() {
            self.metrics.battery_percent = Some(value);
        }
        self.update_channel(channel_id, |channel| {
            channel.metrics.battery_percent = Some(value);
        })
    }

    /// Store a channel voltage and update the producer default value.
    pub fn set_channel_voltage(&mut self, channel_id: u8, value: f32) -> Result<(), Error>
    {
        if self.metrics.voltage.is_none() {
            self.metrics.voltage = Some(value);
        }
        if channel_id == MESHCORE_SELF_CHANNEL && self.metrics.battery_voltage.is_none() {
            self.metrics.battery_voltage = Some(value);
        }
        self.update_channel(channel_id, |channel| {
            channel.metrics.voltage = Some(value);
            if channel_id == MESHCORE_SELF_CHANNEL {
                channel.metrics.battery_voltage = Some(value);
            }
        })
    }

    /// Store a channel current and update the producer default value.
    pub fn set_channel_current(&mut self, channel_id: u8, value: f32) -> Result<(), Error>
    {
        if self.metrics.current_amps.is_none() {
            self.metrics.current_amps = Some(value);
        }
        self.update_channel(channel_id, |channel| {
            channel.metrics.current_amps = Some(value);
        })
    }

    /// Store a channel power value and update the producer default value.
    pub fn set_channel_power(&mut self, channel_id: u8, value: f32) -> Result<(), Error>
    {
        if self.metrics.power_watts.is_none() {
            self.metrics.power_watts = Some(value);
        }
        self.update_channel(channel_id, |channel| {
            channel.metrics.power_watts = Some(value);
        })
    }

    /// Store a channel temperature and update the producer default value.
    pub fn set_channel_temperature(&mut self, channel_id: u8, value: f32) -> Result<(), Error>
    {
        if self.metrics.temperature_celsius.is_none() {
            self.metrics.temperature_celsius = Some(value);
        }
        self.update_channel(channel_id, |channel| {
            channel.metrics.temperature_celsius = Some(value);
        })
    }

    /// Store a channel humidity and update the producer default value.
    pub fn set_channel_humidity(&mut self, channel_id: u8, value: f32) -> Result<(), Error>
    {
        if self.metrics.humidity_percent.is_none() {
            self.metrics.humidity_percent = Some(value);
        }
        self.update_channel(channel_id, |channel| {
            channel.metrics.humidity_percent = Some(value);
        })
    }

    /// Store a channel pressure and update the producer default value.
    pub fn set_channel_pressure(&mut self, channel_id: u8, value: f32) -> Result<(), Error>
    {
        if self.metrics.pressure_pa.is_none() {
            self.metrics.pressure_pa = Some(value);
        }
        self.update_channel(channel_id, |channel| {
            channel.metrics.pressure_pa = Some(value);
        })
    }

    /// Store a channel gas resistance and update the producer default value.
    pub fn set_channel_gas_resistance(&mut self, channel_id: u8, value: f32) -> Result<(), Error>
    {
        if self.metrics.gas_resistance_ohms.is_none() {
            self.metrics.gas_resistance_ohms = Some(value);
        }
        self.update_channel(channel_id, |channel| {
            channel.metrics.gas_resistance_ohms = Some(value);
        })
    }
}

/// Decode supported Cayenne LPP values into producer telemetry.
pub fn decode_lpp_payload(telemetry: &mut ProducerTelemetry, payload: &[u8])
{
    let mut index = 0_usize;
    while index.saturating_add(2) <= payload.len() {
        let channel_id = payload[index];
        let data_type = payload[index + 1];
        index = index.saturating_add(2);

        match data_type {
            LPP_ANALOG_INPUT if index.saturating_add(2) <= payload.len() => {
                let value = i16::from_be_bytes([payload[index], payload[index + 1]]);
                let _ = telemetry.set_channel_gas_resistance(channel_id, value as f32 / 100.0);
                index = index.saturating_add(2);
            },
            LPP_TEMPERATURE if index.saturating_add(2) <= payload.len() => {
                let value = i16::from_be_bytes([payload[index], payload[index + 1]]);
                let _ = telemetry.set_channel_temperature(channel_id, value as f32 / 10.0);
                index = index.saturating_add(2);
            },
            LPP_RELATIVE_HUMIDITY if index < payload.len() => {
                let _ = telemetry.set_channel_humidity(channel_id, payload[index] as f32 / 2.0);
                index = index.saturating_add(1);
            },
            LPP_BAROMETRIC_PRESSURE if index.saturating_add(2) <= payload.len() => {
                let value = u16::from_be_bytes([payload[index], payload[index + 1]]);
                let _ = telemetry.set_channel_pressure(channel_id, value as f32 * 10.0);
                index = index.saturating_add(2);
            },
            LPP_VOLTAGE if index.saturating_add(2) <= payload.len() => {
                let value = u16::from_be_bytes([payload[index], payload[index + 1]]);
                let _ = telemetry.set_channel_voltage(channel_id, value as f32 / 100.0);
                index = index.saturating_add(2);
            },
            LPP_CURRENT if index.saturating_add(2) <= payload.len() => {
                let value = i16::from_be_bytes([payload[index], payload[index + 1]]);
                let _ = telemetry.set_channel_current(channel_id, value as f32 / 1000.0);
                index = index.saturating_add(2);
            },
            LPP_PERCENTAGE if index < payload.len() => {
                if channel_id == MESHCORE_SELF_CHANNEL {
                    let _ =
                        telemetry.set_channel_battery_percent(channel_id, payload[index] as f32);
                }
                index = index.saturating_add(1);
            },
            LPP_POWER if index.saturating_add(2) <= payload.len() => {
                let value = u16::from_be_bytes([payload[index], payload[index + 1]]);
                let _ = telemetry.set_channel_power(channel_id, value as f32);
                index = index.saturating_add(2);
            },
            _ => {
                let Some(length) = lpp_value_length(data_type) else {
                    break;
                };
                if index.saturating_add(length) > payload.len() {
                    break;
                }
                index = index.saturating_add(length);
            },
        }
    }
}

fn lpp_value_length(data_type: u8) -> Option<usize>
{
    match data_type {
        LPP_DIGITAL_INPUT | LPP_DIGITAL_OUTPUT | LPP_PRESENCE | LPP_PERCENTAGE | LPP_SWITCH => {
            Some(1)
        },
        LPP_ANALOG_INPUT
        | LPP_ANALOG_OUTPUT
        | LPP_LUMINOSITY
        | LPP_TEMPERATURE
        | LPP_CONCENTRATION
        | LPP_BAROMETRIC_PRESSURE
        | LPP_VOLTAGE
        | LPP_CURRENT
        | LPP_ALTITUDE
        | LPP_DIRECTION
        | LPP_POWER => Some(2),
        LPP_COLOUR => Some(3),
        LPP_GENERIC_SENSOR | LPP_FREQUENCY | LPP_DISTANCE | LPP_ENERGY | LPP_UNIXTIME => Some(4),
        LPP_ACCELEROMETER | LPP_GYROMETER => Some(6),
        LPP_GPS => Some(9),
        _ => None,
    }
}

/// Gateway-wide telemetry and health metrics.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct GatewayMetrics
{
    /// Gateway uptime in milliseconds.
    pub uptime_ms:                       u64,
    /// Successful scheduled producer polls since boot.
    pub poll_success_total:              u64,
    /// Failed scheduled producer polls since boot.
    pub poll_failure_total:              u64,
    /// Scheduled producer polls that were deferred for retry since boot.
    pub poll_retry_total:                u64,
    /// Operator-requested immediate poll passes since boot.
    pub poll_on_demand_total:            u64,
    /// Radio receive packets observed by the gateway since boot.
    pub radio_rx_total:                  u64,
    /// Radio transmit packets sent by the gateway since boot.
    pub radio_tx_total:                  u64,
    /// Radio packets rejected by CRC since boot.
    pub radio_rx_crc_error_total:        u64,
    /// Radio packets rejected by header error since boot.
    pub radio_rx_header_error_total:     u64,
    /// Radio RX timeout IRQ count since boot.
    pub radio_rx_timeout_total:          u64,
    /// Latest SX126x device error bitmask.
    pub radio_device_error_bits:         u16,
    /// RSSI from the latest decoded LoRa packet in dBm.
    pub radio_last_rssi_dbm:             Option<i16>,
    /// SNR from the latest decoded LoRa packet in tenths of a dB.
    pub radio_last_snr_tenth_db:         Option<i16>,
    /// Rolling idle-RX noise floor estimate in dBm.
    pub radio_noise_floor_dbm:           Option<i16>,
    /// Latest idle-RX instantaneous RSSI sample in dBm.
    pub radio_rssi_inst_dbm:             Option<i16>,
    /// Latest sampled SX126x chip mode nibble.
    pub radio_chip_mode:                 Option<u8>,
    /// TX power most recently applied to the radio in dBm.
    pub radio_last_applied_tx_power_dbm: Option<i8>,
    /// Airtime of the latest successful TX in milliseconds.
    pub radio_last_tx_airtime_ms:        Option<u32>,
    /// Monotonic timestamp of the most recent scheduled poll attempt.
    pub last_poll_ms:                    Option<u64>,
    /// Selected board/radio TX power level, after board mapping.
    pub tx_power_level:                  Option<i8>,
    /// Board-estimated conducted TX output in tenths of dBm.
    pub tx_output_dbm_tenths:            Option<i16>,
    /// Board-estimated conducted TX output in milliwatts.
    pub tx_output_milliwatts:            Option<u16>,
    /// Gateway battery voltage in millivolts, when the board can measure it.
    pub battery_voltage_mv:              Option<u16>,
    /// Gateway battery charge percentage, when the board can estimate it.
    pub battery_percent:                 Option<u8>,
    /// Latest completed scheduled poll latency in milliseconds.
    pub last_poll_latency_ms:            Option<u32>,
    /// Rolling average scheduled poll latency in milliseconds.
    pub avg_poll_latency_ms:             Option<u32>,
    /// Platform-reported free heap bytes, when available.
    pub free_heap_bytes:                 Option<u32>,
    /// Diagnostic events currently retained by the diagnostics collector.
    pub diagnostic_events:               u32,
    /// Diagnostic events dropped by the diagnostics collector.
    pub diagnostic_dropped:              u64,
    /// Retained diagnostic events with error severity.
    pub diagnostic_errors:               u32,
    /// Timestamp of the latest retained error diagnostic.
    pub last_error_ms:                   Option<u64>,
}

impl GatewayMetrics
{
    /// Return total scheduled poll attempts across all producers.
    pub const fn poll_attempt_total(&self) -> u64
    {
        self.poll_success_total
            .saturating_add(self.poll_failure_total)
    }

    /// Return since-boot successful poll ratio in thousandths.
    pub fn poll_success_rate_per_mille(&self) -> Option<u16>
    {
        success_rate_per_mille(self.poll_success_total, self.poll_failure_total)
    }
}

/// Per-producer polling counters.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ProducerPollMetrics
{
    /// Successful scheduled poll count for this producer since boot.
    pub poll_success_total: u64,
    /// Failed scheduled poll count for this producer since boot.
    pub poll_failure_total: u64,
    /// Monotonic timestamp of the latest scheduled poll attempt.
    pub last_poll_ms:       Option<u64>,
    /// Result of the latest scheduled poll attempt.
    pub last_poll_success:  Option<bool>,
    /// Concise reason for the latest failed poll attempt.
    pub last_poll_error:    Option<PollFailureReason>,
}

impl ProducerPollMetrics
{
    /// Return total scheduled poll attempts for this producer.
    pub const fn poll_attempt_total(&self) -> u64
    {
        self.poll_success_total
            .saturating_add(self.poll_failure_total)
    }

    /// Return since-boot successful poll ratio in thousandths.
    pub fn poll_success_rate_per_mille(&self) -> Option<u16>
    {
        success_rate_per_mille(self.poll_success_total, self.poll_failure_total)
    }
}

/// Concise producer poll failure reason for diagnostics and serial output.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PollFailureReason
{
    /// Gateway or producer key material is invalid.
    InvalidConfig,
    /// The producer rejected login/authentication.
    AuthFailed,
    /// The radio TX queue was full.
    TxQueueFull,
    /// No valid LoRa frame was received during the response window.
    NoRadioRxAfterTx,
    /// LoRa CRC errors occurred during the response window.
    RadioCrcErrors,
    /// LoRa header errors occurred during the response window.
    RadioHeaderErrors,
    /// LoRa frames arrived, but no MeshCore response frame was seen.
    MeshResponseNotSeen,
    /// A MeshCore response was addressed to another gateway.
    ResponseNotForGateway,
    /// A MeshCore response came from a different producer.
    ResponseWrongSource,
    /// A MeshCore response failed MAC authentication.
    ResponseMacFailed,
    /// A MeshCore response tag did not match the active request.
    ResponseTagMismatch,
    /// No matching response arrived before timeout.
    ResponseTimeout,
    /// A response arrived but could not be decoded or authenticated.
    DecodeFailed,
    /// Active polling is unsupported for this producer or route.
    Unsupported,
    /// The poll failed without a more specific platform reason.
    Failed,
}

impl PollFailureReason
{
    /// Return a stable serial/JSON label.
    pub const fn as_str(self) -> &'static str
    {
        match self {
            Self::InvalidConfig => "invalid_config",
            Self::AuthFailed => "auth_failed",
            Self::TxQueueFull => "tx_queue_full",
            Self::NoRadioRxAfterTx => "no_radio_rx_after_tx",
            Self::RadioCrcErrors => "radio_crc_errors",
            Self::RadioHeaderErrors => "radio_header_errors",
            Self::MeshResponseNotSeen => "mesh_response_not_seen",
            Self::ResponseNotForGateway => "response_not_for_gateway",
            Self::ResponseWrongSource => "response_wrong_source",
            Self::ResponseMacFailed => "response_mac_failed",
            Self::ResponseTagMismatch => "response_tag_mismatch",
            Self::ResponseTimeout => "response_timeout",
            Self::DecodeFailed => "decode_failed",
            Self::Unsupported => "unsupported",
            Self::Failed => "failed",
        }
    }
}

/// Telemetry and polling state for one configured producer.
#[derive(Debug, Clone, PartialEq)]
pub struct TelemetryRecord
{
    /// Producer configuration associated with this record.
    pub config:    TelemetryProducerConfig,
    /// Last-known producer telemetry.
    pub telemetry: Option<ProducerTelemetry>,
    /// Per-producer poll counters.
    pub poll:      ProducerPollMetrics,
}

impl TelemetryRecord
{
    /// Create an empty telemetry record from producer configuration.
    pub const fn new(config: TelemetryProducerConfig) -> Self
    {
        Self {
            config,
            telemetry: None,
            poll: ProducerPollMetrics {
                poll_success_total: 0,
                poll_failure_total: 0,
                last_poll_ms:       None,
                last_poll_success:  None,
                last_poll_error:    None,
            },
        }
    }
}

/// Snapshot of gateway and producer telemetry.
#[derive(Debug, Clone, PartialEq)]
pub struct TelemetrySnapshot<const N: usize = MAX_TELEMETRY_PRODUCERS>
{
    /// Gateway-wide internal metrics.
    pub gateway: GatewayMetrics,
    /// Per-producer telemetry records.
    pub records: Vec<TelemetryRecord, N>,
}

impl<const N: usize> TelemetrySnapshot<N>
{
    /// Create an empty telemetry snapshot.
    pub const fn new() -> Self
    {
        Self {
            gateway: GatewayMetrics {
                uptime_ms:                       0,
                poll_success_total:              0,
                poll_failure_total:              0,
                poll_retry_total:                0,
                poll_on_demand_total:            0,
                radio_rx_total:                  0,
                radio_tx_total:                  0,
                radio_rx_crc_error_total:        0,
                radio_rx_header_error_total:     0,
                radio_rx_timeout_total:          0,
                radio_device_error_bits:         0,
                radio_last_rssi_dbm:             None,
                radio_last_snr_tenth_db:         None,
                radio_noise_floor_dbm:           None,
                radio_rssi_inst_dbm:             None,
                radio_chip_mode:                 None,
                radio_last_applied_tx_power_dbm: None,
                radio_last_tx_airtime_ms:        None,
                last_poll_ms:                    None,
                tx_power_level:                  None,
                tx_output_dbm_tenths:            None,
                tx_output_milliwatts:            None,
                battery_voltage_mv:              None,
                battery_percent:                 None,
                last_poll_latency_ms:            None,
                avg_poll_latency_ms:             None,
                free_heap_bytes:                 None,
                diagnostic_events:               0,
                diagnostic_dropped:              0,
                diagnostic_errors:               0,
                last_error_ms:                   None,
            },
            records: Vec::new(),
        }
    }
}

impl<const N: usize> Default for TelemetrySnapshot<N>
{
    fn default() -> Self
    {
        Self::new()
    }
}

/// Fixed-capacity telemetry store suitable for embedded targets.
#[derive(Debug, Clone, PartialEq)]
pub struct FixedTelemetryStore<const N: usize = MAX_TELEMETRY_PRODUCERS>
{
    snapshot: TelemetrySnapshot<N>,
}

impl<const N: usize> FixedTelemetryStore<N>
{
    /// Create an empty telemetry store.
    pub const fn new() -> Self
    {
        Self {
            snapshot: TelemetrySnapshot::new(),
        }
    }

    /// Create a telemetry store from configured producers.
    pub fn from_config(config: &GatewayConfig<N>) -> Result<Self, Error>
    {
        let mut store = Self::new();
        for producer in config.producers.iter() {
            store.add_producer(producer.clone())?;
        }
        Ok(store)
    }

    /// Add one producer record to the store.
    pub fn add_producer(&mut self, config: TelemetryProducerConfig) -> Result<(), Error>
    {
        self.snapshot
            .records
            .push(TelemetryRecord::new(config))
            .map_err(|_| Error::Capacity)
    }

    /// Store a concise failure reason for one producer.
    pub fn set_poll_error(
        &mut self,
        producer_id: TelemetryProducerId,
        reason: PollFailureReason,
    ) -> Result<(), Error>
    {
        let record = self.record_mut(producer_id)?;
        record.poll.last_poll_error = Some(reason);
        Ok(())
    }

    fn record_mut(
        &mut self,
        producer_id: TelemetryProducerId,
    ) -> Result<&mut TelemetryRecord, Error>
    {
        self.snapshot
            .records
            .iter_mut()
            .find(|record| record.config.id() == producer_id)
            .ok_or(Error::ProducerNotFound)
    }
}

impl<const N: usize> Default for FixedTelemetryStore<N>
{
    fn default() -> Self
    {
        Self::new()
    }
}

impl<const N: usize> TelemetryStore<N> for FixedTelemetryStore<N>
{
    fn update_producer(
        &mut self,
        producer_id: TelemetryProducerId,
        telemetry: ProducerTelemetry,
    ) -> Result<(), Error>
    {
        let record = self.record_mut(producer_id)?;
        record.telemetry = Some(telemetry);
        Ok(())
    }

    fn record_poll_success(
        &mut self,
        producer_id: TelemetryProducerId,
        timestamp_ms: u64,
    ) -> Result<(), Error>
    {
        let record = self.record_mut(producer_id)?;
        record.poll.poll_success_total = record.poll.poll_success_total.saturating_add(1);
        record.poll.last_poll_ms = Some(timestamp_ms);
        record.poll.last_poll_success = Some(true);
        record.poll.last_poll_error = None;
        self.snapshot.gateway.poll_success_total =
            self.snapshot.gateway.poll_success_total.saturating_add(1);
        self.snapshot.gateway.last_poll_ms = Some(timestamp_ms);
        Ok(())
    }

    fn record_poll_failure(
        &mut self,
        producer_id: TelemetryProducerId,
        timestamp_ms: u64,
    ) -> Result<(), Error>
    {
        let record = self.record_mut(producer_id)?;
        record.poll.poll_failure_total = record.poll.poll_failure_total.saturating_add(1);
        record.poll.last_poll_ms = Some(timestamp_ms);
        record.poll.last_poll_success = Some(false);
        if record.poll.last_poll_error.is_none() {
            record.poll.last_poll_error = Some(PollFailureReason::Failed);
        }
        self.snapshot.gateway.poll_failure_total =
            self.snapshot.gateway.poll_failure_total.saturating_add(1);
        self.snapshot.gateway.last_poll_ms = Some(timestamp_ms);
        Ok(())
    }

    fn set_gateway_metrics(&mut self, metrics: GatewayMetrics)
    {
        self.snapshot.gateway = metrics;
    }

    fn gateway_metrics_mut(&mut self) -> &mut GatewayMetrics
    {
        &mut self.snapshot.gateway
    }

    fn snapshot(&self) -> TelemetrySnapshot<N>
    {
        self.snapshot.clone()
    }
}

fn success_rate_per_mille(success_total: u64, failure_total: u64) -> Option<u16>
{
    let attempt_total = success_total.saturating_add(failure_total);
    if attempt_total == 0 {
        return None;
    }

    Some(
        success_total
            .saturating_mul(1000)
            .saturating_div(attempt_total) as u16,
    )
}

#[cfg(test)]
mod tests
{
    use super::*;

    #[test]
    fn decodes_six_environment_and_power_channels()
    {
        let mut payload = std::vec::Vec::new();
        push_i16(&mut payload, 1, LPP_TEMPERATURE, 234);
        push_u16(&mut payload, 1, LPP_VOLTAGE, 408);
        push_u8(&mut payload, 1, LPP_PERCENTAGE, 87);
        push_i16(&mut payload, 2, LPP_TEMPERATURE, 211);
        push_u8(&mut payload, 2, LPP_RELATIVE_HUMIDITY, 97);
        push_i16(&mut payload, 3, LPP_TEMPERATURE, 222);
        push_u8(&mut payload, 3, LPP_RELATIVE_HUMIDITY, 98);
        push_u16(&mut payload, 3, LPP_BAROMETRIC_PRESSURE, 10_133);
        push_i16(&mut payload, 3, LPP_ALTITUDE, 157);
        push_i16(&mut payload, 3, LPP_ANALOG_INPUT, 12_345);
        push_u16(&mut payload, 4, LPP_VOLTAGE, 1_201);
        push_i16(&mut payload, 4, LPP_CURRENT, -1_234);
        push_u16(&mut payload, 4, LPP_POWER, 15);
        push_u16(&mut payload, 5, LPP_VOLTAGE, 502);
        push_i16(&mut payload, 5, LPP_CURRENT, 456);
        push_u16(&mut payload, 5, LPP_POWER, 2);
        push_u16(&mut payload, 6, LPP_VOLTAGE, 330);
        push_i16(&mut payload, 6, LPP_CURRENT, 123);
        push_u16(&mut payload, 6, LPP_POWER, 1);

        let producer_id = TelemetryProducerId::from_public_key("producer-public-key");
        let mut telemetry = ProducerTelemetry::new(producer_id, 42);
        decode_lpp_payload(&mut telemetry, &payload);

        assert_eq!(telemetry.channels().count(), 6);
        assert_f32_eq(telemetry.metrics.battery_voltage, 4.08);
        assert_f32_eq(telemetry.metrics.battery_percent, 87.0);
        assert_f32_eq(telemetry.metrics.voltage, 4.08);
        assert_f32_eq(telemetry.metrics.current_amps, -1.234);
        assert_f32_eq(telemetry.metrics.power_watts, 15.0);
        assert_f32_eq(telemetry.metrics.gas_resistance_ohms, 123.45);

        let mcu = telemetry.channel(1).unwrap().metrics;
        assert_f32_eq(mcu.temperature_celsius, 23.4);
        assert_f32_eq(mcu.battery_voltage, 4.08);
        assert_f32_eq(mcu.battery_percent, 87.0);

        let sht45 = telemetry.channel(2).unwrap().metrics;
        assert_f32_eq(sht45.temperature_celsius, 21.1);
        assert_f32_eq(sht45.humidity_percent, 48.5);

        let bme680 = telemetry.channel(3).unwrap().metrics;
        assert_f32_eq(bme680.temperature_celsius, 22.2);
        assert_f32_eq(bme680.humidity_percent, 49.0);
        assert_f32_eq(bme680.pressure_pa, 101_330.0);
        assert_f32_eq(bme680.gas_resistance_ohms, 123.45);

        let ina3221_channel_1 = telemetry.channel(4).unwrap().metrics;
        assert_f32_eq(ina3221_channel_1.voltage, 12.01);
        assert_f32_eq(ina3221_channel_1.current_amps, -1.234);
        assert_f32_eq(ina3221_channel_1.power_watts, 15.0);
    }

    fn push_u8(payload: &mut std::vec::Vec<u8>, channel: u8, data_type: u8, value: u8)
    {
        payload.extend_from_slice(&[channel, data_type, value]);
    }

    fn push_i16(payload: &mut std::vec::Vec<u8>, channel: u8, data_type: u8, value: i16)
    {
        payload.push(channel);
        payload.push(data_type);
        payload.extend_from_slice(&value.to_be_bytes());
    }

    fn push_u16(payload: &mut std::vec::Vec<u8>, channel: u8, data_type: u8, value: u16)
    {
        payload.push(channel);
        payload.push(data_type);
        payload.extend_from_slice(&value.to_be_bytes());
    }

    fn assert_f32_eq(actual: Option<f32>, expected: f32)
    {
        let actual = actual.unwrap();
        assert!(
            (actual - expected).abs() < 0.001,
            "expected {expected}, got {actual}"
        );
    }
}
