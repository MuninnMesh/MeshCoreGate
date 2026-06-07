//! Text formatting helpers used by display pages and platform graphics code.

use core::fmt::Write;

use heapless::String;
use muninn_gate_core::{
    DisplayTelemetryValue,
    Error,
    GatewayMetrics,
    ProducerTelemetry,
    TelemetryProducerConfig,
};

/// Format the display label used for a telemetry producer row.
pub fn format_producer_label<const N: usize>(
    producer: &TelemetryProducerConfig,
    max_chars: usize,
) -> Result<String<N>, Error>
{
    let mut line = String::new();
    write_producer_label(&mut line, producer, max_chars).map_err(|_| Error::RenderBufferFull)?;
    Ok(line)
}

/// Format the age of the latest telemetry sample for display.
pub fn format_last_heard_age<const N: usize>(
    telemetry: Option<ProducerTelemetry>,
    now_ms: u64,
) -> Result<String<N>, Error>
{
    let mut line = String::new();
    write_last_heard_age(&mut line, telemetry, now_ms).map_err(|_| Error::RenderBufferFull)?;
    Ok(line)
}

/// Format the configured producer metric value for display.
pub fn format_display_value<const N: usize>(
    value: DisplayTelemetryValue,
    telemetry: Option<ProducerTelemetry>,
    gateway: &GatewayMetrics,
) -> Result<String<N>, Error>
{
    let mut line = String::new();
    write_display_value(&mut line, value, telemetry, gateway)
        .map_err(|_| Error::RenderBufferFull)?;
    Ok(line)
}

/// Write a producer label with a compact truncation marker.
pub fn write_producer_label<const N: usize>(
    line: &mut String<N>,
    producer: &TelemetryProducerConfig,
    max_chars: usize,
) -> core::fmt::Result
{
    if producer.name.is_empty() {
        copy_truncated_with_marker(line, producer.public_key.as_str(), max_chars)
    } else {
        copy_truncated_with_marker(line, producer.name.as_str(), max_chars)
    }
}

/// Copy at most `max_chars`, replacing the final visible character with `~`.
pub fn copy_truncated_with_marker<const N: usize>(
    line: &mut String<N>,
    value: &str,
    max_chars: usize,
) -> core::fmt::Result
{
    if value.chars().count() <= max_chars {
        return write_str(line, value);
    }

    let prefix_len = max_chars.saturating_sub(1);
    for ch in value.chars().take(prefix_len) {
        line.push(ch).map_err(|_| core::fmt::Error)?;
    }
    write_str(line, "~")
}

/// Write a compact duration.
pub fn write_duration<const N: usize>(line: &mut String<N>, duration_ms: u64) -> core::fmt::Result
{
    let seconds = duration_ms / 1000;
    if seconds < 60 {
        write!(line, "{}s", seconds)
    } else if seconds < 3_600 {
        write!(line, "{}m", seconds / 60)
    } else {
        write!(line, "{}h", seconds / 3_600)
    }
}

/// Write the age of a telemetry sample.
pub fn write_last_heard_age<const N: usize>(
    line: &mut String<N>,
    telemetry: Option<ProducerTelemetry>,
    now_ms: u64,
) -> core::fmt::Result
{
    match telemetry {
        Some(telemetry) => write_duration(line, now_ms.saturating_sub(telemetry.timestamp_ms)),
        None => write_str(line, "--"),
    }
}

/// Write a selected telemetry value for a producer row.
pub fn write_display_value<const N: usize>(
    line: &mut String<N>,
    value: DisplayTelemetryValue,
    telemetry: Option<ProducerTelemetry>,
    gateway: &GatewayMetrics,
) -> core::fmt::Result
{
    let Some(telemetry) = telemetry else {
        return write_str(line, "--");
    };
    let metrics = telemetry.metrics;

    match value {
        DisplayTelemetryValue::TemperatureCelsius => match metrics.temperature_celsius {
            Some(value) => write!(line, "{:.0}C", value),
            None => write_str(line, "--"),
        },
        DisplayTelemetryValue::HumidityPercent => match metrics.humidity_percent {
            Some(value) => write!(line, "{:.0}%", value),
            None => write_str(line, "--"),
        },
        DisplayTelemetryValue::StateOfCharge => {
            if let Some(value) = metrics.battery_percent {
                write!(line, "{:.0}%", value)
            } else if let Some(value) = metrics.battery_voltage {
                write!(line, "{:.1}V", value)
            } else {
                write_str(line, "--")
            }
        },
        DisplayTelemetryValue::BatteryVoltage => match metrics.battery_voltage {
            Some(value) => write!(line, "{:.1}V", value),
            None => write_str(line, "--"),
        },
        DisplayTelemetryValue::PressureHpa => match metrics.pressure_pa {
            Some(value) => write!(line, "{:.0}h", value / 100.0),
            None => write_str(line, "--"),
        },
        DisplayTelemetryValue::LuminosityLux => match metrics.luminosity_lux {
            Some(value) if value < 10_000.0 => write!(line, "{:.0}lx", value),
            Some(value) => write!(line, "{:.1}klx", value / 1000.0),
            None => write_str(line, "--"),
        },
        DisplayTelemetryValue::Rssi => match telemetry.rssi {
            Some(value) => write!(line, "{}", value),
            None => write_str(line, "--"),
        },
        DisplayTelemetryValue::PollLatency => match gateway.last_poll_latency_ms {
            Some(value) if value < 1000 => write!(line, "{}ms", value),
            Some(value) => write!(line, "{}s", value / 1000),
            None => write_str(line, "--"),
        },
    }
}

/// Append a string to a fixed-capacity line.
pub fn write_str<const N: usize>(line: &mut String<N>, value: &str) -> core::fmt::Result
{
    line.push_str(value).map_err(|_| core::fmt::Error)
}
