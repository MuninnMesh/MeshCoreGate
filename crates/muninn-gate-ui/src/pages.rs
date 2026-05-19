//! Complete text-page renderers for gateway status screens.
//!
//! Page renderers assemble fixed-capacity text frames from core gateway state.

use core::fmt::Write;

use heapless::String;
use muninn_gate_core::{
    DisplaySettings,
    DisplayTelemetryValue,
    Error,
    GatewayConfig,
    GatewayMetrics,
    GatewayRuntimeState,
    HttpEndpoint,
    TelemetryProducerConfig,
    TelemetryRecord,
    TelemetrySnapshot,
};

use crate::components::rssi_bars;
use crate::format::{
    copy_truncated_with_marker,
    write_display_value,
    write_duration,
    write_last_heard_age,
    write_producer_label,
    write_str,
};
use crate::frame::{
    NODE_ROW_NAME_CHARS,
    Oled128x64Frame,
    Oled128x128Frame,
    TextFrame,
    UI_SCRATCH_CHARS,
};

/// Display page rendered by the local UI.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DisplayPage
{
    /// Compact gateway status page.
    Status,
    /// Provisioned configuration summary page.
    Config,
    /// WiFi and network interface page.
    Wifi,
    /// Telemetry producer polling status page.
    Producers,
}

/// Inputs required to render a display page.
#[derive(Debug, Clone, Copy)]
pub struct DisplayContext<'a, const N: usize>
{
    /// Human-readable board name supplied by the board variant.
    pub board_name: &'a str,
    /// Gateway configuration currently in use.
    pub config:     &'a GatewayConfig<N>,
    /// Last-known gateway and producer telemetry snapshot.
    pub snapshot:   &'a TelemetrySnapshot<N>,
    /// Current provisioning and serving state.
    pub state:      GatewayRuntimeState,
    /// Current monotonic timestamp in milliseconds.
    pub now_ms:     u64,
}

/// Render a page for a 128x64 OLED display.
pub fn render_oled_128x64<const N: usize>(
    page: DisplayPage,
    context: &DisplayContext<'_, N>,
) -> Result<Oled128x64Frame, Error>
{
    render_page(page, context)
}

/// Render a page for a 128x128 OLED display.
pub fn render_oled_128x128<const N: usize>(
    page: DisplayPage,
    context: &DisplayContext<'_, N>,
) -> Result<Oled128x128Frame, Error>
{
    render_page(page, context)
}

/// Render a page into a custom fixed-size text frame.
pub fn render_page<const LINES: usize, const WIDTH: usize, const N: usize>(
    page: DisplayPage,
    context: &DisplayContext<'_, N>,
) -> Result<TextFrame<LINES, WIDTH>, Error>
{
    let mut frame = TextFrame::new();
    match page {
        DisplayPage::Status => render_status(&mut frame, context)?,
        DisplayPage::Config => render_config(&mut frame, context)?,
        DisplayPage::Wifi => render_wifi(&mut frame, context)?,
        DisplayPage::Producers => render_producers(&mut frame, context)?,
    }
    Ok(frame)
}

fn render_status<const LINES: usize, const WIDTH: usize, const N: usize>(
    frame: &mut TextFrame<LINES, WIDTH>,
    context: &DisplayContext<'_, N>,
) -> Result<(), Error>
{
    push_gateway_status_line(frame, context)?;
    push_node_rows(frame, context)
}

fn render_config<const LINES: usize, const WIDTH: usize, const N: usize>(
    frame: &mut TextFrame<LINES, WIDTH>,
    context: &DisplayContext<'_, N>,
) -> Result<(), Error>
{
    push_prefixed_line(frame, "cfg ", context.config.name.as_str())?;
    if let Some(public_key) = context.config.meshcore.public_key.as_ref() {
        push_prefixed_line(frame, "pk ", public_key.as_str())?;
    } else {
        push_line_if_space(frame, "pk missing")?;
    }
    push_formatted_line(frame, |line| {
        write!(line, "out {}", output_label(context.config))
    })?;
    push_formatted_line(frame, |line| {
        write_str(line, "poll ")?;
        write_duration(line, context.config.polling.default_interval_ms())
    })?;
    push_formatted_line(frame, |line| {
        write!(line, "retry {}", context.config.polling.retry_count)
    })?;
    push_formatted_line(frame, |line| {
        write!(line, "jitter {}s", context.config.polling.jitter_secs)
    })?;
    push_formatted_line(frame, |line| {
        write!(line, "tx lvl {}", context.config.radio.tx_power_level)
    })?;
    push_formatted_line(frame, |line| {
        write!(line, "freq {}", context.config.radio.frequency_hz)
    })?;
    push_formatted_line(frame, |line| {
        if let Some(http) = &context.config.http {
            write!(line, "http :{}", http.port)
        } else {
            write_str(line, "http off")
        }
    })?;
    push_formatted_line(frame, |line| {
        write!(
            line,
            "display {}",
            display_status_label(context.config.display)
        )
    })?;
    if let DisplaySettings::Enabled {
        brightness_percent, ..
    } = context.config.display
    {
        push_formatted_line(frame, |line| write!(line, "bright {}%", brightness_percent))?;
    }
    push_formatted_line(frame, |line| {
        write!(
            line,
            "value {}",
            context.config.display.node_display().as_str()
        )
    })?;
    Ok(())
}

fn render_wifi<const LINES: usize, const WIDTH: usize, const N: usize>(
    frame: &mut TextFrame<LINES, WIDTH>,
    context: &DisplayContext<'_, N>,
) -> Result<(), Error>
{
    match &context.config.http {
        None => {
            push_line_if_space(frame, "http off")?;
            push_line_if_space(frame, "serial only")?;
        },
        Some(http) => {
            push_line_if_space(frame, "wifi station")?;
            push_prefixed_line(frame, "ssid ", http.wifi_ssid.as_str())?;
            if let Some(endpoint) = serving_http_endpoint(context.state) {
                push_formatted_line(frame, |line| write_endpoint(line, endpoint))?;
            } else {
                push_formatted_line(frame, |line| write!(line, "http :{}", http.port))?;
            }
            push_formatted_line(frame, |line| write!(line, "tls {}", bool_label(http.tls)))?;
            push_formatted_line(frame, |line| write!(line, "tokens {}", http.tokens.len()))?;
        },
    }
    push_formatted_line(frame, |line| {
        write!(line, "out {}", output_label(context.config))
    })?;
    Ok(())
}

fn render_producers<const LINES: usize, const WIDTH: usize, const N: usize>(
    frame: &mut TextFrame<LINES, WIDTH>,
    context: &DisplayContext<'_, N>,
) -> Result<(), Error>
{
    push_formatted_line(frame, |line| {
        write!(
            line,
            "nodes {}",
            context.config.display.node_display().as_str()
        )
    })?;
    push_node_rows(frame, context)
}

fn push_node_rows<const LINES: usize, const WIDTH: usize, const N: usize>(
    frame: &mut TextFrame<LINES, WIDTH>,
    context: &DisplayContext<'_, N>,
) -> Result<(), Error>
{
    if context.snapshot.records.is_empty() {
        if context.config.producers.is_empty() {
            push_line_if_space(frame, "none configured")?;
        } else {
            for producer in context.config.producers.iter() {
                push_producer_config_line(frame, producer)?;
            }
        }
        return Ok(());
    }

    for record in context.snapshot.records.iter() {
        push_producer_record_line(
            frame,
            record,
            context.now_ms,
            context.config.display.node_display(),
            &context.snapshot.gateway,
        )?;
    }
    Ok(())
}

fn push_gateway_status_line<const LINES: usize, const WIDTH: usize, const N: usize>(
    frame: &mut TextFrame<LINES, WIDTH>,
    context: &DisplayContext<'_, N>,
) -> Result<(), Error>
{
    push_formatted_line(frame, |line| {
        let name = if context.config.name.is_empty() {
            "gate"
        } else {
            context.config.name.as_str()
        };
        copy_truncated_with_marker(line, name, 14)?;
        write!(line, " {}", runtime_state_label(context.state))
    })
}

fn push_producer_config_line<const LINES: usize, const WIDTH: usize>(
    frame: &mut TextFrame<LINES, WIDTH>,
    producer: &TelemetryProducerConfig,
) -> Result<(), Error>
{
    push_formatted_line(frame, |line| {
        write_str(line, "ERR ")?;
        write_producer_label(line, producer, NODE_ROW_NAME_CHARS)?;
        if producer.enabled {
            write_str(line, " -- --")
        } else {
            write_str(line, " off")
        }
    })
}

fn push_producer_record_line<const LINES: usize, const WIDTH: usize>(
    frame: &mut TextFrame<LINES, WIDTH>,
    record: &TelemetryRecord,
    now_ms: u64,
    node_display: DisplayTelemetryValue,
    gateway: &GatewayMetrics,
) -> Result<(), Error>
{
    push_formatted_line(frame, |line| {
        write_signal_indicator(line, record)?;
        write_str(line, " ")?;
        write_producer_label(line, &record.config, NODE_ROW_NAME_CHARS)?;
        if !record.config.enabled {
            return write_str(line, " off");
        }

        write_str(line, " ")?;
        write_last_heard_age(line, record.telemetry, now_ms)?;
        write_str(line, " ")?;
        write_display_value(line, node_display, record.telemetry, gateway)
    })
}

fn push_line_if_space<const LINES: usize, const WIDTH: usize>(
    frame: &mut TextFrame<LINES, WIDTH>,
    value: &str,
) -> Result<(), Error>
{
    if frame.is_full() {
        return Ok(());
    }
    frame.push_line(value)
}

fn push_prefixed_line<const LINES: usize, const WIDTH: usize>(
    frame: &mut TextFrame<LINES, WIDTH>,
    prefix: &str,
    value: &str,
) -> Result<(), Error>
{
    push_formatted_line(frame, |line| {
        write_str(line, prefix)?;
        write_str(line, value)
    })
}

fn push_formatted_line<const LINES: usize, const WIDTH: usize, F>(
    frame: &mut TextFrame<LINES, WIDTH>,
    write_line: F,
) -> Result<(), Error>
where
    F: FnOnce(&mut String<UI_SCRATCH_CHARS>) -> core::fmt::Result,
{
    if frame.is_full() {
        return Ok(());
    }

    let mut line = String::new();
    write_line(&mut line)?;
    frame.push_line(line.as_str())
}

fn write_signal_indicator<const N: usize>(
    line: &mut String<N>,
    record: &TelemetryRecord,
) -> core::fmt::Result
{
    if record.poll.last_poll_success == Some(false) {
        return write_str(line, "ERR");
    }

    match record.telemetry.and_then(|telemetry| telemetry.rssi) {
        Some(rssi) => write!(line, "{}", rssi_bars(rssi)),
        None => write_str(line, "ERR"),
    }
}

fn write_endpoint<const N: usize>(line: &mut String<N>, endpoint: HttpEndpoint)
-> core::fmt::Result
{
    write!(
        line,
        "{}.{}.{}.{}:{}",
        endpoint.ipv4[0], endpoint.ipv4[1], endpoint.ipv4[2], endpoint.ipv4[3], endpoint.port
    )
}

fn serving_http_endpoint(state: GatewayRuntimeState) -> Option<HttpEndpoint>
{
    match state {
        GatewayRuntimeState::Serving { interfaces } => interfaces.http,
        GatewayRuntimeState::Unprovisioned
        | GatewayRuntimeState::Provisioned
        | GatewayRuntimeState::Error { .. } => None,
    }
}

fn output_label<const N: usize>(config: &GatewayConfig<N>) -> &'static str
{
    if config.http.is_some() {
        "all"
    } else {
        "serial"
    }
}

fn bool_label(value: bool) -> &'static str
{
    if value { "on" } else { "off" }
}

fn display_status_label(display: DisplaySettings) -> &'static str
{
    match display {
        DisplaySettings::Off => "off",
        DisplaySettings::Enabled { .. } => "on",
    }
}

fn runtime_state_label(state: GatewayRuntimeState) -> &'static str
{
    match state {
        GatewayRuntimeState::Unprovisioned => "setup",
        GatewayRuntimeState::Provisioned => "starting",
        GatewayRuntimeState::Serving { .. } => "online",
        GatewayRuntimeState::Error { .. } => "error",
    }
}

#[cfg(test)]
mod tests
{
    use muninn_gate_core::config::fixed_string;
    use muninn_gate_core::{
        DisplaySettings,
        DisplayTelemetryValue,
        FixedTelemetryStore,
        GatewayConfig,
        GatewayRuntimeState,
        HttpConfig,
        HttpEndpoint,
        ProducerTelemetry,
        ServingInterfaces,
        TelemetryProducerConfig,
        TelemetryStore,
    };

    use super::{DisplayContext, DisplayPage, render_oled_128x64, render_oled_128x128};

    fn test_config() -> GatewayConfig<4>
    {
        let mut config = GatewayConfig::new("gate-a").unwrap();
        config.meshcore.public_key = Some(fixed_string("gateway-public-key").unwrap());
        config.display = DisplaySettings::Enabled {
            node_display:       DisplayTelemetryValue::TemperatureCelsius,
            brightness_percent: 60,
        };
        let mut http = HttpConfig::new(80, false, "lab-wifi", "hidden").unwrap();
        http.add_token("token").unwrap();
        config.http = Some(http);
        config
            .add_producer(
                TelemetryProducerConfig::new("producer-public-key-roof", "roof_repeater").unwrap(),
            )
            .unwrap();
        config
            .add_producer(TelemetryProducerConfig::new("producer-public-key-yard", "yard").unwrap())
            .unwrap();
        config
    }

    #[test]
    fn renders_status_for_128x64_without_exceeding_line_budget()
    {
        let config = test_config();
        let store = FixedTelemetryStore::from_config(&config).unwrap();
        let snapshot = store.snapshot();
        let context = DisplayContext {
            board_name: "WiFi LoRa 32 V4.x",
            config:     &config,
            snapshot:   &snapshot,
            state:      GatewayRuntimeState::Serving {
                interfaces: ServingInterfaces::new(
                    Some(HttpEndpoint::new([192, 168, 1, 50], 80)),
                    true,
                ),
            },
            now_ms:     1_000,
        };

        let frame = render_oled_128x64(DisplayPage::Status, &context).unwrap();

        assert_eq!(frame.lines[0].as_str(), "gate-a online");
        assert!(
            frame
                .lines
                .iter()
                .any(|line| line.as_str() == "ERR roof_repe~ -- --")
        );
    }

    #[test]
    fn renders_producer_success_and_age()
    {
        let config = test_config();
        let mut store = FixedTelemetryStore::from_config(&config).unwrap();
        let producer_id = config.producers[0].id();
        let mut telemetry = ProducerTelemetry::new(producer_id, 10_000);
        telemetry.rssi = Some(-97);
        telemetry.metrics.temperature_celsius = Some(22.4);
        store.update_producer(producer_id, telemetry).unwrap();
        store.record_poll_success(producer_id, 10_000).unwrap();
        let snapshot = store.snapshot();
        let context = DisplayContext {
            board_name: "WiFi LoRa 32 V4.x",
            config:     &config,
            snapshot:   &snapshot,
            state:      GatewayRuntimeState::Provisioned,
            now_ms:     130_000,
        };

        let frame = render_oled_128x128(DisplayPage::Producers, &context).unwrap();

        assert!(
            frame
                .lines
                .iter()
                .any(|line| line.as_str() == "2 roof_repe~ 2m 22C")
        );
        assert!(
            frame
                .lines
                .iter()
                .any(|line| line.as_str() == "ERR yard -- --")
        );
    }

    #[test]
    fn renders_producer_failure()
    {
        let config = test_config();
        let mut store = FixedTelemetryStore::from_config(&config).unwrap();
        let producer_id = config.producers[1].id();
        store.record_poll_failure(producer_id, 10_000).unwrap();
        let snapshot = store.snapshot();
        let context = DisplayContext {
            board_name: "WiFi LoRa 32 V4.x",
            config:     &config,
            snapshot:   &snapshot,
            state:      GatewayRuntimeState::Provisioned,
            now_ms:     20_000,
        };

        let frame = render_oled_128x128(DisplayPage::Producers, &context).unwrap();

        assert!(
            frame
                .lines
                .iter()
                .any(|line| line.as_str() == "ERR yard -- --")
        );
    }
}
