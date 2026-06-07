//! Telemetry data model and fixed-capacity in-memory store.

use heapless::Vec;

use crate::config::{GatewayConfig, MAX_TELEMETRY_PRODUCERS};
use crate::{Error, TelemetryProducerConfig, TelemetryProducerId, TelemetryStore};

/// Maximum retained MeshCore/LPP telemetry channels per producer.
pub const MAX_TELEMETRY_CHANNELS: usize = 8;
/// MeshCore self-device LPP channel.
pub const MESHCORE_SELF_CHANNEL: u8 = 1;
/// Cayenne LPP analog input data type.
pub const LPP_ANALOG_INPUT: u8 = 2;
/// Cayenne LPP luminosity data type.
pub const LPP_LUMINOSITY: u8 = 101;
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
/// currently bridges: MCU battery/temperature, TSL2591 luminosity, SHT4x
/// temperature/humidity, BME680 temperature/humidity/pressure/gas, and INA3221
/// voltage/current/power.
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
    /// Luminosity in lux.
    pub luminosity_lux:      Option<f32>,
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
            luminosity_lux:      None,
        }
    }

    /// Overlay populated values from another metric set.
    pub fn merge_from(&mut self, other: Self)
    {
        if other.battery_voltage.is_some() {
            self.battery_voltage = other.battery_voltage;
        }
        if other.battery_percent.is_some() {
            self.battery_percent = other.battery_percent;
        }
        if other.voltage.is_some() {
            self.voltage = other.voltage;
        }
        if other.current_amps.is_some() {
            self.current_amps = other.current_amps;
        }
        if other.power_watts.is_some() {
            self.power_watts = other.power_watts;
        }
        if other.temperature_celsius.is_some() {
            self.temperature_celsius = other.temperature_celsius;
        }
        if other.humidity_percent.is_some() {
            self.humidity_percent = other.humidity_percent;
        }
        if other.pressure_pa.is_some() {
            self.pressure_pa = other.pressure_pa;
        }
        if other.gas_resistance_ohms.is_some() {
            self.gas_resistance_ohms = other.gas_resistance_ohms;
        }
        if other.luminosity_lux.is_some() {
            self.luminosity_lux = other.luminosity_lux;
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
/// Repeater self-reported stats from a MeshCore GET_STATUS response.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RepeaterStatus
{
    /// Radio noise floor in dBm.
    pub noise_floor_dbm: i16,
    /// Producer uptime in seconds.
    pub uptime_secs:     u32,
    /// Total LoRa packets sent.
    pub packets_sent:    u32,
    /// Total LoRa packets received.
    pub packets_recv:    u32,
    /// Total LoRa receive errors.
    pub recv_errors:     u32,
    /// Cumulative transmit airtime in seconds.
    pub tx_airtime_secs: u32,
    /// Cumulative receive airtime in seconds.
    pub rx_airtime_secs: u32,
}

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
    /// Repeater stats from the most recent GET_STATUS poll (separate cadence).
    pub status:       Option<RepeaterStatus>,
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
            status: None,
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

    /// Overlay populated values from another telemetry record for this producer.
    pub fn merge_from(&mut self, other: Self)
    {
        self.timestamp_ms = other.timestamp_ms;
        self.metrics.merge_from(other.metrics);
        if other.rssi.is_some() {
            self.rssi = other.rssi;
        }
        if other.snr.is_some() {
            self.snr = other.snr;
        }
        if other.uptime_ms.is_some() {
            self.uptime_ms = other.uptime_ms;
        }
        if other.status.is_some() {
            self.status = other.status;
        }
        for other_channel in other.channels() {
            let _ = self.update_channel(other_channel.channel_id, |channel| {
                channel.metrics.merge_from(other_channel.metrics);
            });
        }
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

    /// Store a channel luminosity value and update the producer default value.
    pub fn set_channel_luminosity(&mut self, channel_id: u8, value: f32) -> Result<(), Error>
    {
        if self.metrics.luminosity_lux.is_none() {
            self.metrics.luminosity_lux = Some(value);
        }
        self.update_channel(channel_id, |channel| {
            channel.metrics.luminosity_lux = Some(value);
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
            LPP_LUMINOSITY if index.saturating_add(2) <= payload.len() => {
                let value = u16::from_be_bytes([payload[index], payload[index + 1]]);
                let _ = telemetry.set_channel_luminosity(channel_id, value as f32);
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

/// Current HTTP TCP socket-state census.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct HttpSocketStateSnapshot
{
    /// Current active HTTP TCP sockets.
    pub active:                 u8,
    /// Current listening HTTP TCP sockets.
    pub listening:              u8,
    /// Current closed HTTP TCP sockets.
    pub closed:                 u8,
    /// Current HTTP TCP sockets in SYN-SENT.
    pub syn_sent:               u8,
    /// Current HTTP TCP sockets in SYN-RECEIVED.
    pub syn_received:           u8,
    /// Current established HTTP TCP sockets.
    pub established:            u8,
    /// Current HTTP TCP sockets in FIN-WAIT-1.
    pub fin_wait_1:             u8,
    /// Current HTTP TCP sockets in FIN-WAIT-2.
    pub fin_wait_2:             u8,
    /// Current HTTP TCP sockets in CLOSE-WAIT.
    pub close_wait:             u8,
    /// Current HTTP TCP sockets in CLOSING.
    pub closing:                u8,
    /// Current HTTP TCP sockets in LAST-ACK.
    pub last_ack:               u8,
    /// Current HTTP TCP sockets in TIME-WAIT.
    pub time_wait:              u8,
    /// Oldest non-listening/non-closed socket age in milliseconds.
    pub oldest_socket_age_ms:   Option<u32>,
    /// Oldest SYN-SENT socket age in milliseconds.
    pub oldest_syn_sent_ms:     Option<u32>,
    /// Oldest SYN-RECEIVED socket age in milliseconds.
    pub oldest_syn_received_ms: Option<u32>,
    /// Oldest established socket age in milliseconds.
    pub oldest_established_ms:  Option<u32>,
    /// Oldest FIN-WAIT-1 socket age in milliseconds.
    pub oldest_fin_wait_1_ms:   Option<u32>,
    /// Oldest FIN-WAIT-2 socket age in milliseconds.
    pub oldest_fin_wait_2_ms:   Option<u32>,
    /// Oldest CLOSE-WAIT socket age in milliseconds.
    pub oldest_close_wait_ms:   Option<u32>,
    /// Oldest CLOSING socket age in milliseconds.
    pub oldest_closing_ms:      Option<u32>,
    /// Oldest LAST-ACK socket age in milliseconds.
    pub oldest_last_ack_ms:     Option<u32>,
    /// Oldest TIME-WAIT socket age in milliseconds.
    pub oldest_time_wait_ms:    Option<u32>,
}

/// Gateway-wide telemetry and health metrics.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct GatewayMetrics
{
    /// Gateway uptime in milliseconds.
    pub uptime_ms:                           u64,
    /// Successful scheduled producer polls since boot.
    pub poll_success_total:                  u64,
    /// Failed scheduled producer polls since boot.
    pub poll_failure_total:                  u64,
    /// Scheduled producer polls that were deferred for retry since boot.
    pub poll_retry_total:                    u64,
    /// Operator-requested immediate poll passes since boot.
    pub poll_on_demand_total:                u64,
    /// Radio receive packets observed by the gateway since boot.
    pub radio_rx_total:                      u64,
    /// Radio transmit packets sent by the gateway since boot.
    pub radio_tx_total:                      u64,
    /// Radio packets rejected by CRC since boot.
    pub radio_rx_crc_error_total:            u64,
    /// Radio packets rejected by header error since boot.
    pub radio_rx_header_error_total:         u64,
    /// Radio RX timeout IRQ count since boot.
    pub radio_rx_timeout_total:              u64,
    /// Latest SX126x device error bitmask.
    pub radio_device_error_bits:             u16,
    /// RSSI from the latest decoded LoRa packet in dBm.
    pub radio_last_rssi_dbm:                 Option<i16>,
    /// SNR from the latest decoded LoRa packet in tenths of a dB.
    pub radio_last_snr_tenth_db:             Option<i16>,
    /// Rolling idle-RX noise floor estimate in dBm.
    pub radio_noise_floor_dbm:               Option<i16>,
    /// Latest idle-RX instantaneous RSSI sample in dBm.
    pub radio_rssi_inst_dbm:                 Option<i16>,
    /// Latest sampled SX126x chip mode nibble.
    pub radio_chip_mode:                     Option<u8>,
    /// TX power most recently applied to the radio in dBm.
    pub radio_last_applied_tx_power_dbm:     Option<i8>,
    /// Airtime of the latest successful TX in milliseconds.
    pub radio_last_tx_airtime_ms:            Option<u32>,
    /// Monotonic timestamp of the most recent scheduled poll attempt.
    pub last_poll_ms:                        Option<u64>,
    /// Selected board/radio TX power level, after board mapping.
    pub tx_power_level:                      Option<i8>,
    /// Board-estimated conducted TX output in tenths of dBm.
    pub tx_output_dbm_tenths:                Option<i16>,
    /// Board-estimated conducted TX output in milliwatts.
    pub tx_output_milliwatts:                Option<u16>,
    /// Gateway battery voltage in millivolts, when the board can measure it.
    pub battery_voltage_mv:                  Option<u16>,
    /// Gateway battery charge percentage, when the board can estimate it.
    pub battery_percent:                     Option<u8>,
    /// Latest completed scheduled poll latency in milliseconds.
    pub last_poll_latency_ms:                Option<u32>,
    /// Rolling average scheduled poll latency in milliseconds.
    pub avg_poll_latency_ms:                 Option<u32>,
    /// Whether the WiFi station is currently associated.
    pub wifi_connected:                      bool,
    /// Monotonic timestamp of the latest WiFi connect request.
    pub wifi_connect_started_ms:             Option<u64>,
    /// Monotonic timestamp of the latest successful WiFi association.
    pub wifi_connected_ms:                   Option<u64>,
    /// WiFi disconnect events observed since boot.
    pub wifi_disconnect_total:               u64,
    /// Latest WiFi disconnect reason code.
    pub wifi_last_disconnect_reason:         Option<u8>,
    /// WiFi connect requests issued by firmware since boot.
    pub wifi_connect_request_total:          u64,
    /// WiFi connect requests that returned an immediate error.
    pub wifi_connect_request_error_total:    u64,
    /// WiFi controller stop/start recoveries since boot.
    pub wifi_deep_recovery_total:            u64,
    /// Latest associated AP RSSI in dBm.
    pub wifi_rssi_dbm:                       Option<i16>,
    /// Latest associated AP primary channel.
    pub wifi_channel:                        Option<u8>,
    /// Latest associated AP BSSID.
    pub wifi_bssid:                          Option<[u8; 6]>,
    /// Latest associated AP auth mode as reported by ESP-IDF.
    pub wifi_auth_mode:                      Option<u8>,
    /// Requested ESP WiFi TX power cap in quarter-dBm units.
    pub wifi_tx_power_requested_quarter_dbm: Option<i8>,
    /// Applied ESP WiFi TX power cap in quarter-dBm units.
    pub wifi_tx_power_applied_quarter_dbm:   Option<i8>,
    /// Whether DHCP currently has a configured IPv4 lease.
    pub dhcp_configured:                     bool,
    /// Monotonic timestamp of the latest DHCP acquisition start.
    pub dhcp_started_ms:                     Option<u64>,
    /// Monotonic timestamp of the latest DHCP configured event.
    pub dhcp_configured_ms:                  Option<u64>,
    /// DHCP configured events since boot.
    pub dhcp_configured_total:               u64,
    /// DHCP deconfigured events since boot.
    pub dhcp_deconfigured_total:             u64,
    /// DHCP reset calls since boot.
    pub dhcp_reset_total:                    u64,
    /// DHCP timeouts that forced WiFi reconnect since boot.
    pub dhcp_timeout_total:                  u64,
    /// Latest DHCP acquisition duration in milliseconds.
    pub dhcp_last_acquire_ms:                Option<u32>,
    /// Current DHCP IPv4 address.
    pub dhcp_ip:                             Option<[u8; 4]>,
    /// Current DHCP default gateway.
    pub dhcp_gateway:                        Option<[u8; 4]>,
    /// Monotonic timestamp of the current network startup attempt.
    pub network_started_ms:                  Option<u64>,
    /// Monotonic timestamp when HTTP last began serving.
    pub http_serving_started_ms:             Option<u64>,
    /// Latest network startup-to-serving duration in milliseconds.
    pub network_startup_to_serving_ms:       Option<u32>,
    /// smoltcp interface polls since boot.
    pub smoltcp_poll_total:                  u64,
    /// Monotonic timestamp of the latest smoltcp poll.
    pub last_smoltcp_poll_ms:                Option<u64>,
    /// Longest observed gap between smoltcp interface polls.
    pub smoltcp_poll_gap_max_ms:             u32,
    /// smoltcp poll gaps that exceeded the diagnostic deadline.
    pub smoltcp_poll_delay_miss_total:       u64,
    /// smoltcp poll gaps that exceeded the bad-gap threshold.
    pub smoltcp_poll_bad_gap_total:          u64,
    /// HTTP requests accepted by the embedded server since boot.
    pub http_requests_total:                 u64,
    /// HTTP responses completed successfully since boot.
    pub http_success_total:                  u64,
    /// HTTP responses aborted while sending since boot.
    pub http_send_error_total:               u64,
    /// TCP sockets explicitly aborted by the HTTP server since boot.
    pub http_socket_abort_total:             u64,
    /// Current active HTTP TCP sockets.
    pub http_active_sockets:                 u8,
    /// Current listening HTTP TCP sockets.
    pub http_listening_sockets:              u8,
    /// Current closed HTTP TCP sockets.
    pub http_closed_sockets:                 u8,
    /// Current HTTP TCP sockets in SYN-SENT.
    pub http_syn_sent_sockets:               u8,
    /// Current HTTP TCP sockets in SYN-RECEIVED.
    pub http_syn_received_sockets:           u8,
    /// Current established HTTP TCP sockets.
    pub http_established_sockets:            u8,
    /// Current HTTP TCP sockets in FIN-WAIT-1.
    pub http_fin_wait_1_sockets:             u8,
    /// Current HTTP TCP sockets in FIN-WAIT-2.
    pub http_fin_wait_2_sockets:             u8,
    /// Current HTTP TCP sockets in CLOSE-WAIT.
    pub http_close_wait_sockets:             u8,
    /// Current HTTP TCP sockets in CLOSING.
    pub http_closing_sockets:                u8,
    /// Current HTTP TCP sockets in LAST-ACK.
    pub http_last_ack_sockets:               u8,
    /// Current HTTP TCP sockets in TIME-WAIT.
    pub http_time_wait_sockets:              u8,
    /// Oldest non-listening/non-closed HTTP socket age in milliseconds.
    pub http_oldest_socket_age_ms:           Option<u32>,
    /// Oldest SYN-SENT HTTP socket age in milliseconds.
    pub http_oldest_syn_sent_ms:             Option<u32>,
    /// Oldest SYN-RECEIVED HTTP socket age in milliseconds.
    pub http_oldest_syn_received_ms:         Option<u32>,
    /// Oldest established HTTP socket age in milliseconds.
    pub http_oldest_established_ms:          Option<u32>,
    /// Oldest FIN-WAIT-1 HTTP socket age in milliseconds.
    pub http_oldest_fin_wait_1_ms:           Option<u32>,
    /// Oldest FIN-WAIT-2 HTTP socket age in milliseconds.
    pub http_oldest_fin_wait_2_ms:           Option<u32>,
    /// Oldest CLOSE-WAIT HTTP socket age in milliseconds.
    pub http_oldest_close_wait_ms:           Option<u32>,
    /// Oldest CLOSING HTTP socket age in milliseconds.
    pub http_oldest_closing_ms:              Option<u32>,
    /// Oldest LAST-ACK HTTP socket age in milliseconds.
    pub http_oldest_last_ack_ms:             Option<u32>,
    /// Oldest TIME-WAIT HTTP socket age in milliseconds.
    pub http_oldest_time_wait_ms:            Option<u32>,
    /// Monotonic timestamp of the latest accepted HTTP request.
    pub last_http_request_ms:                Option<u64>,
    /// Monotonic timestamp of the latest successful HTTP response.
    pub last_http_success_ms:                Option<u64>,
    /// Network recovery actions forced by the firmware since boot.
    pub network_recovery_total:              u64,
    /// Latest observed network main-loop gap in milliseconds.
    pub main_loop_gap_last_ms:               u32,
    /// Longest observed network main-loop gap in milliseconds.
    pub main_loop_gap_max_ms:                u32,
    /// Latest observed scheduler tick duration in milliseconds.
    pub scheduler_tick_last_ms:              u32,
    /// Longest observed scheduler tick duration in milliseconds.
    pub scheduler_tick_max_ms:               u32,
    /// Latest cooperative LoRa service duration in milliseconds.
    pub lora_service_last_ms:                u32,
    /// Longest cooperative LoRa service duration in milliseconds.
    pub lora_service_max_ms:                 u32,
    /// Latest HTTP service pass duration in milliseconds.
    pub http_service_last_ms:                u32,
    /// Longest HTTP service pass duration in milliseconds.
    pub http_service_max_ms:                 u32,
    /// Latest display refresh duration in milliseconds.
    pub display_refresh_last_ms:             u32,
    /// Longest display refresh duration in milliseconds.
    pub display_refresh_max_ms:              u32,
    /// Latest serial emit duration in milliseconds.
    pub serial_emit_last_ms:                 u32,
    /// Longest serial emit duration in milliseconds.
    pub serial_emit_max_ms:                  u32,
    /// Platform-reported free heap bytes, when available.
    pub free_heap_bytes:                     Option<u32>,
    /// Diagnostic events currently retained by the diagnostics collector.
    pub diagnostic_events:                   u32,
    /// Diagnostic events dropped by the diagnostics collector.
    pub diagnostic_dropped:                  u64,
    /// Retained diagnostic events with error severity.
    pub diagnostic_errors:                   u32,
    /// Timestamp of the latest retained error diagnostic.
    pub last_error_ms:                       Option<u64>,
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
                uptime_ms:                           0,
                poll_success_total:                  0,
                poll_failure_total:                  0,
                poll_retry_total:                    0,
                poll_on_demand_total:                0,
                radio_rx_total:                      0,
                radio_tx_total:                      0,
                radio_rx_crc_error_total:            0,
                radio_rx_header_error_total:         0,
                radio_rx_timeout_total:              0,
                radio_device_error_bits:             0,
                radio_last_rssi_dbm:                 None,
                radio_last_snr_tenth_db:             None,
                radio_noise_floor_dbm:               None,
                radio_rssi_inst_dbm:                 None,
                radio_chip_mode:                     None,
                radio_last_applied_tx_power_dbm:     None,
                radio_last_tx_airtime_ms:            None,
                last_poll_ms:                        None,
                tx_power_level:                      None,
                tx_output_dbm_tenths:                None,
                tx_output_milliwatts:                None,
                battery_voltage_mv:                  None,
                battery_percent:                     None,
                last_poll_latency_ms:                None,
                avg_poll_latency_ms:                 None,
                wifi_connected:                      false,
                wifi_connect_started_ms:             None,
                wifi_connected_ms:                   None,
                wifi_disconnect_total:               0,
                wifi_last_disconnect_reason:         None,
                wifi_connect_request_total:          0,
                wifi_connect_request_error_total:    0,
                wifi_deep_recovery_total:            0,
                wifi_rssi_dbm:                       None,
                wifi_channel:                        None,
                wifi_bssid:                          None,
                wifi_auth_mode:                      None,
                wifi_tx_power_requested_quarter_dbm: None,
                wifi_tx_power_applied_quarter_dbm:   None,
                dhcp_configured:                     false,
                dhcp_started_ms:                     None,
                dhcp_configured_ms:                  None,
                dhcp_configured_total:               0,
                dhcp_deconfigured_total:             0,
                dhcp_reset_total:                    0,
                dhcp_timeout_total:                  0,
                dhcp_last_acquire_ms:                None,
                dhcp_ip:                             None,
                dhcp_gateway:                        None,
                network_started_ms:                  None,
                http_serving_started_ms:             None,
                network_startup_to_serving_ms:       None,
                smoltcp_poll_total:                  0,
                last_smoltcp_poll_ms:                None,
                smoltcp_poll_gap_max_ms:             0,
                smoltcp_poll_delay_miss_total:       0,
                smoltcp_poll_bad_gap_total:          0,
                http_requests_total:                 0,
                http_success_total:                  0,
                http_send_error_total:               0,
                http_socket_abort_total:             0,
                http_active_sockets:                 0,
                http_listening_sockets:              0,
                http_closed_sockets:                 0,
                http_syn_sent_sockets:               0,
                http_syn_received_sockets:           0,
                http_established_sockets:            0,
                http_fin_wait_1_sockets:             0,
                http_fin_wait_2_sockets:             0,
                http_close_wait_sockets:             0,
                http_closing_sockets:                0,
                http_last_ack_sockets:               0,
                http_time_wait_sockets:              0,
                http_oldest_socket_age_ms:           None,
                http_oldest_syn_sent_ms:             None,
                http_oldest_syn_received_ms:         None,
                http_oldest_established_ms:          None,
                http_oldest_fin_wait_1_ms:           None,
                http_oldest_fin_wait_2_ms:           None,
                http_oldest_close_wait_ms:           None,
                http_oldest_closing_ms:              None,
                http_oldest_last_ack_ms:             None,
                http_oldest_time_wait_ms:            None,
                last_http_request_ms:                None,
                last_http_success_ms:                None,
                network_recovery_total:              0,
                main_loop_gap_last_ms:               0,
                main_loop_gap_max_ms:                0,
                scheduler_tick_last_ms:              0,
                scheduler_tick_max_ms:               0,
                lora_service_last_ms:                0,
                lora_service_max_ms:                 0,
                http_service_last_ms:                0,
                http_service_max_ms:                 0,
                display_refresh_last_ms:             0,
                display_refresh_max_ms:              0,
                serial_emit_last_ms:                 0,
                serial_emit_max_ms:                  0,
                free_heap_bytes:                     None,
                diagnostic_events:                   0,
                diagnostic_dropped:                  0,
                diagnostic_errors:                   0,
                last_error_ms:                       None,
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

    /// Merge one producer observation with any retained telemetry.
    pub fn merge_producer(
        &mut self,
        producer_id: TelemetryProducerId,
        telemetry: ProducerTelemetry,
    ) -> Result<(), Error>
    {
        let record = self.record_mut(producer_id)?;
        if let Some(existing) = record.telemetry.as_mut() {
            existing.merge_from(telemetry);
        } else {
            record.telemetry = Some(telemetry);
        }
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
        push_u16(&mut payload, 0, LPP_LUMINOSITY, 1_234);
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

        assert_eq!(telemetry.channels().count(), 7);
        assert_f32_eq(telemetry.metrics.luminosity_lux, 1_234.0);
        assert_f32_eq(telemetry.metrics.battery_voltage, 4.08);
        assert_f32_eq(telemetry.metrics.battery_percent, 87.0);
        assert_f32_eq(telemetry.metrics.voltage, 4.08);
        assert_f32_eq(telemetry.metrics.current_amps, -1.234);
        assert_f32_eq(telemetry.metrics.power_watts, 15.0);
        assert_f32_eq(telemetry.metrics.gas_resistance_ohms, 123.45);

        let tsl2591 = telemetry.channel(0).unwrap().metrics;
        assert_f32_eq(tsl2591.luminosity_lux, 1_234.0);

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

    #[test]
    fn merges_partial_observation_without_dropping_channels()
    {
        let producer_id = TelemetryProducerId::from_public_key("producer-public-key");
        let mut telemetry = ProducerTelemetry::new(producer_id, 100);
        telemetry.set_channel_temperature(2, 21.1).unwrap();
        telemetry.set_channel_humidity(2, 48.5).unwrap();
        telemetry.set_channel_voltage(4, 12.01).unwrap();
        telemetry.set_channel_current(4, -1.234).unwrap();
        telemetry.set_channel_power(4, 15.0).unwrap();

        let mut partial = ProducerTelemetry::new(producer_id, 200);
        partial.rssi = Some(-28);
        partial.snr = Some(12.0);
        partial.set_channel_luminosity(0, 777.0).unwrap();
        partial.set_channel_voltage(1, 4.23).unwrap();
        partial.set_channel_temperature(1, 27.7).unwrap();

        telemetry.merge_from(partial);

        assert_eq!(telemetry.timestamp_ms, 200);
        assert_eq!(telemetry.channels().count(), 4);
        assert_eq!(telemetry.rssi, Some(-28));
        assert_f32_eq(telemetry.channel(0).unwrap().metrics.luminosity_lux, 777.0);
        assert_f32_eq(telemetry.channel(1).unwrap().metrics.battery_voltage, 4.23);
        assert_f32_eq(
            telemetry.channel(1).unwrap().metrics.temperature_celsius,
            27.7,
        );
        assert_f32_eq(
            telemetry.channel(2).unwrap().metrics.temperature_celsius,
            21.1,
        );
        assert_f32_eq(telemetry.channel(2).unwrap().metrics.humidity_percent, 48.5);
        assert_f32_eq(telemetry.channel(4).unwrap().metrics.voltage, 12.01);
        assert_f32_eq(telemetry.channel(4).unwrap().metrics.current_amps, -1.234);
        assert_f32_eq(telemetry.channel(4).unwrap().metrics.power_watts, 15.0);
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
