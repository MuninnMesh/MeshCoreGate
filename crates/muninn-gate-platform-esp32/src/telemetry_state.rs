//! Shared ESP32 telemetry store used by HTTP, serial, display, and poll tasks.

use core::cell::RefCell;

use critical_section::Mutex;
use muninn_gate_core::config::MAX_TELEMETRY_PRODUCERS;
use muninn_gate_core::{
    DiagnosticLevel,
    Error,
    FixedTelemetryStore,
    GatewayConfig,
    GatewayMetrics,
    HttpSocketStateSnapshot,
    PollFailureReason,
    PollTelemetryStore,
    ProducerTelemetry,
    TelemetryProducerId,
    TelemetrySnapshot,
    TelemetryStore,
    TxPowerMapping,
};
use muninn_mesh_radio::RxStats;

use crate::{diagnostics, platform, radio};

/// Shared telemetry store capacity for the ESP32 firmware.
pub const ESP32_TELEMETRY_PRODUCER_LIMIT: usize = MAX_TELEMETRY_PRODUCERS;

static TELEMETRY_STORE: Mutex<RefCell<FixedTelemetryStore<ESP32_TELEMETRY_PRODUCER_LIMIT>>> =
    Mutex::new(RefCell::new(FixedTelemetryStore::new()));

/// Per-producer timestamp (gateway ms) of the last GET_STATUS poll attempt, used
/// to throttle status polling to a slower cadence than telemetry.
static STATUS_POLL_LAST_MS: Mutex<
    RefCell<[Option<(TelemetryProducerId, u64)>; ESP32_TELEMETRY_PRODUCER_LIMIT]>,
> = Mutex::new(RefCell::new([None; ESP32_TELEMETRY_PRODUCER_LIMIT]));

/// Returns true when a GET_STATUS poll is due for `producer_id` (at least
/// `interval_ms` elapsed since the last attempt, or it was never polled).
pub fn status_poll_due(producer_id: TelemetryProducerId, now_ms: u64, interval_ms: u64) -> bool
{
    critical_section::with(|cs| {
        let slots = STATUS_POLL_LAST_MS.borrow_ref(cs);
        for slot in slots.iter().flatten() {
            if slot.0 == producer_id {
                return now_ms.saturating_sub(slot.1) >= interval_ms;
            }
        }
        true
    })
}

/// Record a GET_STATUS poll attempt for `producer_id` at `now_ms`.
pub fn mark_status_polled(producer_id: TelemetryProducerId, now_ms: u64)
{
    critical_section::with(|cs| {
        let mut slots = STATUS_POLL_LAST_MS.borrow_ref_mut(cs);
        for slot in slots.iter_mut() {
            if let Some(entry) = slot {
                if entry.0 == producer_id {
                    entry.1 = now_ms;
                    return;
                }
            }
        }
        for slot in slots.iter_mut() {
            if slot.is_none() {
                *slot = Some((producer_id, now_ms));
                return;
            }
        }
    })
}

/// Shared telemetry store adapter used by the scheduler runtime.
#[derive(Debug, Default)]
pub struct SharedTelemetryStore;

impl PollTelemetryStore<ESP32_TELEMETRY_PRODUCER_LIMIT> for SharedTelemetryStore
{
    fn update_producer(
        &mut self,
        producer_id: TelemetryProducerId,
        telemetry: ProducerTelemetry,
    ) -> Result<(), Error>
    {
        critical_section::with(|cs| {
            let mut store = TELEMETRY_STORE.borrow_ref_mut(cs);
            TelemetryStore::update_producer(&mut *store, producer_id, telemetry)
        })
    }

    fn record_poll_success(
        &mut self,
        producer_id: TelemetryProducerId,
        timestamp_ms: u64,
    ) -> Result<(), Error>
    {
        critical_section::with(|cs| {
            let mut store = TELEMETRY_STORE.borrow_ref_mut(cs);
            TelemetryStore::record_poll_success(&mut *store, producer_id, timestamp_ms)
        })
    }

    fn record_poll_failure(
        &mut self,
        producer_id: TelemetryProducerId,
        timestamp_ms: u64,
    ) -> Result<(), Error>
    {
        critical_section::with(|cs| {
            let mut store = TELEMETRY_STORE.borrow_ref_mut(cs);
            TelemetryStore::record_poll_failure(&mut *store, producer_id, timestamp_ms)
        })
    }
}

/// Initialize the shared telemetry store from the loaded gateway config.
pub fn reset_from_config(
    config: &GatewayConfig<MAX_TELEMETRY_PRODUCERS>,
    now_ms: u64,
    tx_power: TxPowerMapping,
) -> Result<(), Error>
{
    let mut store = FixedTelemetryStore::from_config(config)?;
    store.set_gateway_metrics(initial_gateway_metrics(now_ms, tx_power));
    critical_section::with(|cs| {
        *TELEMETRY_STORE.borrow_ref_mut(cs) = store;
    });
    Ok(())
}

/// Refresh gateway runtime metrics without changing producer telemetry.
pub fn refresh_gateway_metrics(now_ms: u64, tx_power: TxPowerMapping)
{
    critical_section::with(|cs| {
        let mut store = TELEMETRY_STORE.borrow_ref_mut(cs);
        let metrics = store.gateway_metrics_mut();
        update_runtime_metrics(metrics, now_ms, tx_power);
        refresh_diagnostic_metrics(metrics);
    });
}

/// Update the gateway radio RX packet counter.
pub fn update_radio_rx_total(rx_total: u64)
{
    critical_section::with(|cs| {
        TELEMETRY_STORE
            .borrow_ref_mut(cs)
            .gateway_metrics_mut()
            .radio_rx_total = rx_total;
    });
}

/// Publish the latest radio health snapshot into gateway metrics.
pub fn update_radio_health(
    now_ms: u64,
    tx_power: TxPowerMapping,
    stats: RxStats,
    last_tx_airtime_ms: u32,
)
{
    critical_section::with(|cs| {
        let mut store = TELEMETRY_STORE.borrow_ref_mut(cs);
        let metrics = store.gateway_metrics_mut();
        update_runtime_metrics(metrics, now_ms, tx_power);
        metrics.radio_rx_total = u64::from(stats.rx_frames);
        metrics.radio_rx_crc_error_total = u64::from(stats.rx_crc_error);
        metrics.radio_rx_header_error_total = u64::from(stats.rx_header_error);
        metrics.radio_rx_timeout_total = u64::from(stats.rx_timeout);
        metrics.radio_device_error_bits = stats.device_errors;
        metrics.radio_last_rssi_dbm = valid_radio_i16(stats.last_rssi_dbm);
        metrics.radio_last_snr_tenth_db = Some(stats.last_snr_tenth_db);
        metrics.radio_noise_floor_dbm = valid_radio_i16(stats.last_noise_floor_dbm);
        metrics.radio_rssi_inst_dbm = if stats.has_rssi_inst {
            valid_radio_i16(stats.last_rssi_inst_dbm)
        } else {
            None
        };
        metrics.radio_chip_mode = if stats.chip_mode == 0 {
            None
        } else {
            Some(stats.chip_mode)
        };
        metrics.radio_last_applied_tx_power_dbm = stats.last_applied_tx_power_dbm;
        metrics.radio_last_tx_airtime_ms = if last_tx_airtime_ms == 0 {
            None
        } else {
            Some(last_tx_airtime_ms)
        };
        refresh_diagnostic_metrics(metrics);
    });
}

/// Increment the gateway radio TX packet counter.
pub fn increment_radio_tx_total()
{
    critical_section::with(|cs| {
        let mut store = TELEMETRY_STORE.borrow_ref_mut(cs);
        let metrics = store.gateway_metrics_mut();
        metrics.radio_tx_total = metrics.radio_tx_total.saturating_add(1);
    });
}

/// Update one producer from a passive MeshCore frame observation.
pub fn record_producer_observation(
    producer_id: TelemetryProducerId,
    telemetry: ProducerTelemetry,
) -> Result<(), Error>
{
    critical_section::with(|cs| {
        let mut store = TELEMETRY_STORE.borrow_ref_mut(cs);
        store.merge_producer(producer_id, telemetry)
    })
}

/// Store a concise producer poll failure reason.
pub fn record_producer_poll_error(
    producer_id: TelemetryProducerId,
    reason: PollFailureReason,
) -> Result<(), Error>
{
    critical_section::with(|cs| {
        TELEMETRY_STORE
            .borrow_ref_mut(cs)
            .set_poll_error(producer_id, reason)
    })
}

/// Record one scheduler pass summary in gateway metrics.
pub fn record_poll_pass(retrying: u32, poll_on_demand_requests: u32, latency_ms: Option<u32>)
{
    critical_section::with(|cs| {
        let mut store = TELEMETRY_STORE.borrow_ref_mut(cs);
        let metrics = store.gateway_metrics_mut();
        metrics.poll_retry_total = metrics.poll_retry_total.saturating_add(u64::from(retrying));
        metrics.poll_on_demand_total = metrics
            .poll_on_demand_total
            .saturating_add(u64::from(poll_on_demand_requests));
        if let Some(latency_ms) = latency_ms {
            metrics.last_poll_latency_ms = Some(latency_ms);
            metrics.avg_poll_latency_ms = Some(match metrics.avg_poll_latency_ms {
                Some(previous) => previous.saturating_add(latency_ms) / 2,
                None => latency_ms,
            });
        }
    });
}

/// Record one accepted HTTP request.
pub fn record_http_request(now_ms: u64)
{
    critical_section::with(|cs| {
        let mut store = TELEMETRY_STORE.borrow_ref_mut(cs);
        let metrics = store.gateway_metrics_mut();
        metrics.http_requests_total = metrics.http_requests_total.saturating_add(1);
        metrics.last_http_request_ms = Some(now_ms);
    });
}

/// Record one completed HTTP response.
pub fn record_http_success(now_ms: u64)
{
    critical_section::with(|cs| {
        let mut store = TELEMETRY_STORE.borrow_ref_mut(cs);
        let metrics = store.gateway_metrics_mut();
        metrics.http_success_total = metrics.http_success_total.saturating_add(1);
        metrics.last_http_success_ms = Some(now_ms);
    });
}

/// Record an HTTP send failure.
pub fn record_http_send_error()
{
    critical_section::with(|cs| {
        let mut store = TELEMETRY_STORE.borrow_ref_mut(cs);
        let metrics = store.gateway_metrics_mut();
        metrics.http_send_error_total = metrics.http_send_error_total.saturating_add(1);
    });
}

/// Record explicitly aborted HTTP sockets.
pub fn record_http_socket_aborts(count: u8)
{
    if count == 0 {
        return;
    }

    critical_section::with(|cs| {
        let mut store = TELEMETRY_STORE.borrow_ref_mut(cs);
        let metrics = store.gateway_metrics_mut();
        metrics.http_socket_abort_total = metrics
            .http_socket_abort_total
            .saturating_add(u64::from(count));
    });
}

/// Publish the current HTTP socket pool state.
pub fn record_http_socket_state(active: u8, listening: u8)
{
    critical_section::with(|cs| {
        let mut store = TELEMETRY_STORE.borrow_ref_mut(cs);
        let metrics = store.gateway_metrics_mut();
        metrics.http_active_sockets = active;
        metrics.http_listening_sockets = listening;
    });
}

/// Publish the current HTTP socket pool state.
pub fn record_http_socket_states(states: HttpSocketStateSnapshot)
{
    critical_section::with(|cs| {
        let mut store = TELEMETRY_STORE.borrow_ref_mut(cs);
        let metrics = store.gateway_metrics_mut();
        metrics.http_active_sockets = states.active;
        metrics.http_listening_sockets = states.listening;
        metrics.http_closed_sockets = states.closed;
        metrics.http_syn_sent_sockets = states.syn_sent;
        metrics.http_syn_received_sockets = states.syn_received;
        metrics.http_established_sockets = states.established;
        metrics.http_fin_wait_1_sockets = states.fin_wait_1;
        metrics.http_fin_wait_2_sockets = states.fin_wait_2;
        metrics.http_close_wait_sockets = states.close_wait;
        metrics.http_closing_sockets = states.closing;
        metrics.http_last_ack_sockets = states.last_ack;
        metrics.http_time_wait_sockets = states.time_wait;
        metrics.http_oldest_socket_age_ms = states.oldest_socket_age_ms;
        metrics.http_oldest_syn_sent_ms = states.oldest_syn_sent_ms;
        metrics.http_oldest_syn_received_ms = states.oldest_syn_received_ms;
        metrics.http_oldest_established_ms = states.oldest_established_ms;
        metrics.http_oldest_fin_wait_1_ms = states.oldest_fin_wait_1_ms;
        metrics.http_oldest_fin_wait_2_ms = states.oldest_fin_wait_2_ms;
        metrics.http_oldest_close_wait_ms = states.oldest_close_wait_ms;
        metrics.http_oldest_closing_ms = states.oldest_closing_ms;
        metrics.http_oldest_last_ack_ms = states.oldest_last_ack_ms;
        metrics.http_oldest_time_wait_ms = states.oldest_time_wait_ms;
    });
}

/// Record the start of a network startup/recovery attempt.
pub fn record_network_started(now_ms: u64)
{
    critical_section::with(|cs| {
        let mut store = TELEMETRY_STORE.borrow_ref_mut(cs);
        let metrics = store.gateway_metrics_mut();
        metrics.network_started_ms = Some(now_ms);
        metrics.http_serving_started_ms = None;
        metrics.network_startup_to_serving_ms = None;
    });
}

/// Record whether WiFi is currently associated.
pub fn record_wifi_connected(connected: bool)
{
    critical_section::with(|cs| {
        TELEMETRY_STORE
            .borrow_ref_mut(cs)
            .gateway_metrics_mut()
            .wifi_connected = connected;
    });
}

/// Record that a WiFi connect request is about to be issued.
pub fn record_wifi_connect_started(now_ms: u64)
{
    critical_section::with(|cs| {
        TELEMETRY_STORE
            .borrow_ref_mut(cs)
            .gateway_metrics_mut()
            .wifi_connect_started_ms = Some(now_ms);
    });
}

/// Record that WiFi has reached associated state.
pub fn record_wifi_connected_at(now_ms: u64)
{
    critical_section::with(|cs| {
        let mut store = TELEMETRY_STORE.borrow_ref_mut(cs);
        let metrics = store.gateway_metrics_mut();
        metrics.wifi_connected = true;
        metrics.wifi_connected_ms = Some(now_ms);
    });
}

/// Record one or more WiFi disconnect events observed by the ESP-IDF event hook.
pub fn record_wifi_disconnects(count: u32, reason: Option<u8>)
{
    if count == 0 {
        return;
    }

    critical_section::with(|cs| {
        let mut store = TELEMETRY_STORE.borrow_ref_mut(cs);
        let metrics = store.gateway_metrics_mut();
        metrics.wifi_connected = false;
        metrics.wifi_disconnect_total = metrics
            .wifi_disconnect_total
            .saturating_add(u64::from(count));
        metrics.wifi_last_disconnect_reason = reason;
    });
}

/// Record one WiFi connect request and whether the request failed immediately.
pub fn record_wifi_connect_request(failed: bool)
{
    critical_section::with(|cs| {
        let mut store = TELEMETRY_STORE.borrow_ref_mut(cs);
        let metrics = store.gateway_metrics_mut();
        metrics.wifi_connect_request_total = metrics.wifi_connect_request_total.saturating_add(1);
        if failed {
            metrics.wifi_connect_request_error_total =
                metrics.wifi_connect_request_error_total.saturating_add(1);
        }
    });
}

/// Record one WiFi controller deep-recovery action.
pub fn record_wifi_deep_recovery()
{
    critical_section::with(|cs| {
        let mut store = TELEMETRY_STORE.borrow_ref_mut(cs);
        let metrics = store.gateway_metrics_mut();
        metrics.wifi_deep_recovery_total = metrics.wifi_deep_recovery_total.saturating_add(1);
    });
}

/// Publish the latest associated AP information.
pub fn record_wifi_ap_info(rssi_dbm: i16, channel: u8, bssid: [u8; 6], auth_mode: Option<u8>)
{
    critical_section::with(|cs| {
        let mut store = TELEMETRY_STORE.borrow_ref_mut(cs);
        let metrics = store.gateway_metrics_mut();
        metrics.wifi_connected = true;
        metrics.wifi_rssi_dbm = Some(rssi_dbm);
        metrics.wifi_channel = Some(channel);
        metrics.wifi_bssid = Some(bssid);
        metrics.wifi_auth_mode = auth_mode;
    });
}

/// Publish requested and applied ESP WiFi TX power caps.
pub fn record_wifi_tx_power(requested_quarter_dbm: i8, applied_quarter_dbm: i8)
{
    critical_section::with(|cs| {
        let mut store = TELEMETRY_STORE.borrow_ref_mut(cs);
        let metrics = store.gateway_metrics_mut();
        metrics.wifi_tx_power_requested_quarter_dbm = Some(requested_quarter_dbm);
        metrics.wifi_tx_power_applied_quarter_dbm = Some(applied_quarter_dbm);
    });
}

/// Record one DHCP state-machine reset.
pub fn record_dhcp_reset()
{
    critical_section::with(|cs| {
        let mut store = TELEMETRY_STORE.borrow_ref_mut(cs);
        let metrics = store.gateway_metrics_mut();
        metrics.dhcp_configured = false;
        metrics.dhcp_reset_total = metrics.dhcp_reset_total.saturating_add(1);
    });
}

/// Record the start of DHCP acquisition.
pub fn record_dhcp_started(now_ms: u64)
{
    critical_section::with(|cs| {
        let mut store = TELEMETRY_STORE.borrow_ref_mut(cs);
        let metrics = store.gateway_metrics_mut();
        metrics.dhcp_started_ms = Some(now_ms);
        metrics.dhcp_configured_ms = None;
        metrics.dhcp_configured = false;
    });
}

/// Record a DHCP IPv4 lease.
pub fn record_dhcp_configured(
    configured_ms: u64,
    acquire_ms: u32,
    ip: [u8; 4],
    gateway: Option<[u8; 4]>,
)
{
    critical_section::with(|cs| {
        let mut store = TELEMETRY_STORE.borrow_ref_mut(cs);
        let metrics = store.gateway_metrics_mut();
        metrics.dhcp_configured = true;
        metrics.dhcp_configured_ms = Some(configured_ms);
        metrics.dhcp_configured_total = metrics.dhcp_configured_total.saturating_add(1);
        metrics.dhcp_last_acquire_ms = Some(acquire_ms);
        metrics.dhcp_ip = Some(ip);
        metrics.dhcp_gateway = gateway;
    });
}

/// Record loss of DHCP IPv4 configuration.
pub fn record_dhcp_deconfigured()
{
    critical_section::with(|cs| {
        let mut store = TELEMETRY_STORE.borrow_ref_mut(cs);
        let metrics = store.gateway_metrics_mut();
        metrics.dhcp_configured = false;
        metrics.dhcp_deconfigured_total = metrics.dhcp_deconfigured_total.saturating_add(1);
        metrics.dhcp_ip = None;
        metrics.dhcp_gateway = None;
    });
}

/// Record that HTTP has begun serving for the current network startup.
pub fn record_http_serving_started(now_ms: u64)
{
    critical_section::with(|cs| {
        let mut store = TELEMETRY_STORE.borrow_ref_mut(cs);
        let metrics = store.gateway_metrics_mut();
        metrics.http_serving_started_ms = Some(now_ms);
        metrics.network_startup_to_serving_ms = metrics
            .network_started_ms
            .map(|started_ms| now_ms.saturating_sub(started_ms).min(u64::from(u32::MAX)) as u32);
    });
}

/// Record one DHCP timeout.
pub fn record_dhcp_timeout()
{
    critical_section::with(|cs| {
        let mut store = TELEMETRY_STORE.borrow_ref_mut(cs);
        let metrics = store.gateway_metrics_mut();
        metrics.dhcp_configured = false;
        metrics.dhcp_timeout_total = metrics.dhcp_timeout_total.saturating_add(1);
    });
}

/// Record one smoltcp interface poll and its gap from the previous poll.
pub fn record_smoltcp_poll(now_ms: u64, gap_ms: Option<u32>, delay_missed: bool, bad_gap: bool)
{
    critical_section::with(|cs| {
        let mut store = TELEMETRY_STORE.borrow_ref_mut(cs);
        let metrics = store.gateway_metrics_mut();
        if let Some(gap_ms) = gap_ms {
            metrics.smoltcp_poll_gap_max_ms = metrics.smoltcp_poll_gap_max_ms.max(gap_ms);
        }
        if delay_missed {
            metrics.smoltcp_poll_delay_miss_total =
                metrics.smoltcp_poll_delay_miss_total.saturating_add(1);
        }
        if bad_gap {
            metrics.smoltcp_poll_bad_gap_total =
                metrics.smoltcp_poll_bad_gap_total.saturating_add(1);
        }
        metrics.last_smoltcp_poll_ms = Some(now_ms);
        metrics.smoltcp_poll_total = metrics.smoltcp_poll_total.saturating_add(1);
    });
}

/// Record one firmware-forced network recovery action.
pub fn record_network_recovery()
{
    critical_section::with(|cs| {
        let mut store = TELEMETRY_STORE.borrow_ref_mut(cs);
        let metrics = store.gateway_metrics_mut();
        metrics.network_recovery_total = metrics.network_recovery_total.saturating_add(1);
    });
}

/// Record a main-loop scheduling gap.
pub fn record_main_loop_gap(gap_ms: u32)
{
    critical_section::with(|cs| {
        let mut store = TELEMETRY_STORE.borrow_ref_mut(cs);
        let metrics = store.gateway_metrics_mut();
        metrics.main_loop_gap_last_ms = gap_ms;
        metrics.main_loop_gap_max_ms = metrics.main_loop_gap_max_ms.max(gap_ms);
    });
}

/// Record a scheduler tick duration.
pub fn record_scheduler_tick(duration_ms: u32)
{
    critical_section::with(|cs| {
        let mut store = TELEMETRY_STORE.borrow_ref_mut(cs);
        let metrics = store.gateway_metrics_mut();
        metrics.scheduler_tick_last_ms = duration_ms;
        metrics.scheduler_tick_max_ms = metrics.scheduler_tick_max_ms.max(duration_ms);
    });
}

/// Record a cooperative LoRa service pass duration.
pub fn record_lora_service(duration_ms: u32)
{
    critical_section::with(|cs| {
        let mut store = TELEMETRY_STORE.borrow_ref_mut(cs);
        let metrics = store.gateway_metrics_mut();
        metrics.lora_service_last_ms = duration_ms;
        metrics.lora_service_max_ms = metrics.lora_service_max_ms.max(duration_ms);
    });
}

/// Record an HTTP service pass duration.
pub fn record_http_service(duration_ms: u32)
{
    critical_section::with(|cs| {
        let mut store = TELEMETRY_STORE.borrow_ref_mut(cs);
        let metrics = store.gateway_metrics_mut();
        metrics.http_service_last_ms = duration_ms;
        metrics.http_service_max_ms = metrics.http_service_max_ms.max(duration_ms);
    });
}

/// Record a display refresh duration.
pub fn record_display_refresh(duration_ms: u32)
{
    critical_section::with(|cs| {
        let mut store = TELEMETRY_STORE.borrow_ref_mut(cs);
        let metrics = store.gateway_metrics_mut();
        metrics.display_refresh_last_ms = duration_ms;
        metrics.display_refresh_max_ms = metrics.display_refresh_max_ms.max(duration_ms);
    });
}

/// Record a serial output duration.
pub fn record_serial_emit(duration_ms: u32)
{
    critical_section::with(|cs| {
        let mut store = TELEMETRY_STORE.borrow_ref_mut(cs);
        let metrics = store.gateway_metrics_mut();
        metrics.serial_emit_last_ms = duration_ms;
        metrics.serial_emit_max_ms = metrics.serial_emit_max_ms.max(duration_ms);
    });
}

/// Return a copy of the current telemetry snapshot.
pub fn snapshot() -> TelemetrySnapshot<ESP32_TELEMETRY_PRODUCER_LIMIT>
{
    critical_section::with(|cs| TELEMETRY_STORE.borrow_ref(cs).snapshot())
}

fn initial_gateway_metrics(now_ms: u64, tx_power: TxPowerMapping) -> GatewayMetrics
{
    let battery_voltage_mv = battery_voltage_mv();
    GatewayMetrics {
        uptime_ms: now_ms,
        poll_success_total: 0,
        poll_failure_total: 0,
        poll_retry_total: 0,
        poll_on_demand_total: 0,
        radio_rx_total: 0,
        radio_tx_total: 0,
        radio_rx_crc_error_total: 0,
        radio_rx_header_error_total: 0,
        radio_rx_timeout_total: 0,
        radio_device_error_bits: 0,
        radio_last_rssi_dbm: None,
        radio_last_snr_tenth_db: None,
        radio_noise_floor_dbm: None,
        radio_rssi_inst_dbm: None,
        radio_chip_mode: None,
        radio_last_applied_tx_power_dbm: None,
        radio_last_tx_airtime_ms: None,
        last_poll_ms: None,
        tx_power_level: Some(tx_power.selected_level),
        tx_output_dbm_tenths: tx_power.output_dbm_tenths,
        tx_output_milliwatts: tx_power.output_milliwatts,
        battery_voltage_mv,
        battery_percent: battery_voltage_mv.map(battery_percent_from_mv),
        last_poll_latency_ms: None,
        avg_poll_latency_ms: None,
        wifi_connected: false,
        wifi_connect_started_ms: None,
        wifi_connected_ms: None,
        wifi_disconnect_total: 0,
        wifi_last_disconnect_reason: None,
        wifi_connect_request_total: 0,
        wifi_connect_request_error_total: 0,
        wifi_deep_recovery_total: 0,
        wifi_rssi_dbm: None,
        wifi_channel: None,
        wifi_bssid: None,
        wifi_auth_mode: None,
        wifi_tx_power_requested_quarter_dbm: None,
        wifi_tx_power_applied_quarter_dbm: None,
        dhcp_configured: false,
        dhcp_started_ms: None,
        dhcp_configured_ms: None,
        dhcp_configured_total: 0,
        dhcp_deconfigured_total: 0,
        dhcp_reset_total: 0,
        dhcp_timeout_total: 0,
        dhcp_last_acquire_ms: None,
        dhcp_ip: None,
        dhcp_gateway: None,
        network_started_ms: None,
        http_serving_started_ms: None,
        network_startup_to_serving_ms: None,
        smoltcp_poll_total: 0,
        last_smoltcp_poll_ms: None,
        smoltcp_poll_gap_max_ms: 0,
        smoltcp_poll_delay_miss_total: 0,
        smoltcp_poll_bad_gap_total: 0,
        http_requests_total: 0,
        http_success_total: 0,
        http_send_error_total: 0,
        http_socket_abort_total: 0,
        http_active_sockets: 0,
        http_listening_sockets: 0,
        http_closed_sockets: 0,
        http_syn_sent_sockets: 0,
        http_syn_received_sockets: 0,
        http_established_sockets: 0,
        http_fin_wait_1_sockets: 0,
        http_fin_wait_2_sockets: 0,
        http_close_wait_sockets: 0,
        http_closing_sockets: 0,
        http_last_ack_sockets: 0,
        http_time_wait_sockets: 0,
        http_oldest_socket_age_ms: None,
        http_oldest_syn_sent_ms: None,
        http_oldest_syn_received_ms: None,
        http_oldest_established_ms: None,
        http_oldest_fin_wait_1_ms: None,
        http_oldest_fin_wait_2_ms: None,
        http_oldest_close_wait_ms: None,
        http_oldest_closing_ms: None,
        http_oldest_last_ack_ms: None,
        http_oldest_time_wait_ms: None,
        last_http_request_ms: None,
        last_http_success_ms: None,
        network_recovery_total: 0,
        main_loop_gap_last_ms: 0,
        main_loop_gap_max_ms: 0,
        scheduler_tick_last_ms: 0,
        scheduler_tick_max_ms: 0,
        lora_service_last_ms: 0,
        lora_service_max_ms: 0,
        http_service_last_ms: 0,
        http_service_max_ms: 0,
        display_refresh_last_ms: 0,
        display_refresh_max_ms: 0,
        serial_emit_last_ms: 0,
        serial_emit_max_ms: 0,
        free_heap_bytes: platform::free_heap_bytes(),
        diagnostic_events: 0,
        diagnostic_dropped: 0,
        diagnostic_errors: 0,
        last_error_ms: None,
    }
}

fn update_runtime_metrics(metrics: &mut GatewayMetrics, now_ms: u64, tx_power: TxPowerMapping)
{
    let battery_voltage_mv = battery_voltage_mv();
    metrics.uptime_ms = now_ms;
    metrics.tx_power_level = Some(tx_power.selected_level);
    metrics.tx_output_dbm_tenths = tx_power.output_dbm_tenths;
    metrics.tx_output_milliwatts = tx_power.output_milliwatts;
    metrics.battery_voltage_mv = battery_voltage_mv;
    metrics.battery_percent = battery_voltage_mv.map(battery_percent_from_mv);
    metrics.free_heap_bytes = platform::free_heap_bytes();
}

fn battery_voltage_mv() -> Option<u16>
{
    match radio::battery_mv() {
        0 => None,
        value => Some(value),
    }
}

fn battery_percent_from_mv(mv: u16) -> u8
{
    const EMPTY_MV: u16 = 3_000;
    const FULL_MV: u16 = 4_200;

    let clamped = mv.clamp(EMPTY_MV, FULL_MV);
    (((u32::from(clamped - EMPTY_MV)) * 100) / u32::from(FULL_MV - EMPTY_MV)) as u8
}

fn valid_radio_i16(value: i16) -> Option<i16>
{
    if value == i16::MIN { None } else { Some(value) }
}

fn refresh_diagnostic_metrics(metrics: &mut GatewayMetrics)
{
    let snapshot = diagnostics::snapshot();
    metrics.diagnostic_events = snapshot.events.len() as u32;
    metrics.diagnostic_dropped = snapshot.dropped_total;
    metrics.diagnostic_errors = snapshot
        .events
        .iter()
        .filter(|event| event.level == DiagnosticLevel::Error)
        .count() as u32;
    metrics.last_error_ms = snapshot
        .events
        .iter()
        .rev()
        .find(|event| event.level == DiagnosticLevel::Error)
        .map(|event| event.timestamp_ms);
}
