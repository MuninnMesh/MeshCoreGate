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
//! - Bind a small pool of TCP sockets to `http.port` and serve `/metrics`, `/logs`, and `/poll`.
//!   Responses are rendered into fixed-capacity buffers before they are written to the socket.
//! - Keep polling WiFi, DHCP, HTTP, the user button, serial JSON, diagnostics, and the telemetry
//!   scheduler in one blocking loop. The LoRa radio owner still runs separately on the APP CPU, so
//!   this network loop can block on TCP writes without missing radio IRQ work.
//! - If WiFi disconnects after startup, clear the network address, reconnect the controller, wait
//!   for DHCP again, and then restart the listener.
//!
//! The code deliberately does not abstract over WiFi stacks. A future nRF52 or
//! USB-only platform should not depend on this module.

use alloc::string::String as AllocString;
use core::cmp::min;
use core::fmt;

use esp_hal::delay::Delay;
use esp_wifi::config::PowerSaveMode;
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
use smoltcp::wire::{EthernetAddress, HardwareAddress, IpCidr};
use static_cell::StaticCell;

use crate::display::LocalDisplay;
use crate::input::UserButton;
use crate::platform::Esp32Platform;
use crate::scheduler::GatewayScheduler;
use crate::{diagnostics, http_metrics, report_state, serial};

/// HTTP request receive buffer size.
pub const HTTP_REQUEST_BUFFER_BYTES: usize = 1024;
/// Number of parallel HTTP sockets. smoltcp has no listen backlog, so one socket is fragile.
pub const HTTP_SOCKET_COUNT: usize = 2;
/// TCP receive buffer size for each HTTP socket.
pub const HTTP_TCP_RX_BUFFER_BYTES: usize = 1024;
/// TCP transmit buffer size for each HTTP socket.
pub const HTTP_TCP_TX_BUFFER_BYTES: usize = 2048;
/// Milliseconds between WiFi association checks.
pub const WIFI_CONNECT_POLL_MS: u32 = 250;
/// Number of connection checks before WiFi startup fails.
pub const WIFI_CONNECT_ATTEMPTS: usize = 80;
/// Number of association checks between explicit connect requests.
pub const WIFI_CONNECT_RETRY_POLLS: usize = 8;
/// Milliseconds between network polls while waiting for DHCP.
pub const DHCP_POLL_MS: u32 = 25;
/// Number of DHCP checks before forcing WiFi association again.
pub const DHCP_ATTEMPTS: usize = 2_400;
/// Number of DHCP checks between progress logs.
pub const DHCP_PROGRESS_POLLS: usize = 320;
/// Number of empty network polls allowed while sending one response.
pub const HTTP_SEND_IDLE_POLLS: usize = 2_000;
/// Number of 1 ms polls allowed while closing one HTTP response.
pub const HTTP_CLOSE_POLLS: usize = 2_000;
/// Number of 10 ms polls allowed while waiting for WiFi start state.
pub const WIFI_START_STATE_POLLS: usize = 200;
/// Reliability-oriented ESP WiFi TX power cap, in 0.25 dBm units.
pub const WIFI_TX_POWER_QUARTER_DBM: i8 = 60;

type HttpTcpRxBuffers = [[u8; HTTP_TCP_RX_BUFFER_BYTES]; HTTP_SOCKET_COUNT];
type HttpTcpTxBuffers = [[u8; HTTP_TCP_TX_BUFFER_BYTES]; HTTP_SOCKET_COUNT];
type HttpRequestBuffers = [[u8; HTTP_REQUEST_BUFFER_BYTES]; HTTP_SOCKET_COUNT];

static HTTP_TCP_RX_BUFFERS: StaticCell<HttpTcpRxBuffers> = StaticCell::new();
static HTTP_TCP_TX_BUFFERS: StaticCell<HttpTcpTxBuffers> = StaticCell::new();
static HTTP_REQUEST_BUFFERS: StaticCell<HttpRequestBuffers> = StaticCell::new();

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
    /// The station did not connect to the configured AP in time.
    ConnectTimeout,
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
            Self::ConnectTimeout => "wifi connect timeout",
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
            | Self::Allocation
            | Self::ConnectTimeout => DiagnosticSubsystem::Wifi,
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
            Self::ConnectTimeout => "CHECK SSID/PASS",
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
    before_serving: impl FnOnce(&mut Esp32Platform, &GatewayConfig<MAX_TELEMETRY_PRODUCERS>),
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
        before_serving,
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
    before_serving: impl FnOnce(&mut Esp32Platform, &GatewayConfig<MAX_TELEMETRY_PRODUCERS>),
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

    install_wifi_event_logging();
    let station_config = station_config(config)?;

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

    if let Err(error) = controller.set_configuration(&Configuration::Client(station_config)) {
        esp_println::println!("WiFi: set_configuration failed: {:?}", error);
        return Err(WifiStartError::Controller);
    }

    if let Err(error) = controller.start() {
        esp_println::println!("WiFi: start failed: {:?}", error);
        return Err(WifiStartError::Controller);
    }

    wait_for_wifi_started(&controller, &delay)?;
    configure_wifi_reliability(&mut controller, platform)?;
    delay.delay_millis(2_000);

    wait_for_wifi(&mut controller, &delay, display.as_deref_mut(), ssid)?;

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
    let request_buffers =
        HTTP_REQUEST_BUFFERS.init_with(|| [[0_u8; HTTP_REQUEST_BUFFER_BYTES]; HTTP_SOCKET_COUNT]);
    let mut socket_storage = [SocketStorage::EMPTY; HTTP_SOCKET_COUNT + 1];
    let mut sockets = SocketSet::new(&mut socket_storage[..]);
    let mut dhcp_socket = dhcpv4::Socket::new();
    let mut retry_config = dhcp_socket.get_retry_config();
    retry_config.discover_timeout = SmoltcpDuration::from_secs(2);
    retry_config.initial_request_timeout = SmoltcpDuration::from_secs(2);
    retry_config.request_retries = 3;
    dhcp_socket.set_retry_config(retry_config);
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
    let mut endpoint = None;
    let mut dhcp_polls = 0_usize;
    let mut reconnect_polls = 0_usize;
    let mut wifi_connected = true;
    let mut serving_state = GatewayRuntimeState::Provisioned;
    let mut before_serving = Some(before_serving);

    serial::write_line("Network: waiting for DHCP");

    loop {
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
                serving_state = GatewayRuntimeState::Provisioned;
                clear_network_config(
                    &mut interface,
                    &mut sockets,
                    dhcp_handle,
                    &http_handles,
                    &mut request_lens,
                );
                let _ = configure_wifi_reliability(&mut controller, platform);
            }

            wifi_connected = false;
            reconnect_polls = reconnect_polls.saturating_add(1);
            if reconnect_polls == 1 || should_retry_wifi_connect(reconnect_polls) {
                let _ = controller.connect();
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
            serial::write_line("WiFi: reconnected");
            record_diagnostic(
                platform.now_ms(),
                DiagnosticLevel::Info,
                DiagnosticSubsystem::Wifi,
                "reconnected",
                "WiFi station reconnected",
            );
            sockets.get_mut::<dhcpv4::Socket>(dhcp_handle).reset();
            reset_http_sockets(&mut sockets, &http_handles, &mut request_lens);
            endpoint = None;
            dhcp_polls = 0;
        }

        poll_network(&mut interface, &mut station_device, &mut sockets, platform);

        if let Some(next_endpoint) = poll_dhcp(
            &mut interface,
            &mut sockets,
            dhcp_handle,
            config.http.as_ref().map_or(80, |http| http.port),
        ) {
            dhcp_polls = 0;
            if endpoint != next_endpoint {
                reset_http_sockets(&mut sockets, &http_handles, &mut request_lens);
                endpoint = next_endpoint;
                if let Some(endpoint) = endpoint {
                    if let Some(start_services) = before_serving.take() {
                        start_services(platform, config);
                    }
                    log_endpoint(endpoint);
                    record_diagnostic(
                        platform.now_ms(),
                        DiagnosticLevel::Info,
                        DiagnosticSubsystem::Wifi,
                        "dhcp_acquired",
                        "DHCP IPv4 lease acquired",
                    );
                    record_diagnostic(
                        platform.now_ms(),
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
                        let snapshot = crate::telemetry_state::snapshot();
                        let _ = display.show_http_dashboard::<V>(
                            config,
                            &snapshot,
                            serving_state,
                            platform.now_ms(),
                        );
                    }
                } else {
                    serial::write_line("Network: DHCP lease lost");
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
                );
                let _ = configure_wifi_reliability(&mut controller, platform);
                wifi_connected = false;
                endpoint = None;
                dhcp_polls = 0;
                delay.delay_millis(1_000);
                continue;
            }
            delay.delay_millis(DHCP_POLL_MS);
            continue;
        }

        scheduler.tick::<V>(
            platform,
            config,
            tx_power,
            serving_state,
            display.as_deref_mut(),
            Some(&mut *button),
        );

        clear_partial_requests_if_idle(&sockets, &http_handles, &mut request_lens);

        ensure_listening(
            &mut sockets,
            &http_handles,
            config.http.as_ref().map_or(80, |http| http.port),
        )?;

        for index in 0..HTTP_SOCKET_COUNT {
            let Some(tcp_handle) = http_handles[index] else {
                continue;
            };
            if let Some(ready_len) = read_request(
                &mut sockets,
                tcp_handle,
                &mut request_buffers[index],
                &mut request_lens[index],
            ) {
                let request = &request_buffers[index][..ready_len];
                let route = http_metrics::route_request(request);
                let mut http_io = HttpSocketIo::new(
                    &mut interface,
                    &mut station_device,
                    &mut sockets,
                    tcp_handle,
                    platform,
                    &delay,
                );
                match handle_http_request(&mut http_io, request, config.http.as_ref(), tx_power) {
                    Ok(()) => {},
                    Err(WifiStartError::HttpSend) => {
                        record_diagnostic(
                            platform.now_ms(),
                            DiagnosticLevel::Debug,
                            DiagnosticSubsystem::Http,
                            "response_aborted",
                            route.as_str(),
                        );
                        http_io.abort();
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
                    },
                }
            }
        }

        delay.delay_millis(1);
    }
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
    let header =
        http_metrics::render_stream_header("200 OK", "text/plain; version=0.0.4; charset=utf-8")
            .map_err(|_| WifiStartError::HttpRender)?;

    io.send_all(header.as_bytes())?;

    {
        let mut writer = HttpSocketWriter::new(io);
        if http_metrics::render_prometheus_into(&snapshot, &mut writer).is_err() {
            return Err(writer.last_error.unwrap_or(WifiStartError::HttpRender));
        }
    }

    io.finish_response()
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

fn alloc_string(value: &str) -> Result<AllocString, WifiStartError>
{
    let mut out = AllocString::new();
    out.try_reserve_exact(value.len())
        .map_err(|_| WifiStartError::Allocation)?;
    out.push_str(value);
    Ok(out)
}

fn wait_for_wifi(
    controller: &mut esp_wifi::wifi::WifiController<'_>,
    delay: &Delay,
    mut display: Option<&mut LocalDisplay>,
    ssid: &str,
) -> Result<(), WifiStartError>
{
    for attempt in 0..WIFI_CONNECT_ATTEMPTS {
        if matches!(controller.is_connected(), Ok(true)) {
            return Ok(());
        }
        if should_retry_wifi_connect(attempt) {
            if let Err(error) = controller.connect() {
                esp_println::println!("WiFi: connect request failed: {:?}", error);
            }
        }
        if let Some(display) = display.as_deref_mut() {
            let _ = display.show_wifi_connecting(ssid, "Connecting", attempt as u8);
        }
        delay.delay_millis(WIFI_CONNECT_POLL_MS);
    }

    Err(WifiStartError::ConnectTimeout)
}

fn should_retry_wifi_connect(polls: usize) -> bool
{
    polls == 0 || polls % WIFI_CONNECT_RETRY_POLLS == 0
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
    delay: &Delay,
) -> Result<(), WifiStartError>
{
    for _ in 0..WIFI_START_STATE_POLLS {
        if matches!(controller.is_started(), Ok(true)) {
            return Ok(());
        }
        delay.delay_millis(10);
    }

    esp_println::println!("WiFi: start state did not become ready");
    Err(WifiStartError::Controller)
}

fn configure_wifi_reliability(
    controller: &mut esp_wifi::wifi::WifiController<'_>,
    platform: &Esp32Platform,
) -> Result<(), WifiStartError>
{
    if let Err(error) = controller.set_power_saving(PowerSaveMode::None) {
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

    set_wifi_tx_power_cap(platform)
}

fn set_wifi_tx_power_cap(platform: &Esp32Platform) -> Result<(), WifiStartError>
{
    let result =
        unsafe { esp_wifi_sys::include::esp_wifi_set_max_tx_power(WIFI_TX_POWER_QUARTER_DBM) };
    if result != 0 {
        esp_println::println!("WiFi: set max TX power failed: {}", result);
        record_diagnostic(
            platform.now_ms(),
            DiagnosticLevel::Warn,
            DiagnosticSubsystem::Wifi,
            "tx_power_failed",
            "failed to set WiFi max TX power",
        );
        return Err(WifiStartError::Controller);
    }

    let mut applied = 0_i8;
    let result = unsafe { esp_wifi_sys::include::esp_wifi_get_max_tx_power(&mut applied) };
    if result == 0 {
        esp_println::println!(
            "WiFi: TX power cap requested={} applied={} (0.25 dBm units)",
            WIFI_TX_POWER_QUARTER_DBM,
            applied,
        );
        record_diagnostic(
            platform.now_ms(),
            DiagnosticLevel::Info,
            DiagnosticSubsystem::Wifi,
            "tx_power_cap",
            "WiFi TX power cap requested",
        );
    }

    Ok(())
}

fn install_wifi_event_logging()
{
    StaDisconnected::update_handler(|event| {
        let reason = event.0.reason;
        esp_println::println!(
            "WiFi: disconnected reason={} ({})",
            reason,
            wifi_disconnect_reason(reason)
        );
    });
}

fn wifi_disconnect_reason(code: u8) -> &'static str
{
    match code {
        2 => "AUTH_EXPIRE",
        4 => "ASSOC_EXPIRE",
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
    let _ = interface.poll(smoltcp_now(platform), device, sockets);
}

fn new_http_socket<'buffer>(
    rx_buffer: &'buffer mut [u8; HTTP_TCP_RX_BUFFER_BYTES],
    tx_buffer: &'buffer mut [u8; HTTP_TCP_TX_BUFFER_BYTES],
) -> tcp::Socket<'buffer>
{
    let tcp_rx = tcp::SocketBuffer::new(&mut rx_buffer[..]);
    let tcp_tx = tcp::SocketBuffer::new(&mut tx_buffer[..]);
    let mut socket = tcp::Socket::new(tcp_rx, tcp_tx);
    socket.set_nagle_enabled(false);
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
)
{
    interface.update_ip_addrs(|addrs| addrs.clear());
    let _ = interface.routes_mut().remove_default_ipv4_route();
    sockets.get_mut::<dhcpv4::Socket>(dhcp_handle).reset();
    reset_http_sockets(sockets, http_handles, request_lens);
}

fn poll_dhcp(
    interface: &mut Interface,
    sockets: &mut SocketSet<'_>,
    dhcp_handle: SocketHandle,
    port: u16,
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

            Some(Some(HttpEndpoint::new(
                config.address.address().octets(),
                port,
            )))
        },
        Some(dhcpv4::Event::Deconfigured) => {
            interface.update_ip_addrs(|addrs| addrs.clear());
            let _ = interface.routes_mut().remove_default_ipv4_route();
            Some(None)
        },
        None => None,
    }
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
)
{
    request_lens.fill(0);
    for maybe_handle in http_handles {
        let Some(tcp_handle) = *maybe_handle else {
            continue;
        };
        sockets.get_mut::<tcp::Socket>(tcp_handle).abort();
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
        let socket_open = sockets.get_mut::<tcp::Socket>(tcp_handle).is_open();
        if !socket_open {
            return Ok(());
        }
        delay.delay_millis(1);
    }

    sockets.get_mut::<tcp::Socket>(tcp_handle).abort();
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
    }
}

struct HttpSocketWriter<'a, 'io, 'device, 'sockets>
{
    io:         &'a mut HttpSocketIo<'io, 'device, 'sockets>,
    last_error: Option<WifiStartError>,
}

impl<'a, 'io, 'device, 'sockets> HttpSocketWriter<'a, 'io, 'device, 'sockets>
{
    fn new(io: &'a mut HttpSocketIo<'io, 'device, 'sockets>) -> Self
    {
        Self {
            io,
            last_error: None,
        }
    }
}

impl fmt::Write for HttpSocketWriter<'_, '_, '_, '_>
{
    fn write_str(&mut self, value: &str) -> fmt::Result
    {
        if value.is_empty() {
            return Ok(());
        }

        match self.io.send_all(value.as_bytes()) {
            Ok(()) => Ok(()),
            Err(error) => {
                self.last_error = Some(error);
                Err(fmt::Error)
            },
        }
    }
}
