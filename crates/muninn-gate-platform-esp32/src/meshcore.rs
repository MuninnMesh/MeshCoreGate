//! ESP32 MeshCore adapter backed by the radio owner queues.
//!
//! This module keeps the scheduler side independent from the SX126x owner. The
//! current ESP32 reliability path services that owner cooperatively from the
//! WiFi/HTTP loop and only talks to it through fixed TX/RX queues. Active
//! polling follows the official MeshCore firmware flow:
//! companion nodes receive encrypted telemetry requests directly, while
//! repeaters receive a login request before the telemetry request.

use core::future::{Ready, ready};
use core::sync::atomic::{AtomicU32, Ordering};

use esp_hal::delay::Delay;
use esp_hal::time::Instant;
use muninn_gate_core::config::MAX_TELEMETRY_PRODUCERS;
use muninn_gate_core::{
    DiagnosticLevel,
    DiagnosticSubsystem,
    Error,
    GatewayConfig,
    GatewayRuntimeState,
    MeshcoreClient,
    PollFailureReason,
    ProducerTelemetry,
    RepeaterStatus,
    TelemetryProducerConfig,
    TelemetryProducerId,
    TelemetryProducerKind,
    decode_lpp_payload,
};
use muninn_mesh_meshcore_lib::{
    MESH_KIND_PATH,
    MESH_KIND_RESPONSE,
    MeshTxFrame,
    MeshcoreClientPacketError,
    MeshcoreContact,
    MeshcoreIdentity,
    MeshcoreRequestRoute,
    build_gateway_advert,
    build_login_request,
    build_status_request,
    build_telemetry_request,
    decrypt_response,
    login_response_is_success,
    meshcore_payload_body,
    response_is_login_success,
    telemetry_response_lpp,
};
use muninn_mesh_radio::MeshRxFrame;

use crate::display::LocalDisplay;
use crate::{diagnostics, radio, storage, telemetry_state};

/// Login wait budget for MeshCore repeater authentication.
///
/// Repeaters may flood the login response when they do not have a known return
/// path for this gateway yet. Five seconds was too tight in real traffic and
/// caused the UI to mark the producer failed just before late responses
/// arrived.
pub const MESHCORE_LOGIN_TIMEOUT_MS: u64 = 12_000;
/// Telemetry response wait budget after a request has been transmitted.
pub const MESHCORE_RESPONSE_TIMEOUT_MS: u64 = 10_000;
/// GET_STATUS poll cadence for repeater producers (slower than telemetry to
/// limit airtime/battery on solar nodes). Status changes slowly.
pub const STATUS_POLL_INTERVAL_MS: u64 = 5 * 60 * 1000;
/// Minimum RepeaterStats payload length (packed struct after the 4-byte tag).
const REPEATER_STATS_MIN_LEN: usize = 56;
/// RX queue polling interval while waiting for a MeshCore response.
pub const MESHCORE_RESPONSE_WAIT_STEP_MS: u32 = 20;
/// Minimum interval between OLED poll spinner frames.
pub const POLL_SPINNER_REFRESH_MS: u64 = 250;
/// Initial high tag range avoids replay rejection against normal Unix time.
pub const MESHCORE_REQUEST_TAG_BASE: u32 = 0xf000_0000;
/// Number of MeshCore request tags reserved per boot in persistent storage.
pub const MESHCORE_REQUEST_TAG_RESERVE_COUNT: u32 = 0x0000_1000;
/// Unix-range timestamp reserve for Bifrost's MeshCore wire timestamps.
pub const MESHCORE_WALL_TIMESTAMP_RESERVE_COUNT: u32 = 0x0000_0400;

static NEXT_REQUEST_TAG: AtomicU32 = AtomicU32::new(MESHCORE_REQUEST_TAG_BASE);
static MIN_MESHCORE_TIMESTAMP: AtomicU32 = AtomicU32::new(1);

/// Reserve and initialize the MeshCore request-tag range for this boot.
pub fn initialize_request_tags(now_ms: u64)
{
    match storage::reserve_request_tag_seed(
        MESHCORE_REQUEST_TAG_BASE,
        MESHCORE_REQUEST_TAG_RESERVE_COUNT,
    ) {
        Ok(seed) => {
            NEXT_REQUEST_TAG.store(seed, Ordering::Relaxed);
            record_poll_diagnostic(
                now_ms,
                DiagnosticLevel::Info,
                "request_tags_reserved",
                "MeshCore request tag range reserved",
            );
        },
        Err(_) => {
            NEXT_REQUEST_TAG.store(MESHCORE_REQUEST_TAG_BASE, Ordering::Relaxed);
            record_poll_diagnostic(
                now_ms,
                DiagnosticLevel::Warn,
                "request_tags_reserve_failed",
                "MeshCore request tags are using volatile fallback seed",
            );
        },
    }
}

/// Initialize MeshCore request timestamps from wall-clock seconds.
///
/// Bifrost uses this path because MeshCore request payloads carry sender time.
/// Using the high persistent request-tag range here makes requests look like
/// year-2097 messages and can poison producer-side replay state.
pub fn initialize_request_tags_from_time(now_ms: u64, unix_seconds: Option<u64>)
{
    let fallback_seed = unix_seconds
        .filter(|seconds| *seconds > 0 && *seconds < u64::from(u32::MAX))
        .map(|seconds| seconds as u32)
        .unwrap_or_else(|| (now_ms / 1_000).clamp(1, u64::from(u32::MAX)) as u32)
        .max(1);
    let seed = match storage::reserve_request_tag_seed_bounded(
        fallback_seed,
        MESHCORE_WALL_TIMESTAMP_RESERVE_COUNT,
        MESHCORE_REQUEST_TAG_BASE.saturating_sub(1),
    ) {
        Ok(seed) => seed,
        Err(_) => fallback_seed,
    };
    NEXT_REQUEST_TAG.store(seed, Ordering::Relaxed);
    MIN_MESHCORE_TIMESTAMP.store(seed, Ordering::Relaxed);
    record_poll_diagnostic(
        now_ms,
        DiagnosticLevel::Info,
        "request_tags_wall_time",
        "MeshCore request tags seeded from wall-clock time",
    );
    esp_println::println!("Meshcore: request timestamps seeded at {}", seed);
}

/// Queue the gateway's signed MeshCore advert after radio startup.
pub fn queue_startup_advert(config: &GatewayConfig<MAX_TELEMETRY_PRODUCERS>, now_ms: u64)
{
    let _ = queue_gateway_advert(config, now_ms, None, "startup");
}

/// Queue a signed direct MeshCore advert for this gateway.
///
/// When wall-clock time is available, use it for the signed advert timestamp.
/// Producers use this field for replay protection, so a real Unix timestamp is
/// safer than boot-relative uptime and easier to inspect on other MeshCore
/// devices. If time has not been seeded yet, fall back to low boot uptime
/// seconds. Never use the high request-tag range here: once a producer accepts
/// a far-future advert timestamp for this public key, normal wall-clock adverts
/// look like replays.
pub fn queue_gateway_advert(
    config: &GatewayConfig<MAX_TELEMETRY_PRODUCERS>,
    now_ms: u64,
    unix_seconds: Option<u64>,
    reason: &'static str,
) -> bool
{
    let identity = match gateway_identity(config) {
        Ok(identity) => identity,
        Err(_) => {
            record_poll_diagnostic(
                now_ms,
                DiagnosticLevel::Warn,
                "startup_advert_skipped",
                "MeshCore identity is not valid for signed adverts",
            );
            esp_println::println!(
                "Meshcore: direct advert skipped reason={} invalid_identity",
                reason
            );
            return false;
        },
    };
    let timestamp_secs = advert_timestamp_secs(now_ms, unix_seconds);
    let advert = match build_gateway_advert(&identity, config.name.as_str(), timestamp_secs) {
        Ok(advert) => advert,
        Err(_) => {
            record_poll_diagnostic(
                now_ms,
                DiagnosticLevel::Warn,
                "startup_advert_build_failed",
                "gateway MeshCore advert could not be built",
            );
            esp_println::println!("Meshcore: direct advert build failed reason={}", reason);
            return false;
        },
    };
    let payload_len = advert.payload_slice().len();
    match radio::enqueue_tx_frame(advert) {
        Ok(()) => {
            record_poll_diagnostic(
                now_ms,
                DiagnosticLevel::Info,
                "startup_advert_queued",
                "gateway MeshCore advert queued for transmit",
            );
            esp_println::println!(
                "Meshcore: direct advert queued reason={} ts={} len={}",
                reason,
                timestamp_secs,
                payload_len,
            );
            true
        },
        Err(_) => {
            record_poll_diagnostic(
                now_ms,
                DiagnosticLevel::Warn,
                "startup_advert_queue_full",
                "gateway MeshCore advert could not be queued",
            );
            esp_println::println!("Meshcore: direct advert queue full reason={}", reason);
            false
        },
    }
}

/// MeshCore client that observes frames received by the radio owner.
pub struct RadioMeshcoreClient<'a>
{
    config:      &'a GatewayConfig<MAX_TELEMETRY_PRODUCERS>,
    identity:    Result<MeshcoreIdentity, PollFailureReason>,
    now_ms:      u64,
    progress:    Option<PollProgressDisplay<'a>>,
    maintenance: Option<&'a mut dyn FnMut()>,
}

impl<'a> RadioMeshcoreClient<'a>
{
    /// Create a client using `now_ms` as the timestamp for returned telemetry.
    pub fn new(config: &'a GatewayConfig<MAX_TELEMETRY_PRODUCERS>, now_ms: u64) -> Self
    {
        Self {
            config,
            identity: gateway_identity(config),
            now_ms,
            progress: None,
            maintenance: None,
        }
    }

    /// Attach an OLED progress renderer for blocking producer polls.
    pub fn set_progress_display(
        &mut self,
        display: Option<&'a mut LocalDisplay>,
        state: GatewayRuntimeState,
        board_name: &'static str,
    )
    {
        self.progress = display.map(|display| PollProgressDisplay {
            display,
            state,
            board_name,
            next_refresh_ms: 0,
            spinner_step: 0,
        });
    }

    /// Update the timestamp used for returned telemetry.
    pub fn set_now_ms(&mut self, now_ms: u64)
    {
        self.now_ms = now_ms;
    }

    /// Attach cooperative work that must continue while MeshCore waits.
    pub fn set_network_maintenance<M>(&mut self, maintenance: &'a mut M)
    where
        M: FnMut() + 'a,
    {
        self.maintenance = Some(maintenance);
    }

    /// Run the blocking request/response poll used by the synchronous scheduler.
    fn poll_producer_blocking(
        &mut self,
        producer: &TelemetryProducerConfig,
    ) -> Result<ProducerTelemetry, Error>
    {
        let producer_id = producer.id();
        let identity = match self.identity {
            Ok(identity) => identity,
            Err(reason) => return Err(record_poll_failure(producer_id, self.now_ms, reason)),
        };
        let contact = match producer_contact(producer) {
            Ok(contact) => contact,
            Err(reason) => return Err(record_poll_failure(producer_id, self.now_ms, reason)),
        };
        self.render_poll_progress(producer_id, self.now_ms);
        if producer.kind == TelemetryProducerKind::Repeater {
            let password = producer
                .password
                .as_ref()
                .map(|password| password.as_str())
                .unwrap_or("");
            let login_tag = next_request_tag(self.now_ms);
            let login = build_login_request(&identity, &contact, password, login_tag)
                .map_err(packet_error_to_reason)
                .map_err(|reason| record_poll_failure(producer_id, self.now_ms, reason))?;
            enqueue_frame(producer_id, self.now_ms, login)?;
            self.wait_for_login(
                &identity,
                &contact,
                producer_id,
                PollRadioBaseline::capture(),
            )
            .map_err(|reason| record_poll_failure(producer_id, self.now_ms, reason))?;
        }

        let request_tag = next_request_tag(self.now_ms);
        let nonce = request_nonce(request_tag, producer_id);
        let request = build_telemetry_request(&identity, &contact, request_tag, nonce)
            .map_err(packet_error_to_reason)
            .map_err(|reason| record_poll_failure(producer_id, self.now_ms, reason))?;
        enqueue_frame(producer_id, self.now_ms, request)?;

        let mut telemetry = self
            .wait_for_telemetry(
                &identity,
                &contact,
                producer_id,
                request_tag,
                PollRadioBaseline::capture(),
            )
            .map_err(|reason| record_poll_failure(producer_id, self.now_ms, reason))?;

        // Repeater GET_STATUS (noise floor, packet counters, airtime, uptime) on
        // a slower cadence than telemetry. Best-effort: a failure never fails the
        // telemetry poll, and the attempt is throttled regardless of outcome.
        if producer.kind == TelemetryProducerKind::Repeater
            && telemetry_state::status_poll_due(producer_id, self.now_ms, STATUS_POLL_INTERVAL_MS)
        {
            telemetry_state::mark_status_polled(producer_id, self.now_ms);
            let status_tag = next_request_tag(self.now_ms);
            if let Ok(status_req) = build_status_request(&identity, &contact, status_tag) {
                if enqueue_frame(producer_id, self.now_ms, status_req).is_ok() {
                    if let Ok(status) = self.wait_for_status(
                        &identity,
                        &contact,
                        producer_id,
                        status_tag,
                        PollRadioBaseline::capture(),
                    ) {
                        telemetry.status = Some(status);
                    }
                }
            }
        }

        Ok(telemetry)
    }

    /// Wait for a GET_STATUS response and parse the repeater stats blob.
    fn wait_for_status(
        &mut self,
        identity: &MeshcoreIdentity,
        contact: &MeshcoreContact,
        producer_id: TelemetryProducerId,
        request_tag: u32,
        baseline: PollRadioBaseline,
    ) -> Result<RepeaterStatus, PollFailureReason>
    {
        let mut last_error = None;
        let mut observed_rx_frames = 0_u32;
        let mut observed_response_frames = 0_u32;
        let start = Instant::now();
        let delay = Delay::new();
        while start.elapsed().as_millis() < MESHCORE_RESPONSE_TIMEOUT_MS {
            self.render_poll_progress(producer_id, poll_wait_now_ms(self.now_ms, start));
            while let Some(frame) = radio::try_dequeue_rx_frame() {
                observed_rx_frames = observed_rx_frames.saturating_add(1);
                if !is_response_frame(&frame) {
                    self.record_passive_observation(&frame);
                    continue;
                }
                match decrypt_response(identity, contact, frame.raw_payload()) {
                    Ok(response) => {
                        if response_is_login_success(&response) {
                            continue;
                        }
                        observed_response_frames = observed_response_frames.saturating_add(1);
                        match telemetry_response_lpp(&response, request_tag) {
                            Ok(blob) => match parse_repeater_stats(blob) {
                                Some(status) => return Ok(status),
                                None => last_error = Some(PollFailureReason::DecodeFailed),
                            },
                            Err(MeshcoreClientPacketError::ResponseTagMismatch) => {
                                last_error = Some(PollFailureReason::ResponseTagMismatch);
                            },
                            Err(_) => last_error = Some(PollFailureReason::DecodeFailed),
                        }
                    },
                    Err(error) if packet_error_is_background(error) => {
                        log_response_reject("status/background", producer_id, &frame, error);
                    },
                    Err(error) => {
                        log_response_reject("status", producer_id, &frame, error);
                        last_error = Some(packet_error_to_reason(error));
                    },
                }
            }
            self.maintenance_delay(&delay);
        }
        Err(baseline.classify_timeout(observed_rx_frames, observed_response_frames, last_error))
    }

    /// Wait for a login response from the producer.
    fn wait_for_login(
        &mut self,
        identity: &MeshcoreIdentity,
        contact: &MeshcoreContact,
        producer_id: TelemetryProducerId,
        baseline: PollRadioBaseline,
    ) -> Result<(), PollFailureReason>
    {
        let mut last_error = None;
        let mut observed_rx_frames = 0_u32;
        let mut observed_response_frames = 0_u32;
        let start = Instant::now();
        let delay = Delay::new();
        while start.elapsed().as_millis() < MESHCORE_LOGIN_TIMEOUT_MS {
            self.render_poll_progress(producer_id, poll_wait_now_ms(self.now_ms, start));
            while let Some(frame) = radio::try_dequeue_rx_frame() {
                observed_rx_frames = observed_rx_frames.saturating_add(1);
                if !is_response_frame(&frame) {
                    self.record_passive_observation(&frame);
                    continue;
                }
                match decrypt_response(identity, contact, frame.raw_payload()) {
                    Ok(response) => {
                        observed_response_frames = observed_response_frames.saturating_add(1);
                        match login_response_is_success(&response) {
                            Ok(()) => return Ok(()),
                            Err(MeshcoreClientPacketError::AuthFailed) => {
                                return Err(PollFailureReason::AuthFailed);
                            },
                            Err(_) => {
                                last_error = Some(PollFailureReason::DecodeFailed);
                            },
                        }
                    },
                    Err(error) if packet_error_is_background(error) => {
                        log_response_reject("login/background", producer_id, &frame, error);
                    },
                    Err(error) if packet_error_can_be_ignored(error) => {
                        observed_response_frames = observed_response_frames.saturating_add(1);
                        log_response_reject("login", producer_id, &frame, error);
                        last_error = Some(packet_error_to_reason(error));
                    },
                    Err(error) => {
                        observed_response_frames = observed_response_frames.saturating_add(1);
                        log_response_reject("login", producer_id, &frame, error);
                        last_error = Some(packet_error_to_reason(error));
                    },
                }
            }
            self.maintenance_delay(&delay);
        }

        let reason =
            baseline.classify_timeout(observed_rx_frames, observed_response_frames, last_error);
        Err(reason)
    }

    /// Wait for a telemetry response and decode the returned LPP payload.
    fn wait_for_telemetry(
        &mut self,
        identity: &MeshcoreIdentity,
        contact: &MeshcoreContact,
        producer_id: TelemetryProducerId,
        request_tag: u32,
        baseline: PollRadioBaseline,
    ) -> Result<ProducerTelemetry, PollFailureReason>
    {
        let mut last_error = None;
        let mut observed_rx_frames = 0_u32;
        let mut observed_response_frames = 0_u32;
        let start = Instant::now();
        let delay = Delay::new();
        while start.elapsed().as_millis() < MESHCORE_RESPONSE_TIMEOUT_MS {
            self.render_poll_progress(producer_id, poll_wait_now_ms(self.now_ms, start));
            while let Some(frame) = radio::try_dequeue_rx_frame() {
                observed_rx_frames = observed_rx_frames.saturating_add(1);
                if !is_response_frame(&frame) {
                    self.record_passive_observation(&frame);
                    continue;
                }
                match decrypt_response(identity, contact, frame.raw_payload()) {
                    Ok(response) => {
                        observed_response_frames = observed_response_frames.saturating_add(1);
                        if response_is_login_success(&response) {
                            continue;
                        }
                        match telemetry_response_lpp(&response, request_tag) {
                            Ok(payload) => {
                                let mut telemetry =
                                    ProducerTelemetry::new(producer_id, self.now_ms);
                                telemetry.rssi = Some(frame.message.rssi_dbm);
                                telemetry.snr = Some(frame.message.snr_tenth_db as f32 / 10.0);
                                decode_lpp_payload(&mut telemetry, payload);
                                return Ok(telemetry);
                            },
                            Err(MeshcoreClientPacketError::ResponseTagMismatch) => {
                                last_error = Some(PollFailureReason::ResponseTagMismatch);
                            },
                            Err(_) => {
                                last_error = Some(PollFailureReason::DecodeFailed);
                            },
                        }
                    },
                    Err(error) if packet_error_is_background(error) => {
                        log_response_reject("telemetry/background", producer_id, &frame, error);
                    },
                    Err(error) if packet_error_can_be_ignored(error) => {
                        observed_response_frames = observed_response_frames.saturating_add(1);
                        log_response_reject("telemetry", producer_id, &frame, error);
                        last_error = Some(packet_error_to_reason(error));
                    },
                    Err(error) => {
                        observed_response_frames = observed_response_frames.saturating_add(1);
                        log_response_reject("telemetry", producer_id, &frame, error);
                        last_error = Some(packet_error_to_reason(error));
                    },
                }
            }
            self.maintenance_delay(&delay);
        }

        let reason =
            baseline.classify_timeout(observed_rx_frames, observed_response_frames, last_error);
        Err(reason)
    }

    /// Preserve passive observations while active polling drains the RX queue.
    fn record_passive_observation(&self, frame: &MeshRxFrame)
    {
        if let Some((producer_id, telemetry)) =
            telemetry_for_any_producer(self.config, frame, self.now_ms)
        {
            let _ = telemetry_state::record_producer_observation(producer_id, telemetry);
        }
    }

    fn render_poll_progress(&mut self, producer_id: TelemetryProducerId, now_ms: u64)
    {
        let Some(progress) = self.progress.as_mut() else {
            return;
        };
        progress.render(self.config, producer_id, now_ms);
    }

    fn maintenance_delay(&mut self, delay: &Delay)
    {
        for _ in 0..MESHCORE_RESPONSE_WAIT_STEP_MS {
            if let Some(maintenance) = self.maintenance.as_deref_mut() {
                maintenance();
            }
            delay.delay_millis(1);
        }
    }
}

struct PollProgressDisplay<'a>
{
    display:         &'a mut LocalDisplay,
    state:           GatewayRuntimeState,
    board_name:      &'static str,
    next_refresh_ms: u64,
    spinner_step:    u8,
}

impl PollProgressDisplay<'_>
{
    fn render(
        &mut self,
        config: &GatewayConfig<MAX_TELEMETRY_PRODUCERS>,
        producer_id: TelemetryProducerId,
        now_ms: u64,
    )
    {
        if now_ms < self.next_refresh_ms {
            return;
        }

        let snapshot = telemetry_state::snapshot();
        let _ = self.display.show_polling_dashboard(
            config,
            &snapshot,
            self.state,
            now_ms,
            self.board_name,
            (producer_id, self.spinner_step),
        );
        self.spinner_step = self.spinner_step.wrapping_add(1);
        self.next_refresh_ms = now_ms.saturating_add(POLL_SPINNER_REFRESH_MS);
    }
}

impl MeshcoreClient for RadioMeshcoreClient<'_>
{
    type PollFuture<'a>
        = Ready<Result<ProducerTelemetry, Error>>
    where
        Self: 'a;

    fn poll_producer<'a>(
        &'a mut self,
        producer: &'a TelemetryProducerConfig,
    ) -> Self::PollFuture<'a>
    {
        ready(self.poll_producer_blocking(producer))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct PollRadioBaseline
{
    rx_total:              u64,
    rx_crc_error_total:    u64,
    rx_header_error_total: u64,
}

impl PollRadioBaseline
{
    fn capture() -> Self
    {
        let gateway = telemetry_state::snapshot().gateway;
        Self {
            rx_total:              gateway.radio_rx_total,
            rx_crc_error_total:    gateway.radio_rx_crc_error_total,
            rx_header_error_total: gateway.radio_rx_header_error_total,
        }
    }

    fn classify_timeout(
        self,
        observed_rx_frames: u32,
        observed_response_frames: u32,
        last_error: Option<PollFailureReason>,
    ) -> PollFailureReason
    {
        if let Some(reason) = last_error {
            return reason;
        }

        if observed_response_frames > 0 {
            return PollFailureReason::ResponseTimeout;
        }

        if observed_rx_frames > 0 {
            return PollFailureReason::MeshResponseNotSeen;
        }

        let after = Self::capture();
        if after
            .rx_crc_error_total
            .saturating_sub(self.rx_crc_error_total)
            > 0
        {
            return PollFailureReason::RadioCrcErrors;
        }

        if after
            .rx_header_error_total
            .saturating_sub(self.rx_header_error_total)
            > 0
        {
            return PollFailureReason::RadioHeaderErrors;
        }

        if after.rx_total.saturating_sub(self.rx_total) == 0 {
            PollFailureReason::NoRadioRxAfterTx
        } else {
            PollFailureReason::MeshResponseNotSeen
        }
    }
}

/// Drain queued radio frames into passive producer observations.
pub fn drain_observations(config: &GatewayConfig<MAX_TELEMETRY_PRODUCERS>, now_ms: u64) -> u32
{
    let mut updated = 0_u32;
    while let Some(frame) = radio::try_dequeue_rx_frame() {
        if is_response_frame(&frame) {
            continue;
        }
        if let Some((producer_id, telemetry)) = telemetry_for_any_producer(config, &frame, now_ms)
            && telemetry_state::record_producer_observation(producer_id, telemetry).is_ok()
        {
            updated = updated.saturating_add(1);
        }
    }
    updated
}

fn telemetry_for_any_producer(
    config: &GatewayConfig<MAX_TELEMETRY_PRODUCERS>,
    frame: &MeshRxFrame,
    now_ms: u64,
) -> Option<(TelemetryProducerId, ProducerTelemetry)>
{
    for producer in config.enabled_producers() {
        if producer_matches_frame(producer, frame) {
            let producer_id = producer.id();
            return Some((
                producer_id,
                telemetry_from_frame(producer_id, frame, now_ms),
            ));
        }
    }
    None
}

fn producer_matches_frame(producer: &TelemetryProducerConfig, frame: &MeshRxFrame) -> bool
{
    if frame.message.has_pubkey4
        && producer_public_key_prefix(producer.public_key.as_str())
            .is_some_and(|prefix| prefix == frame.message.pubkey4)
    {
        return true;
    }

    if producer.name.is_empty() {
        return false;
    }

    frame
        .message
        .sender_name_str()
        .is_some_and(|sender| sender == producer.name.as_str())
}

fn telemetry_from_frame(
    producer_id: TelemetryProducerId,
    frame: &MeshRxFrame,
    now_ms: u64,
) -> ProducerTelemetry
{
    let mut telemetry = ProducerTelemetry::new(producer_id, now_ms);
    telemetry.rssi = Some(frame.message.rssi_dbm);
    telemetry.snr = Some(frame.message.snr_tenth_db as f32 / 10.0);

    if let Ok((_kind, body)) = meshcore_payload_body(frame.raw_payload()) {
        decode_lpp_payload(&mut telemetry, body);
        if body.len() > 4 {
            decode_lpp_payload(&mut telemetry, &body[4..]);
        }
    }

    telemetry
}

fn producer_public_key_prefix(public_key: &str) -> Option<u16>
{
    let mut value = 0_u16;
    let mut digits = 0_u8;

    for byte in public_key.bytes() {
        let Some(nibble) = hex_nibble(byte) else {
            continue;
        };
        value = (value << 4) | u16::from(nibble);
        digits = digits.saturating_add(1);
        if digits == 4 {
            return Some(value);
        }
    }

    None
}

fn hex_nibble(byte: u8) -> Option<u8>
{
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

fn gateway_identity(
    config: &GatewayConfig<MAX_TELEMETRY_PRODUCERS>,
) -> Result<MeshcoreIdentity, PollFailureReason>
{
    let public_key = config
        .meshcore
        .public_key
        .as_ref()
        .ok_or(PollFailureReason::InvalidConfig)?;
    let private_key = config
        .meshcore
        .private_key
        .as_ref()
        .ok_or(PollFailureReason::InvalidConfig)?;
    MeshcoreIdentity::from_hex(public_key.as_str(), private_key.as_str())
        .map_err(packet_error_to_reason)
}

fn producer_contact(
    producer: &TelemetryProducerConfig,
) -> Result<MeshcoreContact, PollFailureReason>
{
    MeshcoreContact::from_hex(producer.public_key.as_str(), MeshcoreRequestRoute::Direct)
        .map_err(packet_error_to_reason)
}

fn enqueue_frame(
    producer_id: TelemetryProducerId,
    now_ms: u64,
    frame: MeshTxFrame,
) -> Result<(), Error>
{
    match radio::enqueue_tx_frame(frame) {
        Ok(()) => Ok(()),
        Err(_) => Err(record_poll_failure(
            producer_id,
            now_ms,
            PollFailureReason::TxQueueFull,
        )),
    }
}

fn is_response_frame(frame: &MeshRxFrame) -> bool
{
    matches!(
        meshcore_payload_body(frame.raw_payload()),
        Ok((MESH_KIND_RESPONSE, _)) | Ok((MESH_KIND_PATH, _))
    )
}

/// Parse a MeshCore repeater `RepeaterStats` blob (the bytes after the 4-byte
/// response tag). Field offsets match the firmware's packed little-endian
/// struct. Returns `None` if the blob is too short.
fn parse_repeater_stats(blob: &[u8]) -> Option<RepeaterStatus>
{
    if blob.len() < REPEATER_STATS_MIN_LEN {
        return None;
    }
    let i16le = |o: usize| i16::from_le_bytes([blob[o], blob[o + 1]]);
    let u32le = |o: usize| u32::from_le_bytes([blob[o], blob[o + 1], blob[o + 2], blob[o + 3]]);
    Some(RepeaterStatus {
        noise_floor_dbm: i16le(4),
        packets_recv:    u32le(8),
        packets_sent:    u32le(12),
        tx_airtime_secs: u32le(16),
        uptime_secs:     u32le(20),
        rx_airtime_secs: u32le(48),
        recv_errors:     u32le(52),
    })
}

fn packet_error_is_background(error: MeshcoreClientPacketError) -> bool
{
    matches!(
        error,
        MeshcoreClientPacketError::NotForGateway | MeshcoreClientPacketError::WrongSource
    )
}

fn packet_error_can_be_ignored(error: MeshcoreClientPacketError) -> bool
{
    matches!(error, MeshcoreClientPacketError::ResponseTagMismatch)
}

fn log_response_reject(
    phase: &'static str,
    producer_id: TelemetryProducerId,
    frame: &MeshRxFrame,
    error: MeshcoreClientPacketError,
)
{
    esp_println::println!(
        "Meshcore: rejected {} response producer={} src={:02X} dst={:02X} kind=0x{:02X} \
         hash={:08X} error={:?}",
        phase,
        producer_id,
        frame.message.src,
        frame.message.dst,
        frame.message.kind,
        frame.raw_hash,
        error,
    );
}

fn packet_error_to_reason(error: MeshcoreClientPacketError) -> PollFailureReason
{
    match error {
        MeshcoreClientPacketError::InvalidKey => PollFailureReason::InvalidConfig,
        MeshcoreClientPacketError::AuthFailed => PollFailureReason::AuthFailed,
        MeshcoreClientPacketError::UnsupportedRoute => PollFailureReason::Unsupported,
        MeshcoreClientPacketError::NotForGateway => PollFailureReason::ResponseNotForGateway,
        MeshcoreClientPacketError::WrongSource => PollFailureReason::ResponseWrongSource,
        MeshcoreClientPacketError::Mac => PollFailureReason::ResponseMacFailed,
        MeshcoreClientPacketError::ResponseTagMismatch => PollFailureReason::ResponseTagMismatch,
        MeshcoreClientPacketError::PayloadTooLarge | MeshcoreClientPacketError::Decode => {
            PollFailureReason::DecodeFailed
        },
    }
}

fn record_poll_failure(
    producer_id: TelemetryProducerId,
    now_ms: u64,
    reason: PollFailureReason,
) -> Error
{
    let _ = telemetry_state::record_producer_poll_error(producer_id, reason);
    record_poll_diagnostic(
        now_ms,
        DiagnosticLevel::Warn,
        reason.as_str(),
        "producer poll failed",
    );
    reason_to_error(reason)
}

fn reason_to_error(reason: PollFailureReason) -> Error
{
    match reason {
        PollFailureReason::InvalidConfig => Error::InvalidConfig,
        PollFailureReason::TxQueueFull => Error::Radio,
        PollFailureReason::NoRadioRxAfterTx
        | PollFailureReason::RadioCrcErrors
        | PollFailureReason::RadioHeaderErrors => Error::Radio,
        PollFailureReason::MeshResponseNotSeen
        | PollFailureReason::ResponseTimeout
        | PollFailureReason::ResponseNotForGateway
        | PollFailureReason::ResponseWrongSource
        | PollFailureReason::ResponseMacFailed
        | PollFailureReason::ResponseTagMismatch => Error::Time,
        PollFailureReason::Unsupported => Error::Unsupported,
        PollFailureReason::AuthFailed
        | PollFailureReason::DecodeFailed
        | PollFailureReason::Failed => Error::Meshcore,
    }
}

fn record_poll_diagnostic(now_ms: u64, level: DiagnosticLevel, code: &str, message: &str)
{
    let _ = diagnostics::record(now_ms, level, DiagnosticSubsystem::Meshcore, code, message);
}

fn poll_wait_now_ms(start_ms: u64, start: Instant) -> u64
{
    start_ms.saturating_add(start.elapsed().as_millis())
}

fn advert_timestamp_secs(now_ms: u64, unix_seconds: Option<u64>) -> u32
{
    if let Some(unix_seconds) = unix_seconds
        && unix_seconds > 0
        && unix_seconds <= u64::from(u32::MAX)
    {
        return (unix_seconds as u32).max(MIN_MESHCORE_TIMESTAMP.load(Ordering::Relaxed));
    }

    ((now_ms / 1_000).clamp(1, u64::from(u32::MAX)) as u32)
        .max(MIN_MESHCORE_TIMESTAMP.load(Ordering::Relaxed))
}

fn next_request_tag(now_ms: u64) -> u32
{
    let next = NEXT_REQUEST_TAG.fetch_add(1, Ordering::Relaxed);
    if next == u32::MAX {
        NEXT_REQUEST_TAG.store(MESHCORE_REQUEST_TAG_BASE, Ordering::Relaxed);
    }
    let _ = now_ms;
    next
}

fn request_nonce(tag: u32, producer_id: TelemetryProducerId) -> u32
{
    const NONCE_MIX_LEFT_ROTATION: u32 = 13;
    const NONCE_MIX_MULTIPLIER: u32 = 0x9e37_79b1;
    const NONCE_MIX_RIGHT_ROTATION: u32 = 11;

    let mut value = tag ^ producer_id.as_u64() as u32;
    value ^= value.rotate_left(NONCE_MIX_LEFT_ROTATION);
    value = value.wrapping_mul(NONCE_MIX_MULTIPLIER);
    value ^ value.rotate_right(NONCE_MIX_RIGHT_ROTATION)
}
