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
        TelemetryStore::update_producer(&mut *store, producer_id, telemetry)
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
