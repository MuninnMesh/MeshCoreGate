//! Diagnostic event model and collector helpers.

use core::fmt::{self, Write};

use heapless::{String, Vec};

use crate::Error;

/// Default number of diagnostic events a backend may retain.
pub const DEFAULT_DIAGNOSTIC_EVENT_LIMIT: usize = 128;
/// Maximum diagnostic event code length.
pub const MAX_DIAGNOSTIC_CODE_LEN: usize = 32;
/// Maximum diagnostic event message length.
pub const MAX_DIAGNOSTIC_MESSAGE_LEN: usize = 96;

/// Short diagnostic event code.
pub type DiagnosticCode = String<MAX_DIAGNOSTIC_CODE_LEN>;
/// Human-readable diagnostic event message.
pub type DiagnosticMessage = String<MAX_DIAGNOSTIC_MESSAGE_LEN>;

/// Severity level for a diagnostic event.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiagnosticLevel
{
    /// Verbose diagnostic detail.
    Debug,
    /// Normal operational event.
    Info,
    /// Recoverable problem or degraded behavior.
    Warn,
    /// Error that prevented requested work from completing.
    Error,
}

impl DiagnosticLevel
{
    /// Return the stable lowercase wire name.
    pub const fn as_str(self) -> &'static str
    {
        match self {
            Self::Debug => "debug",
            Self::Info => "info",
            Self::Warn => "warn",
            Self::Error => "error",
        }
    }
}

/// Firmware subsystem that produced a diagnostic event.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiagnosticSubsystem
{
    /// Boot and runtime state transitions.
    Runtime,
    /// Configuration loading, validation, or provisioning.
    Config,
    /// Persistent storage backend.
    Storage,
    /// WiFi station or network service.
    Wifi,
    /// HTTP server or response rendering.
    Http,
    /// USB or UART serial transport.
    Serial,
    /// LoRa radio device and radio owner.
    Radio,
    /// MeshCore protocol or packet handling.
    Meshcore,
    /// Poll scheduler and producer polling.
    Poller,
    /// Local display or UI output.
    Display,
}

impl DiagnosticSubsystem
{
    /// Return the stable lowercase wire name.
    pub const fn as_str(self) -> &'static str
    {
        match self {
            Self::Runtime => "runtime",
            Self::Config => "config",
            Self::Storage => "storage",
            Self::Wifi => "wifi",
            Self::Http => "http",
            Self::Serial => "serial",
            Self::Radio => "radio",
            Self::Meshcore => "meshcore",
            Self::Poller => "poller",
            Self::Display => "display",
        }
    }
}

/// One diagnostic event retained for operator debugging.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiagnosticEvent
{
    /// Monotonic sequence assigned by the collector.
    pub sequence:     u64,
    /// Monotonic event timestamp in milliseconds.
    pub timestamp_ms: u64,
    /// Event severity.
    pub level:        DiagnosticLevel,
    /// Subsystem that produced the event.
    pub subsystem:    DiagnosticSubsystem,
    /// Stable short event code.
    pub code:         DiagnosticCode,
    /// Human-readable event message.
    pub message:      DiagnosticMessage,
}

impl DiagnosticEvent
{
    /// Create a diagnostic event with sequence `0`.
    pub fn new(
        timestamp_ms: u64,
        level: DiagnosticLevel,
        subsystem: DiagnosticSubsystem,
        code: &str,
        message: &str,
    ) -> Result<Self, Error>
    {
        Ok(Self {
            sequence: 0,
            timestamp_ms,
            level,
            subsystem,
            code: fixed_string(code)?,
            message: fixed_string(message)?,
        })
    }
}

/// Snapshot of retained diagnostic events.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiagnosticSnapshot<const N: usize = DEFAULT_DIAGNOSTIC_EVENT_LIMIT>
{
    /// Retained diagnostic events ordered from oldest to newest.
    pub events:        Vec<DiagnosticEvent, N>,
    /// Events dropped because the backing collector was full.
    pub dropped_total: u64,
}

impl<const N: usize> DiagnosticSnapshot<N>
{
    /// Create an empty diagnostic snapshot.
    pub const fn new() -> Self
    {
        Self {
            events:        Vec::new(),
            dropped_total: 0,
        }
    }
}

impl<const N: usize> Default for DiagnosticSnapshot<N>
{
    fn default() -> Self
    {
        Self::new()
    }
}

/// Diagnostic event collector boundary supplied by platform code.
pub trait DiagnosticCollector<const N: usize = DEFAULT_DIAGNOSTIC_EVENT_LIMIT>
{
    /// Record one diagnostic event.
    fn record(&mut self, event: DiagnosticEvent) -> Result<(), Error>;

    /// Return the retained diagnostic events.
    fn snapshot(&self) -> DiagnosticSnapshot<N>;
}

/// Fixed-capacity diagnostic collector useful for tests and simple builds.
///
/// Production ESP32 builds should prefer a flash-backed implementation
/// when retaining diagnostics across crashes or reboots is more important
/// than minimizing writes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FixedDiagnosticRing<const N: usize = DEFAULT_DIAGNOSTIC_EVENT_LIMIT>
{
    events:        Vec<DiagnosticEvent, N>,
    next_sequence: u64,
    dropped_total: u64,
}

impl<const N: usize> FixedDiagnosticRing<N>
{
    /// Create an empty diagnostic ring.
    pub const fn new() -> Self
    {
        Self {
            events:        Vec::new(),
            next_sequence: 0,
            dropped_total: 0,
        }
    }
}

impl<const N: usize> Default for FixedDiagnosticRing<N>
{
    fn default() -> Self
    {
        Self::new()
    }
}

impl<const N: usize> DiagnosticCollector<N> for FixedDiagnosticRing<N>
{
    fn record(&mut self, mut event: DiagnosticEvent) -> Result<(), Error>
    {
        event.sequence = self.next_sequence;
        self.next_sequence = self.next_sequence.saturating_add(1);

        if self.events.len() == N {
            self.dropped_total = self.dropped_total.saturating_add(1);
            if N == 0 {
                return Ok(());
            }
            let _ = self.events.remove(0);
        }

        self.events.push(event).map_err(|_| Error::Capacity)
    }

    fn snapshot(&self) -> DiagnosticSnapshot<N>
    {
        DiagnosticSnapshot {
            events:        self.events.clone(),
            dropped_total: self.dropped_total,
        }
    }
}

/// Diagnostic collector that intentionally drops all events.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct NullDiagnosticCollector;

impl<const N: usize> DiagnosticCollector<N> for NullDiagnosticCollector
{
    fn record(&mut self, _event: DiagnosticEvent) -> Result<(), Error>
    {
        Ok(())
    }

    fn snapshot(&self) -> DiagnosticSnapshot<N>
    {
        DiagnosticSnapshot::new()
    }
}

/// Render diagnostic events as compact JSON for HTTP or serial debugging.
pub fn render_diagnostics_json<const N: usize, const OUT: usize>(
    snapshot: &DiagnosticSnapshot<N>,
) -> Result<String<OUT>, Error>
{
    let mut out = String::new();
    write!(
        out,
        "{{\"dropped_total\":{},\"events\":[",
        snapshot.dropped_total
    )?;

    for (index, event) in snapshot.events.iter().enumerate() {
        if index > 0 {
            write!(out, ",")?;
        }
        write!(
            out,
            "{{\"sequence\":{},\"timestamp_ms\":{},\"level\":\"{}\",\"subsystem\":\"{}\",\"code\":\
             \"",
            event.sequence,
            event.timestamp_ms,
            event.level.as_str(),
            event.subsystem.as_str()
        )?;
        write_json_string_content(&mut out, event.code.as_str())?;
        write!(out, "\",\"message\":\"")?;
        write_json_string_content(&mut out, event.message.as_str())?;
        write!(out, "\"}}")?;
    }

    write!(out, "]}}")?;
    Ok(out)
}

fn fixed_string<const N: usize>(value: &str) -> Result<String<N>, Error>
{
    let mut out = String::new();
    out.push_str(value).map_err(|_| Error::InvalidConfig)?;
    Ok(out)
}

fn write_json_string_content<const OUT: usize>(
    out: &mut String<OUT>,
    value: &str,
) -> Result<(), fmt::Error>
{
    for ch in value.chars() {
        match ch {
            '"' => out.push_str("\\\"").map_err(|_| fmt::Error)?,
            '\\' => out.push_str("\\\\").map_err(|_| fmt::Error)?,
            '\n' => out.push_str("\\n").map_err(|_| fmt::Error)?,
            '\r' => out.push_str("\\r").map_err(|_| fmt::Error)?,
            '\t' => out.push_str("\\t").map_err(|_| fmt::Error)?,
            _ => out.push(ch).map_err(|_| fmt::Error)?,
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests
{
    use crate::diagnostics::{
        DiagnosticCollector,
        DiagnosticEvent,
        DiagnosticLevel,
        DiagnosticSubsystem,
        FixedDiagnosticRing,
        render_diagnostics_json,
    };

    #[test]
    fn fixed_ring_retains_last_n_events()
    {
        let mut ring = FixedDiagnosticRing::<2>::new();
        for index in 0..3 {
            ring.record(
                DiagnosticEvent::new(
                    index,
                    DiagnosticLevel::Info,
                    DiagnosticSubsystem::Poller,
                    "poll",
                    "poll completed",
                )
                .unwrap(),
            )
            .unwrap();
        }

        let snapshot = ring.snapshot();

        assert_eq!(snapshot.dropped_total, 1);
        assert_eq!(snapshot.events.len(), 2);
        assert_eq!(snapshot.events[0].sequence, 1);
        assert_eq!(snapshot.events[1].sequence, 2);
    }

    #[test]
    fn renders_diagnostics_json()
    {
        let mut ring = FixedDiagnosticRing::<1>::new();
        ring.record(
            DiagnosticEvent::new(
                42,
                DiagnosticLevel::Warn,
                DiagnosticSubsystem::Wifi,
                "wifi_retry",
                "retry \"ssid\"",
            )
            .unwrap(),
        )
        .unwrap();

        let rendered = render_diagnostics_json::<1, 512>(&ring.snapshot()).unwrap();

        assert!(rendered.contains("\"dropped_total\":0"));
        assert!(rendered.contains("\"level\":\"warn\""));
        assert!(rendered.contains("\"subsystem\":\"wifi\""));
        assert!(rendered.contains("retry \\\"ssid\\\""));
    }
}
