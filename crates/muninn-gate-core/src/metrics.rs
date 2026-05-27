//! Prometheus metrics rendering helpers.

use core::fmt;

use heapless::String;

use crate::{Error, MetricsRenderer, ProducerTelemetryChannel, TelemetryRecord, TelemetrySnapshot};

/// Stateless Prometheus text renderer.
pub struct PrometheusRenderer;

impl<const OUT: usize, const N: usize> MetricsRenderer<OUT, N> for PrometheusRenderer
{
    fn render_prometheus(&self, snapshot: &TelemetrySnapshot<N>) -> Result<String<OUT>, Error>
    {
        render_prometheus(snapshot)
    }
}

/// Render a telemetry snapshot as Prometheus text.
pub fn render_prometheus<const N: usize, const OUT: usize>(
    snapshot: &TelemetrySnapshot<N>,
) -> Result<String<OUT>, Error>
{
    let mut out = String::new();
    render_prometheus_into(snapshot, &mut out)?;
    Ok(out)
}

/// Write a telemetry snapshot as Prometheus text into an existing writer.
pub fn render_prometheus_into<const N: usize, W>(
    snapshot: &TelemetrySnapshot<N>,
    out: &mut W,
) -> Result<(), Error>
where
    W: fmt::Write + ?Sized,
{
    write_help(
        out,
        "muninn_gate_up",
        "Whether the gateway is running.",
        "gauge",
    )?;
    writeln!(out, "muninn_gate_up 1")?;
    writeln!(out)?;

    write_help(
        out,
        "muninn_gate_uptime_ms",
        "Gateway uptime in milliseconds.",
        "gauge",
    )?;
    writeln!(out, "muninn_gate_uptime_ms {}", snapshot.gateway.uptime_ms)?;
    writeln!(out)?;

    write_help(
        out,
        "muninn_gate_gateway_poll_success_total",
        "Successful telemetry polls across all producers.",
        "counter",
    )?;
    writeln!(
        out,
        "muninn_gate_gateway_poll_success_total {}",
        snapshot.gateway.poll_success_total
    )?;
    writeln!(out)?;

    write_help(
        out,
        "muninn_gate_gateway_poll_failure_total",
        "Failed telemetry polls across all producers.",
        "counter",
    )?;
    writeln!(
        out,
        "muninn_gate_gateway_poll_failure_total {}",
        snapshot.gateway.poll_failure_total
    )?;
    writeln!(out)?;

    write_help(
        out,
        "muninn_gate_gateway_poll_retry_total",
        "Telemetry polls deferred for retry across all producers.",
        "counter",
    )?;
    writeln!(
        out,
        "muninn_gate_gateway_poll_retry_total {}",
        snapshot.gateway.poll_retry_total
    )?;
    writeln!(out)?;

    write_help(
        out,
        "muninn_gate_poll_on_demand_total",
        "Operator-requested immediate poll passes.",
        "counter",
    )?;
    writeln!(
        out,
        "muninn_gate_poll_on_demand_total {}",
        snapshot.gateway.poll_on_demand_total
    )?;
    writeln!(out)?;

    if let Some(rate) = snapshot.gateway.poll_success_rate_per_mille() {
        write_help(
            out,
            "muninn_gate_gateway_poll_success_rate",
            "Successful telemetry poll ratio across all producers.",
            "gauge",
        )?;
        writeln!(
            out,
            "muninn_gate_gateway_poll_success_rate {}",
            PerMille(rate)
        )?;
        writeln!(out)?;
    }

    write_help(
        out,
        "muninn_gate_radio_rx_total",
        "Radio receive packets observed by the gateway.",
        "counter",
    )?;
    writeln!(
        out,
        "muninn_gate_radio_rx_total {}",
        snapshot.gateway.radio_rx_total
    )?;
    writeln!(out)?;

    write_help(
        out,
        "muninn_gate_radio_tx_total",
        "Radio transmit packets sent by the gateway.",
        "counter",
    )?;
    writeln!(
        out,
        "muninn_gate_radio_tx_total {}",
        snapshot.gateway.radio_tx_total
    )?;
    writeln!(out)?;

    write_help(
        out,
        "muninn_gate_radio_rx_crc_error_total",
        "Radio RX packets rejected by CRC.",
        "counter",
    )?;
    writeln!(
        out,
        "muninn_gate_radio_rx_crc_error_total {}",
        snapshot.gateway.radio_rx_crc_error_total
    )?;
    writeln!(out)?;

    write_help(
        out,
        "muninn_gate_radio_rx_header_error_total",
        "Radio RX packets rejected by header error.",
        "counter",
    )?;
    writeln!(
        out,
        "muninn_gate_radio_rx_header_error_total {}",
        snapshot.gateway.radio_rx_header_error_total
    )?;
    writeln!(out)?;

    write_help(
        out,
        "muninn_gate_radio_rx_timeout_total",
        "Radio RX timeout IRQ count.",
        "counter",
    )?;
    writeln!(
        out,
        "muninn_gate_radio_rx_timeout_total {}",
        snapshot.gateway.radio_rx_timeout_total
    )?;
    writeln!(out)?;

    write_help(
        out,
        "muninn_gate_radio_device_error_bits",
        "Latest SX126x device error bitmask.",
        "gauge",
    )?;
    writeln!(
        out,
        "muninn_gate_radio_device_error_bits {}",
        snapshot.gateway.radio_device_error_bits
    )?;
    writeln!(out)?;

    if let Some(rssi) = snapshot.gateway.radio_last_rssi_dbm {
        write_help(
            out,
            "muninn_gate_radio_last_rssi_dbm",
            "RSSI from the latest decoded LoRa packet.",
            "gauge",
        )?;
        writeln!(out, "muninn_gate_radio_last_rssi_dbm {}", rssi)?;
        writeln!(out)?;
    }

    if let Some(snr) = snapshot.gateway.radio_last_snr_tenth_db {
        write_help(
            out,
            "muninn_gate_radio_last_snr_db",
            "SNR from the latest decoded LoRa packet.",
            "gauge",
        )?;
        writeln!(out, "muninn_gate_radio_last_snr_db {}", Tenths(snr))?;
        writeln!(out)?;
    }

    if let Some(noise_floor) = snapshot.gateway.radio_noise_floor_dbm {
        write_help(
            out,
            "muninn_gate_radio_noise_floor_dbm",
            "Rolling idle-RX noise floor estimate.",
            "gauge",
        )?;
        writeln!(out, "muninn_gate_radio_noise_floor_dbm {}", noise_floor)?;
        writeln!(out)?;
    }

    if let Some(rssi_inst) = snapshot.gateway.radio_rssi_inst_dbm {
        write_help(
            out,
            "muninn_gate_radio_rssi_inst_dbm",
            "Latest idle-RX instantaneous RSSI sample.",
            "gauge",
        )?;
        writeln!(out, "muninn_gate_radio_rssi_inst_dbm {}", rssi_inst)?;
        writeln!(out)?;
    }

    if let Some(chip_mode) = snapshot.gateway.radio_chip_mode {
        write_help(
            out,
            "muninn_gate_radio_chip_mode",
            "Latest sampled SX126x chip mode nibble.",
            "gauge",
        )?;
        writeln!(out, "muninn_gate_radio_chip_mode {}", chip_mode)?;
        writeln!(out)?;
    }

    if let Some(tx_power) = snapshot.gateway.radio_last_applied_tx_power_dbm {
        write_help(
            out,
            "muninn_gate_radio_last_applied_tx_power_dbm",
            "TX power most recently applied to the radio.",
            "gauge",
        )?;
        writeln!(
            out,
            "muninn_gate_radio_last_applied_tx_power_dbm {}",
            tx_power
        )?;
        writeln!(out)?;
    }

    if let Some(airtime_ms) = snapshot.gateway.radio_last_tx_airtime_ms {
        write_help(
            out,
            "muninn_gate_radio_last_tx_airtime_ms",
            "Airtime of the latest successful TX.",
            "gauge",
        )?;
        writeln!(out, "muninn_gate_radio_last_tx_airtime_ms {}", airtime_ms)?;
        writeln!(out)?;
    }

    if let Some(last_poll_ms) = snapshot.gateway.last_poll_ms {
        write_help(
            out,
            "muninn_gate_last_poll_age_ms",
            "Age of the last poll attempt in milliseconds.",
            "gauge",
        )?;
        writeln!(
            out,
            "muninn_gate_last_poll_age_ms {}",
            snapshot.gateway.uptime_ms.saturating_sub(last_poll_ms)
        )?;
        writeln!(out)?;
    }

    if let Some(tx_power_level) = snapshot.gateway.tx_power_level {
        write_help(
            out,
            "muninn_gate_tx_power_level",
            "Selected board radio TX power level.",
            "gauge",
        )?;
        writeln!(out, "muninn_gate_tx_power_level {}", tx_power_level)?;
        writeln!(out)?;
    }

    if let Some(tx_output_dbm_tenths) = snapshot.gateway.tx_output_dbm_tenths {
        write_help(
            out,
            "muninn_gate_tx_output_dbm",
            "Approximate conducted TX power in dBm.",
            "gauge",
        )?;
        writeln!(
            out,
            "muninn_gate_tx_output_dbm {}",
            Tenths(tx_output_dbm_tenths)
        )?;
        writeln!(out)?;
    }

    if let Some(tx_output_milliwatts) = snapshot.gateway.tx_output_milliwatts {
        write_help(
            out,
            "muninn_gate_tx_output_milliwatts",
            "Approximate conducted TX power in milliwatts.",
            "gauge",
        )?;
        writeln!(
            out,
            "muninn_gate_tx_output_milliwatts {}",
            tx_output_milliwatts
        )?;
        writeln!(out)?;
    }

    if let Some(battery_voltage_mv) = snapshot.gateway.battery_voltage_mv {
        write_help(
            out,
            "muninn_gate_battery_voltage_mv",
            "Gateway battery voltage in millivolts.",
            "gauge",
        )?;
        writeln!(out, "muninn_gate_battery_voltage_mv {}", battery_voltage_mv)?;
        writeln!(out)?;
    }

    if let Some(battery_percent) = snapshot.gateway.battery_percent {
        write_help(
            out,
            "muninn_gate_battery_percent",
            "Gateway battery charge percentage.",
            "gauge",
        )?;
        writeln!(out, "muninn_gate_battery_percent {}", battery_percent)?;
        writeln!(out)?;
    }

    if let Some(last_poll_latency_ms) = snapshot.gateway.last_poll_latency_ms {
        write_help(
            out,
            "muninn_gate_last_poll_latency_ms",
            "Latest completed poll latency in milliseconds.",
            "gauge",
        )?;
        writeln!(
            out,
            "muninn_gate_last_poll_latency_ms {}",
            last_poll_latency_ms
        )?;
        writeln!(out)?;
    }

    if let Some(avg_poll_latency_ms) = snapshot.gateway.avg_poll_latency_ms {
        write_help(
            out,
            "muninn_gate_avg_poll_latency_ms",
            "Rolling average poll latency in milliseconds.",
            "gauge",
        )?;
        writeln!(
            out,
            "muninn_gate_avg_poll_latency_ms {}",
            avg_poll_latency_ms
        )?;
        writeln!(out)?;
    }

    write_help(
        out,
        "muninn_gate_wifi_connected",
        "Whether the ESP WiFi station is associated.",
        "gauge",
    )?;
    writeln!(
        out,
        "muninn_gate_wifi_connected {}",
        u8::from(snapshot.gateway.wifi_connected)
    )?;
    writeln!(out)?;

    write_help(
        out,
        "muninn_gate_wifi_disconnect_total",
        "WiFi disconnect events observed by the firmware.",
        "counter",
    )?;
    writeln!(
        out,
        "muninn_gate_wifi_disconnect_total {}",
        snapshot.gateway.wifi_disconnect_total
    )?;
    writeln!(out)?;

    if let Some(reason) = snapshot.gateway.wifi_last_disconnect_reason {
        write_help(
            out,
            "muninn_gate_wifi_last_disconnect_reason",
            "Latest WiFi disconnect reason code.",
            "gauge",
        )?;
        writeln!(out, "muninn_gate_wifi_last_disconnect_reason {}", reason)?;
        writeln!(out)?;
    }

    write_help(
        out,
        "muninn_gate_wifi_connect_request_total",
        "WiFi connect requests issued by firmware.",
        "counter",
    )?;
    writeln!(
        out,
        "muninn_gate_wifi_connect_request_total {}",
        snapshot.gateway.wifi_connect_request_total
    )?;
    writeln!(out)?;

    write_help(
        out,
        "muninn_gate_wifi_connect_request_error_total",
        "WiFi connect requests that returned an immediate error.",
        "counter",
    )?;
    writeln!(
        out,
        "muninn_gate_wifi_connect_request_error_total {}",
        snapshot.gateway.wifi_connect_request_error_total
    )?;
    writeln!(out)?;

    write_help(
        out,
        "muninn_gate_wifi_deep_recovery_total",
        "WiFi controller stop/start recoveries.",
        "counter",
    )?;
    writeln!(
        out,
        "muninn_gate_wifi_deep_recovery_total {}",
        snapshot.gateway.wifi_deep_recovery_total
    )?;
    writeln!(out)?;

    if let Some(rssi) = snapshot.gateway.wifi_rssi_dbm {
        write_help(
            out,
            "muninn_gate_wifi_rssi_dbm",
            "Latest associated AP RSSI in dBm.",
            "gauge",
        )?;
        writeln!(out, "muninn_gate_wifi_rssi_dbm {}", rssi)?;
        writeln!(out)?;
    }

    if let Some(channel) = snapshot.gateway.wifi_channel {
        write_help(
            out,
            "muninn_gate_wifi_channel",
            "Latest associated AP primary channel.",
            "gauge",
        )?;
        writeln!(out, "muninn_gate_wifi_channel {}", channel)?;
        writeln!(out)?;
    }

    if let Some(bssid) = snapshot.gateway.wifi_bssid {
        write_help(
            out,
            "muninn_gate_wifi_ap_info",
            "Latest associated AP identity.",
            "gauge",
        )?;
        write!(out, "muninn_gate_wifi_ap_info{{bssid=\"")?;
        write_mac(out, bssid)?;
        writeln!(out, "\"}} 1")?;
        writeln!(out)?;
    }

    if let Some(auth_mode) = snapshot.gateway.wifi_auth_mode {
        write_help(
            out,
            "muninn_gate_wifi_auth_mode",
            "Latest associated AP auth mode as reported by ESP-IDF.",
            "gauge",
        )?;
        writeln!(out, "muninn_gate_wifi_auth_mode {}", auth_mode)?;
        writeln!(out)?;
    }

    if let Some(requested) = snapshot.gateway.wifi_tx_power_requested_quarter_dbm {
        write_help(
            out,
            "muninn_gate_wifi_tx_power_requested_quarter_dbm",
            "Requested ESP WiFi TX power cap in quarter-dBm units.",
            "gauge",
        )?;
        writeln!(
            out,
            "muninn_gate_wifi_tx_power_requested_quarter_dbm {}",
            requested
        )?;
        writeln!(out)?;
    }

    if let Some(applied) = snapshot.gateway.wifi_tx_power_applied_quarter_dbm {
        write_help(
            out,
            "muninn_gate_wifi_tx_power_applied_quarter_dbm",
            "Applied ESP WiFi TX power cap in quarter-dBm units.",
            "gauge",
        )?;
        writeln!(
            out,
            "muninn_gate_wifi_tx_power_applied_quarter_dbm {}",
            applied
        )?;
        writeln!(out)?;
    }

    write_help(
        out,
        "muninn_gate_dhcp_configured",
        "Whether DHCP currently has a configured IPv4 lease.",
        "gauge",
    )?;
    writeln!(
        out,
        "muninn_gate_dhcp_configured {}",
        u8::from(snapshot.gateway.dhcp_configured)
    )?;
    writeln!(out)?;

    write_help(
        out,
        "muninn_gate_dhcp_configured_total",
        "DHCP configured events.",
        "counter",
    )?;
    writeln!(
        out,
        "muninn_gate_dhcp_configured_total {}",
        snapshot.gateway.dhcp_configured_total
    )?;
    writeln!(out)?;

    write_help(
        out,
        "muninn_gate_dhcp_deconfigured_total",
        "DHCP deconfigured events.",
        "counter",
    )?;
    writeln!(
        out,
        "muninn_gate_dhcp_deconfigured_total {}",
        snapshot.gateway.dhcp_deconfigured_total
    )?;
    writeln!(out)?;

    write_help(
        out,
        "muninn_gate_dhcp_reset_total",
        "DHCP reset calls.",
        "counter",
    )?;
    writeln!(
        out,
        "muninn_gate_dhcp_reset_total {}",
        snapshot.gateway.dhcp_reset_total
    )?;
    writeln!(out)?;

    write_help(
        out,
        "muninn_gate_dhcp_timeout_total",
        "DHCP timeouts that forced WiFi reconnect.",
        "counter",
    )?;
    writeln!(
        out,
        "muninn_gate_dhcp_timeout_total {}",
        snapshot.gateway.dhcp_timeout_total
    )?;
    writeln!(out)?;

    if let Some(acquire_ms) = snapshot.gateway.dhcp_last_acquire_ms {
        write_help(
            out,
            "muninn_gate_dhcp_last_acquire_ms",
            "Latest DHCP acquisition duration in milliseconds.",
            "gauge",
        )?;
        writeln!(out, "muninn_gate_dhcp_last_acquire_ms {}", acquire_ms)?;
        writeln!(out)?;
    }

    write_help(
        out,
        "muninn_gate_smoltcp_poll_total",
        "smoltcp interface polls since boot.",
        "counter",
    )?;
    writeln!(
        out,
        "muninn_gate_smoltcp_poll_total {}",
        snapshot.gateway.smoltcp_poll_total
    )?;
    writeln!(out)?;

    write_help(
        out,
        "muninn_gate_smoltcp_poll_gap_max_ms",
        "Longest observed gap between smoltcp interface polls.",
        "gauge",
    )?;
    writeln!(
        out,
        "muninn_gate_smoltcp_poll_gap_max_ms {}",
        snapshot.gateway.smoltcp_poll_gap_max_ms
    )?;
    writeln!(out)?;

    if let Some(last_poll_ms) = snapshot.gateway.last_smoltcp_poll_ms {
        write_help(
            out,
            "muninn_gate_smoltcp_ms_since_last_poll",
            "Age of the latest smoltcp interface poll.",
            "gauge",
        )?;
        writeln!(
            out,
            "muninn_gate_smoltcp_ms_since_last_poll {}",
            snapshot.gateway.uptime_ms.saturating_sub(last_poll_ms)
        )?;
        writeln!(out)?;
    }

    write_help(
        out,
        "muninn_gate_smoltcp_poll_delay_miss_total",
        "smoltcp poll gaps that exceeded the diagnostic deadline.",
        "counter",
    )?;
    writeln!(
        out,
        "muninn_gate_smoltcp_poll_delay_miss_total {}",
        snapshot.gateway.smoltcp_poll_delay_miss_total
    )?;
    writeln!(out)?;

    write_help(
        out,
        "muninn_gate_smoltcp_poll_bad_gap_total",
        "smoltcp poll gaps that exceeded the bad-gap threshold.",
        "counter",
    )?;
    writeln!(
        out,
        "muninn_gate_smoltcp_poll_bad_gap_total {}",
        snapshot.gateway.smoltcp_poll_bad_gap_total
    )?;
    writeln!(out)?;

    write_help(
        out,
        "muninn_gate_http_requests_total",
        "HTTP requests accepted by the embedded server.",
        "counter",
    )?;
    writeln!(
        out,
        "muninn_gate_http_requests_total {}",
        snapshot.gateway.http_requests_total
    )?;
    writeln!(out)?;

    write_help(
        out,
        "muninn_gate_http_success_total",
        "HTTP responses completed successfully by the embedded server.",
        "counter",
    )?;
    writeln!(
        out,
        "muninn_gate_http_success_total {}",
        snapshot.gateway.http_success_total
    )?;
    writeln!(out)?;

    write_help(
        out,
        "muninn_gate_http_send_error_total",
        "HTTP responses aborted while sending.",
        "counter",
    )?;
    writeln!(
        out,
        "muninn_gate_http_send_error_total {}",
        snapshot.gateway.http_send_error_total
    )?;
    writeln!(out)?;

    write_help(
        out,
        "muninn_gate_http_socket_abort_total",
        "HTTP TCP sockets explicitly aborted by the firmware.",
        "counter",
    )?;
    writeln!(
        out,
        "muninn_gate_http_socket_abort_total {}",
        snapshot.gateway.http_socket_abort_total
    )?;
    writeln!(out)?;

    write_help(
        out,
        "muninn_gate_http_active_sockets",
        "Current active HTTP TCP sockets.",
        "gauge",
    )?;
    writeln!(
        out,
        "muninn_gate_http_active_sockets {}",
        snapshot.gateway.http_active_sockets
    )?;
    writeln!(out)?;

    write_help(
        out,
        "muninn_gate_http_listening_sockets",
        "Current HTTP TCP sockets listening for new clients.",
        "gauge",
    )?;
    writeln!(
        out,
        "muninn_gate_http_listening_sockets {}",
        snapshot.gateway.http_listening_sockets
    )?;
    writeln!(out)?;

    write_help(
        out,
        "muninn_gate_http_closed_sockets",
        "Current closed HTTP TCP sockets.",
        "gauge",
    )?;
    writeln!(
        out,
        "muninn_gate_http_closed_sockets {}",
        snapshot.gateway.http_closed_sockets
    )?;
    writeln!(out)?;

    write_help(
        out,
        "muninn_gate_http_established_sockets",
        "Current established HTTP TCP sockets.",
        "gauge",
    )?;
    writeln!(
        out,
        "muninn_gate_http_established_sockets {}",
        snapshot.gateway.http_established_sockets
    )?;
    writeln!(out)?;

    write_help(
        out,
        "muninn_gate_http_syn_sent_sockets",
        "Current HTTP TCP sockets in SYN-SENT.",
        "gauge",
    )?;
    writeln!(
        out,
        "muninn_gate_http_syn_sent_sockets {}",
        snapshot.gateway.http_syn_sent_sockets
    )?;
    writeln!(out)?;

    write_help(
        out,
        "muninn_gate_http_syn_received_sockets",
        "Current HTTP TCP sockets in SYN-RECEIVED.",
        "gauge",
    )?;
    writeln!(
        out,
        "muninn_gate_http_syn_received_sockets {}",
        snapshot.gateway.http_syn_received_sockets
    )?;
    writeln!(out)?;

    write_help(
        out,
        "muninn_gate_http_fin_wait_1_sockets",
        "Current HTTP TCP sockets in FIN-WAIT-1.",
        "gauge",
    )?;
    writeln!(
        out,
        "muninn_gate_http_fin_wait_1_sockets {}",
        snapshot.gateway.http_fin_wait_1_sockets
    )?;
    writeln!(out)?;

    write_help(
        out,
        "muninn_gate_http_fin_wait_2_sockets",
        "Current HTTP TCP sockets in FIN-WAIT-2.",
        "gauge",
    )?;
    writeln!(
        out,
        "muninn_gate_http_fin_wait_2_sockets {}",
        snapshot.gateway.http_fin_wait_2_sockets
    )?;
    writeln!(out)?;

    write_help(
        out,
        "muninn_gate_http_close_wait_sockets",
        "Current HTTP TCP sockets in CLOSE-WAIT.",
        "gauge",
    )?;
    writeln!(
        out,
        "muninn_gate_http_close_wait_sockets {}",
        snapshot.gateway.http_close_wait_sockets
    )?;
    writeln!(out)?;

    write_help(
        out,
        "muninn_gate_http_time_wait_sockets",
        "Current HTTP TCP sockets in TIME-WAIT.",
        "gauge",
    )?;
    writeln!(
        out,
        "muninn_gate_http_time_wait_sockets {}",
        snapshot.gateway.http_time_wait_sockets
    )?;
    writeln!(out)?;

    write_help(
        out,
        "muninn_gate_http_closing_sockets",
        "Current HTTP TCP sockets in CLOSING.",
        "gauge",
    )?;
    writeln!(
        out,
        "muninn_gate_http_closing_sockets {}",
        snapshot.gateway.http_closing_sockets
    )?;
    writeln!(out)?;

    write_help(
        out,
        "muninn_gate_http_last_ack_sockets",
        "Current HTTP TCP sockets in LAST-ACK.",
        "gauge",
    )?;
    writeln!(
        out,
        "muninn_gate_http_last_ack_sockets {}",
        snapshot.gateway.http_last_ack_sockets
    )?;
    writeln!(out)?;

    if let Some(oldest_ms) = snapshot.gateway.http_oldest_socket_age_ms {
        write_help(
            out,
            "muninn_gate_http_oldest_socket_age_ms",
            "Oldest non-listening/non-closed HTTP socket age.",
            "gauge",
        )?;
        writeln!(out, "muninn_gate_http_oldest_socket_age_ms {}", oldest_ms)?;
        writeln!(out)?;
    }

    if let Some(last_http_request_ms) = snapshot.gateway.last_http_request_ms {
        write_help(
            out,
            "muninn_gate_http_last_request_age_ms",
            "Age of the latest accepted HTTP request in milliseconds.",
            "gauge",
        )?;
        writeln!(
            out,
            "muninn_gate_http_last_request_age_ms {}",
            snapshot
                .gateway
                .uptime_ms
                .saturating_sub(last_http_request_ms)
        )?;
        writeln!(out)?;
    }

    if let Some(last_http_success_ms) = snapshot.gateway.last_http_success_ms {
        write_help(
            out,
            "muninn_gate_http_last_success_age_ms",
            "Age of the latest successful HTTP response in milliseconds.",
            "gauge",
        )?;
        writeln!(
            out,
            "muninn_gate_http_last_success_age_ms {}",
            snapshot
                .gateway
                .uptime_ms
                .saturating_sub(last_http_success_ms)
        )?;
        writeln!(out)?;
    }

    write_help(
        out,
        "muninn_gate_network_recovery_total",
        "Network recovery actions forced by the firmware.",
        "counter",
    )?;
    writeln!(
        out,
        "muninn_gate_network_recovery_total {}",
        snapshot.gateway.network_recovery_total
    )?;
    writeln!(out)?;

    write_help(
        out,
        "muninn_gate_main_loop_gap_max_ms",
        "Longest observed network main-loop gap in milliseconds.",
        "gauge",
    )?;
    writeln!(
        out,
        "muninn_gate_main_loop_gap_max_ms {}",
        snapshot.gateway.main_loop_gap_max_ms
    )?;
    writeln!(out)?;

    write_help(
        out,
        "muninn_gate_main_loop_gap_last_ms",
        "Latest observed network main-loop gap in milliseconds.",
        "gauge",
    )?;
    writeln!(
        out,
        "muninn_gate_main_loop_gap_last_ms {}",
        snapshot.gateway.main_loop_gap_last_ms
    )?;
    writeln!(out)?;

    write_help(
        out,
        "muninn_gate_scheduler_tick_max_ms",
        "Longest observed scheduler tick duration in milliseconds.",
        "gauge",
    )?;
    writeln!(
        out,
        "muninn_gate_scheduler_tick_max_ms {}",
        snapshot.gateway.scheduler_tick_max_ms
    )?;
    writeln!(out)?;

    write_help(
        out,
        "muninn_gate_scheduler_tick_last_ms",
        "Latest observed scheduler tick duration in milliseconds.",
        "gauge",
    )?;
    writeln!(
        out,
        "muninn_gate_scheduler_tick_last_ms {}",
        snapshot.gateway.scheduler_tick_last_ms
    )?;
    writeln!(out)?;

    write_help(
        out,
        "muninn_gate_lora_service_max_ms",
        "Longest cooperative LoRa service duration in milliseconds.",
        "gauge",
    )?;
    writeln!(
        out,
        "muninn_gate_lora_service_max_ms {}",
        snapshot.gateway.lora_service_max_ms
    )?;
    writeln!(out)?;

    write_help(
        out,
        "muninn_gate_http_service_max_ms",
        "Longest HTTP service pass duration in milliseconds.",
        "gauge",
    )?;
    writeln!(
        out,
        "muninn_gate_http_service_max_ms {}",
        snapshot.gateway.http_service_max_ms
    )?;
    writeln!(out)?;

    if let Some(free_heap_bytes) = snapshot.gateway.free_heap_bytes {
        write_help(
            out,
            "muninn_gate_free_heap_bytes",
            "Platform-reported free heap in bytes.",
            "gauge",
        )?;
        writeln!(out, "muninn_gate_free_heap_bytes {}", free_heap_bytes)?;
        writeln!(out)?;
    }

    write_help(
        out,
        "muninn_gate_diagnostic_events",
        "Diagnostic events currently retained by the gateway.",
        "gauge",
    )?;
    writeln!(
        out,
        "muninn_gate_diagnostic_events {}",
        snapshot.gateway.diagnostic_events
    )?;
    writeln!(out)?;

    write_help(
        out,
        "muninn_gate_diagnostic_dropped_total",
        "Diagnostic events dropped by the gateway.",
        "counter",
    )?;
    writeln!(
        out,
        "muninn_gate_diagnostic_dropped_total {}",
        snapshot.gateway.diagnostic_dropped
    )?;
    writeln!(out)?;

    write_help(
        out,
        "muninn_gate_diagnostic_error_events",
        "Retained diagnostic events with error severity.",
        "gauge",
    )?;
    writeln!(
        out,
        "muninn_gate_diagnostic_error_events {}",
        snapshot.gateway.diagnostic_errors
    )?;
    writeln!(out)?;

    if let Some(last_error_ms) = snapshot.gateway.last_error_ms {
        write_help(
            out,
            "muninn_gate_last_error_age_ms",
            "Age of the latest retained error diagnostic in milliseconds.",
            "gauge",
        )?;
        writeln!(
            out,
            "muninn_gate_last_error_age_ms {}",
            snapshot.gateway.uptime_ms.saturating_sub(last_error_ms)
        )?;
        writeln!(out)?;
    }

    write_producer_metric_families(out, snapshot.gateway.uptime_ms, &snapshot.records)?;

    Ok(())
}

fn write_help<W>(_out: &mut W, _name: &str, _help: &str, _metric_type: &str) -> Result<(), Error>
where
    W: fmt::Write + ?Sized,
{
    Ok(())
}

fn write_producer_metric_families<W>(
    out: &mut W,
    gateway_uptime_ms: u64,
    records: &[TelemetryRecord],
) -> Result<(), Error>
where
    W: fmt::Write + ?Sized,
{
    write_optional_producer_family(
        out,
        records,
        "muninn_gate_node_rssi",
        "Last received RSSI from telemetry producer.",
        "gauge",
        |record| record.telemetry.and_then(|telemetry| telemetry.rssi),
    )?;
    write_optional_producer_family(
        out,
        records,
        "muninn_gate_node_snr",
        "Last received SNR from telemetry producer.",
        "gauge",
        |record| record.telemetry.and_then(|telemetry| telemetry.snr),
    )?;
    write_optional_producer_family(
        out,
        records,
        "muninn_gate_node_uptime_ms",
        "Telemetry producer reported uptime in milliseconds.",
        "gauge",
        |record| record.telemetry.and_then(|telemetry| telemetry.uptime_ms),
    )?;
    write_optional_channel_family(
        out,
        records,
        "muninn_gate_node_channel_battery_voltage",
        "Telemetry producer channel battery voltage.",
        "gauge",
        |channel| channel.metrics.battery_voltage,
    )?;
    write_optional_channel_family(
        out,
        records,
        "muninn_gate_node_channel_battery_percent",
        "Telemetry producer channel battery charge percent.",
        "gauge",
        |channel| channel.metrics.battery_percent,
    )?;
    write_optional_channel_family(
        out,
        records,
        "muninn_gate_node_channel_voltage",
        "Telemetry producer channel voltage.",
        "gauge",
        |channel| channel.metrics.voltage,
    )?;
    write_optional_channel_family(
        out,
        records,
        "muninn_gate_node_channel_current_amps",
        "Telemetry producer channel current in amperes.",
        "gauge",
        |channel| channel.metrics.current_amps,
    )?;
    write_optional_channel_family(
        out,
        records,
        "muninn_gate_node_channel_power_watts",
        "Telemetry producer channel power in watts.",
        "gauge",
        |channel| channel.metrics.power_watts,
    )?;
    write_optional_channel_family(
        out,
        records,
        "muninn_gate_node_channel_temperature_celsius",
        "Telemetry producer channel temperature in Celsius.",
        "gauge",
        |channel| channel.metrics.temperature_celsius,
    )?;
    write_optional_channel_family(
        out,
        records,
        "muninn_gate_node_channel_humidity_percent",
        "Telemetry producer channel relative humidity percent.",
        "gauge",
        |channel| channel.metrics.humidity_percent,
    )?;
    write_optional_channel_family(
        out,
        records,
        "muninn_gate_node_channel_pressure_pa",
        "Telemetry producer channel pressure in pascals.",
        "gauge",
        |channel| channel.metrics.pressure_pa,
    )?;
    write_optional_channel_family(
        out,
        records,
        "muninn_gate_node_channel_gas_resistance_ohms",
        "Telemetry producer channel gas sensor resistance in ohms.",
        "gauge",
        |channel| channel.metrics.gas_resistance_ohms,
    )?;
    write_optional_producer_family(
        out,
        records,
        "muninn_gate_node_last_heard_age_ms",
        "Age of the last received telemetry from the telemetry producer in milliseconds.",
        "gauge",
        |record| {
            record
                .telemetry
                .map(|telemetry| gateway_uptime_ms.saturating_sub(telemetry.timestamp_ms))
        },
    )?;
    write_optional_producer_family(
        out,
        records,
        "muninn_gate_node_last_poll_age_ms",
        "Age of the last poll attempt for the telemetry producer in milliseconds.",
        "gauge",
        |record| {
            record
                .poll
                .last_poll_ms
                .map(|last_poll_ms| gateway_uptime_ms.saturating_sub(last_poll_ms))
        },
    )?;
    write_required_producer_family(
        out,
        records,
        "muninn_gate_poll_success_total",
        "Successful telemetry polls.",
        "counter",
        |record| record.poll.poll_success_total,
    )?;
    write_required_producer_family(
        out,
        records,
        "muninn_gate_poll_failure_total",
        "Failed telemetry polls.",
        "counter",
        |record| record.poll.poll_failure_total,
    )?;
    write_optional_producer_family(
        out,
        records,
        "muninn_gate_poll_success_rate",
        "Successful telemetry poll ratio.",
        "gauge",
        |record| record.poll.poll_success_rate_per_mille().map(PerMille),
    )?;
    Ok(())
}

fn write_required_producer_family<W, F, T>(
    out: &mut W,
    records: &[TelemetryRecord],
    name: &str,
    help: &str,
    metric_type: &str,
    value: F,
) -> Result<(), Error>
where
    W: fmt::Write + ?Sized,
    F: Fn(&TelemetryRecord) -> T,
    T: fmt::Display,
{
    write_help(out, name, help, metric_type)?;
    for record in records {
        write_producer_value(out, name, record, value(record))?;
    }
    writeln!(out)?;
    Ok(())
}

fn write_optional_producer_family<W, F, T>(
    out: &mut W,
    records: &[TelemetryRecord],
    name: &str,
    help: &str,
    metric_type: &str,
    value: F,
) -> Result<(), Error>
where
    W: fmt::Write + ?Sized,
    F: Fn(&TelemetryRecord) -> Option<T>,
    T: fmt::Display,
{
    write_help(out, name, help, metric_type)?;
    for record in records {
        if let Some(value) = value(record) {
            write_producer_value(out, name, record, value)?;
        }
    }
    writeln!(out)?;
    Ok(())
}

fn write_optional_channel_family<W, F, T>(
    out: &mut W,
    records: &[TelemetryRecord],
    name: &str,
    help: &str,
    metric_type: &str,
    value: F,
) -> Result<(), Error>
where
    W: fmt::Write + ?Sized,
    F: Fn(&ProducerTelemetryChannel) -> Option<T>,
    T: fmt::Display,
{
    write_help(out, name, help, metric_type)?;
    for record in records {
        if let Some(telemetry) = record.telemetry {
            for channel in telemetry.channels() {
                if let Some(value) = value(channel) {
                    write_producer_channel_value(out, name, record, channel.channel_id, value)?;
                }
            }
        }
    }
    writeln!(out)?;
    Ok(())
}

fn write_producer_value<W, T: fmt::Display>(
    out: &mut W,
    metric_name: &str,
    record: &TelemetryRecord,
    value: T,
) -> Result<(), Error>
where
    W: fmt::Write + ?Sized,
{
    write!(out, "{metric_name}{{node=\"")?;
    write_escaped_label(out, record.config.name.as_str())?;
    writeln!(out, "\"}} {value}")?;
    Ok(())
}

fn write_producer_channel_value<W, T: fmt::Display>(
    out: &mut W,
    metric_name: &str,
    record: &TelemetryRecord,
    channel_id: u8,
    value: T,
) -> Result<(), Error>
where
    W: fmt::Write + ?Sized,
{
    write!(out, "{metric_name}{{node=\"")?;
    write_escaped_label(out, record.config.name.as_str())?;
    writeln!(out, "\",channel=\"{channel_id}\"}} {value}")?;
    Ok(())
}

fn write_escaped_label<W>(out: &mut W, value: &str) -> Result<(), fmt::Error>
where
    W: fmt::Write + ?Sized,
{
    for ch in value.chars() {
        match ch {
            '\\' => out.write_str("\\\\")?,
            '"' => out.write_str("\\\"")?,
            '\n' => out.write_str("\\n")?,
            _ => out.write_char(ch)?,
        }
    }
    Ok(())
}

fn write_mac<W>(out: &mut W, mac: [u8; 6]) -> Result<(), fmt::Error>
where
    W: fmt::Write + ?Sized,
{
    write!(
        out,
        "{:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}",
        mac[0], mac[1], mac[2], mac[3], mac[4], mac[5]
    )
}

struct PerMille(u16);

impl fmt::Display for PerMille
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result
    {
        write!(f, "{}.{:03}", self.0 / 1000, self.0 % 1000)
    }
}

struct Tenths(i16);

impl fmt::Display for Tenths
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result
    {
        let value = i32::from(self.0);
        let abs = value.abs();
        if value < 0 {
            write!(f, "-")?;
        }
        write!(f, "{}.{}", abs / 10, abs % 10)
    }
}

#[cfg(test)]
mod tests
{
    use crate::TelemetryStore;
    use crate::config::{GatewayConfig, TelemetryProducerConfig, fixed_string};
    use crate::metrics::render_prometheus;
    use crate::telemetry::{FixedTelemetryStore, ProducerTelemetry};

    #[test]
    fn renders_producer_telemetry_as_prometheus_text()
    {
        let mut config = GatewayConfig::<4>::new("gate").unwrap();
        config.meshcore.public_key = Some(fixed_string("gateway-public-key").unwrap());
        config
            .add_producer(
                TelemetryProducerConfig::new("producer-public-key", "roof_repeater ").unwrap(),
            )
            .unwrap();

        let mut store = FixedTelemetryStore::from_config(&config).unwrap();
        let producer_id = config.producers[0].id();
        let mut telemetry = ProducerTelemetry::new(producer_id, 1_000);
        telemetry.set_channel_voltage(1, 4.08).unwrap();
        telemetry.set_channel_temperature(2, 22.5).unwrap();
        telemetry.set_channel_gas_resistance(3, 123.45).unwrap();
        telemetry.set_channel_voltage(4, 12.01).unwrap();
        telemetry.set_channel_current(4, -1.234).unwrap();
        telemetry.set_channel_power(4, 15.0).unwrap();
        telemetry.rssi = Some(-97);
        let gateway = store.gateway_metrics_mut();
        gateway.uptime_ms = 2_000;
        gateway.tx_power_level = Some(14);
        gateway.tx_output_dbm_tenths = Some(243);
        gateway.tx_output_milliwatts = Some(268);
        gateway.poll_retry_total = 2;
        gateway.poll_on_demand_total = 1;
        gateway.radio_rx_crc_error_total = 4;
        gateway.radio_rx_header_error_total = 5;
        gateway.radio_rx_timeout_total = 6;
        gateway.radio_device_error_bits = 7;
        gateway.radio_last_rssi_dbm = Some(-98);
        gateway.radio_last_snr_tenth_db = Some(34);
        gateway.radio_noise_floor_dbm = Some(-121);
        gateway.radio_rssi_inst_dbm = Some(-118);
        gateway.radio_chip_mode = Some(5);
        gateway.radio_last_applied_tx_power_dbm = Some(14);
        gateway.radio_last_tx_airtime_ms = Some(123);
        gateway.last_poll_latency_ms = Some(128);
        gateway.avg_poll_latency_ms = Some(128);
        gateway.wifi_connected = true;
        gateway.wifi_connect_started_ms = Some(1_100);
        gateway.wifi_connected_ms = Some(1_200);
        gateway.wifi_disconnect_total = 2;
        gateway.wifi_last_disconnect_reason = Some(2);
        gateway.wifi_connect_request_total = 3;
        gateway.wifi_connect_request_error_total = 1;
        gateway.wifi_deep_recovery_total = 1;
        gateway.wifi_rssi_dbm = Some(-55);
        gateway.wifi_channel = Some(6);
        gateway.wifi_bssid = Some([1, 2, 3, 4, 5, 6]);
        gateway.wifi_auth_mode = Some(3);
        gateway.wifi_tx_power_requested_quarter_dbm = Some(60);
        gateway.wifi_tx_power_applied_quarter_dbm = Some(60);
        gateway.dhcp_configured = true;
        gateway.dhcp_started_ms = Some(1_250);
        gateway.dhcp_configured_ms = Some(1_500);
        gateway.dhcp_configured_total = 1;
        gateway.dhcp_last_acquire_ms = Some(250);
        gateway.network_started_ms = Some(1_000);
        gateway.http_serving_started_ms = Some(1_600);
        gateway.network_startup_to_serving_ms = Some(600);
        gateway.smoltcp_poll_total = 44;
        gateway.last_smoltcp_poll_ms = Some(1_990);
        gateway.smoltcp_poll_gap_max_ms = 111;
        gateway.smoltcp_poll_delay_miss_total = 4;
        gateway.smoltcp_poll_bad_gap_total = 2;
        gateway.http_requests_total = 9;
        gateway.http_success_total = 8;
        gateway.http_send_error_total = 1;
        gateway.http_socket_abort_total = 3;
        gateway.http_active_sockets = 1;
        gateway.http_listening_sockets = 1;
        gateway.http_closed_sockets = 2;
        gateway.http_established_sockets = 1;
        gateway.http_close_wait_sockets = 1;
        gateway.http_time_wait_sockets = 1;
        gateway.http_oldest_socket_age_ms = Some(1234);
        gateway.last_http_request_ms = Some(1_750);
        gateway.last_http_success_ms = Some(1_700);
        gateway.network_recovery_total = 2;
        gateway.main_loop_gap_last_ms = 12;
        gateway.main_loop_gap_max_ms = 5_001;
        gateway.scheduler_tick_last_ms = 34;
        gateway.scheduler_tick_max_ms = 5_002;
        gateway.lora_service_last_ms = 56;
        gateway.lora_service_max_ms = 78;
        gateway.http_service_last_ms = 9;
        gateway.http_service_max_ms = 10;
        gateway.diagnostic_events = 3;
        gateway.diagnostic_dropped = 1;
        gateway.diagnostic_errors = 1;
        gateway.last_error_ms = Some(1_500);
        store.update_producer(producer_id, telemetry).unwrap();
        store.record_poll_success(producer_id, 1_000).unwrap();

        let rendered = render_prometheus::<4, 24576>(&store.snapshot()).unwrap();

        assert!(rendered.contains("muninn_gate_up 1"));
        assert!(rendered.contains("muninn_gate_tx_power_level 14"));
        assert!(rendered.contains("muninn_gate_tx_output_dbm 24.3"));
        assert!(rendered.contains("muninn_gate_gateway_poll_retry_total 2"));
        assert!(rendered.contains("muninn_gate_poll_on_demand_total 1"));
        assert!(rendered.contains("muninn_gate_radio_rx_crc_error_total 4"));
        assert!(rendered.contains("muninn_gate_radio_rx_header_error_total 5"));
        assert!(rendered.contains("muninn_gate_radio_rx_timeout_total 6"));
        assert!(rendered.contains("muninn_gate_radio_device_error_bits 7"));
        assert!(rendered.contains("muninn_gate_radio_last_rssi_dbm -98"));
        assert!(rendered.contains("muninn_gate_radio_last_snr_db 3.4"));
        assert!(rendered.contains("muninn_gate_radio_noise_floor_dbm -121"));
        assert!(rendered.contains("muninn_gate_radio_rssi_inst_dbm -118"));
        assert!(rendered.contains("muninn_gate_radio_chip_mode 5"));
        assert!(rendered.contains("muninn_gate_radio_last_applied_tx_power_dbm 14"));
        assert!(rendered.contains("muninn_gate_radio_last_tx_airtime_ms 123"));
        assert!(rendered.contains("muninn_gate_last_poll_latency_ms 128"));
        assert!(rendered.contains("muninn_gate_gateway_poll_success_rate 1.000"));
        assert!(rendered.contains("muninn_gate_wifi_connected 1"));
        assert!(rendered.contains("muninn_gate_wifi_disconnect_total 2"));
        assert!(rendered.contains("muninn_gate_wifi_last_disconnect_reason 2"));
        assert!(rendered.contains("muninn_gate_wifi_connect_request_total 3"));
        assert!(rendered.contains("muninn_gate_wifi_connect_request_error_total 1"));
        assert!(rendered.contains("muninn_gate_wifi_deep_recovery_total 1"));
        assert!(rendered.contains("muninn_gate_wifi_rssi_dbm -55"));
        assert!(rendered.contains("muninn_gate_wifi_channel 6"));
        assert!(rendered.contains("muninn_gate_wifi_ap_info{bssid=\"01:02:03:04:05:06\"} 1"));
        assert!(rendered.contains("muninn_gate_wifi_auth_mode 3"));
        assert!(rendered.contains("muninn_gate_wifi_tx_power_requested_quarter_dbm 60"));
        assert!(rendered.contains("muninn_gate_wifi_tx_power_applied_quarter_dbm 60"));
        assert!(rendered.contains("muninn_gate_dhcp_configured 1"));
        assert!(rendered.contains("muninn_gate_dhcp_configured_total 1"));
        assert!(rendered.contains("muninn_gate_dhcp_last_acquire_ms 250"));
        assert!(rendered.contains("muninn_gate_smoltcp_poll_total 44"));
        assert!(rendered.contains("muninn_gate_smoltcp_ms_since_last_poll 10"));
        assert!(rendered.contains("muninn_gate_smoltcp_poll_gap_max_ms 111"));
        assert!(rendered.contains("muninn_gate_smoltcp_poll_delay_miss_total 4"));
        assert!(rendered.contains("muninn_gate_smoltcp_poll_bad_gap_total 2"));
        assert!(rendered.contains("muninn_gate_http_requests_total 9"));
        assert!(rendered.contains("muninn_gate_http_success_total 8"));
        assert!(rendered.contains("muninn_gate_http_send_error_total 1"));
        assert!(rendered.contains("muninn_gate_http_socket_abort_total 3"));
        assert!(rendered.contains("muninn_gate_http_active_sockets 1"));
        assert!(rendered.contains("muninn_gate_http_listening_sockets 1"));
        assert!(rendered.contains("muninn_gate_http_closed_sockets 2"));
        assert!(rendered.contains("muninn_gate_http_established_sockets 1"));
        assert!(rendered.contains("muninn_gate_http_close_wait_sockets 1"));
        assert!(rendered.contains("muninn_gate_http_time_wait_sockets 1"));
        assert!(rendered.contains("muninn_gate_http_oldest_socket_age_ms 1234"));
        assert!(rendered.contains("muninn_gate_http_last_request_age_ms 250"));
        assert!(rendered.contains("muninn_gate_http_last_success_age_ms 300"));
        assert!(rendered.contains("muninn_gate_network_recovery_total 2"));
        assert!(rendered.contains("muninn_gate_main_loop_gap_last_ms 12"));
        assert!(rendered.contains("muninn_gate_main_loop_gap_max_ms 5001"));
        assert!(rendered.contains("muninn_gate_scheduler_tick_last_ms 34"));
        assert!(rendered.contains("muninn_gate_scheduler_tick_max_ms 5002"));
        assert!(rendered.contains("muninn_gate_lora_service_max_ms 78"));
        assert!(rendered.contains("muninn_gate_http_service_max_ms 10"));
        assert!(rendered.contains("muninn_gate_diagnostic_events 3"));
        assert!(rendered.contains("muninn_gate_diagnostic_dropped_total 1"));
        assert!(rendered.contains("muninn_gate_diagnostic_error_events 1"));
        assert!(rendered.contains("muninn_gate_last_error_age_ms 500"));
        assert!(!rendered.contains("muninn_gate_node_battery_voltage{"));
        assert!(!rendered.contains("muninn_gate_node_temperature_celsius{"));
        assert!(!rendered.contains("muninn_gate_node_humidity_percent{"));
        assert!(!rendered.contains("muninn_gate_node_pressure_pa{"));
        assert!(rendered.contains(
            "muninn_gate_node_channel_battery_voltage{node=\"roof_repeater\",channel=\"1\"} 4.08"
        ));
        assert!(rendered.contains(
            "muninn_gate_node_channel_temperature_celsius{node=\"roof_repeater\",channel=\"2\"} \
             22.5"
        ));
        assert!(rendered.contains(
            "muninn_gate_node_channel_gas_resistance_ohms{node=\"roof_repeater\",channel=\"3\"} \
             123.45"
        ));
        assert!(rendered.contains(
            "muninn_gate_node_channel_voltage{node=\"roof_repeater\",channel=\"4\"} 12.01"
        ));
        assert!(rendered.contains(
            "muninn_gate_node_channel_current_amps{node=\"roof_repeater\",channel=\"4\"} -1.234"
        ));
        assert!(rendered.contains(
            "muninn_gate_node_channel_power_watts{node=\"roof_repeater\",channel=\"4\"} 15"
        ));
        assert!(rendered.contains("muninn_gate_node_rssi{node=\"roof_repeater\"} -97"));
        assert!(
            rendered.contains("muninn_gate_node_last_heard_age_ms{node=\"roof_repeater\"} 1000")
        );
        assert!(rendered.contains("muninn_gate_poll_success_total{node=\"roof_repeater\"} 1"));
        assert!(rendered.contains("muninn_gate_poll_success_rate{node=\"roof_repeater\"} 1.000"));
        assert!(
            rendered.contains("muninn_gate_node_last_poll_age_ms{node=\"roof_repeater\"} 1000")
        );
        assert_family_sample_before_next_help(
            &rendered,
            "muninn_gate_poll_success_total",
            "muninn_gate_poll_failure_total",
            "muninn_gate_poll_success_total{node=\"roof_repeater\"} 1",
        );
    }

    #[test]
    fn escapes_prometheus_label_values()
    {
        let mut config = GatewayConfig::<1>::new("gate").unwrap();
        config.meshcore.public_key = Some(fixed_string("gateway-public-key").unwrap());
        config
            .add_producer(TelemetryProducerConfig::new("producer-public-key", "lab\"node").unwrap())
            .unwrap();

        let store = FixedTelemetryStore::from_config(&config).unwrap();
        let rendered = render_prometheus::<1, 8192>(&store.snapshot()).unwrap();

        assert!(rendered.contains("muninn_gate_poll_success_total{node=\"lab\\\"node\"} 0"));
    }

    fn assert_family_sample_before_next_help(
        rendered: &str,
        family: &str,
        next_family: &str,
        sample: &str,
    )
    {
        let family_sample = find_required(rendered, &format!("{family}{{"));
        let sample = find_required(rendered, sample);
        let next_sample = find_required(rendered, &format!("{next_family}{{"));

        assert!(family_sample <= sample);
        assert!(sample < next_sample);
    }

    fn find_required(rendered: &str, needle: &str) -> usize
    {
        rendered
            .find(needle)
            .unwrap_or_else(|| panic!("missing expected output: {needle}"))
    }
}
