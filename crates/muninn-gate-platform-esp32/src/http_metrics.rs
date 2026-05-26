//! ESP32 HTTP response rendering.

use core::fmt::Write;

use muninn_gate_core::config::MAX_TELEMETRY_PRODUCERS;
use muninn_gate_core::{
    DiagnosticSnapshot,
    Error,
    TelemetrySnapshot,
    TxPowerMapping,
    metrics,
    render_diagnostics_json as render_diagnostics_json_core,
};

use crate::{diagnostics, poll_control, telemetry_state};

/// Fixed buffer size for rendered Prometheus metrics.
pub const METRICS_BUFFER_BYTES: usize = 8_192;
/// Fixed buffer size for rendered diagnostic JSON.
pub const DIAGNOSTICS_BUFFER_BYTES: usize = 8_192;
/// Fixed buffer size for HTTP response bodies.
pub const HTTP_BODY_BUFFER_BYTES: usize = 8_192;
/// Fixed buffer size for HTTP response headers.
pub const HTTP_HEADER_BUFFER_BYTES: usize = 256;

/// HTTP route supported by the ESP32 metrics server.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HttpRoute
{
    /// Prometheus metrics endpoint.
    Metrics,
    /// Retained event log JSON endpoint.
    Logs,
    /// Operator-requested immediate producer poll.
    PollNow,
    /// Request did not satisfy configured HTTP authentication.
    Unauthorized,
    /// Request path is not served by this firmware.
    NotFound,
}

impl HttpRoute
{
    /// Return a compact route label for diagnostics.
    pub const fn as_str(self) -> &'static str
    {
        match self {
            Self::Metrics => "metrics",
            Self::Logs => "logs",
            Self::PollNow => "poll",
            Self::Unauthorized => "unauthorized",
            Self::NotFound => "not_found",
        }
    }
}

/// Rendered HTTP response components.
pub struct HttpResponse
{
    /// HTTP status text, for example `200 OK`.
    pub status:       &'static str,
    /// Response content type.
    pub content_type: &'static str,
    /// Response body.
    pub body:         heapless::String<HTTP_BODY_BUFFER_BYTES>,
}

/// Classify a small HTTP request into one of the supported routes.
pub fn route_request(request: &[u8]) -> HttpRoute
{
    if request_path_is(request, b"/metrics") {
        HttpRoute::Metrics
    } else if request_path_is(request, b"/logs") {
        HttpRoute::Logs
    } else if request_path_is(request, b"/poll") {
        HttpRoute::PollNow
    } else {
        HttpRoute::NotFound
    }
}

/// Return whether enough bytes have arrived to classify and handle an HTTP request.
pub fn request_headers_complete(request: &[u8]) -> bool
{
    request.windows(4).any(|window| window == b"\r\n\r\n")
        || request.windows(2).any(|window| window == b"\n\n")
}

/// Build an HTTP response for one route.
pub fn build_response(
    route: HttpRoute,
    now_ms: u64,
    tx_power: TxPowerMapping,
) -> Result<HttpResponse, Error>
{
    match route {
        HttpRoute::Metrics => {
            telemetry_state::refresh_gateway_metrics(now_ms, tx_power);
            let snapshot = telemetry_state::snapshot();
            Ok(HttpResponse {
                status:       "200 OK",
                content_type: "text/plain; version=0.0.4; charset=utf-8",
                body:         render_prometheus(&snapshot)?,
            })
        },
        HttpRoute::Logs => {
            let diagnostics = diagnostics::snapshot();
            Ok(HttpResponse {
                status:       "200 OK",
                content_type: "application/json",
                body:         render_diagnostics_json(&diagnostics)?,
            })
        },
        HttpRoute::PollNow => {
            let count = poll_control::request_poll_now();
            let mut body = heapless::String::new();
            writeln!(body, "poll requested {}", count)?;
            Ok(HttpResponse {
                status: "202 Accepted",
                content_type: "text/plain; charset=utf-8",
                body,
            })
        },
        HttpRoute::Unauthorized => {
            let mut body = heapless::String::new();
            body.push_str("unauthorized\n")
                .map_err(|_| Error::RenderBufferFull)?;
            Ok(HttpResponse {
                status: "401 Unauthorized",
                content_type: "text/plain; charset=utf-8",
                body,
            })
        },
        HttpRoute::NotFound => {
            let mut body = heapless::String::new();
            body.push_str("not found\n")
                .map_err(|_| Error::RenderBufferFull)?;
            Ok(HttpResponse {
                status: "404 Not Found",
                content_type: "text/plain; charset=utf-8",
                body,
            })
        },
    }
}

/// Render an HTTP/1.1 response header for a body.
pub fn render_http_header(
    response: &HttpResponse,
) -> Result<heapless::String<HTTP_HEADER_BUFFER_BYTES>, Error>
{
    let mut header = heapless::String::new();
    write!(
        header,
        "HTTP/1.1 {}\r\nContent-Type: {}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        response.status,
        response.content_type,
        response.body.len(),
    )?;
    Ok(header)
}

/// Render an HTTP/1.1 response header whose body is delimited by connection close.
pub fn render_stream_header(
    status: &str,
    content_type: &str,
) -> Result<heapless::String<HTTP_HEADER_BUFFER_BYTES>, Error>
{
    let mut header = heapless::String::new();
    write!(
        header,
        "HTTP/1.1 {}\r\nContent-Type: {}\r\nConnection: close\r\n\r\n",
        status, content_type,
    )?;
    Ok(header)
}

/// Render a telemetry snapshot as Prometheus text for the HTTP endpoint.
pub fn render_prometheus(
    snapshot: &TelemetrySnapshot<MAX_TELEMETRY_PRODUCERS>,
) -> Result<heapless::String<METRICS_BUFFER_BYTES>, muninn_gate_core::Error>
{
    metrics::render_prometheus::<MAX_TELEMETRY_PRODUCERS, METRICS_BUFFER_BYTES>(snapshot)
}

/// Stream a telemetry snapshot as Prometheus text into an existing writer.
pub fn render_prometheus_into<W>(
    snapshot: &TelemetrySnapshot<MAX_TELEMETRY_PRODUCERS>,
    out: &mut W,
) -> Result<(), muninn_gate_core::Error>
where
    W: core::fmt::Write + ?Sized,
{
    metrics::render_prometheus_into(snapshot, out)
}

/// Render retained diagnostics as JSON for the HTTP diagnostics endpoint.
pub fn render_diagnostics_json<const N: usize>(
    snapshot: &DiagnosticSnapshot<N>,
) -> Result<heapless::String<DIAGNOSTICS_BUFFER_BYTES>, muninn_gate_core::Error>
{
    render_diagnostics_json_core::<N, DIAGNOSTICS_BUFFER_BYTES>(snapshot)
}

fn request_path_is(request: &[u8], path: &[u8]) -> bool
{
    request.starts_with(b"GET ")
        && request
            .get(4..)
            .is_some_and(|suffix| suffix.starts_with(path))
        && matches!(
            request.get(4 + path.len()),
            Some(b' ' | b'?' | b'\r' | b'\n')
        )
}
