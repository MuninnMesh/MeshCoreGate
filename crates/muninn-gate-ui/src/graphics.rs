//! Reusable monochrome graphics primitives for small embedded displays.

use core::fmt::Write;

use embedded_graphics::mono_font::MonoTextStyle;
use embedded_graphics::mono_font::ascii::{FONT_5X7, FONT_6X10};
use embedded_graphics::pixelcolor::BinaryColor;
use embedded_graphics::prelude::{DrawTarget, Drawable, Point, Primitive, Size};
use embedded_graphics::primitives::{PrimitiveStyle, Rectangle};
use embedded_graphics::text::{Baseline, Text};
use heapless::String;
use muninn_gate_core::{
    DisplayTelemetryValue,
    Error,
    GatewayConfig,
    GatewayMetrics,
    GatewayRuntimeError,
    GatewayRuntimeState,
    HttpEndpoint,
    TelemetryProducerConfig,
    TelemetryProducerId,
    TelemetryRecord,
    TelemetrySnapshot,
};

use crate::{
    Oled128x64Frame,
    battery_fill_height,
    format_display_value,
    format_last_heard_age,
    format_producer_label,
    rssi_bars,
};

/// OLED 128x64 line height in pixels.
pub const OLED_128X64_LINE_HEIGHT_PX: i32 = 8;
/// OLED 128x64 width in pixels.
pub const OLED_128X64_WIDTH_PX: i32 = 128;
/// OLED 128x64 height in pixels.
pub const OLED_128X64_HEIGHT_PX: i32 = 64;
/// Approximate character width for notice text.
pub const OLED_128X64_NOTICE_FONT_WIDTH_PX: i32 = 6;
/// First dashboard row y coordinate.
pub const OLED_128X64_DASHBOARD_FIRST_ROW_Y_PX: i32 = 23;
/// Dashboard row height.
pub const OLED_128X64_DASHBOARD_ROW_HEIGHT_PX: i32 = 8;
/// Maximum producer label characters in dashboard rows.
pub const OLED_128X64_DASHBOARD_PRODUCER_NAME_CHARS: usize = 10;
/// Maximum gateway name characters before header indicators.
pub const OLED_128X64_DASHBOARD_GATEWAY_NAME_CHARS: usize = 14;
/// Maximum metric characters in the dashboard value column.
pub const OLED_128X64_DASHBOARD_VALUE_CHARS: usize = 5;
/// Header WiFi icon origin.
pub const OLED_128X64_DASHBOARD_WIFI_ORIGIN: Point = Point::new(101, 2);
/// Header battery icon origin.
pub const OLED_128X64_DASHBOARD_BATTERY_ORIGIN: Point = Point::new(120, 2);

/// Error returned by generic graphics renderers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GraphicsError<E>
{
    /// The target display rejected a drawing operation.
    Draw(E),
    /// A fixed-capacity formatting buffer was too small.
    Buffer,
}

/// Dashboard render inputs for the 128x64 graphics renderer.
#[derive(Debug, Clone, Copy)]
pub struct Oled128x64Dashboard<'a, const N: usize>
{
    /// Gateway configuration.
    pub config:      &'a GatewayConfig<N>,
    /// Latest telemetry snapshot.
    pub snapshot:    &'a TelemetrySnapshot<N>,
    /// Runtime state used for status text and WiFi indicator state.
    pub state:       GatewayRuntimeState,
    /// Current monotonic timestamp in milliseconds.
    pub now_ms:      u64,
    /// HTTP endpoint when the gateway is serving over WiFi.
    pub endpoint:    Option<HttpEndpoint>,
    /// Board name used when the configured gateway name is empty.
    pub board_name:  &'static str,
    /// Active poll producer and animation step.
    pub active_poll: Option<(TelemetryProducerId, u8)>,
}

/// Draw a fixed text frame on a 128x64 monochrome target.
pub fn draw_text_frame_128x64<D>(
    target: &mut D,
    frame: &Oled128x64Frame,
) -> Result<(), GraphicsError<D::Error>>
where
    D: DrawTarget<Color = BinaryColor>,
{
    target
        .clear(BinaryColor::Off)
        .map_err(GraphicsError::Draw)?;
    let style = MonoTextStyle::new(&FONT_5X7, BinaryColor::On);

    for (index, line) in frame.lines.iter().enumerate() {
        Text::with_baseline(
            line.as_str(),
            Point::new(0, index as i32 * OLED_128X64_LINE_HEIGHT_PX),
            style,
            Baseline::Top,
        )
        .draw(target)
        .map_err(GraphicsError::Draw)?;
    }
    Ok(())
}

/// Draw a framed three-line notice on a 128x64 monochrome target.
pub fn draw_framed_notice_128x64<D>(
    target: &mut D,
    top: &str,
    middle: &str,
    bottom: &str,
) -> Result<(), GraphicsError<D::Error>>
where
    D: DrawTarget<Color = BinaryColor>,
{
    target
        .clear(BinaryColor::Off)
        .map_err(GraphicsError::Draw)?;
    Rectangle::new(
        Point::new(0, 0),
        Size::new(OLED_128X64_WIDTH_PX as u32, OLED_128X64_HEIGHT_PX as u32),
    )
    .into_styled(PrimitiveStyle::with_stroke(BinaryColor::On, 1))
    .draw(target)
    .map_err(GraphicsError::Draw)?;

    let style = MonoTextStyle::new(&FONT_6X10, BinaryColor::On);
    draw_centered_notice_text(target, top, 8, style)?;
    draw_centered_notice_text(target, middle, 25, style)?;
    draw_centered_notice_text(target, bottom, 42, style)?;
    Ok(())
}

/// Draw the WiFi connection progress page.
pub fn draw_wifi_connecting_128x64<D>(
    target: &mut D,
    ssid: &str,
    detail: &str,
    step: u8,
) -> Result<(), GraphicsError<D::Error>>
where
    D: DrawTarget<Color = BinaryColor>,
{
    target
        .clear(BinaryColor::Off)
        .map_err(GraphicsError::Draw)?;
    let small = MonoTextStyle::new(&FONT_5X7, BinaryColor::On);

    Text::with_baseline("Muninn Gate", Point::new(0, 0), small, Baseline::Top)
        .draw(target)
        .map_err(GraphicsError::Draw)?;
    Rectangle::new(Point::new(0, 12), Size::new(128, 1))
        .into_styled(PrimitiveStyle::with_fill(BinaryColor::On))
        .draw(target)
        .map_err(GraphicsError::Draw)?;

    let status = wifi_connecting_label(detail);
    Text::with_baseline(status, Point::new(0, 20), small, Baseline::Top)
        .draw(target)
        .map_err(GraphicsError::Draw)?;
    let bars_x = ((status.len() as i32 * 6) + 6).min(112);
    draw_connecting_bar_activity(target, Point::new(bars_x, 18), step, 10)
        .map_err(GraphicsError::Draw)?;

    let ssid = truncate_text::<24, D::Error>(ssid, 19)?;
    Text::with_baseline("SSID", Point::new(0, 38), small, Baseline::Top)
        .draw(target)
        .map_err(GraphicsError::Draw)?;
    Text::with_baseline(ssid.as_str(), Point::new(0, 50), small, Baseline::Top)
        .draw(target)
        .map_err(GraphicsError::Draw)?;
    Ok(())
}

/// Draw the operational 128x64 dashboard.
pub fn draw_dashboard_128x64<D, const N: usize>(
    target: &mut D,
    context: Oled128x64Dashboard<'_, N>,
) -> Result<(), GraphicsError<D::Error>>
where
    D: DrawTarget<Color = BinaryColor>,
{
    target
        .clear(BinaryColor::Off)
        .map_err(GraphicsError::Draw)?;
    let small = MonoTextStyle::new(&FONT_5X7, BinaryColor::On);

    draw_status_indicators(
        target,
        context.config,
        context.state,
        &context.snapshot.gateway,
    )?;

    let title = if context.config.name.is_empty() {
        context.board_name
    } else {
        context.config.name.as_str()
    };
    let title = truncate_text::<32, D::Error>(title, OLED_128X64_DASHBOARD_GATEWAY_NAME_CHARS)?;
    Text::with_baseline(title.as_str(), Point::new(0, 0), small, Baseline::Top)
        .draw(target)
        .map_err(GraphicsError::Draw)?;

    let detail =
        dashboard_detail_line::<32, N, D::Error>(context.config, context.state, context.endpoint)?;
    Text::with_baseline(detail.as_str(), Point::new(0, 10), small, Baseline::Top)
        .draw(target)
        .map_err(GraphicsError::Draw)?;

    Rectangle::new(Point::new(0, 20), Size::new(128, 1))
        .into_styled(PrimitiveStyle::with_fill(BinaryColor::On))
        .draw(target)
        .map_err(GraphicsError::Draw)?;

    draw_dashboard_rows(target, context)?;
    Ok(())
}

/// Draw a five-bar signal indicator.
pub fn draw_bar_indicator<D>(
    target: &mut D,
    origin: Point,
    filled_bars: u8,
    max_height: i32,
) -> Result<(), D::Error>
where
    D: DrawTarget<Color = BinaryColor>,
{
    for index in 0..5_i32 {
        let height = 2 + (index * (max_height - 2) / 4);
        let x = origin.x + index * 3;
        let y = origin.y + max_height - height;
        let style = if index < i32::from(filled_bars) {
            PrimitiveStyle::with_fill(BinaryColor::On)
        } else {
            PrimitiveStyle::with_stroke(BinaryColor::On, 1)
        };
        Rectangle::new(Point::new(x, y), Size::new(2, height as u32))
            .into_styled(style)
            .draw(target)?;
    }
    Ok(())
}

/// Draw a compact three-bar producer signal indicator.
pub fn draw_producer_signal_indicator<D>(
    target: &mut D,
    origin: Point,
    filled_bars: u8,
    max_height: i32,
) -> Result<(), D::Error>
where
    D: DrawTarget<Color = BinaryColor>,
{
    for index in 0..3_i32 {
        let height = 2 + (index * (max_height - 2) / 2);
        let x = origin.x + index * 4;
        let y = origin.y + max_height - height;
        let style = if index < i32::from(filled_bars) {
            PrimitiveStyle::with_fill(BinaryColor::On)
        } else {
            PrimitiveStyle::with_stroke(BinaryColor::On, 1)
        };
        Rectangle::new(Point::new(x, y), Size::new(2, height as u32))
            .into_styled(style)
            .draw(target)?;
    }
    Ok(())
}

/// Draw a WiFi-sized activity animation using the same geometry as signal bars.
fn draw_connecting_bar_activity<D>(
    target: &mut D,
    origin: Point,
    step: u8,
    max_height: i32,
) -> Result<(), D::Error>
where
    D: DrawTarget<Color = BinaryColor>,
{
    let phase = step % 10;
    let active = if phase < 5 { phase } else { 9 - phase };
    for index in 0..5_i32 {
        if index > i32::from(active) {
            continue;
        }
        let height = 2 + (index * (max_height - 2) / 4);
        let x = origin.x + index * 3;
        let y = origin.y + max_height - height;
        Rectangle::new(Point::new(x, y), Size::new(2, height as u32))
            .into_styled(PrimitiveStyle::with_fill(BinaryColor::On))
            .draw(target)?;
    }
    Ok(())
}

fn draw_centered_notice_text<D>(
    target: &mut D,
    text: &str,
    y: i32,
    style: MonoTextStyle<'_, BinaryColor>,
) -> Result<(), GraphicsError<D::Error>>
where
    D: DrawTarget<Color = BinaryColor>,
{
    let width = text.len() as i32 * OLED_128X64_NOTICE_FONT_WIDTH_PX;
    let x = OLED_128X64_WIDTH_PX.saturating_sub(width) / 2;
    Text::with_baseline(text, Point::new(x, y), style, Baseline::Top)
        .draw(target)
        .map_err(GraphicsError::Draw)?;
    Ok(())
}

fn draw_status_indicators<D, const N: usize>(
    target: &mut D,
    config: &GatewayConfig<N>,
    state: GatewayRuntimeState,
    gateway: &GatewayMetrics,
) -> Result<(), GraphicsError<D::Error>>
where
    D: DrawTarget<Color = BinaryColor>,
{
    draw_vertical_battery(
        target,
        OLED_128X64_DASHBOARD_BATTERY_ORIGIN,
        gateway.battery_percent,
    )
    .map_err(GraphicsError::Draw)?;
    let Some(bars) = wifi_signal_bars(config, state) else {
        let style = MonoTextStyle::new(&FONT_5X7, BinaryColor::On);
        Text::with_baseline(
            "USB",
            OLED_128X64_DASHBOARD_WIFI_ORIGIN,
            style,
            Baseline::Top,
        )
        .draw(target)
        .map_err(GraphicsError::Draw)?;
        return Ok(());
    };
    draw_bar_indicator(target, OLED_128X64_DASHBOARD_WIFI_ORIGIN, bars, 10)
        .map_err(GraphicsError::Draw)
}

fn draw_dashboard_rows<D, const N: usize>(
    target: &mut D,
    context: Oled128x64Dashboard<'_, N>,
) -> Result<(), GraphicsError<D::Error>>
where
    D: DrawTarget<Color = BinaryColor>,
{
    let max_rows = ((OLED_128X64_HEIGHT_PX - OLED_128X64_DASHBOARD_FIRST_ROW_Y_PX)
        / OLED_128X64_DASHBOARD_ROW_HEIGHT_PX)
        .max(0) as usize;
    let rows_to_draw = max_rows.min(context.snapshot.records.len());

    if rows_to_draw > 0 {
        for (index, record) in context
            .snapshot
            .records
            .iter()
            .take(rows_to_draw)
            .enumerate()
        {
            let y = OLED_128X64_DASHBOARD_FIRST_ROW_Y_PX
                + index as i32 * OLED_128X64_DASHBOARD_ROW_HEIGHT_PX;
            draw_record_row(
                target,
                record,
                context.now_ms,
                context.config.display.node_display(),
                &context.snapshot.gateway,
                y,
                context.active_poll,
            )?;
        }
        return Ok(());
    }

    if context.config.producers.is_empty() {
        let style = MonoTextStyle::new(&FONT_5X7, BinaryColor::On);
        Text::with_baseline(
            "No producers configured",
            Point::new(0, OLED_128X64_DASHBOARD_FIRST_ROW_Y_PX),
            style,
            Baseline::Top,
        )
        .draw(target)
        .map_err(GraphicsError::Draw)?;
        return Ok(());
    }

    for (index, producer) in context.config.producers.iter().take(max_rows).enumerate() {
        let y = OLED_128X64_DASHBOARD_FIRST_ROW_Y_PX
            + index as i32 * OLED_128X64_DASHBOARD_ROW_HEIGHT_PX;
        draw_configured_producer_row(target, producer, y, context.active_poll)?;
    }
    Ok(())
}

fn draw_configured_producer_row<D>(
    target: &mut D,
    producer: &TelemetryProducerConfig,
    y: i32,
    active_poll: Option<(TelemetryProducerId, u8)>,
) -> Result<(), GraphicsError<D::Error>>
where
    D: DrawTarget<Color = BinaryColor>,
{
    let style = MonoTextStyle::new(&FONT_5X7, BinaryColor::On);
    if let Some((active_id, step)) = active_poll
        && active_id == producer.id()
    {
        draw_poll_activity(target, Point::new(2, y), step).map_err(GraphicsError::Draw)?;
    } else {
        let signal = if producer.enabled { "ERR" } else { "OFF" };
        Text::with_baseline(signal, Point::new(0, y), style, Baseline::Top)
            .draw(target)
            .map_err(GraphicsError::Draw)?;
    }

    let label = format_producer_label::<16>(producer, OLED_128X64_DASHBOARD_PRODUCER_NAME_CHARS)
        .map_err(map_buffer_error)?;
    Text::with_baseline(label.as_str(), Point::new(18, y), style, Baseline::Top)
        .draw(target)
        .map_err(GraphicsError::Draw)?;
    Text::with_baseline("--", Point::new(80, y), style, Baseline::Top)
        .draw(target)
        .map_err(GraphicsError::Draw)?;
    Text::with_baseline("--", Point::new(99, y), style, Baseline::Top)
        .draw(target)
        .map_err(GraphicsError::Draw)?;
    Ok(())
}

fn draw_record_row<D>(
    target: &mut D,
    record: &TelemetryRecord,
    now_ms: u64,
    node_display: DisplayTelemetryValue,
    gateway: &GatewayMetrics,
    y: i32,
    active_poll: Option<(TelemetryProducerId, u8)>,
) -> Result<(), GraphicsError<D::Error>>
where
    D: DrawTarget<Color = BinaryColor>,
{
    let style = MonoTextStyle::new(&FONT_5X7, BinaryColor::On);
    if let Some((active_id, step)) = active_poll
        && active_id == record.config.id()
    {
        draw_poll_activity(target, Point::new(2, y), step).map_err(GraphicsError::Draw)?;
    } else {
        draw_record_signal(target, record, y)?;
    }

    let label =
        format_producer_label::<16>(&record.config, OLED_128X64_DASHBOARD_PRODUCER_NAME_CHARS)
            .map_err(map_buffer_error)?;
    Text::with_baseline(label.as_str(), Point::new(18, y), style, Baseline::Top)
        .draw(target)
        .map_err(GraphicsError::Draw)?;

    let age = format_last_heard_age::<8>(record.telemetry, now_ms).map_err(map_buffer_error)?;
    Text::with_baseline(age.as_str(), Point::new(80, y), style, Baseline::Top)
        .draw(target)
        .map_err(GraphicsError::Draw)?;

    let value = format_display_value::<16>(node_display, record.telemetry, gateway)
        .map_err(map_buffer_error)?;
    let value = truncate_text::<8, D::Error>(value.as_str(), OLED_128X64_DASHBOARD_VALUE_CHARS)?;
    Text::with_baseline(value.as_str(), Point::new(99, y), style, Baseline::Top)
        .draw(target)
        .map_err(GraphicsError::Draw)?;
    Ok(())
}

fn draw_record_signal<D>(
    target: &mut D,
    record: &TelemetryRecord,
    y: i32,
) -> Result<(), GraphicsError<D::Error>>
where
    D: DrawTarget<Color = BinaryColor>,
{
    let style = MonoTextStyle::new(&FONT_5X7, BinaryColor::On);
    if !record.config.enabled {
        Text::with_baseline("OFF", Point::new(0, y), style, Baseline::Top)
            .draw(target)
            .map_err(GraphicsError::Draw)?;
        return Ok(());
    }

    if record.poll.last_poll_success == Some(false) {
        Text::with_baseline("ERR", Point::new(0, y), style, Baseline::Top)
            .draw(target)
            .map_err(GraphicsError::Draw)?;
        return Ok(());
    }

    match record.telemetry.and_then(|telemetry| telemetry.rssi) {
        Some(rssi) => draw_producer_signal_indicator(target, Point::new(1, y), rssi_bars(rssi), 6)
            .map_err(GraphicsError::Draw),
        None => {
            Text::with_baseline("ERR", Point::new(0, y), style, Baseline::Top)
                .draw(target)
                .map_err(GraphicsError::Draw)?;
            Ok(())
        },
    }
}

fn dashboard_detail_line<const N: usize, const P: usize, E>(
    config: &GatewayConfig<P>,
    state: GatewayRuntimeState,
    endpoint: Option<HttpEndpoint>,
) -> Result<String<N>, GraphicsError<E>>
{
    let mut line = String::new();
    if let Some(endpoint) = endpoint {
        write!(
            &mut line,
            "{}.{}.{}.{}:{}",
            endpoint.ipv4[0], endpoint.ipv4[1], endpoint.ipv4[2], endpoint.ipv4[3], endpoint.port
        )
        .map_err(|_| GraphicsError::Buffer)?;
        return Ok(line);
    }

    match state {
        GatewayRuntimeState::Unprovisioned => line.push_str("Provision over USB"),
        GatewayRuntimeState::Provisioned => line.push_str("Starting services"),
        GatewayRuntimeState::Serving { .. } if config.http.is_some() => {
            line.push_str("WiFi starting")
        },
        GatewayRuntimeState::Serving { .. } => line.push_str("USB serial"),
        GatewayRuntimeState::Error { reason } => line.push_str(runtime_error_label(reason)),
    }
    .map_err(|_| GraphicsError::Buffer)?;
    Ok(line)
}

fn wifi_signal_bars<const N: usize>(
    config: &GatewayConfig<N>,
    state: GatewayRuntimeState,
) -> Option<u8>
{
    config.http.as_ref()?;

    match state {
        GatewayRuntimeState::Serving { interfaces } if interfaces.http.is_some() => Some(5),
        GatewayRuntimeState::Error {
            reason: GatewayRuntimeError::Network,
        } => Some(0),
        GatewayRuntimeState::Error { .. } => Some(1),
        GatewayRuntimeState::Unprovisioned | GatewayRuntimeState::Provisioned => Some(1),
        GatewayRuntimeState::Serving { .. } => Some(2),
    }
}

fn wifi_connecting_label(detail: &str) -> &'static str
{
    match detail {
        "Getting IP" => "Getting IP",
        "Reconnecting" => "Reconnecting",
        _ => "Connecting WiFi",
    }
}

fn runtime_error_label(reason: GatewayRuntimeError) -> &'static str
{
    match reason {
        GatewayRuntimeError::InvalidConfig => "Config error",
        GatewayRuntimeError::Storage => "Storage error",
        GatewayRuntimeError::Radio => "Radio error",
        GatewayRuntimeError::Meshcore => "MeshCore error",
        GatewayRuntimeError::Network => "WiFi error",
        GatewayRuntimeError::UnsupportedOutput => "Output error",
        GatewayRuntimeError::Platform => "Platform error",
    }
}

fn truncate_text<const N: usize, E>(
    value: &str,
    max_chars: usize,
) -> Result<String<N>, GraphicsError<E>>
{
    let mut out = String::new();
    if value.chars().count() <= max_chars {
        out.push_str(value).map_err(|_| GraphicsError::Buffer)?;
        return Ok(out);
    }

    let prefix_len = max_chars.saturating_sub(1);
    for ch in value.chars().take(prefix_len) {
        out.push(ch).map_err(|_| GraphicsError::Buffer)?;
    }
    out.push_str("~").map_err(|_| GraphicsError::Buffer)?;
    Ok(out)
}

fn map_buffer_error<E>(_error: Error) -> GraphicsError<E>
{
    GraphicsError::Buffer
}

/// Draw a vertical battery icon with bottom-up fill.
pub fn draw_vertical_battery<D>(
    target: &mut D,
    origin: Point,
    percent: Option<u8>,
) -> Result<(), D::Error>
where
    D: DrawTarget<Color = BinaryColor>,
{
    const BODY_WIDTH: u32 = 7;
    const BODY_HEIGHT: u32 = 9;
    const TERMINAL_WIDTH: u32 = 3;
    const TERMINAL_HEIGHT: u32 = 1;
    const INNER_FILL_WIDTH: u32 = 3;
    const INNER_FILL_HEIGHT: u32 = 7;
    const UNKNOWN_FILL_HEIGHT: u32 = 2;

    Rectangle::new(
        Point::new(origin.x + 2, origin.y),
        Size::new(TERMINAL_WIDTH, TERMINAL_HEIGHT),
    )
    .into_styled(PrimitiveStyle::with_fill(BinaryColor::On))
    .draw(target)?;
    Rectangle::new(
        Point::new(origin.x, origin.y + 1),
        Size::new(BODY_WIDTH, BODY_HEIGHT),
    )
    .into_styled(PrimitiveStyle::with_stroke(BinaryColor::On, 1))
    .draw(target)?;

    let fill_height = percent
        .map(battery_fill_height)
        .unwrap_or(UNKNOWN_FILL_HEIGHT)
        .min(INNER_FILL_HEIGHT);
    if fill_height > 0 {
        Rectangle::new(
            Point::new(
                origin.x + 2,
                origin.y + 2 + (INNER_FILL_HEIGHT - fill_height) as i32,
            ),
            Size::new(INNER_FILL_WIDTH, fill_height),
        )
        .into_styled(PrimitiveStyle::with_fill(BinaryColor::On))
        .draw(target)?;
    }
    Ok(())
}

/// Draw a compact three-bar activity animation.
pub fn draw_poll_activity<D>(target: &mut D, origin: Point, step: u8) -> Result<(), D::Error>
where
    D: DrawTarget<Color = BinaryColor>,
{
    const FRAMES: [[i32; 3]; 4] = [[2, 4, 6], [4, 6, 4], [6, 4, 2], [4, 2, 4]];
    let heights = FRAMES[usize::from(step % FRAMES.len() as u8)];

    for (index, height) in heights.iter().copied().enumerate() {
        let x = origin.x + index as i32 * 4;
        let y = origin.y + 6 - height;
        Rectangle::new(Point::new(x, y), Size::new(2, height as u32))
            .into_styled(PrimitiveStyle::with_fill(BinaryColor::On))
            .draw(target)?;
    }
    Ok(())
}
