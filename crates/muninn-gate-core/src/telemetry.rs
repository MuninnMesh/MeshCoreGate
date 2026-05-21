//! Telemetry data model and fixed-capacity in-memory store.

use heapless::Vec;

use crate::config::{GatewayConfig, MAX_TELEMETRY_PRODUCERS};
use crate::{Error, TelemetryProducerConfig, TelemetryProducerId, TelemetryStore};

/// Maximum retained MeshCore/LPP telemetry channels per producer.
pub const MAX_TELEMETRY_CHANNELS: usize = 1;
/// Cayenne LPP temperature data type.
pub const LPP_TEMPERATURE: u8 = 103;
/// Cayenne LPP relative humidity data type.
pub const LPP_RELATIVE_HUMIDITY: u8 = 104;
/// Cayenne LPP barometric pressure data type.
pub const LPP_BAROMETRIC_PRESSURE: u8 = 115;
/// Cayenne LPP voltage data type.
pub const LPP_VOLTAGE: u8 = 116;
/// Cayenne LPP GPS data type.
pub const LPP_GPS: u8 = 136;

/// Reusable sensor metrics reported by a producer or one producer channel.
///
/// This is a practical superset for the sensor families Muninn Gate expects to
/// bridge: BME680/BME688, BMP280, SHT3x/SHT4x, GPS receivers, common IMUs, and
/// INA219/INA228/INA3221 power monitors. Fields stay optional because a given
/// producer usually reports only a small subset.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct TelemetryMetrics
{
    /// Battery voltage in volts.
    pub battery_voltage:        Option<f32>,
    /// Battery charge percentage.
    pub battery_percent:        Option<f32>,
    /// Generic voltage in volts when the value is not specifically battery or bus voltage.
    pub voltage:                Option<f32>,
    /// Power-monitor bus voltage in volts.
    pub bus_voltage:            Option<f32>,
    /// Power-monitor shunt voltage in volts.
    pub shunt_voltage:          Option<f32>,
    /// Current in amperes.
    pub current_amps:           Option<f32>,
    /// Power in watts.
    pub power_watts:            Option<f32>,
    /// Accumulated energy in joules.
    pub energy_joules:          Option<f32>,
    /// Accumulated charge in coulombs.
    pub charge_coulombs:        Option<f32>,
    /// Temperature in Celsius.
    pub temperature_celsius:    Option<f32>,
    /// Relative humidity percentage.
    pub humidity_percent:       Option<f32>,
    /// Pressure in pascals.
    pub pressure_pa:            Option<f32>,
    /// Gas sensor resistance in ohms.
    pub gas_resistance_ohms:    Option<f32>,
    /// Air-quality index value from a producer-side algorithm.
    pub iaq_index:              Option<f32>,
    /// Equivalent CO2 in parts per million from a producer-side algorithm.
    pub co2_equivalent_ppm:     Option<f32>,
    /// Total volatile organic compounds in parts per billion.
    pub tvoc_ppb:               Option<f32>,
    /// Latitude in decimal degrees.
    pub latitude_degrees:       Option<f32>,
    /// Longitude in decimal degrees.
    pub longitude_degrees:      Option<f32>,
    /// Altitude in meters.
    pub altitude_meters:        Option<f32>,
    /// Ground speed in meters per second.
    pub speed_mps:              Option<f32>,
    /// Heading or course over ground in degrees.
    pub heading_degrees:        Option<f32>,
    /// Horizontal dilution of precision.
    pub hdop:                   Option<f32>,
    /// Number of satellites used or visible for the reported fix.
    pub satellites:             Option<u8>,
    /// Producer-defined GPS fix quality/type.
    pub gps_fix:                Option<u8>,
    /// Acceleration on the X axis in meters per second squared.
    pub acceleration_x_mps2:    Option<f32>,
    /// Acceleration on the Y axis in meters per second squared.
    pub acceleration_y_mps2:    Option<f32>,
    /// Acceleration on the Z axis in meters per second squared.
    pub acceleration_z_mps2:    Option<f32>,
    /// Angular velocity on the X axis in degrees per second.
    pub angular_velocity_x_dps: Option<f32>,
    /// Angular velocity on the Y axis in degrees per second.
    pub angular_velocity_y_dps: Option<f32>,
    /// Angular velocity on the Z axis in degrees per second.
    pub angular_velocity_z_dps: Option<f32>,
    /// Magnetic field on the X axis in microtesla.
    pub magnetic_field_x_ut:    Option<f32>,
    /// Magnetic field on the Y axis in microtesla.
    pub magnetic_field_y_ut:    Option<f32>,
    /// Magnetic field on the Z axis in microtesla.
    pub magnetic_field_z_ut:    Option<f32>,
    /// Roll angle in degrees.
    pub roll_degrees:           Option<f32>,
    /// Pitch angle in degrees.
    pub pitch_degrees:          Option<f32>,
    /// Yaw angle in degrees.
    pub yaw_degrees:            Option<f32>,
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
            battery_voltage:        None,
            battery_percent:        None,
            voltage:                None,
            bus_voltage:            None,
            shunt_voltage:          None,
            current_amps:           None,
            power_watts:            None,
            energy_joules:          None,
            charge_coulombs:        None,
            temperature_celsius:    None,
            humidity_percent:       None,
            pressure_pa:            None,
            gas_resistance_ohms:    None,
            iaq_index:              None,
            co2_equivalent_ppm:     None,
            tvoc_ppb:               None,
            latitude_degrees:       None,
            longitude_degrees:      None,
            altitude_meters:        None,
            speed_mps:              None,
            heading_degrees:        None,
            hdop:                   None,
            satellites:             None,
            gps_fix:                None,
            acceleration_x_mps2:    None,
            acceleration_y_mps2:    None,
            acceleration_z_mps2:    None,
            angular_velocity_x_dps: None,
            angular_velocity_y_dps: None,
            angular_velocity_z_dps: None,
            magnetic_field_x_ut:    None,
            magnetic_field_y_ut:    None,
            magnetic_field_z_ut:    None,
            roll_degrees:           None,
            pitch_degrees:          None,
            yaw_degrees:            None,
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

    /// Store channel GPS position and update the producer default value.
    pub fn set_channel_gps(
        &mut self,
        channel_id: u8,
        latitude_degrees: f32,
        longitude_degrees: f32,
        altitude_meters: f32,
    ) -> Result<(), Error>
    {
        if self.metrics.latitude_degrees.is_none() {
            self.metrics.latitude_degrees = Some(latitude_degrees);
            self.metrics.longitude_degrees = Some(longitude_degrees);
            self.metrics.altitude_meters = Some(altitude_meters);
        }
        self.update_channel(channel_id, |channel| {
            channel.metrics.latitude_degrees = Some(latitude_degrees);
            channel.metrics.longitude_degrees = Some(longitude_degrees);
            channel.metrics.altitude_meters = Some(altitude_meters);
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
                let _ = telemetry.set_channel_battery_voltage(channel_id, value as f32 / 100.0);
                index = index.saturating_add(2);
            },
            LPP_GPS if index.saturating_add(9) <= payload.len() => {
                let latitude = signed_24([payload[index], payload[index + 1], payload[index + 2]])
                    as f32
                    / 10_000.0;
                let longitude =
                    signed_24([payload[index + 3], payload[index + 4], payload[index + 5]]) as f32
                        / 10_000.0;
                let altitude =
                    signed_24([payload[index + 6], payload[index + 7], payload[index + 8]]) as f32
                        / 100.0;
                let _ = telemetry.set_channel_gps(channel_id, latitude, longitude, altitude);
                index = index.saturating_add(9);
            },
            _ => break,
        }
    }
}

fn signed_24(bytes: [u8; 3]) -> i32
{
    let value = i32::from_be_bytes([0, bytes[0], bytes[1], bytes[2]]);
    if value & 0x0080_0000 == 0 {
        value
    } else {
        value | !0x00ff_ffff
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
