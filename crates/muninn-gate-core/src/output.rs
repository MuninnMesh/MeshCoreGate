//! Serial JSON rendering helpers.

#[cfg(feature = "serial-json")]
use core::fmt::Write;

#[cfg(feature = "serial-json")]
use heapless::String;

#[cfg(feature = "serial-json")]
use crate::{
    Error,
    GatewayMetrics,
    ProducerTelemetry,
    ProducerTelemetryChannel,
    TelemetryMetrics,
    TelemetryRecord,
};

/// Render gateway telemetry as a newline-delimited JSON payload.
#[cfg(feature = "serial-json")]
pub fn render_gateway_serial_json<const OUT: usize>(
    metrics: &GatewayMetrics,
) -> Result<String<OUT>, Error>
{
    let mut out = String::new();
    write!(out, "{{\"type\":\"gateway_telemetry\"")?;
    write!(out, ",\"uptime_ms\":{}", metrics.uptime_ms)?;
    write!(
        out,
        ",\"poll_success_total\":{},\"poll_failure_total\":{}",
        metrics.poll_success_total, metrics.poll_failure_total
    )?;
    write!(
        out,
        ",\"poll_retry_total\":{},\"poll_on_demand_total\":{}",
        metrics.poll_retry_total, metrics.poll_on_demand_total
    )?;
    write!(
        out,
        ",\"radio_rx_total\":{},\"radio_tx_total\":{}",
        metrics.radio_rx_total, metrics.radio_tx_total
    )?;
    write!(
        out,
        ",\"radio_rx_crc_error_total\":{},\"radio_rx_header_error_total\":{}",
        metrics.radio_rx_crc_error_total, metrics.radio_rx_header_error_total
    )?;
    write!(
        out,
        ",\"radio_rx_timeout_total\":{},\"radio_device_error_bits\":{}",
        metrics.radio_rx_timeout_total, metrics.radio_device_error_bits
    )?;

    if let Some(rssi) = metrics.radio_last_rssi_dbm {
        write!(out, ",\"radio_last_rssi_dbm\":{}", rssi)?;
    }
    if let Some(snr) = metrics.radio_last_snr_tenth_db {
        write!(out, ",\"radio_last_snr_db\":")?;
        write_tenths(&mut out, snr)?;
    }
    if let Some(noise_floor) = metrics.radio_noise_floor_dbm {
        write!(out, ",\"radio_noise_floor_dbm\":{}", noise_floor)?;
    }
    if let Some(rssi_inst) = metrics.radio_rssi_inst_dbm {
        write!(out, ",\"radio_rssi_inst_dbm\":{}", rssi_inst)?;
    }
    if let Some(chip_mode) = metrics.radio_chip_mode {
        write!(out, ",\"radio_chip_mode\":{}", chip_mode)?;
    }
    if let Some(tx_power) = metrics.radio_last_applied_tx_power_dbm {
        write!(out, ",\"radio_last_applied_tx_power_dbm\":{}", tx_power)?;
    }
    if let Some(airtime_ms) = metrics.radio_last_tx_airtime_ms {
        write!(out, ",\"radio_last_tx_airtime_ms\":{}", airtime_ms)?;
    }
    if let Some(rate) = metrics.poll_success_rate_per_mille() {
        write!(out, ",\"poll_success_rate\":")?;
        write_per_mille(&mut out, rate)?;
    }
    if let Some(last_poll_ms) = metrics.last_poll_ms {
        write!(out, ",\"last_poll_ms\":{}", last_poll_ms)?;
    }
    if let Some(tx_power_level) = metrics.tx_power_level {
        write!(out, ",\"tx_power_level\":{}", tx_power_level)?;
    }
    if let Some(tx_output_dbm_tenths) = metrics.tx_output_dbm_tenths {
        write!(out, ",\"tx_output_dbm\":")?;
        write_tenths(&mut out, tx_output_dbm_tenths)?;
    }
    if let Some(tx_output_milliwatts) = metrics.tx_output_milliwatts {
        write!(out, ",\"tx_output_milliwatts\":{}", tx_output_milliwatts)?;
    }
    if let Some(battery_voltage_mv) = metrics.battery_voltage_mv {
        write!(out, ",\"battery_voltage_mv\":{}", battery_voltage_mv)?;
    }
    if let Some(battery_percent) = metrics.battery_percent {
        write!(out, ",\"battery_percent\":{}", battery_percent)?;
    }
    if let Some(last_poll_latency_ms) = metrics.last_poll_latency_ms {
        write!(out, ",\"last_poll_latency_ms\":{}", last_poll_latency_ms)?;
    }
    if let Some(avg_poll_latency_ms) = metrics.avg_poll_latency_ms {
        write!(out, ",\"avg_poll_latency_ms\":{}", avg_poll_latency_ms)?;
    }
    if let Some(free_heap_bytes) = metrics.free_heap_bytes {
        write!(out, ",\"free_heap_bytes\":{}", free_heap_bytes)?;
    }
    write!(
        out,
        ",\"diagnostic_events\":{},\"diagnostic_dropped_total\":{},\"diagnostic_error_events\":{}",
        metrics.diagnostic_events, metrics.diagnostic_dropped, metrics.diagnostic_errors
    )?;
    if let Some(last_error_ms) = metrics.last_error_ms {
        write!(out, ",\"last_error_ms\":{}", last_error_ms)?;
    }

    write!(out, "}}")?;
    Ok(out)
}

/// Render one telemetry record as a newline-delimited JSON payload.
#[cfg(feature = "serial-json")]
pub fn render_serial_json<const OUT: usize>(record: &TelemetryRecord)
-> Result<String<OUT>, Error>
{
    let mut out = String::new();
    write!(out, "{{\"type\":\"producer_telemetry\",\"producer\":\"")?;
    write_json_string_content(&mut out, record.config.name.as_str())?;
    write!(out, "\",\"public_key\":\"")?;
    write_json_string_content(&mut out, record.config.public_key.as_str())?;
    write!(out, "\",")?;

    if let Some(telemetry) = record.telemetry {
        write!(out, "\"timestamp_ms\":{}", telemetry.timestamp_ms)?;
        write_metrics_json(&mut out, &telemetry.metrics)?;
        if let Some(rssi) = telemetry.rssi {
            write!(out, ",\"rssi\":{}", rssi)?;
        }
        write_optional_f32(&mut out, "snr", telemetry.snr)?;
        if let Some(uptime_ms) = telemetry.uptime_ms {
            write!(out, ",\"uptime_ms\":{}", uptime_ms)?;
        }
        write_channels_json(&mut out, &telemetry)?;
    } else {
        write!(out, "\"timestamp_ms\":null")?;
    }

    write!(
        out,
        ",\"poll_success_total\":{},\"poll_failure_total\":{}",
        record.poll.poll_success_total, record.poll.poll_failure_total
    )?;
    match record.poll.last_poll_success {
        Some(true) => write!(out, ",\"poll_last_success\":true")?,
        Some(false) => write!(out, ",\"poll_last_success\":false")?,
        None => write!(out, ",\"poll_last_success\":null")?,
    }
    if let Some(reason) = record.poll.last_poll_error {
        write!(out, ",\"poll_error\":\"{}\"", reason.as_str())?;
    }
    write!(out, "}}")?;
    Ok(out)
}

#[cfg(feature = "serial-json")]
fn write_channels_json<const OUT: usize>(
    out: &mut String<OUT>,
    telemetry: &ProducerTelemetry,
) -> Result<(), Error>
{
    let mut first = true;
    for channel in telemetry.channels() {
        if first {
            write!(out, ",\"channels\":[")?;
            first = false;
        } else {
            write!(out, ",")?;
        }
        write_channel_json(out, channel)?;
    }
    if !first {
        write!(out, "]")?;
    }
    Ok(())
}

#[cfg(feature = "serial-json")]
fn write_channel_json<const OUT: usize>(
    out: &mut String<OUT>,
    channel: &ProducerTelemetryChannel,
) -> Result<(), Error>
{
    write!(out, "{{\"channel\":{}", channel.channel_id)?;
    write_metrics_json(out, &channel.metrics)?;
    write!(out, "}}")?;
    Ok(())
}

#[cfg(feature = "serial-json")]
fn write_metrics_json<const OUT: usize>(
    out: &mut String<OUT>,
    metrics: &TelemetryMetrics,
) -> Result<(), Error>
{
    write_optional_f32(out, "battery_voltage", metrics.battery_voltage)?;
    write_optional_f32(out, "battery_percent", metrics.battery_percent)?;
    write_optional_f32(out, "voltage", metrics.voltage)?;
    write_optional_f32(out, "current_amps", metrics.current_amps)?;
    write_optional_f32(out, "power_watts", metrics.power_watts)?;
    write_optional_f32(out, "temperature_celsius", metrics.temperature_celsius)?;
    write_optional_f32(out, "humidity_percent", metrics.humidity_percent)?;
    write_optional_f32(out, "pressure_pa", metrics.pressure_pa)?;
    write_optional_f32(out, "gas_resistance_ohms", metrics.gas_resistance_ohms)?;
    Ok(())
}

#[cfg(feature = "serial-json")]
fn write_json_string_content<const OUT: usize>(
    out: &mut String<OUT>,
    value: &str,
) -> Result<(), Error>
{
    for ch in value.chars() {
        match ch {
            '"' => out.push_str("\\\"").map_err(|_| Error::RenderBufferFull)?,
            '\\' => out.push_str("\\\\").map_err(|_| Error::RenderBufferFull)?,
            '\n' => out.push_str("\\n").map_err(|_| Error::RenderBufferFull)?,
            '\r' => out.push_str("\\r").map_err(|_| Error::RenderBufferFull)?,
            '\t' => out.push_str("\\t").map_err(|_| Error::RenderBufferFull)?,
            _ => out.push(ch).map_err(|_| Error::RenderBufferFull)?,
        }
    }
    Ok(())
}

#[cfg(feature = "serial-json")]
fn write_optional_f32<const OUT: usize>(
    out: &mut String<OUT>,
    name: &str,
    value: Option<f32>,
) -> Result<(), Error>
{
    if let Some(value) = value {
        write!(out, ",\"{name}\":{value}")?;
    }
    Ok(())
}

#[cfg(feature = "serial-json")]
fn write_per_mille<const OUT: usize>(out: &mut String<OUT>, value: u16) -> Result<(), Error>
{
    write!(out, "{}.{:03}", value / 1000, value % 1000)?;
    Ok(())
}

#[cfg(feature = "serial-json")]
fn write_tenths<const OUT: usize>(out: &mut String<OUT>, value: i16) -> Result<(), Error>
{
    let value = i32::from(value);
    let abs = value.abs();
    if value < 0 {
        write!(out, "-")?;
    }
    write!(out, "{}.{}", abs / 10, abs % 10)?;
    Ok(())
}

#[cfg(not(feature = "serial-json"))]
/// Return false when serial JSON rendering is compiled out.
pub const fn serial_json_enabled() -> bool
{
    false
}

#[cfg(feature = "serial-json")]
/// Return true when serial JSON rendering is compiled in.
pub const fn serial_json_enabled() -> bool
{
    true
}
