//! ESP32 scheduler runtime integration.
//!
//! The scheduler runs on the PRO CPU alongside WiFi, HTTP, USB serial, and the
//! display. The APP CPU owns LoRa IRQ polling and only communicates through
//! fixed queues, so this loop cannot block or directly touch radio hardware.

use embassy_futures::block_on;
use muninn_gate_core::config::MAX_TELEMETRY_PRODUCERS;
use muninn_gate_core::poller::{PollScheduler, PollSummary};
use muninn_gate_core::{
    Clock,
    DiagnosticLevel,
    DiagnosticSubsystem,
    Error,
    GateFirmwareVariant,
    GatewayConfig,
    GatewayRuntimeState,
    TxPowerMapping,
};

use crate::display::LocalDisplay;
use crate::input::UserButton;
use crate::platform::Esp32Platform;
use crate::{diagnostics, meshcore, poll_control, serial, telemetry_state};

/// Minimum interval between unchanged serial telemetry snapshots.
pub const SERIAL_HEARTBEAT_MS: u64 = 60_000;
/// Minimum interval between unchanged OLED dashboard refreshes.
pub const DISPLAY_REFRESH_MS: u64 = 5_000;
/// Duration for the on-device poll-request acknowledgement panel.
pub const POLL_REQUEST_NOTICE_MS: u64 = 1_000;

/// Stateful scheduler runtime used by HTTP and serial-only loops.
pub struct GatewayScheduler
{
    scheduler:            PollScheduler<MAX_TELEMETRY_PRODUCERS>,
    last_serial_json_ms:  u64,
    last_display_ms:      u64,
    poll_notice_until_ms: Option<u64>,
}

impl GatewayScheduler
{
    /// Build scheduler state from validated gateway configuration.
    pub fn new(
        config: &GatewayConfig<MAX_TELEMETRY_PRODUCERS>,
        start_ms: u64,
    ) -> Result<Self, Error>
    {
        Ok(Self {
            scheduler:            PollScheduler::new(config, start_ms)?,
            last_serial_json_ms:  start_ms,
            last_display_ms:      start_ms,
            poll_notice_until_ms: None,
        })
    }

    /// Run one non-blocking scheduler and output pass.
    pub fn tick<V>(
        &mut self,
        platform: &Esp32Platform,
        config: &GatewayConfig<MAX_TELEMETRY_PRODUCERS>,
        tx_power: TxPowerMapping,
        state: GatewayRuntimeState,
        mut display: Option<&mut LocalDisplay>,
        button: Option<&mut UserButton>,
    ) where
        V: GateFirmwareVariant,
    {
        let now_ms = platform.now_ms();
        telemetry_state::refresh_gateway_metrics(now_ms, tx_power);

        if let Some(button) = button
            && button.poll_pressed(now_ms)
        {
            let _ = poll_control::request_poll_now();
            self.poll_notice_until_ms = Some(now_ms.saturating_add(POLL_REQUEST_NOTICE_MS));
            self.last_display_ms = now_ms;
            serial::write_line("Poll: requested from user button");
            record_event(
                now_ms,
                DiagnosticLevel::Info,
                "poll_button",
                "operator requested immediate producer poll from user button",
            );
            if let Some(display) = display.as_deref_mut() {
                let _ = display.show_poll_requested();
            }
        }

        let poll_now_requests = poll_control::take_poll_now_requests();
        if poll_now_requests > 0 {
            self.scheduler.force_all_due(now_ms);
            record_event(
                now_ms,
                DiagnosticLevel::Info,
                "poll_now",
                "operator requested immediate producer poll",
            );
        }

        let due_count = self.scheduler.due_producers(now_ms).len();
        let summary = if due_count > 0 {
            self.poll_due::<V>(config, platform, state, display.as_deref_mut())
        } else {
            PollSummary::default()
        };

        let observed = meshcore::drain_observations(config, platform.now_ms());
        let changed = summary.attempted > 0 || observed > 0 || poll_now_requests > 0;

        if summary.attempted > 0 || poll_now_requests > 0 {
            telemetry_state::record_poll_pass(
                summary.retrying,
                poll_now_requests,
                if summary.attempted > 0 {
                    Some(poll_latency_ms(platform, now_ms))
                } else {
                    None
                },
            );
            record_poll_summary(platform.now_ms(), summary);
        }

        telemetry_state::refresh_gateway_metrics(platform.now_ms(), tx_power);
        let snapshot = telemetry_state::snapshot();
        let display_now_ms = platform.now_ms();
        let notice_active = self
            .poll_notice_until_ms
            .is_some_and(|until_ms| display_now_ms < until_ms);
        let notice_expired = self
            .poll_notice_until_ms
            .is_some_and(|until_ms| display_now_ms >= until_ms);
        if notice_expired {
            self.poll_notice_until_ms = None;
        }

        if changed
            || platform.now_ms().saturating_sub(self.last_serial_json_ms) >= SERIAL_HEARTBEAT_MS
        {
            if serial::write_snapshot_json(&snapshot).is_ok() {
                self.last_serial_json_ms = platform.now_ms();
            } else {
                record_event(
                    platform.now_ms(),
                    DiagnosticLevel::Error,
                    "serial_render_failed",
                    "serial telemetry JSON render failed",
                );
            }
        }

        if let Some(display) = display.as_mut()
            && !notice_active
            && (changed
                || notice_expired
                || platform.now_ms().saturating_sub(self.last_display_ms) >= DISPLAY_REFRESH_MS)
        {
            let result = if state.http_endpoint().is_some() {
                display.show_http_dashboard::<V>(config, &snapshot, state, platform.now_ms())
            } else {
                display.show_status::<V>(config, &snapshot, state, platform.now_ms())
            };
            if result.is_ok() {
                self.last_display_ms = platform.now_ms();
            } else {
                record_event(
                    platform.now_ms(),
                    DiagnosticLevel::Warn,
                    "display_refresh_failed",
                    "OLED status refresh failed",
                );
            }
        }
    }

    fn poll_due<V>(
        &mut self,
        config: &GatewayConfig<MAX_TELEMETRY_PRODUCERS>,
        platform: &Esp32Platform,
        state: GatewayRuntimeState,
        display: Option<&mut LocalDisplay>,
    ) -> PollSummary
    where
        V: GateFirmwareVariant,
    {
        let now_ms = platform.now_ms();
        let mut client = meshcore::RadioMeshcoreClient::new(config, now_ms);
        client.set_now_ms(now_ms);
        client.set_progress_display(display, state, V::BOARD);
        let mut store = telemetry_state::SharedTelemetryStore;
        block_on(
            self.scheduler
                .poll_due_producers(config, &mut client, &mut store, now_ms),
        )
    }
}

trait RuntimeStateEndpoint
{
    fn http_endpoint(self) -> Option<muninn_gate_core::HttpEndpoint>;
}

impl RuntimeStateEndpoint for GatewayRuntimeState
{
    fn http_endpoint(self) -> Option<muninn_gate_core::HttpEndpoint>
    {
        match self {
            Self::Serving { interfaces } => interfaces.http,
            Self::Unprovisioned | Self::Provisioned | Self::Error { .. } => None,
        }
    }
}

fn poll_latency_ms(platform: &Esp32Platform, started_ms: u64) -> u32
{
    platform
        .now_ms()
        .saturating_sub(started_ms)
        .min(u64::from(u32::MAX)) as u32
}

fn record_poll_summary(now_ms: u64, summary: PollSummary)
{
    if summary.attempted == 0 {
        return;
    }

    let (level, code, message) = if summary.failed > 0 {
        (
            DiagnosticLevel::Warn,
            "poll_pass_degraded",
            "scheduled poll pass completed with failures",
        )
    } else if summary.retrying > 0 {
        (
            DiagnosticLevel::Warn,
            "poll_pass_retrying",
            "scheduled poll pass deferred producers for retry",
        )
    } else {
        (
            DiagnosticLevel::Info,
            "poll_pass_complete",
            "scheduled poll pass completed",
        )
    };
    record_event(now_ms, level, code, message);
}

fn record_event(timestamp_ms: u64, level: DiagnosticLevel, code: &str, message: &str)
{
    let _ = diagnostics::record(
        timestamp_ms,
        level,
        DiagnosticSubsystem::Poller,
        code,
        message,
    );
}
