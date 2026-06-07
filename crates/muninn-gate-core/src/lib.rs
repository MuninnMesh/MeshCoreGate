#![cfg_attr(not(test), no_std)]
#![warn(missing_docs)]
//! Platform-independent Muninn Gate firmware core.
//!
//! This crate owns gateway configuration, telemetry producer scheduling,
//! telemetry storage, metrics rendering, runtime state, and the traits that
//! platform and board crates implement.

pub mod config;
pub mod diagnostics;
pub mod error;
pub mod metrics;
pub mod output;
pub mod poller;
pub mod runtime;
pub mod telemetry;
pub mod variant;

use core::future::Future;

pub use config::{
    DisplaySettings,
    DisplayTelemetryValue,
    GatewayConfig,
    GatewayName,
    HttpAuthToken,
    HttpConfig,
    MeshcoreConfig,
    MeshcoreMaterial,
    MeshcoreRoutingConfig,
    PollingConfig,
    RadioParams,
    TelemetryProducerConfig,
    TelemetryProducerId,
    TelemetryProducerKind,
    TelemetryProducerName,
    TelemetryProducerPath,
    TelemetryRoute,
};
pub use diagnostics::{
    DEFAULT_DIAGNOSTIC_EVENT_LIMIT,
    DiagnosticCode,
    DiagnosticCollector,
    DiagnosticEvent,
    DiagnosticLevel,
    DiagnosticMessage,
    DiagnosticSnapshot,
    DiagnosticSubsystem,
    FixedDiagnosticRing,
    NullDiagnosticCollector,
    render_diagnostics_json,
};
pub use error::Error;
pub use muninn_gate_time::{
    CST_UTC_OFFSET_MINUTES,
    GatewayTime,
    LocalDateTime,
    RtcTimeSource,
    TimeSettings,
};
pub use runtime::{GatewayRuntimeError, GatewayRuntimeState, HttpEndpoint, ServingInterfaces};
pub use telemetry::{
    FixedTelemetryStore,
    GatewayMetrics,
    HttpSocketStateSnapshot,
    MAX_TELEMETRY_CHANNELS,
    PollFailureReason,
    ProducerPollMetrics,
    ProducerTelemetry,
    ProducerTelemetryChannel,
    RepeaterStatus,
    TelemetryMetrics,
    TelemetryRecord,
    TelemetrySnapshot,
    decode_lpp_payload,
};
pub use variant::{DisplayVariant, GatewayCapabilities, TxPowerMapping};

/// Monotonic clock service supplied by a platform crate.
pub trait Clock
{
    /// Return monotonic milliseconds since boot or platform start.
    fn now_ms(&self) -> u64;
}

/// In-memory telemetry storage boundary used by the poll scheduler.
pub trait TelemetryStore<const N: usize>
{
    /// Update last-known telemetry for one producer.
    fn update_producer(
        &mut self,
        producer_id: TelemetryProducerId,
        telemetry: ProducerTelemetry,
    ) -> Result<(), Error>;

    /// Record a successful poll attempt for one producer.
    fn record_poll_success(
        &mut self,
        producer_id: TelemetryProducerId,
        timestamp_ms: u64,
    ) -> Result<(), Error>;

    /// Record a failed poll attempt for one producer.
    fn record_poll_failure(
        &mut self,
        producer_id: TelemetryProducerId,
        timestamp_ms: u64,
    ) -> Result<(), Error>;

    /// Replace gateway-wide metrics.
    fn set_gateway_metrics(&mut self, metrics: GatewayMetrics);

    /// Return mutable gateway-wide metrics.
    fn gateway_metrics_mut(&mut self) -> &mut GatewayMetrics;

    /// Return a fixed-capacity telemetry snapshot.
    fn snapshot(&self) -> TelemetrySnapshot<N>;
}

/// Minimal telemetry sink required by the polling scheduler.
///
/// Platform crates can implement this for shared stores that cannot safely
/// hand out direct mutable references to their gateway metrics.
pub trait PollTelemetryStore<const N: usize>
{
    /// Update last-known telemetry for one producer.
    fn update_producer(
        &mut self,
        producer_id: TelemetryProducerId,
        telemetry: ProducerTelemetry,
    ) -> Result<(), Error>;

    /// Record a successful poll attempt for one producer.
    fn record_poll_success(
        &mut self,
        producer_id: TelemetryProducerId,
        timestamp_ms: u64,
    ) -> Result<(), Error>;

    /// Record a failed poll attempt for one producer.
    fn record_poll_failure(
        &mut self,
        producer_id: TelemetryProducerId,
        timestamp_ms: u64,
    ) -> Result<(), Error>;
}

impl<const N: usize, T> PollTelemetryStore<N> for T
where
    T: TelemetryStore<N>,
{
    fn update_producer(
        &mut self,
        producer_id: TelemetryProducerId,
        telemetry: ProducerTelemetry,
    ) -> Result<(), Error>
    {
        TelemetryStore::update_producer(self, producer_id, telemetry)
    }

    fn record_poll_success(
        &mut self,
        producer_id: TelemetryProducerId,
        timestamp_ms: u64,
    ) -> Result<(), Error>
    {
        TelemetryStore::record_poll_success(self, producer_id, timestamp_ms)
    }

    fn record_poll_failure(
        &mut self,
        producer_id: TelemetryProducerId,
        timestamp_ms: u64,
    ) -> Result<(), Error>
    {
        TelemetryStore::record_poll_failure(self, producer_id, timestamp_ms)
    }
}

/// MeshCore polling client supplied by a board/platform integration.
pub trait MeshcoreClient
{
    /// Future returned by one producer poll request.
    type PollFuture<'a>: Future<Output = Result<ProducerTelemetry, Error>>
    where
        Self: 'a;

    /// Poll one configured MeshCore-compatible telemetry producer.
    fn poll_producer<'a>(
        &'a mut self,
        producer: &'a TelemetryProducerConfig,
    ) -> Self::PollFuture<'a>;
}

/// Renderer boundary for platform-independent metrics output.
pub trait MetricsRenderer<const OUT: usize, const N: usize>
{
    /// Render a Prometheus text snapshot into a fixed-capacity string.
    fn render_prometheus(
        &self,
        snapshot: &TelemetrySnapshot<N>,
    ) -> Result<heapless::String<OUT>, Error>;
}

/// Board-specific firmware variant selected by the top-level firmware binary.
///
/// Implementations live in board crates. The binary crate owns the actual
/// entrypoint and chooses one implementation with Cargo features.
pub trait GateFirmwareVariant
{
    /// Human-readable variant name for logs and diagnostics.
    const NAME: &'static str;

    /// MCU/platform family name.
    const PLATFORM: &'static str;

    /// Concrete board name.
    const BOARD: &'static str;

    /// Hardware and interface capabilities for this variant.
    const CAPABILITIES: GatewayCapabilities;

    /// Map a configured TX power level to the setting this board should apply.
    fn map_tx_power(requested_level: i8) -> TxPowerMapping
    {
        TxPowerMapping::passthrough(requested_level)
    }
}
