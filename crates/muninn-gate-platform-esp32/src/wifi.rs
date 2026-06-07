//! ESP32 WiFi station and blocking HTTP service.
//!
//! This module is ESP32-specific because it owns `esp-wifi` controller setup,
//! the `smoltcp` interface, and the blocking HTTP socket loop used by the
//! current firmware.
//!
//! Runtime flow:
//!
//! - Read `GatewayConfig.http`; when it is absent the firmware uses serial-only output and this
//!   module is not started.
//! - Start the ESP WiFi controller in station mode using the configured SSID and password.
//! - Poll the controller until it associates with the AP, updating the OLED connection page while
//!   waiting.
//! - Create a `smoltcp` Ethernet interface over the ESP station device and poll DHCP until an IPv4
//!   lease is available.
//! - Bind a fixed pool of TCP sockets to `http.port` and serve `/metrics`, `/logs`, and `/poll`.
//!   `/metrics` is streamed with chunked transfer; smaller responses use bounded buffers.
//! - Keep polling WiFi, DHCP, HTTP, the user button, diagnostics, the cooperative LoRa radio owner,
//!   and the telemetry scheduler in one blocking loop. This avoids running Espressif's WiFi blob
//!   alongside a second-core application task.
//! - If WiFi disconnects after startup, clear the network address, reconnect the controller, wait
//!   for DHCP again, and then restart the listener.
//!
//! The code deliberately does not abstract over WiFi stacks. A future nRF52 or
//! USB-only platform should not depend on this module.

use alloc::string::String as AllocString;
use core::cmp::min;
use core::fmt::{self, Write as _};
use core::mem::MaybeUninit;
use core::sync::atomic::{AtomicU8, AtomicU32, Ordering};

use esp_hal::delay::Delay;
use esp_wifi::wifi::event::{EventExt as _, StaDisconnected};
use esp_wifi::wifi::{AuthMethod, ClientConfiguration, Configuration, WifiDevice};
use muninn_gate_core::config::MAX_TELEMETRY_PRODUCERS;
use muninn_gate_core::{
    Clock,
    DiagnosticLevel,
    DiagnosticSubsystem,
    GateFirmwareVariant,
    GatewayConfig,
    GatewayRuntimeError,
    GatewayRuntimeState,
    HttpConfig,
    HttpEndpoint,
    HttpSocketStateSnapshot,
    ServingInterfaces,
    TxPowerMapping,
};
use smoltcp::iface::{
    Config as InterfaceConfig,
    Interface,
    SocketHandle,
    SocketSet,
    SocketStorage,
};
use smoltcp::socket::{dhcpv4, tcp};
use smoltcp::time::{Duration as SmoltcpDuration, Instant as SmoltcpInstant};
use smoltcp::wire::{DhcpOption, EthernetAddress, HardwareAddress, IpCidr};
use static_cell::StaticCell;

use crate::display::LocalDisplay;
use crate::input::UserButton;
use crate::platform::Esp32Platform;
use crate::radio::CooperativeRadioOwner;
use crate::scheduler::GatewayScheduler;
use crate::{diagnostics, http_metrics, report_state, serial};

/// HTTP request receive buffer size.
pub const HTTP_REQUEST_BUFFER_BYTES: usize = 1024;
/// Number of parallel HTTP sockets. smoltcp has no listen backlog and TIME_WAIT is per socket.
pub const HTTP_SOCKET_COUNT: usize = 16;
/// TCP receive buffer size for each HTTP socket.
pub const HTTP_TCP_RX_BUFFER_BYTES: usize = 512;
/// TCP transmit buffer size for each HTTP socket.
pub const HTTP_TCP_TX_BUFFER_BYTES: usize = 1024;
/// Chunk staging buffer used while streaming Prometheus metrics.
pub const HTTP_CHUNK_BUFFER_BYTES: usize = 2048;
/// Milliseconds between WiFi association checks.
pub const WIFI_CONNECT_POLL_MS: u32 = 250;
/// Maximum interval between WiFi connect requests once the driver is flapping.
pub const WIFI_CONNECT_BACKOFF_MAX_MS: u64 = 20_000;
/// Time without association before forcing a WiFi stop/start cycle.
pub const WIFI_CONNECT_DEEP_RECOVERY_MS: u64 = 45_000;
/// Minimum interval between WiFi connect progress logs.
pub const WIFI_CONNECT_PROGRESS_LOG_MS: u64 = 30_000;
/// Milliseconds between network polls while waiting for DHCP.
pub const DHCP_POLL_MS: u32 = 25;
/// Number of DHCP checks before forcing WiFi association again.
pub const DHCP_ATTEMPTS: usize = 2_400;
/// Number of DHCP checks between progress logs.
pub const DHCP_PROGRESS_POLLS: usize = 320;
/// Number of empty network polls allowed while sending one response.
pub const HTTP_SEND_IDLE_POLLS: usize = 2_000;
/// Number of 1 ms polls allowed while draining one HTTP close.
pub const HTTP_CLOSE_POLLS: usize = 500;
/// Maximum age for a non-listening HTTP socket before it is force-recycled.
pub const HTTP_SOCKET_STALL_MS: u64 = 15_000;
/// Grace period for normal TCP close handshakes before force-recycling a socket.
pub const HTTP_SOCKET_CLOSE_GRACE_MS: u64 = 15_000;
/// Maximum smoltcp poll gap before a serial warning is emitted.
pub const SMOLTCP_POLL_GAP_WARN_MS: u32 = 25;
/// smoltcp poll gap that is already bad enough to explain ping/curl loss.
pub const SMOLTCP_POLL_GAP_BAD_MS: u32 = 100;
/// Minimum spacing between smoltcp poll-gap serial warnings.
pub const SMOLTCP_POLL_GAP_LOG_COOLDOWN_MS: u32 = 30_000;
/// Milliseconds between HTTP socket-pool metric updates.
pub const HTTP_SOCKET_STATE_REPORT_MS: u64 = 1_000;
/// HTTP idle window after which a previously-used endpoint is considered wedged.
pub const HTTP_IDLE_RECOVERY_MS: u64 = 10 * 60 * 1_000;
/// Socket-pool full/stalled window after which the network stack is recycled.
pub const HTTP_SOCKET_POOL_STALLED_RECOVERY_MS: u64 = 30_000;
/// Minimum spacing between firmware-forced network recoveries.
pub const NETWORK_RECOVERY_COOLDOWN_MS: u64 = 60_000;
/// HTTP send-error window before treating the network path as wedged.
pub const HTTP_SEND_ERROR_RECOVERY_WINDOW_MS: u64 = 120_000;
/// Repeated HTTP send errors required before forcing WiFi recovery.
pub const HTTP_SEND_ERROR_RECOVERY_THRESHOLD: u8 = 3;
/// Main-loop gap that deserves an operator diagnostic.
pub const MAIN_LOOP_GAP_DIAGNOSTIC_MS: u32 = 5_000;
/// Scheduler tick duration that deserves an operator diagnostic.
pub const SCHEDULER_TICK_DIAGNOSTIC_MS: u32 = 5_000;
/// Minimum spacing between repeated long-loop diagnostics.
pub const LONG_LOOP_DIAGNOSTIC_COOLDOWN_MS: u64 = 60_000;

type HttpTcpRxBuffers = [[u8; HTTP_TCP_RX_BUFFER_BYTES]; HTTP_SOCKET_COUNT];
type HttpTcpTxBuffers = [[u8; HTTP_TCP_TX_BUFFER_BYTES]; HTTP_SOCKET_COUNT];
type HttpRequestBuffers = [[u8; HTTP_REQUEST_BUFFER_BYTES]; HTTP_SOCKET_COUNT];
type HttpSocketActivity = [Option<u64>; HTTP_SOCKET_COUNT];

static HTTP_TCP_RX_BUFFERS: StaticCell<HttpTcpRxBuffers> = StaticCell::new();
static HTTP_TCP_TX_BUFFERS: StaticCell<HttpTcpTxBuffers> = StaticCell::new();
static HTTP_REQUEST_BUFFERS: StaticCell<HttpRequestBuffers> = StaticCell::new();
static DHCP_HOSTNAME: StaticCell<[u8; crate::wifi_common::DHCP_HOSTNAME_MAX_LEN]> =
    StaticCell::new();
static DHCP_OPTIONS: StaticCell<[DhcpOption<'static>; 1]> = StaticCell::new();
static WIFI_DISCONNECT_EVENTS: AtomicU32 = AtomicU32::new(0);
static WIFI_LAST_DISCONNECT_REASON: AtomicU8 = AtomicU8::new(0);
static SMOLTCP_LAST_POLL_MS: AtomicU32 = AtomicU32::new(0);
static SMOLTCP_LAST_GAP_LOG_MS: AtomicU32 = AtomicU32::new(0);

struct NetworkWatchdog
{
    last_loop_ms:                 u64,
    last_http_activity_ms:        Option<u64>,
    saw_http_client:              bool,
    last_socket_state_ms:         u64,
    socket_pool_stalled_since_ms: Option<u64>,
    first_http_send_error_ms:     Option<u64>,
    http_send_error_count:        u8,
    last_recovery_ms:             u64,
    forced_recovery_reason:       Option<&'static str>,
    last_loop_diagnostic_ms:      u64,
    last_scheduler_diagnostic_ms: u64,
}

struct WifiConnectRecovery
{
    first_unconnected_ms: Option<u64>,
    next_connect_ms:      u64,
    last_progress_log_ms: u64,
    last_disconnect_seq:  u32,
    connect_in_flight:    bool,
    connect_attempts:     u32,
    deep_recovery_total:  u32,
}

impl WifiConnectRecovery
{
    const fn new(now_ms: u64) -> Self
    {
        Self {
            first_unconnected_ms: Some(now_ms),
            next_connect_ms:      now_ms,
            last_progress_log_ms: now_ms,
            last_disconnect_seq:  0,
            connect_in_flight:    false,
            connect_attempts:     0,
            deep_recovery_total:  0,
        }
    }

    fn reset_unconnected(&mut self, now_ms: u64, disconnect_seq: u32)
    {
        self.first_unconnected_ms = Some(now_ms);
        self.next_connect_ms = now_ms;
        self.last_progress_log_ms = now_ms;
        self.last_disconnect_seq = disconnect_seq;
        self.connect_in_flight = false;
        self.connect_attempts = 0;
    }

    fn mark_connected(&mut self, now_ms: u64)
    {
        self.first_unconnected_ms = None;
        self.next_connect_ms = now_ms;
        self.last_progress_log_ms = now_ms;
        self.connect_in_flight = false;
        self.connect_attempts = 0;
    }

    fn should_connect(&self, now_ms: u64, disconnect_seq: u32) -> bool
    {
        if now_ms < self.next_connect_ms {
            return false;
        }

        !self.connect_in_flight || disconnect_seq != self.last_disconnect_seq
    }

    fn mark_connect_request(&mut self, now_ms: u64, disconnect_seq: u32)
    {
        self.connect_attempts = self.connect_attempts.saturating_add(1);
        self.last_disconnect_seq = disconnect_seq;
        self.connect_in_flight = true;
        self.next_connect_ms = now_ms.saturating_add(self.next_backoff_ms());
    }

    fn should_deep_recover(&self, now_ms: u64) -> bool
    {
        self.first_unconnected_ms.is_some_and(|started_ms| {
            now_ms.saturating_sub(started_ms) >= WIFI_CONNECT_DEEP_RECOVERY_MS
        })
    }

    fn mark_deep_recovery(&mut self, now_ms: u64)
    {
        self.deep_recovery_total = self.deep_recovery_total.saturating_add(1);
        self.first_unconnected_ms = Some(now_ms);
        self.next_connect_ms = now_ms.saturating_add(1_000);
        self.last_progress_log_ms = now_ms;
        self.last_disconnect_seq = wifi_disconnect_events();
        self.connect_in_flight = false;
        self.connect_attempts = 0;
    }

    fn should_log_progress(&mut self, now_ms: u64) -> bool
    {
        if now_ms.saturating_sub(self.last_progress_log_ms) < WIFI_CONNECT_PROGRESS_LOG_MS {
            return false;
        }
        self.last_progress_log_ms = now_ms;
        true
    }

    const fn connect_attempts(&self) -> u32
    {
        self.connect_attempts
    }

    const fn deep_recovery_total(&self) -> u32
    {
        self.deep_recovery_total
    }

    fn next_backoff_ms(&self) -> u64
    {
        match self.connect_attempts {
            0 | 1 => 2_000,
            2 => 5_000,
            3 => 10_000,
            _ => WIFI_CONNECT_BACKOFF_MAX_MS,
        }
    }
}

impl NetworkWatchdog
{
    const fn new(now_ms: u64) -> Self
    {
        Self {
            last_loop_ms:                 now_ms,
            last_http_activity_ms:        None,
            saw_http_client:              false,
            last_socket_state_ms:         now_ms,
            socket_pool_stalled_since_ms: None,
            first_http_send_error_ms:     None,
            http_send_error_count:        0,
            last_recovery_ms:             0,
            forced_recovery_reason:       None,
            last_loop_diagnostic_ms:      0,
            last_scheduler_diagnostic_ms: 0,
        }
    }

    fn observe_loop(&mut self, now_ms: u64) -> u32
    {
        let gap_ms = now_ms
            .saturating_sub(self.last_loop_ms)
            .min(u64::from(u32::MAX)) as u32;
        self.last_loop_ms = now_ms;
        gap_ms
    }

    fn record_http_activity(&mut self, now_ms: u64)
    {
        self.saw_http_client = true;
        self.last_http_activity_ms = Some(now_ms);
    }

    fn record_http_success(&mut self, now_ms: u64)
    {
        self.record_http_activity(now_ms);
        self.first_http_send_error_ms = None;
        self.http_send_error_count = 0;
    }

    fn record_http_send_error(&mut self, now_ms: u64)
    {
        if match self.first_http_send_error_ms {
            Some(first_ms) => now_ms.saturating_sub(first_ms) > HTTP_SEND_ERROR_RECOVERY_WINDOW_MS,
            None => true,
        } {
            self.first_http_send_error_ms = Some(now_ms);
            self.http_send_error_count = 0;
        }

        self.http_send_error_count = self.http_send_error_count.saturating_add(1);
        if self.http_send_error_count >= HTTP_SEND_ERROR_RECOVERY_THRESHOLD {
            self.request_recovery("http_send_error");
        }
    }

    fn mark_endpoint_ready(&mut self, now_ms: u64)
    {
        if self.saw_http_client {
            self.last_http_activity_ms = Some(now_ms);
        }
        self.socket_pool_stalled_since_ms = None;
    }

    fn should_report_socket_state(&self, now_ms: u64) -> bool
    {
        now_ms.saturating_sub(self.last_socket_state_ms) >= HTTP_SOCKET_STATE_REPORT_MS
    }

    fn record_socket_state(&mut self, now_ms: u64, active: u8, listening: u8)
    {
        self.last_socket_state_ms = now_ms;
        if listening == 0 && usize::from(active) >= HTTP_SOCKET_COUNT {
            if self.socket_pool_stalled_since_ms.is_none() {
                self.socket_pool_stalled_since_ms = Some(now_ms);
            }
        } else {
            self.socket_pool_stalled_since_ms = None;
        }
    }

    fn recovery_reason(&self, now_ms: u64) -> Option<&'static str>
    {
        if self.last_recovery_ms != 0
            && now_ms.saturating_sub(self.last_recovery_ms) < NETWORK_RECOVERY_COOLDOWN_MS
        {
            return None;
        }

        if let Some(reason) = self.forced_recovery_reason {
            return Some(reason);
        }

        if let Some(stalled_since_ms) = self.socket_pool_stalled_since_ms
            && now_ms.saturating_sub(stalled_since_ms) >= HTTP_SOCKET_POOL_STALLED_RECOVERY_MS
        {
            return Some("socket_pool_stalled");
        }

        if self.saw_http_client
            && self
                .last_http_activity_ms
                .is_some_and(|last_ms| now_ms.saturating_sub(last_ms) >= HTTP_IDLE_RECOVERY_MS)
        {
            return Some("http_idle");
        }

        None
    }

    fn request_recovery(&mut self, reason: &'static str)
    {
        self.forced_recovery_reason = Some(reason);
    }

    fn mark_recovery(&mut self, now_ms: u64)
    {
        self.last_recovery_ms = now_ms;
        if self.saw_http_client {
            self.last_http_activity_ms = Some(now_ms);
        }
        self.socket_pool_stalled_since_ms = None;
        self.forced_recovery_reason = None;
        self.first_http_send_error_ms = None;
        self.http_send_error_count = 0;
    }

    fn should_log_loop_gap(&mut self, now_ms: u64, gap_ms: u32) -> bool
    {
        if gap_ms < MAIN_LOOP_GAP_DIAGNOSTIC_MS
            || now_ms.saturating_sub(self.last_loop_diagnostic_ms)
                < LONG_LOOP_DIAGNOSTIC_COOLDOWN_MS
        {
            return false;
        }
        self.last_loop_diagnostic_ms = now_ms;
        true
    }

    fn should_log_scheduler_tick(&mut self, now_ms: u64, duration_ms: u32) -> bool
    {
        if duration_ms < SCHEDULER_TICK_DIAGNOSTIC_MS
            || now_ms.saturating_sub(self.last_scheduler_diagnostic_ms)
                < LONG_LOOP_DIAGNOSTIC_COOLDOWN_MS
        {
            return false;
        }
        self.last_scheduler_diagnostic_ms = now_ms;
        true
    }
}

/// Error returned while starting or serving WiFi HTTP.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WifiStartError
{
    /// Configuration did not contain WiFi station credentials.
    Disabled,
    /// ESP HAL WiFi resources were already consumed.
    ResourcesUnavailable,
    /// The WiFi driver failed to initialize.
    DriverInit,
    /// The WiFi controller failed to configure or start.
    Controller,
    /// Heap allocation for the WiFi configuration failed.
    Allocation,
    /// The TCP listener could not bind to the configured port.
    HttpListen,
    /// HTTP response rendering failed.
    HttpRender,
    /// HTTP response transmission failed.
    HttpSend,
}

impl WifiStartError
{
    /// Return a compact log label for this error.
    pub const fn as_str(self) -> &'static str
    {
        match self {
            Self::Disabled => "wifi disabled",
            Self::ResourcesUnavailable => "wifi resources unavailable",
            Self::DriverInit => "wifi driver init failed",
            Self::Controller => "wifi controller failed",
            Self::Allocation => "wifi allocation failed",
            Self::HttpListen => "http listen failed",
            Self::HttpRender => "http render failed",
            Self::HttpSend => "http send failed",
        }
    }

    /// Return the subsystem most closely associated with this error.
    pub const fn subsystem(self) -> DiagnosticSubsystem
    {
        match self {
            Self::HttpListen | Self::HttpRender | Self::HttpSend => DiagnosticSubsystem::Http,
            Self::Disabled
            | Self::ResourcesUnavailable
            | Self::DriverInit
            | Self::Controller
            | Self::Allocation => DiagnosticSubsystem::Wifi,
        }
    }

    /// Return a compact display hint for a terminal startup failure.
    pub const fn display_hint(self) -> &'static str
    {
        match self {
            Self::Disabled => "HTTP CONFIG OFF",
            Self::ResourcesUnavailable => "WIFI UNAVAILABLE",
            Self::DriverInit => "DRIVER ERROR",
            Self::Controller => "CHECK WIFI CONFIG",
            Self::Allocation => "LOW MEMORY",
            Self::HttpListen => "HTTP LISTEN FAIL",
            Self::HttpRender => "HTTP RENDER FAIL",
            Self::HttpSend => "HTTP SEND FAIL",
        }
    }
}

/// Connect WiFi and serve HTTP endpoints forever.
pub fn serve_http_forever<V>(
    platform: &mut Esp32Platform,
    config: &GatewayConfig<MAX_TELEMETRY_PRODUCERS>,
    tx_power: TxPowerMapping,
    mut display: Option<&mut LocalDisplay>,
    scheduler: &mut GatewayScheduler,
    button: &mut UserButton,
    radio_owner: Option<&mut CooperativeRadioOwner>,
) -> !
where
    V: GateFirmwareVariant,
{
    if let Err(error) = serve_http::<V>(
        platform,
        config,
        tx_power,
        display.as_deref_mut(),
        scheduler,
        button,
        radio_owner,
    ) {
        esp_println::println!("Network: startup failed ({})", error.as_str());
        record_diagnostic(
            platform.now_ms(),
            DiagnosticLevel::Error,
            error.subsystem(),
            "startup_failed",
            error.as_str(),
        );
        report_state(GatewayRuntimeState::Error {
            reason: GatewayRuntimeError::Network,
        });
        if let Some(display) = display {
            let _ = display.show_wifi_error(error.display_hint());
        }
    }

    loop {
        core::hint::spin_loop();
    }
}

fn serve_http<V>(
    platform: &mut Esp32Platform,
    config: &GatewayConfig<MAX_TELEMETRY_PRODUCERS>,
    tx_power: TxPowerMapping,
    mut display: Option<&mut LocalDisplay>,
    scheduler: &mut GatewayScheduler,
    button: &mut UserButton,
    mut radio_owner: Option<&mut CooperativeRadioOwner>,
) -> Result<(), WifiStartError>
where
    V: GateFirmwareVariant,
{
    let ssid = config
        .http
        .as_ref()
        .map_or("", |http| http.wifi_ssid.as_str());
    if let Some(display) = display.as_deref_mut() {
        let _ = display.show_wifi_connecting(ssid, "Starting WiFi", 1);
    }

    crate::telemetry_state::record_network_started(platform.now_ms());
    install_wifi_event_logging();
    let mut station_config = station_config(config)?;

    let resources = platform
        .take_wifi_resources()
        .ok_or(WifiStartError::ResourcesUnavailable)?;

    let (wifi_init, wifi) = resources.initialize().map_err(|error| match error {
        crate::platform::Esp32WifiInitError::ResourcesUnavailable => {
            WifiStartError::ResourcesUnavailable
        },
        crate::platform::Esp32WifiInitError::DriverInit => WifiStartError::DriverInit,
    })?;

    let (mut controller, interfaces) = esp_wifi::wifi::new(wifi_init, wifi).map_err(|error| {
        esp_println::println!("WiFi: new failed: {:?}", error);
        WifiStartError::Controller
    })?;
    let delay = Delay::new();

    if let Err(error) = controller.set_configuration(&Configuration::Client(station_config.clone()))
    {
        esp_println::println!("WiFi: set_configuration failed: {:?}", error);
        return Err(WifiStartError::Controller);
    }

    if let Err(error) = controller.start() {
        esp_println::println!("WiFi: start failed: {:?}", error);
        return Err(WifiStartError::Controller);
    }

    wait_for_wifi_started(&controller, &delay)?;
    configure_wifi_reliability(&mut controller, platform)?;
    if let Some(pinned_config) =
        station_config_pinned_to_best_ap(&mut controller, platform, &station_config)
    {
        if let Err(error) =
            controller.set_configuration(&Configuration::Client(pinned_config.clone()))
        {
            esp_println::println!("WiFi: set pinned AP configuration failed: {:?}", error);
            record_diagnostic(
                platform.now_ms(),
                DiagnosticLevel::Warn,
                DiagnosticSubsystem::Wifi,
                "ap_pin_config_failed",
                "failed to apply scanned BSSID/channel pin",
            );
        } else {
            station_config = pinned_config;
        }
    }
    delay.delay_millis(500);

    wait_for_wifi_recovering(
        &mut controller,
        &delay,
        display.as_deref_mut(),
        ssid,
        platform,
        &station_config,
    )?;

    serial::write_line("WiFi: connected");
    record_diagnostic(
        platform.now_ms(),
        DiagnosticLevel::Info,
        DiagnosticSubsystem::Wifi,
        "connected",
        "WiFi station connected",
    );

    let mut station_device = interfaces.sta;
    let mut interface = network_interface(&mut station_device, platform);

    let tcp_rx_buffers =
        HTTP_TCP_RX_BUFFERS.init_with(|| [[0_u8; HTTP_TCP_RX_BUFFER_BYTES]; HTTP_SOCKET_COUNT]);
    let tcp_tx_buffers =
        HTTP_TCP_TX_BUFFERS.init_with(|| [[0_u8; HTTP_TCP_TX_BUFFER_BYTES]; HTTP_SOCKET_COUNT]);
    let mut request_buffers =
        HTTP_REQUEST_BUFFERS.init_with(|| [[0_u8; HTTP_REQUEST_BUFFER_BYTES]; HTTP_SOCKET_COUNT]);
    let mut socket_storage = [SocketStorage::EMPTY; HTTP_SOCKET_COUNT + 1];
    let mut sockets = SocketSet::new(&mut socket_storage[..]);
    let mut dhcp_socket = dhcpv4::Socket::new();
    crate::wifi_common::configure_dhcp_retry(&mut dhcp_socket);
    let dhcp_hostname = crate::wifi_common::configure_dhcp_hostname(
        &mut dhcp_socket,
        config.name.as_str(),
        &DHCP_HOSTNAME,
        &DHCP_OPTIONS,
    );
    esp_println::println!("WiFi: DHCP hostname=\"{}\"", dhcp_hostname);
    let dhcp_handle = sockets.add(dhcp_socket);
    let mut http_handles = [None; HTTP_SOCKET_COUNT];
    for (index, (rx_buffer, tx_buffer)) in tcp_rx_buffers
        .iter_mut()
        .zip(tcp_tx_buffers.iter_mut())
        .enumerate()
    {
        http_handles[index] = Some(sockets.add(new_http_socket(rx_buffer, tx_buffer)));
    }
    let mut request_lens = [0_usize; HTTP_SOCKET_COUNT];
    let mut socket_active_since_ms: HttpSocketActivity = [None; HTTP_SOCKET_COUNT];
    let mut endpoint = None;
    let mut dhcp_polls = 0_usize;
    let mut dhcp_started_ms = platform.now_ms();
    crate::telemetry_state::record_dhcp_started(dhcp_started_ms);
    let mut reconnect_polls = 0_usize;
    let mut wifi_connected = true;
    let mut wifi_recovery = WifiConnectRecovery::new(platform.now_ms());
    let mut serving_state = GatewayRuntimeState::Provisioned;
    let mut watchdog = NetworkWatchdog::new(platform.now_ms());
    let startup_disconnect_events = wifi_disconnect_events();
    if startup_disconnect_events > 0 {
        crate::telemetry_state::record_wifi_disconnects(
            startup_disconnect_events,
            Some(wifi_last_disconnect_reason()),
        );
    }
    let mut observed_disconnect_events = startup_disconnect_events;
    log_connected_ap_info(platform, "connected");
    crate::telemetry_state::record_wifi_connected_at(platform.now_ms());

    serial::write_line("Network: waiting for DHCP");

    loop {
        service_radio_owner_once(radio_owner.as_deref_mut(), platform);

        let loop_now_ms = platform.now_ms();
        let disconnect_events = wifi_disconnect_events();
        if disconnect_events != observed_disconnect_events {
            let delta = disconnect_events.saturating_sub(observed_disconnect_events);
            observed_disconnect_events = disconnect_events;
            crate::telemetry_state::record_wifi_disconnects(
                delta,
                Some(wifi_last_disconnect_reason()),
            );
        }
        let loop_gap_ms = watchdog.observe_loop(loop_now_ms);
        if loop_gap_ms >= 10 {
            crate::telemetry_state::record_main_loop_gap(loop_gap_ms);
        }
        if watchdog.should_log_loop_gap(loop_now_ms, loop_gap_ms) {
            esp_println::println!("Network: long main-loop gap {}ms", loop_gap_ms);
            record_diagnostic(
                loop_now_ms,
                DiagnosticLevel::Warn,
                DiagnosticSubsystem::Http,
                "main_loop_gap",
                "network loop was blocked long enough to affect HTTP",
            );
        }

        if !matches!(controller.is_connected(), Ok(true)) {
            if wifi_connected {
                serial::write_line("WiFi: disconnected; reconnecting");
                record_diagnostic(
                    platform.now_ms(),
                    DiagnosticLevel::Warn,
                    DiagnosticSubsystem::Wifi,
                    "disconnected",
                    "WiFi station disconnected; reconnecting",
                );
                endpoint = None;
                dhcp_polls = 0;
                reconnect_polls = 0;
                wifi_recovery.reset_unconnected(platform.now_ms(), wifi_disconnect_events());
                serving_state = GatewayRuntimeState::Provisioned;
                clear_network_config(
                    &mut interface,
                    &mut sockets,
                    dhcp_handle,
                    &http_handles,
                    &mut request_lens,
                    &mut socket_active_since_ms,
                );
                let _ = configure_wifi_reliability(&mut controller, platform);
                crate::telemetry_state::record_network_started(platform.now_ms());
            }

            wifi_connected = false;
            reconnect_polls = reconnect_polls.saturating_add(1);
            let now_ms = platform.now_ms();
            if wifi_recovery.should_deep_recover(now_ms) {
                deep_recover_wifi(
                    &mut controller,
                    &delay,
                    platform,
                    &station_config,
                    "reconnect",
                );
                wifi_recovery.mark_deep_recovery(now_ms);
            }
            let disconnect_seq = wifi_disconnect_events();
            if wifi_recovery.should_connect(now_ms, disconnect_seq) {
                request_wifi_connect(&mut controller, platform, "reconnect");
                wifi_recovery.mark_connect_request(now_ms, disconnect_seq);
            }
            if wifi_recovery.should_log_progress(now_ms) {
                esp_println::println!(
                    "WiFi: still reconnecting attempts={} deep_recoveries={}",
                    wifi_recovery.connect_attempts(),
                    wifi_recovery.deep_recovery_total()
                );
            }
            if let Some(display) = display.as_deref_mut() {
                let _ = display.show_wifi_connecting(ssid, "Reconnecting", reconnect_polls as u8);
            }
            delay.delay_millis(WIFI_CONNECT_POLL_MS);
            continue;
        }

        if !wifi_connected {
            wifi_connected = true;
            reconnect_polls = 0;
            wifi_recovery.mark_connected(platform.now_ms());
            serial::write_line("WiFi: reconnected");
            record_diagnostic(
                platform.now_ms(),
                DiagnosticLevel::Info,
                DiagnosticSubsystem::Wifi,
                "reconnected",
                "WiFi station reconnected",
            );
            crate::telemetry_state::record_wifi_connected(true);
            crate::telemetry_state::record_wifi_connected_at(platform.now_ms());
            log_connected_ap_info(platform, "reconnected");
            reset_dhcp_socket(&mut sockets, dhcp_handle);
            reset_http_sockets(
                &mut sockets,
                &http_handles,
                &mut request_lens,
                &mut socket_active_since_ms,
            );
            endpoint = None;
            dhcp_started_ms = platform.now_ms();
            crate::telemetry_state::record_dhcp_started(dhcp_started_ms);
            dhcp_polls = 0;
        }

        poll_network(&mut interface, &mut station_device, &mut sockets, platform);

        if let Some(next_endpoint) = poll_dhcp(
            &mut interface,
            &mut sockets,
            dhcp_handle,
            config.http.as_ref().map_or(80, |http| http.port),
            dhcp_started_ms,
            platform.now_ms(),
        ) {
            dhcp_polls = 0;
            if endpoint != next_endpoint {
                reset_http_sockets(
                    &mut sockets,
                    &http_handles,
                    &mut request_lens,
                    &mut socket_active_since_ms,
                );
                endpoint = next_endpoint;
                if let Some(endpoint) = endpoint {
                    let serving_started_ms = platform.now_ms();
                    watchdog.mark_endpoint_ready(serving_started_ms);
                    crate::telemetry_state::record_http_serving_started(serving_started_ms);
                    log_endpoint(endpoint);
                    record_diagnostic(
                        serving_started_ms,
                        DiagnosticLevel::Info,
                        DiagnosticSubsystem::Wifi,
                        "dhcp_acquired",
                        "DHCP IPv4 lease acquired",
                    );
                    record_diagnostic(
                        serving_started_ms,
                        DiagnosticLevel::Info,
                        DiagnosticSubsystem::Http,
                        "listening",
                        "HTTP telemetry endpoints are listening",
                    );
                    report_state(GatewayRuntimeState::Serving {
                        interfaces: ServingInterfaces::new(
                            Some(endpoint),
                            V::CAPABILITIES.usb_serial,
                        ),
                    });
                    serving_state = GatewayRuntimeState::Serving {
                        interfaces: ServingInterfaces::new(
                            Some(endpoint),
                            V::CAPABILITIES.usb_serial,
                        ),
                    };
                    if let Some(display) = display.as_deref_mut() {
                        let display_started_ms = platform.now_ms();
                        let snapshot = crate::telemetry_state::snapshot();
                        let _ = display.show_http_dashboard::<V>(
                            config,
                            &snapshot,
                            serving_state,
                            platform.now_ms(),
                        );
                        record_duration(
                            display_started_ms,
                            platform.now_ms(),
                            crate::telemetry_state::record_display_refresh,
                        );
                    }
                } else {
                    serial::write_line("Network: DHCP lease lost");
                    crate::telemetry_state::record_dhcp_deconfigured();
                    record_diagnostic(
                        platform.now_ms(),
                        DiagnosticLevel::Warn,
                        DiagnosticSubsystem::Wifi,
                        "dhcp_lost",
                        "DHCP IPv4 lease was lost",
                    );
                }
            }
        }

        if endpoint.is_none() {
            dhcp_polls = dhcp_polls.saturating_add(1);
            if let Some(display) = display.as_deref_mut() {
                let step = dhcp_progress_step(dhcp_polls);
                let _ = display.show_wifi_connecting(ssid, "Getting IP", step);
            }
            if dhcp_polls % DHCP_PROGRESS_POLLS == 0 {
                esp_println::println!(
                    "Network: still waiting for DHCP ({}s)",
                    poll_elapsed_seconds(dhcp_polls, DHCP_POLL_MS)
                );
            }
            if dhcp_polls > DHCP_ATTEMPTS {
                serial::write_line("Network: DHCP timeout; reconnecting WiFi");
                crate::telemetry_state::record_dhcp_timeout();
                record_diagnostic(
                    platform.now_ms(),
                    DiagnosticLevel::Warn,
                    DiagnosticSubsystem::Wifi,
                    "dhcp_timeout",
                    "DHCP timed out; reconnecting WiFi",
                );
                let _ = controller.disconnect();
                clear_network_config(
                    &mut interface,
                    &mut sockets,
                    dhcp_handle,
                    &http_handles,
                    &mut request_lens,
                    &mut socket_active_since_ms,
                );
                let _ = configure_wifi_reliability(&mut controller, platform);
                crate::telemetry_state::record_network_started(platform.now_ms());
                wifi_connected = false;
                wifi_recovery.reset_unconnected(platform.now_ms(), wifi_disconnect_events());
                endpoint = None;
                dhcp_started_ms = platform.now_ms();
                crate::telemetry_state::record_dhcp_started(dhcp_started_ms);
                dhcp_polls = 0;
                delay.delay_millis(1_000);
                continue;
            }
            delay.delay_millis(DHCP_POLL_MS);
            continue;
        }

        service_http_once(
            &mut interface,
            &mut station_device,
            &mut sockets,
            &http_handles,
            &mut request_buffers,
            &mut request_lens,
            &mut socket_active_since_ms,
            config,
            tx_power,
            platform,
            &delay,
            &mut watchdog,
        )?;

        if let Some(recovery_reason) = watchdog.recovery_reason(platform.now_ms()) {
            let now_ms = platform.now_ms();
            esp_println::println!("Network: {} watchdog; reconnecting WiFi", recovery_reason);
            record_diagnostic(
                now_ms,
                DiagnosticLevel::Warn,
                DiagnosticSubsystem::Wifi,
                "network_recovery",
                recovery_reason,
            );
            crate::telemetry_state::record_network_recovery();
            watchdog.mark_recovery(now_ms);
            let _ = controller.disconnect();
            clear_network_config(
                &mut interface,
                &mut sockets,
                dhcp_handle,
                &http_handles,
                &mut request_lens,
                &mut socket_active_since_ms,
            );
            let _ = configure_wifi_reliability(&mut controller, platform);
            crate::telemetry_state::record_network_started(now_ms);
            wifi_connected = false;
            wifi_recovery.reset_unconnected(now_ms, wifi_disconnect_events());
            endpoint = None;
            dhcp_started_ms = platform.now_ms();
            crate::telemetry_state::record_dhcp_started(dhcp_started_ms);
            dhcp_polls = 0;
            reconnect_polls = 0;
            serving_state = GatewayRuntimeState::Provisioned;
            delay.delay_millis(1_000);
            continue;
        }

        let (active_after_http, _) = http_socket_counts(
            &sockets,
            &http_handles,
            &socket_active_since_ms,
            platform.now_ms(),
        );
        if active_after_http == 0 {
            let scheduler_started_ms = platform.now_ms();
            let mut scheduler_error = None;
            {
                let mut network_maintenance = || {
                    service_radio_owner_once(radio_owner.as_deref_mut(), platform);
                    if scheduler_error.is_none() {
                        scheduler_error = service_http_once(
                            &mut interface,
                            &mut station_device,
                            &mut sockets,
                            &http_handles,
                            &mut request_buffers,
                            &mut request_lens,
                            &mut socket_active_since_ms,
                            config,
                            tx_power,
                            platform,
                            &delay,
                            &mut watchdog,
                        )
                        .err();
                    }
                };
                scheduler.tick_with_maintenance::<V, _>(
                    platform,
                    config,
                    tx_power,
                    serving_state,
                    display.as_deref_mut(),
                    Some(&mut *button),
                    &mut network_maintenance,
                );
            }
            if let Some(error) = scheduler_error {
                return Err(error);
            }
            let scheduler_duration_ms = platform
                .now_ms()
                .saturating_sub(scheduler_started_ms)
                .min(u64::from(u32::MAX)) as u32;
            if scheduler_duration_ms > 0 {
                crate::telemetry_state::record_scheduler_tick(scheduler_duration_ms);
            }
            if watchdog.should_log_scheduler_tick(platform.now_ms(), scheduler_duration_ms) {
                esp_println::println!(
                    "Network: scheduler blocked HTTP for {}ms",
                    scheduler_duration_ms
                );
                record_diagnostic(
                    platform.now_ms(),
                    DiagnosticLevel::Warn,
                    DiagnosticSubsystem::Poller,
                    "scheduler_blocked_http",
                    "scheduled polling blocked the HTTP network loop",
                );
            }
        }

        delay.delay_millis(1);
    }
}

fn service_http_once(
    interface: &mut Interface,
    station_device: &mut WifiDevice<'_>,
    sockets: &mut SocketSet<'_>,
    http_handles: &[Option<SocketHandle>],
    request_buffers: &mut HttpRequestBuffers,
    request_lens: &mut [usize; HTTP_SOCKET_COUNT],
    socket_active_since_ms: &mut HttpSocketActivity,
    config: &GatewayConfig<MAX_TELEMETRY_PRODUCERS>,
    tx_power: TxPowerMapping,
    platform: &Esp32Platform,
    delay: &Delay,
    watchdog: &mut NetworkWatchdog,
) -> Result<(), WifiStartError>
{
    let service_started_ms = platform.now_ms();
    poll_network(interface, station_device, sockets, platform);
    clear_partial_requests_if_idle(sockets, http_handles, request_lens);
    recycle_stale_http_sockets(
        sockets,
        http_handles,
        request_lens,
        socket_active_since_ms,
        platform.now_ms(),
    );

    ensure_listening(
        sockets,
        http_handles,
        config.http.as_ref().map_or(80, |http| http.port),
    )?;

    let socket_state_ms = platform.now_ms();
    if watchdog.should_report_socket_state(socket_state_ms) {
        let states = http_socket_states(
            sockets,
            http_handles,
            socket_active_since_ms,
            socket_state_ms,
        );
        watchdog.record_socket_state(socket_state_ms, states.active, states.listening);
        crate::telemetry_state::record_http_socket_states(states);
    }

    for index in 0..HTTP_SOCKET_COUNT {
        let Some(tcp_handle) = http_handles[index] else {
            continue;
        };
        if let Some(ready_len) = read_request(
            sockets,
            tcp_handle,
            &mut request_buffers[index],
            &mut request_lens[index],
        ) {
            socket_active_since_ms[index] = Some(platform.now_ms());
            let request_ms = platform.now_ms();
            watchdog.record_http_activity(request_ms);
            crate::telemetry_state::record_http_request(request_ms);
            let request = &request_buffers[index][..ready_len];
            let route = http_metrics::route_request(request);
            let mut http_io = HttpSocketIo::new(
                interface,
                station_device,
                sockets,
                tcp_handle,
                platform,
                delay,
            );
            match handle_http_request(&mut http_io, request, config.http.as_ref(), tx_power) {
                Ok(()) => {
                    let success_ms = platform.now_ms();
                    watchdog.record_http_success(success_ms);
                    crate::telemetry_state::record_http_success(success_ms);
                    let socket = http_io.sockets.get::<tcp::Socket>(tcp_handle);
                    socket_active_since_ms[index] = if socket.is_open() || socket.is_active() {
                        Some(success_ms)
                    } else {
                        None
                    };
                },
                Err(WifiStartError::HttpSend) => {
                    crate::telemetry_state::record_http_send_error();
                    watchdog.record_http_send_error(platform.now_ms());
                    record_diagnostic(
                        platform.now_ms(),
                        DiagnosticLevel::Debug,
                        DiagnosticSubsystem::Http,
                        "response_aborted",
                        route.as_str(),
                    );
                    http_io.abort();
                    socket_active_since_ms[index] = None;
                },
                Err(error) => {
                    esp_println::println!("HTTP: request failed ({})", error.as_str());
                    record_diagnostic(
                        platform.now_ms(),
                        DiagnosticLevel::Warn,
                        DiagnosticSubsystem::Http,
                        "request_failed",
                        error.as_str(),
                    );
                    http_io.abort();
                    socket_active_since_ms[index] = None;
                },
            }
        }
    }

    record_duration(
        service_started_ms,
        platform.now_ms(),
        crate::telemetry_state::record_http_service,
    );
    Ok(())
}

fn service_radio_owner_once(owner: Option<&mut CooperativeRadioOwner>, platform: &Esp32Platform)
{
    let Some(owner) = owner else {
        return;
    };
    let started_ms = platform.now_ms();
    owner.service();
    record_duration(
        started_ms,
        platform.now_ms(),
        crate::telemetry_state::record_lora_service,
    );
}

fn record_duration(started_ms: u64, finished_ms: u64, recorder: fn(u32))
{
    let duration_ms = finished_ms
        .saturating_sub(started_ms)
        .min(u64::from(u32::MAX)) as u32;
    recorder(duration_ms);
}

fn handle_http_request(
    io: &mut HttpSocketIo<'_, '_, '_>,
    request: &[u8],
    http: Option<&HttpConfig>,
    tx_power: TxPowerMapping,
) -> Result<(), WifiStartError>
{
    let mut route = http_metrics::route_request(request);
    if !request_is_authorized(request, http) {
        route = http_metrics::HttpRoute::Unauthorized;
    }

    if route == http_metrics::HttpRoute::PollNow {
        serial::write_line("Poll: requested over HTTP");
        record_diagnostic(
            io.platform.now_ms(),
            DiagnosticLevel::Info,
            DiagnosticSubsystem::Http,
            "poll_requested",
            "operator requested immediate poll over HTTP",
        );
    }

    if route == http_metrics::HttpRoute::Metrics {
        return stream_metrics_response(io, tx_power);
    }

    let response = http_metrics::build_response(route, io.platform.now_ms(), tx_power)
        .map_err(|_| WifiStartError::HttpRender)?;
    let header =
        http_metrics::render_http_header(&response).map_err(|_| WifiStartError::HttpRender)?;

    io.send_all(header.as_bytes())?;
    io.send_all(response.body.as_bytes())?;
    io.finish_response()
}

fn stream_metrics_response(
    io: &mut HttpSocketIo<'_, '_, '_>,
    tx_power: TxPowerMapping,
) -> Result<(), WifiStartError>
{
    crate::telemetry_state::refresh_gateway_metrics(io.platform.now_ms(), tx_power);
    let snapshot = crate::telemetry_state::snapshot();
    let gateway = &snapshot.gateway;

    io.send_all(
        b"HTTP/1.1 200 OK\r\nContent-Type: text/plain; version=0.0.4; charset=utf-8\r\n\
          Transfer-Encoding: chunked\r\nConnection: close\r\n\r\n",
    )?;

    let mut writer = ChunkedHttpSocketWriter::new(io);

    macro_rules! metric {
        ($($arg:tt)*) => {
            if writeln!(writer, $($arg)*).is_err() {
                return Err(writer.last_error.unwrap_or(WifiStartError::HttpRender));
            }
        };
    }

    metric!("muninn_gate_up 1");
    metric!("muninn_gate_uptime_ms {}", gateway.uptime_ms);
    if let Some(battery_voltage_mv) = gateway.battery_voltage_mv {
        metric!("muninn_gate_battery_voltage_mv {}", battery_voltage_mv);
    }
    metric!(
        "muninn_gate_poll_success_total {}",
        gateway.poll_success_total
    );
    metric!(
        "muninn_gate_poll_failure_total {}",
        gateway.poll_failure_total
    );
    metric!("muninn_gate_radio_rx_total {}", gateway.radio_rx_total);
    metric!("muninn_gate_radio_tx_total {}", gateway.radio_tx_total);
    metric!(
        "muninn_gate_radio_rx_crc_error_total {}",
        gateway.radio_rx_crc_error_total
    );
    if let Some(rssi) = gateway.radio_last_rssi_dbm {
        metric!("muninn_gate_radio_last_rssi_dbm {}", rssi);
    }
    if let Some(snr) = gateway.radio_last_snr_tenth_db {
        write_tenths_metric(&mut writer, "muninn_gate_radio_last_snr_db", snr)?;
    }
    if let Some(noise_floor) = gateway.radio_noise_floor_dbm {
        metric!("muninn_gate_radio_noise_floor_dbm {}", noise_floor);
    }
    if let Some(airtime_ms) = gateway.radio_last_tx_airtime_ms {
        metric!("muninn_gate_radio_last_tx_airtime_ms {}", airtime_ms);
    }
    if let Some(tx_output_milliwatts) = gateway.tx_output_milliwatts {
        metric!("muninn_gate_tx_output_milliwatts {}", tx_output_milliwatts);
    }
    metric!(
        "muninn_gate_wifi_connected {}",
        u8::from(gateway.wifi_connected)
    );
    metric!(
        "muninn_gate_wifi_disconnect_total {}",
        gateway.wifi_disconnect_total
    );
    if let Some(rssi) = gateway.wifi_rssi_dbm {
        metric!("muninn_gate_wifi_rssi_dbm {}", rssi);
    }
    if let Some(channel) = gateway.wifi_channel {
        metric!("muninn_gate_wifi_channel {}", channel);
    }
    if let Some(connect_started_ms) = gateway.wifi_connect_started_ms {
        metric!(
            "muninn_gate_wifi_connect_started_age_ms {}",
            gateway.uptime_ms.saturating_sub(connect_started_ms)
        );
    }
    if let Some(connected_ms) = gateway.wifi_connected_ms {
        metric!(
            "muninn_gate_wifi_connected_age_ms {}",
            gateway.uptime_ms.saturating_sub(connected_ms)
        );
    }
    metric!(
        "muninn_gate_wifi_connect_request_total {}",
        gateway.wifi_connect_request_total
    );
    metric!(
        "muninn_gate_wifi_connect_request_error_total {}",
        gateway.wifi_connect_request_error_total
    );
    metric!(
        "muninn_gate_dhcp_configured {}",
        u8::from(gateway.dhcp_configured)
    );
    if let Some(dhcp_started_ms) = gateway.dhcp_started_ms {
        metric!(
            "muninn_gate_dhcp_started_age_ms {}",
            gateway.uptime_ms.saturating_sub(dhcp_started_ms)
        );
    }
    if let Some(dhcp_configured_ms) = gateway.dhcp_configured_ms {
        metric!(
            "muninn_gate_dhcp_configured_age_ms {}",
            gateway.uptime_ms.saturating_sub(dhcp_configured_ms)
        );
    }
    if let Some(acquire_ms) = gateway.dhcp_last_acquire_ms {
        metric!("muninn_gate_dhcp_last_acquire_ms {}", acquire_ms);
    }
    if let Some(startup_ms) = gateway.network_startup_to_serving_ms {
        metric!("muninn_gate_network_startup_to_serving_ms {}", startup_ms);
    }
    metric!(
        "muninn_gate_http_requests_total {}",
        gateway.http_requests_total
    );
    metric!(
        "muninn_gate_http_success_total {}",
        gateway.http_success_total
    );
    metric!(
        "muninn_gate_http_send_error_total {}",
        gateway.http_send_error_total
    );
    if let Some(last_http_success_ms) = gateway.last_http_success_ms {
        metric!(
            "muninn_gate_http_last_success_age_ms {}",
            gateway.uptime_ms.saturating_sub(last_http_success_ms)
        );
    }
    metric!(
        "muninn_gate_http_listening_sockets {}",
        gateway.http_listening_sockets
    );
    write_http_socket_state_metrics(&mut writer, gateway)?;
    metric!(
        "muninn_gate_smoltcp_poll_total {}",
        gateway.smoltcp_poll_total
    );
    if let Some(last_poll_ms) = gateway.last_smoltcp_poll_ms {
        metric!(
            "muninn_gate_smoltcp_ms_since_last_poll {}",
            gateway.uptime_ms.saturating_sub(last_poll_ms)
        );
    }
    metric!(
        "muninn_gate_smoltcp_poll_gap_max_ms {}",
        gateway.smoltcp_poll_gap_max_ms
    );
    metric!(
        "muninn_gate_smoltcp_poll_delay_miss_total {}",
        gateway.smoltcp_poll_delay_miss_total
    );
    metric!(
        "muninn_gate_smoltcp_poll_bad_gap_total {}",
        gateway.smoltcp_poll_bad_gap_total
    );
    metric!(
        "muninn_gate_main_loop_gap_last_ms {}",
        gateway.main_loop_gap_last_ms
    );
    metric!(
        "muninn_gate_main_loop_gap_max_ms {}",
        gateway.main_loop_gap_max_ms
    );
    metric!(
        "muninn_gate_scheduler_tick_last_ms {}",
        gateway.scheduler_tick_last_ms
    );
    metric!(
        "muninn_gate_scheduler_tick_max_ms {}",
        gateway.scheduler_tick_max_ms
    );
    metric!(
        "muninn_gate_lora_service_last_ms {}",
        gateway.lora_service_last_ms
    );
    metric!(
        "muninn_gate_lora_service_max_ms {}",
        gateway.lora_service_max_ms
    );
    metric!(
        "muninn_gate_http_service_last_ms {}",
        gateway.http_service_last_ms
    );
    metric!(
        "muninn_gate_http_service_max_ms {}",
        gateway.http_service_max_ms
    );

    for record in snapshot.records.iter() {
        write_producer_metrics(&mut writer, gateway.uptime_ms, record)?;
    }

    writer.finish()?;
    io.finish_response()
}

fn write_http_socket_state_metrics<W>(
    writer: &mut W,
    gateway: &muninn_gate_core::GatewayMetrics,
) -> Result<(), WifiStartError>
where
    W: fmt::Write + ?Sized,
{
    write_labeled_metric(
        writer,
        "muninn_gate_http_sockets",
        "closed",
        gateway.http_closed_sockets,
    )?;
    write_labeled_metric(
        writer,
        "muninn_gate_http_sockets",
        "listen",
        gateway.http_listening_sockets,
    )?;
    write_labeled_metric(
        writer,
        "muninn_gate_http_sockets",
        "syn_sent",
        gateway.http_syn_sent_sockets,
    )?;
    write_labeled_metric(
        writer,
        "muninn_gate_http_sockets",
        "syn_received",
        gateway.http_syn_received_sockets,
    )?;
    write_labeled_metric(
        writer,
        "muninn_gate_http_sockets",
        "established",
        gateway.http_established_sockets,
    )?;
    write_labeled_metric(
        writer,
        "muninn_gate_http_sockets",
        "fin_wait_1",
        gateway.http_fin_wait_1_sockets,
    )?;
    write_labeled_metric(
        writer,
        "muninn_gate_http_sockets",
        "fin_wait_2",
        gateway.http_fin_wait_2_sockets,
    )?;
    write_labeled_metric(
        writer,
        "muninn_gate_http_sockets",
        "close_wait",
        gateway.http_close_wait_sockets,
    )?;
    write_labeled_metric(
        writer,
        "muninn_gate_http_sockets",
        "closing",
        gateway.http_closing_sockets,
    )?;
    write_labeled_metric(
        writer,
        "muninn_gate_http_sockets",
        "last_ack",
        gateway.http_last_ack_sockets,
    )?;
    write_labeled_metric(
        writer,
        "muninn_gate_http_sockets",
        "time_wait",
        gateway.http_time_wait_sockets,
    )?;

    write_optional_labeled_metric(
        writer,
        "muninn_gate_http_oldest_socket_age_ms",
        "any",
        gateway.http_oldest_socket_age_ms,
    )?;
    write_optional_labeled_metric(
        writer,
        "muninn_gate_http_oldest_socket_age_ms",
        "syn_sent",
        gateway.http_oldest_syn_sent_ms,
    )?;
    write_optional_labeled_metric(
        writer,
        "muninn_gate_http_oldest_socket_age_ms",
        "syn_received",
        gateway.http_oldest_syn_received_ms,
    )?;
    write_optional_labeled_metric(
        writer,
        "muninn_gate_http_oldest_socket_age_ms",
        "established",
        gateway.http_oldest_established_ms,
    )?;
    write_optional_labeled_metric(
        writer,
        "muninn_gate_http_oldest_socket_age_ms",
        "fin_wait_1",
        gateway.http_oldest_fin_wait_1_ms,
    )?;
    write_optional_labeled_metric(
        writer,
        "muninn_gate_http_oldest_socket_age_ms",
        "fin_wait_2",
        gateway.http_oldest_fin_wait_2_ms,
    )?;
    write_optional_labeled_metric(
        writer,
        "muninn_gate_http_oldest_socket_age_ms",
        "close_wait",
        gateway.http_oldest_close_wait_ms,
    )?;
    write_optional_labeled_metric(
        writer,
        "muninn_gate_http_oldest_socket_age_ms",
        "closing",
        gateway.http_oldest_closing_ms,
    )?;
    write_optional_labeled_metric(
        writer,
        "muninn_gate_http_oldest_socket_age_ms",
        "last_ack",
        gateway.http_oldest_last_ack_ms,
    )?;
    write_optional_labeled_metric(
        writer,
        "muninn_gate_http_oldest_socket_age_ms",
        "time_wait",
        gateway.http_oldest_time_wait_ms,
    )?;
    Ok(())
}

fn write_labeled_metric<W, T>(
    writer: &mut W,
    name: &str,
    state: &str,
    value: T,
) -> Result<(), WifiStartError>
where
    W: fmt::Write + ?Sized,
    T: fmt::Display,
{
    writeln!(writer, "{}{{state=\"{}\"}} {}", name, state, value)
        .map_err(|_| WifiStartError::HttpRender)
}

fn write_optional_labeled_metric<W, T>(
    writer: &mut W,
    name: &str,
    state: &str,
    value: Option<T>,
) -> Result<(), WifiStartError>
where
    W: fmt::Write + ?Sized,
    T: fmt::Display,
{
    if let Some(value) = value {
        write_labeled_metric(writer, name, state, value)?;
    }
    Ok(())
}

fn write_tenths_metric<W>(writer: &mut W, name: &str, value: i16) -> Result<(), WifiStartError>
where
    W: fmt::Write + ?Sized,
{
    let value = i32::from(value);
    let sign = if value < 0 { "-" } else { "" };
    let abs = value.abs();
    writeln!(writer, "{} {}{}.{}", name, sign, abs / 10, abs % 10)
        .map_err(|_| WifiStartError::HttpRender)
}

fn write_producer_metrics<W>(
    writer: &mut W,
    gateway_uptime_ms: u64,
    record: &muninn_gate_core::TelemetryRecord,
) -> Result<(), WifiStartError>
where
    W: fmt::Write + ?Sized,
{
    write_node_metric(
        writer,
        "muninn_gate_node_poll_success_total",
        record,
        record.poll.poll_success_total,
    )?;
    write_node_metric(
        writer,
        "muninn_gate_node_poll_failure_total",
        record,
        record.poll.poll_failure_total,
    )?;

    let Some(telemetry) = record.telemetry else {
        return Ok(());
    };

    write_node_metric(
        writer,
        "muninn_gate_node_last_heard_age_ms",
        record,
        gateway_uptime_ms.saturating_sub(telemetry.timestamp_ms),
    )?;
    if let Some(rssi) = telemetry.rssi {
        write_node_metric(writer, "muninn_gate_node_rssi", record, rssi)?;
    }
    if let Some(snr) = telemetry.snr {
        write_node_metric(writer, "muninn_gate_node_snr", record, snr)?;
    }

    for channel in telemetry.channels() {
        write_optional_channel_metric(
            writer,
            "muninn_gate_node_channel_battery_voltage",
            record,
            channel.channel_id,
            channel.metrics.battery_voltage,
        )?;
        write_optional_channel_metric(
            writer,
            "muninn_gate_node_channel_battery_percent",
            record,
            channel.channel_id,
            channel.metrics.battery_percent,
        )?;
        write_optional_channel_metric(
            writer,
            "muninn_gate_node_channel_voltage",
            record,
            channel.channel_id,
            channel.metrics.voltage,
        )?;
        write_optional_channel_metric(
            writer,
            "muninn_gate_node_channel_current_amps",
            record,
            channel.channel_id,
            channel.metrics.current_amps,
        )?;
        write_optional_channel_metric(
            writer,
            "muninn_gate_node_channel_power_watts",
            record,
            channel.channel_id,
            channel.metrics.power_watts,
        )?;
        write_optional_channel_metric(
            writer,
            "muninn_gate_node_channel_temperature_celsius",
            record,
            channel.channel_id,
            channel.metrics.temperature_celsius,
        )?;
        write_optional_channel_metric(
            writer,
            "muninn_gate_node_channel_humidity_percent",
            record,
            channel.channel_id,
            channel.metrics.humidity_percent,
        )?;
        write_optional_channel_metric(
            writer,
            "muninn_gate_node_channel_pressure_pa",
            record,
            channel.channel_id,
            channel.metrics.pressure_pa,
        )?;
        write_optional_channel_metric(
            writer,
            "muninn_gate_node_channel_gas_resistance_ohms",
            record,
            channel.channel_id,
            channel.metrics.gas_resistance_ohms,
        )?;
        write_optional_channel_metric(
            writer,
            "muninn_gate_node_channel_luminosity_lux",
            record,
            channel.channel_id,
            channel.metrics.luminosity_lux,
        )?;
    }

    Ok(())
}

fn write_optional_channel_metric<W>(
    writer: &mut W,
    name: &str,
    record: &muninn_gate_core::TelemetryRecord,
    channel_id: u8,
    value: Option<f32>,
) -> Result<(), WifiStartError>
where
    W: fmt::Write + ?Sized,
{
    if let Some(value) = value {
        write!(writer, "{}{{node=\"", name).map_err(|_| WifiStartError::HttpRender)?;
        write_label_value(writer, record.config.name.as_str())?;
        writeln!(writer, "\",channel=\"{}\"}} {}", channel_id, value)
            .map_err(|_| WifiStartError::HttpRender)?;
    }
    Ok(())
}

fn write_node_metric<W, T>(
    writer: &mut W,
    name: &str,
    record: &muninn_gate_core::TelemetryRecord,
    value: T,
) -> Result<(), WifiStartError>
where
    W: fmt::Write + ?Sized,
    T: fmt::Display,
{
    write!(writer, "{}{{node=\"", name).map_err(|_| WifiStartError::HttpRender)?;
    write_label_value(writer, record.config.name.as_str())?;
    writeln!(writer, "\"}} {}", value).map_err(|_| WifiStartError::HttpRender)
}

fn write_label_value<W>(writer: &mut W, value: &str) -> Result<(), WifiStartError>
where
    W: fmt::Write + ?Sized,
{
    for ch in value.chars() {
        match ch {
            '\\' => writer
                .write_str("\\\\")
                .map_err(|_| WifiStartError::HttpRender)?,
            '"' => writer
                .write_str("\\\"")
                .map_err(|_| WifiStartError::HttpRender)?,
            '\n' => writer
                .write_char(' ')
                .map_err(|_| WifiStartError::HttpRender)?,
            _ => writer
                .write_char(ch)
                .map_err(|_| WifiStartError::HttpRender)?,
        }
    }
    Ok(())
}

fn station_config(
    config: &GatewayConfig<MAX_TELEMETRY_PRODUCERS>,
) -> Result<ClientConfiguration, WifiStartError>
{
    let http = config.http.as_ref().ok_or(WifiStartError::Disabled)?;
    let ssid = alloc_string(http.wifi_ssid.as_str())?;
    let password = alloc_string(http.wifi_password.as_str())?;
    let auth_method = if http.wifi_password.is_empty() {
        AuthMethod::None
    } else {
        AuthMethod::WPA2Personal
    };

    Ok(ClientConfiguration {
        ssid,
        bssid: None,
        auth_method,
        password,
        channel: None,
    })
}

fn station_config_pinned_to_best_ap(
    controller: &mut esp_wifi::wifi::WifiController<'_>,
    platform: &Esp32Platform,
    station_config: &ClientConfiguration,
) -> Option<ClientConfiguration>
{
    let ssid = station_config.ssid.as_str();
    let Some(pin) = crate::wifi_common::scan_strongest_bssid(
        controller,
        ssid,
        crate::wifi_common::SCAN_PIN_RESULTS,
    ) else {
        // `scan_strongest_bssid` already logged the scan / no-match
        // reason — translate to the Heltec diagnostics ring here so
        // the operator can grep for it the same way as before.
        esp_println::println!("WiFi: no scan result matched configured SSID \"{}\"", ssid);
        record_diagnostic(
            platform.now_ms(),
            DiagnosticLevel::Warn,
            DiagnosticSubsystem::Wifi,
            "ap_scan_no_match",
            "WiFi scan did not return the configured SSID",
        );
        return None;
    };

    esp_println::println!(
        "WiFi: pinning AP bssid={:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x} channel={} rssi={}",
        pin.bssid[0],
        pin.bssid[1],
        pin.bssid[2],
        pin.bssid[3],
        pin.bssid[4],
        pin.bssid[5],
        pin.channel,
        pin.rssi,
    );
    record_diagnostic(
        platform.now_ms(),
        DiagnosticLevel::Info,
        DiagnosticSubsystem::Wifi,
        "ap_pinned",
        "WiFi station pinned to strongest scanned BSSID/channel",
    );

    let mut pinned = station_config.clone();
    pinned.bssid = Some(pin.bssid);
    pinned.channel = Some(pin.channel);
    Some(pinned)
}

fn alloc_string(value: &str) -> Result<AllocString, WifiStartError>
{
    let mut out = AllocString::new();
    out.try_reserve_exact(value.len())
        .map_err(|_| WifiStartError::Allocation)?;
    out.push_str(value);
    Ok(out)
}

fn wait_for_wifi_recovering(
    controller: &mut esp_wifi::wifi::WifiController<'_>,
    delay: &Delay,
    mut display: Option<&mut LocalDisplay>,
    ssid: &str,
    platform: &Esp32Platform,
    station_config: &ClientConfiguration,
) -> Result<(), WifiStartError>
{
    let mut recovery = WifiConnectRecovery::new(platform.now_ms());
    loop {
        if matches!(controller.is_connected(), Ok(true)) {
            recovery.mark_connected(platform.now_ms());
            return Ok(());
        }

        let now_ms = platform.now_ms();
        if recovery.should_deep_recover(now_ms) {
            deep_recover_wifi(controller, delay, platform, station_config, "startup");
            recovery.mark_deep_recovery(now_ms);
        }

        let disconnect_seq = wifi_disconnect_events();
        if recovery.should_connect(now_ms, disconnect_seq) {
            request_wifi_connect(controller, platform, "startup");
            recovery.mark_connect_request(now_ms, disconnect_seq);
        }

        if recovery.should_log_progress(now_ms) {
            esp_println::println!(
                "WiFi: still connecting attempts={} deep_recoveries={}",
                recovery.connect_attempts(),
                recovery.deep_recovery_total()
            );
        }

        if let Some(display) = display.as_deref_mut() {
            let _ =
                display.show_wifi_connecting(ssid, "Connecting", recovery.connect_attempts() as u8);
        }
        delay.delay_millis(WIFI_CONNECT_POLL_MS);
    }
}

fn request_wifi_connect(
    controller: &mut esp_wifi::wifi::WifiController<'_>,
    platform: &Esp32Platform,
    phase: &str,
)
{
    crate::telemetry_state::record_wifi_connect_started(platform.now_ms());
    match controller.connect() {
        Ok(()) => {
            crate::telemetry_state::record_wifi_connect_request(false);
        },
        Err(error) => {
            crate::telemetry_state::record_wifi_connect_request(true);
            esp_println::println!("WiFi: connect request failed during {}: {:?}", phase, error);
            record_diagnostic(
                platform.now_ms(),
                DiagnosticLevel::Debug,
                DiagnosticSubsystem::Wifi,
                "connect_request_failed",
                phase,
            );
        },
    }
}

fn deep_recover_wifi(
    controller: &mut esp_wifi::wifi::WifiController<'_>,
    delay: &Delay,
    platform: &Esp32Platform,
    station_config: &ClientConfiguration,
    phase: &str,
)
{
    crate::telemetry_state::record_wifi_deep_recovery();
    esp_println::println!(
        "WiFi: deep recovery during {}; restarting controller",
        phase
    );
    record_diagnostic(
        platform.now_ms(),
        DiagnosticLevel::Warn,
        DiagnosticSubsystem::Wifi,
        "deep_recovery",
        phase,
    );

    let _ = controller.disconnect();
    delay.delay_millis(250);
    let _ = controller.stop();
    delay.delay_millis(500);

    if let Err(error) = controller.set_configuration(&Configuration::Client(station_config.clone()))
    {
        esp_println::println!("WiFi: deep recovery set_configuration failed: {:?}", error);
        record_diagnostic(
            platform.now_ms(),
            DiagnosticLevel::Warn,
            DiagnosticSubsystem::Wifi,
            "deep_recovery_config_failed",
            phase,
        );
        delay.delay_millis(1_000);
        return;
    }

    if let Err(error) = controller.start() {
        esp_println::println!("WiFi: deep recovery start failed: {:?}", error);
        record_diagnostic(
            platform.now_ms(),
            DiagnosticLevel::Warn,
            DiagnosticSubsystem::Wifi,
            "deep_recovery_start_failed",
            phase,
        );
        delay.delay_millis(1_000);
        return;
    }

    if let Err(error) = wait_for_wifi_started(controller, delay) {
        record_diagnostic(
            platform.now_ms(),
            DiagnosticLevel::Warn,
            DiagnosticSubsystem::Wifi,
            "deep_recovery_start_timeout",
            error.as_str(),
        );
        delay.delay_millis(1_000);
        return;
    }

    let _ = configure_wifi_reliability(controller, platform);
    delay.delay_millis(500);
}

fn dhcp_progress_step(polls: usize) -> u8
{
    poll_elapsed_seconds(polls, DHCP_POLL_MS)
        .saturating_mul(4)
        .min(u64::from(u8::MAX)) as u8
}

fn poll_elapsed_seconds(polls: usize, poll_ms: u32) -> u64
{
    (polls as u64).saturating_mul(u64::from(poll_ms)) / 1_000
}

fn wait_for_wifi_started(
    controller: &esp_wifi::wifi::WifiController<'_>,
    _delay: &Delay,
) -> Result<(), WifiStartError>
{
    if crate::wifi_common::wait_for_started(controller) {
        Ok(())
    } else {
        esp_println::println!("WiFi: start state did not become ready");
        Err(WifiStartError::Controller)
    }
}

fn configure_wifi_reliability(
    controller: &mut esp_wifi::wifi::WifiController<'_>,
    platform: &Esp32Platform,
) -> Result<(), WifiStartError>
{
    if let Err(error) = crate::wifi_common::disable_power_save(controller) {
        esp_println::println!("WiFi: disable power save failed: {:?}", error);
        record_diagnostic(
            platform.now_ms(),
            DiagnosticLevel::Warn,
            DiagnosticSubsystem::Wifi,
            "power_save_failed",
            "failed to disable WiFi power saving",
        );
        return Err(WifiStartError::Controller);
    }

    match crate::wifi_common::set_max_tx_power() {
        crate::wifi_common::SetTxPowerResult::Applied(applied) => {
            crate::telemetry_state::record_wifi_tx_power(
                crate::wifi_common::TX_POWER_QUARTER_DBM,
                applied,
            );
            esp_println::println!(
                "WiFi: TX power cap requested={} applied={} (0.25 dBm units)",
                crate::wifi_common::TX_POWER_QUARTER_DBM,
                applied,
            );
            record_diagnostic(
                platform.now_ms(),
                DiagnosticLevel::Info,
                DiagnosticSubsystem::Wifi,
                "tx_power_cap",
                "WiFi TX power cap requested",
            );
        },
        crate::wifi_common::SetTxPowerResult::SetFailed(rc) => {
            esp_println::println!("WiFi: set max TX power failed: {}", rc);
            record_diagnostic(
                platform.now_ms(),
                DiagnosticLevel::Warn,
                DiagnosticSubsystem::Wifi,
                "tx_power_failed",
                "failed to set WiFi max TX power",
            );
            return Err(WifiStartError::Controller);
        },
        crate::wifi_common::SetTxPowerResult::GetFailed => {
            // The cap took effect but readback failed; surface as info
            // since the chip is still in the desired state.
            esp_println::println!(
                "WiFi: TX power cap requested={} (readback failed)",
                crate::wifi_common::TX_POWER_QUARTER_DBM,
            );
        },
    }

    Ok(())
}

fn log_connected_ap_info(platform: &Esp32Platform, phase: &str)
{
    let mut ap_info = MaybeUninit::<esp_wifi_sys::include::wifi_ap_record_t>::uninit();
    let result = unsafe { esp_wifi_sys::include::esp_wifi_sta_get_ap_info(ap_info.as_mut_ptr()) };
    if result != 0 {
        esp_println::println!("WiFi: AP info unavailable during {}: {}", phase, result);
        return;
    }

    let ap_info = unsafe { ap_info.assume_init() };
    crate::telemetry_state::record_wifi_ap_info(
        i16::from(ap_info.rssi),
        ap_info.primary,
        ap_info.bssid,
        Some(ap_info.authmode as u8),
    );
    esp_println::println!(
        "WiFi: AP phase={} bssid={:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x} channel={} rssi={} \
         auth={}",
        phase,
        ap_info.bssid[0],
        ap_info.bssid[1],
        ap_info.bssid[2],
        ap_info.bssid[3],
        ap_info.bssid[4],
        ap_info.bssid[5],
        ap_info.primary,
        ap_info.rssi,
        ap_info.authmode,
    );
    record_diagnostic(
        platform.now_ms(),
        DiagnosticLevel::Info,
        DiagnosticSubsystem::Wifi,
        "ap_info",
        phase,
    );
}

fn install_wifi_event_logging()
{
    StaDisconnected::update_handler(|event| {
        let reason = event.0.reason;
        WIFI_LAST_DISCONNECT_REASON.store(reason, Ordering::Relaxed);
        WIFI_DISCONNECT_EVENTS.fetch_add(1, Ordering::Relaxed);
        esp_println::println!(
            "WiFi: disconnected reason={} ({})",
            reason,
            wifi_disconnect_reason(reason)
        );
    });
}

fn wifi_disconnect_events() -> u32
{
    WIFI_DISCONNECT_EVENTS.load(Ordering::Relaxed)
}

fn wifi_last_disconnect_reason() -> u8
{
    WIFI_LAST_DISCONNECT_REASON.load(Ordering::Relaxed)
}

fn wifi_disconnect_reason(code: u8) -> &'static str
{
    match code {
        2 => "AUTH_EXPIRE",
        4 => "ASSOC_EXPIRE",
        8 => "ASSOC_LEAVE",
        14 => "MIC_FAILURE",
        15 => "4WAY_HANDSHAKE_TIMEOUT",
        16 => "GROUP_KEY_UPDATE_TIMEOUT",
        17 => "IE_IN_4WAY_DIFFERS",
        19 => "PAIRWISE_CIPHER_INVALID",
        20 => "AKMP_INVALID",
        23 => "802_1X_AUTH_FAILED",
        39 => "TIMEOUT",
        46 => "PEER_INITIATED",
        47 => "AP_INITIATED",
        200 => "BEACON_TIMEOUT",
        201 => "NO_AP_FOUND",
        202 => "AUTH_FAIL",
        203 => "ASSOC_FAIL",
        204 => "HANDSHAKE_TIMEOUT",
        205 => "CONNECTION_FAIL",
        206 => "AP_TSF_RESET",
        207 => "ROAMING",
        210 => "NO_AP_FOUND_COMPATIBLE_SECURITY",
        211 => "NO_AP_FOUND_IN_AUTHMODE_THRESHOLD",
        212 => "NO_AP_FOUND_IN_RSSI_THRESHOLD",
        _ => "OTHER",
    }
}

fn network_interface(device: &mut WifiDevice<'_>, platform: &Esp32Platform) -> Interface
{
    let hardware_addr = HardwareAddress::Ethernet(EthernetAddress(device.mac_address()));
    let mut config = InterfaceConfig::new(hardware_addr);
    config.random_seed = platform.now_ms();
    Interface::new(config, device, smoltcp_now(platform))
}

fn poll_network(
    interface: &mut Interface,
    device: &mut WifiDevice<'_>,
    sockets: &mut SocketSet<'_>,
    platform: &Esp32Platform,
)
{
    let now_ms = platform.now_ms();
    let _ = interface.poll(smoltcp_now(platform), device, sockets);
    let previous_ms = SMOLTCP_LAST_POLL_MS.swap(now_ms as u32, Ordering::Relaxed);
    let mut observed_gap_ms = None;
    if previous_ms != 0 {
        let gap_ms = now_ms
            .saturating_sub(u64::from(previous_ms))
            .min(u64::from(u32::MAX)) as u32;
        observed_gap_ms = Some(gap_ms);
        let last_log_ms = SMOLTCP_LAST_GAP_LOG_MS.load(Ordering::Relaxed);
        if gap_ms >= SMOLTCP_POLL_GAP_WARN_MS
            && (last_log_ms == 0
                || (now_ms as u32).wrapping_sub(last_log_ms) >= SMOLTCP_POLL_GAP_LOG_COOLDOWN_MS)
        {
            SMOLTCP_LAST_GAP_LOG_MS.store(now_ms as u32, Ordering::Relaxed);
            esp_println::println!("Network: smoltcp poll gap {}ms", gap_ms);
        }
    }
    crate::telemetry_state::record_smoltcp_poll(
        now_ms,
        observed_gap_ms,
        observed_gap_ms.is_some_and(|gap_ms| gap_ms >= SMOLTCP_POLL_GAP_WARN_MS),
        observed_gap_ms.is_some_and(|gap_ms| gap_ms >= SMOLTCP_POLL_GAP_BAD_MS),
    );
}

fn new_http_socket<'buffer>(
    rx_buffer: &'buffer mut [u8; HTTP_TCP_RX_BUFFER_BYTES],
    tx_buffer: &'buffer mut [u8; HTTP_TCP_TX_BUFFER_BYTES],
) -> tcp::Socket<'buffer>
{
    let tcp_rx = tcp::SocketBuffer::new(&mut rx_buffer[..]);
    let tcp_tx = tcp::SocketBuffer::new(&mut tx_buffer[..]);
    let mut socket = tcp::Socket::new(tcp_rx, tcp_tx);
    socket.set_timeout(Some(SmoltcpDuration::from_millis(5_000)));
    socket.set_keep_alive(Some(SmoltcpDuration::from_millis(5_000)));
    socket
}

fn clear_network_config(
    interface: &mut Interface,
    sockets: &mut SocketSet<'_>,
    dhcp_handle: SocketHandle,
    http_handles: &[Option<SocketHandle>],
    request_lens: &mut [usize],
    socket_active_since_ms: &mut [Option<u64>],
)
{
    interface.update_ip_addrs(|addrs| addrs.clear());
    let _ = interface.routes_mut().remove_default_ipv4_route();
    crate::telemetry_state::record_dhcp_deconfigured();
    reset_dhcp_socket(sockets, dhcp_handle);
    reset_http_sockets(sockets, http_handles, request_lens, socket_active_since_ms);
}

fn poll_dhcp(
    interface: &mut Interface,
    sockets: &mut SocketSet<'_>,
    dhcp_handle: SocketHandle,
    port: u16,
    dhcp_started_ms: u64,
    now_ms: u64,
) -> Option<Option<HttpEndpoint>>
{
    match sockets.get_mut::<dhcpv4::Socket>(dhcp_handle).poll() {
        Some(dhcpv4::Event::Configured(config)) => {
            interface.update_ip_addrs(|addrs| {
                addrs.clear();
                let _ = addrs.push(IpCidr::Ipv4(config.address));
            });

            if let Some(router) = config.router {
                let _ = interface.routes_mut().add_default_ipv4_route(router);
            }

            let ip = config.address.address().octets();
            let gateway = config.router.map(|router| router.octets());
            let acquire_ms = now_ms
                .saturating_sub(dhcp_started_ms)
                .min(u64::from(u32::MAX)) as u32;
            crate::telemetry_state::record_dhcp_configured(now_ms, acquire_ms, ip, gateway);

            Some(Some(HttpEndpoint::new(
                config.address.address().octets(),
                port,
            )))
        },
        Some(dhcpv4::Event::Deconfigured) => {
            interface.update_ip_addrs(|addrs| addrs.clear());
            let _ = interface.routes_mut().remove_default_ipv4_route();
            crate::telemetry_state::record_dhcp_deconfigured();
            Some(None)
        },
        None => None,
    }
}

fn reset_dhcp_socket(sockets: &mut SocketSet<'_>, dhcp_handle: SocketHandle)
{
    sockets.get_mut::<dhcpv4::Socket>(dhcp_handle).reset();
    crate::telemetry_state::record_dhcp_reset();
}

fn ensure_listening(
    sockets: &mut SocketSet<'_>,
    http_handles: &[Option<SocketHandle>],
    port: u16,
) -> Result<(), WifiStartError>
{
    for maybe_handle in http_handles {
        let Some(tcp_handle) = *maybe_handle else {
            continue;
        };
        let socket = sockets.get_mut::<tcp::Socket>(tcp_handle);
        if !socket.is_open() {
            socket
                .listen(port)
                .map_err(|_| WifiStartError::HttpListen)?;
        }
    }
    Ok(())
}

fn reset_http_sockets(
    sockets: &mut SocketSet<'_>,
    http_handles: &[Option<SocketHandle>],
    request_lens: &mut [usize],
    socket_active_since_ms: &mut [Option<u64>],
)
{
    request_lens.fill(0);
    socket_active_since_ms.fill(None);
    let mut aborted = 0_u8;
    for maybe_handle in http_handles {
        let Some(tcp_handle) = *maybe_handle else {
            continue;
        };
        let socket = sockets.get_mut::<tcp::Socket>(tcp_handle);
        if socket.is_open() || socket.is_active() {
            aborted = aborted.saturating_add(1);
        }
        socket.abort();
    }
    crate::telemetry_state::record_http_socket_aborts(aborted);
}

fn http_socket_counts(
    sockets: &SocketSet<'_>,
    http_handles: &[Option<SocketHandle>],
    socket_active_since_ms: &[Option<u64>],
    now_ms: u64,
) -> (u8, u8)
{
    let states = http_socket_states(sockets, http_handles, socket_active_since_ms, now_ms);
    (states.active, states.listening)
}

fn http_socket_states(
    sockets: &SocketSet<'_>,
    http_handles: &[Option<SocketHandle>],
    socket_active_since_ms: &[Option<u64>],
    now_ms: u64,
) -> HttpSocketStateSnapshot
{
    let mut states = HttpSocketStateSnapshot::default();

    for (index, maybe_handle) in http_handles.iter().enumerate() {
        let Some(tcp_handle) = *maybe_handle else {
            continue;
        };
        let socket = sockets.get::<tcp::Socket>(tcp_handle);
        if socket.is_active() {
            states.active = states.active.saturating_add(1);
        }
        let age_ms = socket_active_since_ms.get(index).and_then(|active_since| {
            active_since.map(|active_since| socket_age_ms(now_ms, active_since))
        });
        match socket.state() {
            tcp::State::Closed => states.closed = states.closed.saturating_add(1),
            tcp::State::Listen => states.listening = states.listening.saturating_add(1),
            tcp::State::SynSent => {
                states.syn_sent = states.syn_sent.saturating_add(1);
                record_socket_age(&mut states.oldest_socket_age_ms, age_ms);
                record_socket_age(&mut states.oldest_syn_sent_ms, age_ms);
            },
            tcp::State::SynReceived => {
                states.syn_received = states.syn_received.saturating_add(1);
                record_socket_age(&mut states.oldest_socket_age_ms, age_ms);
                record_socket_age(&mut states.oldest_syn_received_ms, age_ms);
            },
            tcp::State::Established => {
                states.established = states.established.saturating_add(1);
                record_socket_age(&mut states.oldest_socket_age_ms, age_ms);
                record_socket_age(&mut states.oldest_established_ms, age_ms);
            },
            tcp::State::FinWait1 => {
                states.fin_wait_1 = states.fin_wait_1.saturating_add(1);
                record_socket_age(&mut states.oldest_socket_age_ms, age_ms);
                record_socket_age(&mut states.oldest_fin_wait_1_ms, age_ms);
            },
            tcp::State::FinWait2 => {
                states.fin_wait_2 = states.fin_wait_2.saturating_add(1);
                record_socket_age(&mut states.oldest_socket_age_ms, age_ms);
                record_socket_age(&mut states.oldest_fin_wait_2_ms, age_ms);
            },
            tcp::State::CloseWait => {
                states.close_wait = states.close_wait.saturating_add(1);
                record_socket_age(&mut states.oldest_socket_age_ms, age_ms);
                record_socket_age(&mut states.oldest_close_wait_ms, age_ms);
            },
            tcp::State::Closing => {
                states.closing = states.closing.saturating_add(1);
                record_socket_age(&mut states.oldest_socket_age_ms, age_ms);
                record_socket_age(&mut states.oldest_closing_ms, age_ms);
            },
            tcp::State::LastAck => {
                states.last_ack = states.last_ack.saturating_add(1);
                record_socket_age(&mut states.oldest_socket_age_ms, age_ms);
                record_socket_age(&mut states.oldest_last_ack_ms, age_ms);
            },
            tcp::State::TimeWait => {
                states.time_wait = states.time_wait.saturating_add(1);
                record_socket_age(&mut states.oldest_socket_age_ms, age_ms);
                record_socket_age(&mut states.oldest_time_wait_ms, age_ms);
            },
        }
    }

    states
}

fn socket_age_ms(now_ms: u64, active_since_ms: u64) -> u32
{
    now_ms
        .saturating_sub(active_since_ms)
        .min(u64::from(u32::MAX)) as u32
}

fn record_socket_age(oldest_ms: &mut Option<u32>, age_ms: Option<u32>)
{
    let Some(age_ms) = age_ms else {
        return;
    };
    *oldest_ms = Some(oldest_ms.map_or(age_ms, |oldest| oldest.max(age_ms)));
}

fn recycle_stale_http_sockets(
    sockets: &mut SocketSet<'_>,
    http_handles: &[Option<SocketHandle>],
    request_lens: &mut [usize],
    socket_active_since_ms: &mut [Option<u64>],
    now_ms: u64,
)
{
    for (index, maybe_handle) in http_handles.iter().enumerate() {
        let Some(tcp_handle) = *maybe_handle else {
            request_lens[index] = 0;
            socket_active_since_ms[index] = None;
            continue;
        };

        let socket = sockets.get_mut::<tcp::Socket>(tcp_handle);
        match socket.state() {
            tcp::State::Closed | tcp::State::Listen => {
                request_lens[index] = 0;
                socket_active_since_ms[index] = None;
            },
            tcp::State::CloseWait
            | tcp::State::FinWait1
            | tcp::State::FinWait2
            | tcp::State::Closing
            | tcp::State::LastAck
            | tcp::State::TimeWait => {
                let active_since = socket_active_since_ms[index].get_or_insert(now_ms);
                let age_ms = now_ms.saturating_sub(*active_since);
                if age_ms < HTTP_SOCKET_CLOSE_GRACE_MS {
                    continue;
                }
                socket.abort();
                request_lens[index] = 0;
                socket_active_since_ms[index] = None;
                crate::telemetry_state::record_http_socket_aborts(1);
            },
            _ => {
                let active_since = socket_active_since_ms[index].get_or_insert(now_ms);
                if now_ms.saturating_sub(*active_since) >= HTTP_SOCKET_STALL_MS {
                    esp_println::println!(
                        "HTTP: recycling stalled socket {} state={} age_ms={}",
                        index,
                        socket.state(),
                        now_ms.saturating_sub(*active_since)
                    );
                    socket.abort();
                    request_lens[index] = 0;
                    socket_active_since_ms[index] = None;
                    crate::telemetry_state::record_http_socket_aborts(1);
                }
            },
        }
    }
}

fn clear_partial_requests_if_idle(
    sockets: &SocketSet<'_>,
    http_handles: &[Option<SocketHandle>],
    request_lens: &mut [usize],
)
{
    for (maybe_handle, request_len) in http_handles.iter().zip(request_lens.iter_mut()) {
        if *request_len == 0 {
            continue;
        }

        let Some(tcp_handle) = *maybe_handle else {
            *request_len = 0;
            continue;
        };
        let socket = sockets.get::<tcp::Socket>(tcp_handle);
        if !socket.is_active() {
            *request_len = 0;
        }
    }
}

fn read_request(
    sockets: &mut SocketSet<'_>,
    tcp_handle: SocketHandle,
    request_buffer: &mut [u8; HTTP_REQUEST_BUFFER_BYTES],
    request_len: &mut usize,
) -> Option<usize>
{
    let socket = sockets.get_mut::<tcp::Socket>(tcp_handle);
    if !socket.can_recv() {
        return None;
    }

    let remaining = request_buffer.len().saturating_sub(*request_len);
    let copied = socket
        .recv(|data| {
            let copy_len = min(data.len(), remaining);
            let end = (*request_len).saturating_add(copy_len);
            request_buffer[*request_len..end].copy_from_slice(&data[..copy_len]);
            (data.len(), copy_len)
        })
        .ok()?;

    *request_len = (*request_len).saturating_add(copied);

    if http_metrics::request_headers_complete(&request_buffer[..*request_len])
        || *request_len == request_buffer.len()
        || !sockets.get::<tcp::Socket>(tcp_handle).may_recv()
    {
        let ready_len = *request_len;
        *request_len = 0;
        Some(ready_len)
    } else {
        None
    }
}

fn send_all(
    interface: &mut Interface,
    device: &mut WifiDevice<'_>,
    sockets: &mut SocketSet<'_>,
    tcp_handle: SocketHandle,
    data: &[u8],
    platform: &Esp32Platform,
    delay: &Delay,
) -> Result<(), WifiStartError>
{
    let mut written = 0_usize;
    let mut idle_polls = 0_usize;

    while written < data.len() {
        let sent = {
            let socket = sockets.get_mut::<tcp::Socket>(tcp_handle);
            if !socket.may_send() {
                return Err(WifiStartError::HttpSend);
            }
            if socket.can_send() {
                socket
                    .send_slice(&data[written..])
                    .map_err(|_| WifiStartError::HttpSend)?
            } else {
                0
            }
        };

        if sent == 0 {
            idle_polls = idle_polls.saturating_add(1);
            if idle_polls > HTTP_SEND_IDLE_POLLS {
                return Err(WifiStartError::HttpSend);
            }
            delay.delay_millis(1);
        } else {
            written = written.saturating_add(sent);
            idle_polls = 0;
        }

        poll_network(interface, device, sockets, platform);
    }

    Ok(())
}

fn finish_response(
    interface: &mut Interface,
    device: &mut WifiDevice<'_>,
    sockets: &mut SocketSet<'_>,
    tcp_handle: SocketHandle,
    platform: &Esp32Platform,
    delay: &Delay,
) -> Result<(), WifiStartError>
{
    sockets.get_mut::<tcp::Socket>(tcp_handle).close();

    for _ in 0..HTTP_CLOSE_POLLS {
        poll_network(interface, device, sockets, platform);
        let socket = sockets.get::<tcp::Socket>(tcp_handle);
        if !socket.is_active() {
            break;
        }
        delay.delay_millis(1);
    }

    poll_network(interface, device, sockets, platform);
    Ok(())
}

fn smoltcp_now(platform: &Esp32Platform) -> SmoltcpInstant
{
    SmoltcpInstant::from_millis(platform.now_ms() as i64)
}

fn log_endpoint(endpoint: HttpEndpoint)
{
    esp_println::println!(
        "Network: DHCP address {}.{}.{}.{}",
        endpoint.ipv4[0],
        endpoint.ipv4[1],
        endpoint.ipv4[2],
        endpoint.ipv4[3],
    );
    esp_println::println!("HTTP: listening on port {}", endpoint.port);
}

fn request_is_authorized(request: &[u8], http: Option<&HttpConfig>) -> bool
{
    let Some(http) = http else {
        return true;
    };

    if http.tokens.is_empty() {
        return true;
    }

    let Some(value) = authorization_header_value(request) else {
        return false;
    };
    let Some(token) = value.strip_prefix("Bearer ") else {
        return false;
    };

    http.tokens
        .iter()
        .any(|accepted| accepted.as_str().as_bytes() == token.as_bytes())
}

fn authorization_header_value(request: &[u8]) -> Option<&str>
{
    let text = core::str::from_utf8(request).ok()?;
    for line in text.split("\r\n") {
        let Some((name, value)) = line.split_once(':') else {
            continue;
        };
        if name.eq_ignore_ascii_case("authorization") {
            return Some(value.trim());
        }
    }
    None
}

fn record_diagnostic(
    timestamp_ms: u64,
    level: DiagnosticLevel,
    subsystem: DiagnosticSubsystem,
    code: &str,
    message: &str,
)
{
    let _ = diagnostics::record(timestamp_ms, level, subsystem, code, message);
}

struct HttpSocketIo<'a, 'device, 'sockets>
{
    interface:  &'a mut Interface,
    device:     &'a mut WifiDevice<'device>,
    sockets:    &'a mut SocketSet<'sockets>,
    tcp_handle: SocketHandle,
    platform:   &'a Esp32Platform,
    delay:      &'a Delay,
}

impl<'a, 'device, 'sockets> HttpSocketIo<'a, 'device, 'sockets>
{
    fn new(
        interface: &'a mut Interface,
        device: &'a mut WifiDevice<'device>,
        sockets: &'a mut SocketSet<'sockets>,
        tcp_handle: SocketHandle,
        platform: &'a Esp32Platform,
        delay: &'a Delay,
    ) -> Self
    {
        Self {
            interface,
            device,
            sockets,
            tcp_handle,
            platform,
            delay,
        }
    }

    fn send_all(&mut self, data: &[u8]) -> Result<(), WifiStartError>
    {
        send_all(
            self.interface,
            self.device,
            self.sockets,
            self.tcp_handle,
            data,
            self.platform,
            self.delay,
        )
    }

    fn finish_response(&mut self) -> Result<(), WifiStartError>
    {
        finish_response(
            self.interface,
            self.device,
            self.sockets,
            self.tcp_handle,
            self.platform,
            self.delay,
        )
    }

    fn abort(&mut self)
    {
        self.sockets.get_mut::<tcp::Socket>(self.tcp_handle).abort();
        crate::telemetry_state::record_http_socket_aborts(1);
    }
}

struct ChunkedHttpSocketWriter<'a, 'io, 'device, 'sockets>
{
    io:         &'a mut HttpSocketIo<'io, 'device, 'sockets>,
    buffer:     heapless::Vec<u8, HTTP_CHUNK_BUFFER_BYTES>,
    last_error: Option<WifiStartError>,
}

impl<'a, 'io, 'device, 'sockets> ChunkedHttpSocketWriter<'a, 'io, 'device, 'sockets>
{
    fn new(io: &'a mut HttpSocketIo<'io, 'device, 'sockets>) -> Self
    {
        Self {
            io,
            buffer: heapless::Vec::new(),
            last_error: None,
        }
    }

    fn finish(&mut self) -> Result<(), WifiStartError>
    {
        if let Some(error) = self.last_error {
            return Err(error);
        }
        self.flush_buffer()?;
        self.io.send_all(b"0\r\n\r\n")
    }

    fn flush_buffer(&mut self) -> Result<(), WifiStartError>
    {
        if let Some(error) = self.last_error {
            return Err(error);
        }
        if self.buffer.is_empty() {
            return Ok(());
        }

        let mut chunk_header = heapless::String::<16>::new();
        if write!(chunk_header, "{:x}\r\n", self.buffer.len()).is_err() {
            self.last_error = Some(WifiStartError::HttpRender);
            return Err(WifiStartError::HttpRender);
        }

        let result = self
            .io
            .send_all(chunk_header.as_bytes())
            .and_then(|()| self.io.send_all(self.buffer.as_slice()))
            .and_then(|()| self.io.send_all(b"\r\n"));
        self.buffer.clear();
        if let Err(error) = result {
            self.last_error = Some(error);
            return Err(error);
        }
        Ok(())
    }
}

impl fmt::Write for ChunkedHttpSocketWriter<'_, '_, '_, '_>
{
    fn write_str(&mut self, value: &str) -> fmt::Result
    {
        if value.is_empty() {
            return Ok(());
        }
        if self.last_error.is_some() {
            return Err(fmt::Error);
        }

        let mut remaining = value.as_bytes();
        while !remaining.is_empty() {
            if self.buffer.is_full() && self.flush_buffer().is_err() {
                return Err(fmt::Error);
            }

            let free = HTTP_CHUNK_BUFFER_BYTES.saturating_sub(self.buffer.len());
            let take = min(free, remaining.len());
            if self.buffer.extend_from_slice(&remaining[..take]).is_err() {
                self.last_error = Some(WifiStartError::HttpRender);
                return Err(fmt::Error);
            }
            remaining = &remaining[take..];
        }
        Ok(())
    }
}
