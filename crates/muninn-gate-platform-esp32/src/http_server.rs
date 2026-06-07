//! Chip-level HTTP server shared by every ESP32 board variant.
//!
//! Owns the smoltcp interface, a pool of TCP listen sockets, and the
//! per-request read / route / write pipeline. The actual response
//! generation lives in [`crate::http_metrics`] which is platform
//! neutral; this module just plumbs requests through that pipeline
//! against a real network stack.
//!
//! Responsibilities split:
//!
//! - [`HttpServer`] owns: smoltcp `Interface`, `SocketSet`, DHCP socket, TCP listen socket pool,
//!   request-staging buffers, per-socket activity tracking, the configured listen port, the
//!   resolved `HttpEndpoint` after DHCP succeeds.
//! - Caller owns: `WifiController` (for is_connected / reconnect), `WifiDevice` (passed to each
//!   poll), telemetry / scheduler / radio orchestration.
//!
//! Pool sizing: 8 TCP sockets + 1 DHCP, sized to keep static buffers in
//! the ~10 KiB range. The Heltec runtime's 16-socket pool is a TODO
//! follow-up that swaps the StaticCell size.

use alloc::boxed::Box;
use alloc::string::String as AllocString;
use alloc::vec::Vec as AllocVec;
use core::cmp::min;
use core::fmt::Write as _;

use esp_hal::delay::Delay;
use esp_hal_ota::{Ota, OtaError};
use esp_storage::FlashStorage;
use esp_wifi::wifi::WifiDevice;
use muninn_gate_core::config::MAX_TELEMETRY_PRODUCERS;
use muninn_gate_core::{GatewayConfig, HttpEndpoint, TimeSettings, TxPowerMapping};
use serde::Deserialize;
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

use crate::provisioning::{self, ProvisioningCommand, USB_CONFIG_JSON_BYTES};
use crate::{http_metrics, wifi_common};

/// HTTP request receive buffer size — enough for the longest path +
/// headers we serve.
pub const HTTP_REQUEST_BUFFER_BYTES: usize = 1024;
/// Number of TCP listen sockets in the pool. 8 keeps static buffers
/// around 10 KiB; bumpable per board if telemetry scrapers hammer the
/// gate hard.
pub const HTTP_SOCKET_COUNT: usize = 8;
/// Per-socket TCP receive buffer.
pub const HTTP_TCP_RX_BUFFER_BYTES: usize = 512;
/// Per-socket TCP transmit buffer.
pub const HTTP_TCP_TX_BUFFER_BYTES: usize = 1024;
/// Polls allowed inside `send_all` waiting for the TCP send buffer to
/// drain.
const HTTP_SEND_IDLE_POLLS: usize = 2_000;
/// Polls allowed while waiting for a POST body to arrive after headers.
const HTTP_BODY_IDLE_POLLS: usize = 5_000;
/// Maximum JSON body accepted by `/time`.
const HTTP_TIME_BODY_BYTES: usize = 256;
/// Maximum age (ms) for a stuck non-listening socket before we force a
/// recycle to clear it from the pool.
const HTTP_SOCKET_STALL_MS: u64 = 15_000;
/// Buffer firmware writes to one flash sector so OTA does not erase the
/// same sector repeatedly when TCP delivers sub-sector chunks.
const HTTP_OTA_WRITE_BUFFER_BYTES: usize = 4096;
/// Maximum TCP chunks to consume per HTTP poll while an OTA upload is active.
///
/// Keeping this bounded lets the board runtime continue servicing UI/LED
/// cadence and radio background work between flash writes.
const HTTP_OTA_MAX_CHUNKS_PER_POLL: usize = 4;
/// Coarse progress log interval for HTTP OTA uploads.
const HTTP_OTA_PROGRESS_LOG_BYTES: usize = 64 * 1024;
/// Network drain window after writing the OTA success response before software reset.
const HTTP_OTA_REBOOT_DRAIN_MS: u32 = 1_500;

type HttpTcpRxBuffers = [[u8; HTTP_TCP_RX_BUFFER_BYTES]; HTTP_SOCKET_COUNT];
type HttpTcpTxBuffers = [[u8; HTTP_TCP_TX_BUFFER_BYTES]; HTTP_SOCKET_COUNT];
type HttpRequestBuffers = [[u8; HTTP_REQUEST_BUFFER_BYTES]; HTTP_SOCKET_COUNT];

static HTTP_TCP_RX: StaticCell<HttpTcpRxBuffers> = StaticCell::new();
static HTTP_TCP_TX: StaticCell<HttpTcpTxBuffers> = StaticCell::new();
static HTTP_REQ: StaticCell<HttpRequestBuffers> = StaticCell::new();
static DHCP_HOSTNAME: StaticCell<[u8; wifi_common::DHCP_HOSTNAME_MAX_LEN]> = StaticCell::new();
static DHCP_OPTIONS: StaticCell<[DhcpOption<'static>; 1]> = StaticCell::new();

/// Errors returned by [`HttpServer::poll`].
#[derive(Debug, Clone, Copy)]
pub enum HttpError
{
    /// TCP `listen` rejected — usually means the port is already bound
    /// or the socket is in a bad state.
    Listen,
    /// Response rendering ran out of buffer / failed to format.
    Render,
    /// Response write to the TCP socket failed (peer closed / timeout).
    Send,
}

/// Runtime command accepted through the HTTP control endpoints.
#[derive(Debug)]
pub enum HttpControlCommand
{
    /// Replace the active gateway config with a validated document.
    SetConfig
    {
        /// Source JSON/JSONC document to persist.
        document: AllocString,
        /// Parsed and validated config.
        config:   Box<GatewayConfig<MAX_TELEMETRY_PRODUCERS>>,
    },
    /// Apply a wall-clock seed and/or UTC offset.
    SyncTime(TimeSettings),
    /// Operator requested firmware update/OTA mode.
    OtaRequested,
    /// Operator cleared firmware update/OTA mode.
    OtaFinished,
}

/// Snapshot of the active HTTP OTA upload, if any.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OtaUploadSnapshot
{
    /// Firmware bytes received from the HTTP body.
    pub received_bytes: u32,
    /// Expected body length from `Content-Length`.
    pub total_bytes:    u32,
}

/// Shared HTTP server. Owns the WifiDevice and everything that needs
/// to live as long as the network stack — interface, socket pool,
/// buffers. Caller manages the WifiController separately (for
/// `is_connected()` / `connect()`); this struct only needs the device
/// to push packets through smoltcp.
pub struct HttpServer
{
    device:                 WifiDevice<'static>,
    interface:              Interface,
    sockets:                SocketSet<'static>,
    dhcp_handle:            SocketHandle,
    http_handles:           [SocketHandle; HTTP_SOCKET_COUNT],
    request_buffers:        &'static mut HttpRequestBuffers,
    request_lens:           [usize; HTTP_SOCKET_COUNT],
    socket_active_since_ms: [Option<u64>; HTTP_SOCKET_COUNT],
    port:                   u16,
    endpoint:               Option<HttpEndpoint>,
    dhcp_started_ms:        u64,
    last_http_success_ms:   Option<u64>,
    send_error_total:       u32,
    pending_control:        Option<HttpControlCommand>,
    ota_upload:             Option<OtaUploadSession>,
}

impl HttpServer
{
    /// Build the server. Takes ownership of the WifiDevice — once the
    /// server is alive, the device must not be touched directly. The
    /// smoltcp socket set + listen sockets are created up-front and
    /// stay listening until the server is dropped.
    pub fn new(
        mut device: WifiDevice<'static>,
        port: u16,
        now_ms: u64,
        configured_name: &str,
    ) -> Self
    {
        static SOCKET_STORAGE: StaticCell<[SocketStorage<'static>; HTTP_SOCKET_COUNT + 1]> =
            StaticCell::new();
        let storage = SOCKET_STORAGE.init_with(|| [SocketStorage::EMPTY; HTTP_SOCKET_COUNT + 1]);

        let mut config = InterfaceConfig::new(HardwareAddress::Ethernet(EthernetAddress(
            device.mac_address(),
        )));
        config.random_seed = now_ms;
        let interface = Interface::new(
            config,
            &mut device,
            SmoltcpInstant::from_millis(now_ms as i64),
        );

        let tcp_rx = HTTP_TCP_RX.init_with(|| [[0u8; HTTP_TCP_RX_BUFFER_BYTES]; HTTP_SOCKET_COUNT]);
        let tcp_tx = HTTP_TCP_TX.init_with(|| [[0u8; HTTP_TCP_TX_BUFFER_BYTES]; HTTP_SOCKET_COUNT]);
        let request_buffers =
            HTTP_REQ.init_with(|| [[0u8; HTTP_REQUEST_BUFFER_BYTES]; HTTP_SOCKET_COUNT]);

        let mut sockets = SocketSet::new(&mut storage[..]);

        // DHCP socket with the shared retry config.
        let mut dhcp = dhcpv4::Socket::new();
        wifi_common::configure_dhcp_retry(&mut dhcp);
        let dhcp_hostname = wifi_common::configure_dhcp_hostname(
            &mut dhcp,
            configured_name,
            &DHCP_HOSTNAME,
            &DHCP_OPTIONS,
        );
        esp_println::println!("WiFi: DHCP hostname=\"{}\"", dhcp_hostname);
        let dhcp_handle = sockets.add(dhcp);

        // Listening TCP socket pool.
        let mut http_handles: [Option<SocketHandle>; HTTP_SOCKET_COUNT] = [None; HTTP_SOCKET_COUNT];
        for (index, (rx_buffer, tx_buffer)) in tcp_rx.iter_mut().zip(tcp_tx.iter_mut()).enumerate()
        {
            let socket = new_listen_socket(rx_buffer, tx_buffer);
            http_handles[index] = Some(sockets.add(socket));
        }
        let http_handles = http_handles.map(|h| h.expect("socket pool fully populated above"));

        Self {
            device,
            interface,
            sockets,
            dhcp_handle,
            http_handles,
            request_buffers,
            request_lens: [0; HTTP_SOCKET_COUNT],
            socket_active_since_ms: [None; HTTP_SOCKET_COUNT],
            port,
            endpoint: None,
            dhcp_started_ms: now_ms,
            last_http_success_ms: None,
            send_error_total: 0,
            pending_control: None,
            ota_upload: None,
        }
    }

    /// Run the DHCP client until a lease lands or the budget is
    /// exhausted. On success, the interface gains its IPv4 + default
    /// route and `self.endpoint()` becomes `Some(_)`. Stalls the
    /// caller for up to ~40 s — call only once at startup.
    pub fn acquire_dhcp(&mut self, now_ms_at_call: u64) -> wifi_common::DhcpOutcome
    {
        self.dhcp_started_ms = now_ms_at_call;
        let outcome = wifi_common::run_dhcp_blocking(
            &mut self.interface,
            &mut self.device,
            &mut self.sockets,
            self.dhcp_handle,
            now_ms_at_call,
            wifi_common::DHCP_DEFAULT_TIMEOUT_POLLS,
        );
        if let wifi_common::DhcpOutcome::Acquired { ip, .. } = outcome {
            self.endpoint = Some(HttpEndpoint::new(ip, self.port));
        }
        outcome
    }

    /// Clear the IPv4 configuration and recycle every DHCP/TCP socket,
    /// while keeping the owned WifiDevice and smoltcp allocation alive.
    pub fn reset_network(&mut self)
    {
        self.interface.update_ip_addrs(|addrs| addrs.clear());
        let _ = self.interface.routes_mut().remove_default_ipv4_route();
        self.endpoint = None;
        self.reset_dhcp_socket();
        self.reset_http_sockets();
        crate::telemetry_state::record_dhcp_deconfigured();
    }

    /// Reset DHCP/TCP state and block until DHCP returns a fresh lease or
    /// the shared DHCP budget expires.
    pub fn reacquire_dhcp(&mut self, now_ms_at_call: u64) -> wifi_common::DhcpOutcome
    {
        self.reset_network();
        self.acquire_dhcp(now_ms_at_call)
    }

    /// Resolved listen endpoint, available after a successful DHCP
    /// lease. `None` before DHCP completes or after the lease is
    /// dropped (TODO: handle lease loss / re-acquire).
    pub const fn endpoint(&self) -> Option<HttpEndpoint>
    {
        self.endpoint
    }

    /// Last successful HTTP response timestamp, if any client has made a
    /// complete request since this server was created.
    pub const fn last_http_success_ms(&self) -> Option<u64>
    {
        self.last_http_success_ms
    }

    /// Total response send errors observed by this server instance.
    pub const fn send_error_total(&self) -> u32
    {
        self.send_error_total
    }

    /// Take the next validated HTTP control command, if a handler queued one.
    pub fn take_control_command(&mut self) -> Option<HttpControlCommand>
    {
        self.pending_control.take()
    }

    /// Current OTA upload progress for UI rendering.
    pub fn ota_upload_snapshot(&self) -> Option<OtaUploadSnapshot>
    {
        self.ota_upload.as_ref().map(|session| OtaUploadSnapshot {
            received_bytes: session.received,
            total_bytes:    session.content_len,
        })
    }

    /// One pass: drive the smoltcp stack, ensure all sockets are
    /// listening, and handle any complete inbound HTTP requests.
    /// Non-blocking — returns immediately when no sockets have data.
    pub fn poll(
        &mut self,
        now_ms: u64,
        config: &GatewayConfig<MAX_TELEMETRY_PRODUCERS>,
        tx_power: TxPowerMapping,
    ) -> Result<(), HttpError>
    {
        let _ = self.interface.poll(
            SmoltcpInstant::from_millis(now_ms as i64),
            &mut self.device,
            &mut self.sockets,
        );
        self.poll_dhcp_events(now_ms);
        self.poll_ota_upload(now_ms)?;

        self.recycle_stale_sockets(now_ms);
        self.ensure_listening()?;

        // Iterate over a snapshot of handles to avoid double-borrow
        // when calling response writers that also need `&mut self`.
        let handles = self.http_handles;
        for (index, tcp_handle) in handles.iter().enumerate() {
            if self
                .ota_upload
                .as_ref()
                .is_some_and(|session| session.tcp_handle == *tcp_handle)
            {
                continue;
            }

            let request_len = {
                let socket = self.sockets.get_mut::<tcp::Socket>(*tcp_handle);
                if !socket.may_recv() {
                    continue;
                }
                let buffer = &mut self.request_buffers[index];
                let cap = buffer.len() - self.request_lens[index];
                if cap == 0 {
                    None
                } else {
                    let stage = &mut buffer[self.request_lens[index]..];
                    match socket.recv_slice(stage) {
                        Ok(0) => None,
                        Ok(n) => {
                            self.request_lens[index] += n;
                            if http_metrics::request_headers_complete(
                                &buffer[..self.request_lens[index]],
                            ) {
                                Some(self.request_lens[index])
                            } else {
                                None
                            }
                        },
                        Err(_) => None,
                    }
                }
            };

            let Some(ready_len) = request_len else {
                continue;
            };

            self.socket_active_since_ms[index] = Some(now_ms);
            crate::telemetry_state::record_http_request(now_ms);
            // Snapshot the request bytes so we don't keep the borrow
            // on `self.request_buffers` alive across the response.
            let mut request_copy = [0u8; HTTP_REQUEST_BUFFER_BYTES];
            request_copy[..ready_len].copy_from_slice(&self.request_buffers[index][..ready_len]);
            self.request_lens[index] = 0;

            let response_result = self.handle_request(
                *tcp_handle,
                &request_copy[..ready_len],
                config,
                tx_power,
                now_ms,
            );
            if let Err(error) = response_result {
                esp_println::println!("HTTP: socket[{}] failed ({:?})", index, error);
                if matches!(error, HttpError::Send) {
                    self.send_error_total = self.send_error_total.saturating_add(1);
                    crate::telemetry_state::record_http_send_error();
                }
                let socket = self.sockets.get_mut::<tcp::Socket>(*tcp_handle);
                socket.abort();
                self.socket_active_since_ms[index] = None;
            } else {
                self.last_http_success_ms = Some(now_ms);
                crate::telemetry_state::record_http_success(now_ms);
                let socket = self.sockets.get_mut::<tcp::Socket>(*tcp_handle);
                self.socket_active_since_ms[index] = if socket.is_open() || socket.is_active() {
                    Some(now_ms)
                } else {
                    None
                };
            }
        }

        Ok(())
    }

    fn poll_dhcp_events(&mut self, now_ms: u64)
    {
        match self
            .sockets
            .get_mut::<dhcpv4::Socket>(self.dhcp_handle)
            .poll()
        {
            Some(dhcpv4::Event::Configured(config)) => {
                self.interface.update_ip_addrs(|addrs| {
                    addrs.clear();
                    let _ = addrs.push(IpCidr::Ipv4(config.address));
                });

                let _ = self.interface.routes_mut().remove_default_ipv4_route();
                if let Some(router) = config.router {
                    let _ = self.interface.routes_mut().add_default_ipv4_route(router);
                }

                let ip = config.address.address().octets();
                let gateway = config.router.map(|router| router.octets());
                let endpoint = HttpEndpoint::new(ip, self.port);
                let acquire_ms = now_ms
                    .saturating_sub(self.dhcp_started_ms)
                    .min(u64::from(u32::MAX)) as u32;
                crate::telemetry_state::record_dhcp_configured(now_ms, acquire_ms, ip, gateway);
                if self.endpoint != Some(endpoint) {
                    esp_println::println!(
                        "DHCP: lease configured {}.{}.{}.{} during poll",
                        ip[0],
                        ip[1],
                        ip[2],
                        ip[3],
                    );
                    self.reset_http_sockets();
                }
                self.endpoint = Some(endpoint);
            },
            Some(dhcpv4::Event::Deconfigured) => {
                esp_println::println!("DHCP: lease deconfigured during poll");
                self.interface.update_ip_addrs(|addrs| addrs.clear());
                let _ = self.interface.routes_mut().remove_default_ipv4_route();
                self.endpoint = None;
                self.reset_http_sockets();
                crate::telemetry_state::record_dhcp_deconfigured();
            },
            None => {},
        }
    }

    fn ensure_listening(&mut self) -> Result<(), HttpError>
    {
        for handle in &self.http_handles {
            let socket = self.sockets.get_mut::<tcp::Socket>(*handle);
            if !socket.is_open() {
                socket.listen(self.port).map_err(|_| HttpError::Listen)?;
            }
        }
        Ok(())
    }

    fn recycle_stale_sockets(&mut self, now_ms: u64)
    {
        for (index, handle) in self.http_handles.iter().enumerate() {
            if self
                .ota_upload
                .as_ref()
                .is_some_and(|session| session.tcp_handle == *handle)
            {
                self.socket_active_since_ms[index] = Some(now_ms);
                continue;
            }
            let Some(active_since_ms) = self.socket_active_since_ms[index] else {
                continue;
            };
            if now_ms.saturating_sub(active_since_ms) < HTTP_SOCKET_STALL_MS {
                continue;
            }
            let socket = self.sockets.get_mut::<tcp::Socket>(*handle);
            if socket.is_open() || socket.is_active() {
                socket.abort();
            }
            self.request_lens[index] = 0;
            self.socket_active_since_ms[index] = None;
        }
    }

    fn reset_dhcp_socket(&mut self)
    {
        self.sockets
            .get_mut::<dhcpv4::Socket>(self.dhcp_handle)
            .reset();
        crate::telemetry_state::record_dhcp_reset();
    }

    fn reset_http_sockets(&mut self)
    {
        self.request_lens.fill(0);
        self.socket_active_since_ms.fill(None);
        let mut aborted = 0_u8;
        for handle in &self.http_handles {
            let socket = self.sockets.get_mut::<tcp::Socket>(*handle);
            if socket.is_open() || socket.is_active() {
                aborted = aborted.saturating_add(1);
            }
            socket.abort();
        }
        crate::telemetry_state::record_http_socket_aborts(aborted);
    }

    fn handle_request(
        &mut self,
        tcp_handle: SocketHandle,
        request: &[u8],
        config: &GatewayConfig<MAX_TELEMETRY_PRODUCERS>,
        tx_power: TxPowerMapping,
        now_ms: u64,
    ) -> Result<(), HttpError>
    {
        if !request_is_authorized(request, config) {
            let response = http_metrics::build_response(
                http_metrics::HttpRoute::Unauthorized,
                now_ms,
                tx_power,
            )
            .map_err(|_| HttpError::Render)?;
            return self.send_response_bytes(
                tcp_handle,
                response.status,
                response.content_type,
                response.body.as_bytes(),
                now_ms,
            );
        }

        if method_path_is(request, b"POST", b"/config") {
            return self.handle_config_update(tcp_handle, request, now_ms);
        }

        if method_path_is(request, b"POST", b"/time") {
            return self.handle_time_sync(tcp_handle, request, config.time, now_ms);
        }

        if method_path_is(request, b"POST", b"/ota") {
            if content_length(request).unwrap_or(0) > 0 || parse_ota_crc32(request).is_some() {
                return self.handle_ota_upload_start(tcp_handle, request, now_ms);
            }
            return self.handle_ota_control(
                tcp_handle,
                HttpControlCommand::OtaRequested,
                b"ota requested\n",
                now_ms,
            );
        }

        if method_path_is(request, b"POST", b"/ota/start") {
            return self.handle_ota_control(
                tcp_handle,
                HttpControlCommand::OtaRequested,
                b"ota requested\n",
                now_ms,
            );
        }

        if method_path_is(request, b"POST", b"/ota/done")
            || method_path_is(request, b"POST", b"/ota/finish")
            || method_path_is(request, b"POST", b"/ota/complete")
        {
            return self.handle_ota_control(
                tcp_handle,
                HttpControlCommand::OtaFinished,
                b"ota done\n",
                now_ms,
            );
        }

        let route = http_metrics::route_request(request);
        let response =
            http_metrics::build_response(route, now_ms, tx_power).map_err(|_| HttpError::Render)?;
        self.send_response_bytes(
            tcp_handle,
            response.status,
            response.content_type,
            response.body.as_bytes(),
            now_ms,
        )
    }

    fn handle_config_update(
        &mut self,
        tcp_handle: SocketHandle,
        request: &[u8],
        now_ms: u64,
    ) -> Result<(), HttpError>
    {
        if self.pending_control.is_some() {
            return self.send_text_response(
                tcp_handle,
                "409 Conflict",
                b"control command already pending\n",
                now_ms,
            );
        }

        let Some(content_len) = content_length(request) else {
            return self.send_text_response(
                tcp_handle,
                "411 Length Required",
                b"missing content-length\n",
                now_ms,
            );
        };
        if content_len == 0 {
            return self.send_text_response(
                tcp_handle,
                "400 Bad Request",
                b"empty config body\n",
                now_ms,
            );
        }
        if content_len > USB_CONFIG_JSON_BYTES {
            return self.send_text_response(
                tcp_handle,
                "413 Payload Too Large",
                b"config body too large\n",
                now_ms,
            );
        }

        let body = self.read_body(tcp_handle, request, content_len, now_ms)?;
        let Ok(text) = core::str::from_utf8(body.as_slice()) else {
            return self.send_text_response(
                tcp_handle,
                "400 Bad Request",
                b"config body must be utf-8\n",
                now_ms,
            );
        };
        let command = provisioning::parse_usb_document(text);
        let config = match command {
            Ok(ProvisioningCommand::SetConfig(config)) => config,
            Ok(_) => {
                return self.send_text_response(
                    tcp_handle,
                    "400 Bad Request",
                    b"body must be a gateway config or set_config command\n",
                    now_ms,
                );
            },
            Err(_) => {
                return self.send_text_response(
                    tcp_handle,
                    "400 Bad Request",
                    b"invalid config\n",
                    now_ms,
                );
            },
        };

        let mut document = AllocString::new();
        document
            .try_reserve_exact(text.len())
            .map_err(|_| HttpError::Render)?;
        document.push_str(text);
        self.pending_control = Some(HttpControlCommand::SetConfig { document, config });
        self.send_text_response(tcp_handle, "202 Accepted", b"config accepted\n", now_ms)
    }

    fn handle_time_sync(
        &mut self,
        tcp_handle: SocketHandle,
        request: &[u8],
        current_settings: TimeSettings,
        now_ms: u64,
    ) -> Result<(), HttpError>
    {
        if self.pending_control.is_some() {
            return self.send_text_response(
                tcp_handle,
                "409 Conflict",
                b"control command already pending\n",
                now_ms,
            );
        }

        let content_len = content_length(request).unwrap_or(0);
        if content_len > HTTP_TIME_BODY_BYTES {
            return self.send_text_response(
                tcp_handle,
                "413 Payload Too Large",
                b"time body too large\n",
                now_ms,
            );
        }

        let mut settings = TimeSettings {
            utc_offset_minutes: current_settings.utc_offset_minutes,
            unix_time_seconds:  None,
        };
        let mut seen_time_input = false;
        if let Some(query) = request_query_for(request, b"POST", b"/time") {
            let Some(query_settings) = parse_time_query(query, settings.utc_offset_minutes) else {
                return self.send_text_response(
                    tcp_handle,
                    "400 Bad Request",
                    b"invalid time query\n",
                    now_ms,
                );
            };
            settings = query_settings;
            seen_time_input = true;
        }

        if content_len > 0 {
            let body = self.read_body(tcp_handle, request, content_len, now_ms)?;
            let Some(body_settings) = parse_time_json(body.as_slice(), settings.utc_offset_minutes)
            else {
                return self.send_text_response(
                    tcp_handle,
                    "400 Bad Request",
                    b"invalid time body\n",
                    now_ms,
                );
            };
            settings = body_settings;
            seen_time_input = true;
        }

        if !seen_time_input || !settings.is_valid() {
            return self.send_text_response(
                tcp_handle,
                "400 Bad Request",
                b"missing or invalid time settings\n",
                now_ms,
            );
        }

        self.pending_control = Some(HttpControlCommand::SyncTime(settings));
        self.send_text_response(tcp_handle, "202 Accepted", b"time accepted\n", now_ms)
    }

    fn handle_ota_control(
        &mut self,
        tcp_handle: SocketHandle,
        command: HttpControlCommand,
        accepted_body: &'static [u8],
        now_ms: u64,
    ) -> Result<(), HttpError>
    {
        if self.pending_control.is_some() {
            return self.send_text_response(
                tcp_handle,
                "409 Conflict",
                b"control command already pending\n",
                now_ms,
            );
        }
        self.pending_control = Some(command);
        self.send_text_response(tcp_handle, "202 Accepted", accepted_body, now_ms)
    }

    fn handle_ota_upload_start(
        &mut self,
        tcp_handle: SocketHandle,
        request: &[u8],
        now_ms: u64,
    ) -> Result<(), HttpError>
    {
        if self.ota_upload.is_some() {
            return self.send_text_response(
                tcp_handle,
                "409 Conflict",
                b"ota upload already active\n",
                now_ms,
            );
        }
        if self.pending_control.is_some() {
            return self.send_text_response(
                tcp_handle,
                "409 Conflict",
                b"control command already pending\n",
                now_ms,
            );
        }

        let Some(content_len) = content_length(request) else {
            return self.send_text_response(
                tcp_handle,
                "411 Length Required",
                b"missing content-length\n",
                now_ms,
            );
        };
        if content_len == 0 {
            return self.send_text_response(
                tcp_handle,
                "400 Bad Request",
                b"empty ota body\n",
                now_ms,
            );
        }
        if content_len > u32::MAX as usize {
            return self.send_text_response(
                tcp_handle,
                "413 Payload Too Large",
                b"ota body too large\n",
                now_ms,
            );
        }
        let Some(target_crc) = parse_ota_crc32(request) else {
            return self.send_text_response(
                tcp_handle,
                "400 Bad Request",
                b"missing crc32 query or x-ota-crc32 header\n",
                now_ms,
            );
        };

        let session = match OtaUploadSession::new(tcp_handle, content_len as u32, target_crc) {
            Ok(session) => session,
            Err(error) => {
                esp_println::println!("OTA: refused upload ({:?})", error);
                return self.send_text_response(
                    tcp_handle,
                    "500 Internal Server Error",
                    ota_error_response_body(&error),
                    now_ms,
                );
            },
        };

        esp_println::println!(
            "OTA: HTTP upload started bytes={} crc=0x{:08x}",
            content_len,
            target_crc,
        );
        self.ota_upload = Some(session);
        if self.pending_control.is_none() {
            self.pending_control = Some(HttpControlCommand::OtaRequested);
        }

        if request_has_expect_continue(request) {
            self.send_all(tcp_handle, b"HTTP/1.1 100 Continue\r\n\r\n", now_ms)?;
        }

        let body_start = header_end_index(request).unwrap_or(request.len());
        if body_start < request.len() {
            let initial_len = min(content_len, request.len() - body_start);
            if initial_len > 0
                && let Err(error) =
                    self.stage_ota_upload_bytes(&request[body_start..body_start + initial_len])
            {
                return self.abort_ota_upload(tcp_handle, error, now_ms);
            }
        }

        if self
            .ota_upload
            .as_ref()
            .is_some_and(|session| session.is_complete())
        {
            return self.finish_ota_upload(now_ms);
        }

        Ok(())
    }

    fn poll_ota_upload(&mut self, now_ms: u64) -> Result<(), HttpError>
    {
        let Some(tcp_handle) = self.ota_upload.as_ref().map(|session| session.tcp_handle) else {
            return Ok(());
        };

        let mut scratch = [0u8; HTTP_TCP_RX_BUFFER_BYTES];
        let mut chunks = 0_usize;
        while chunks < HTTP_OTA_MAX_CHUNKS_PER_POLL {
            let want = self
                .ota_upload
                .as_ref()
                .map(|session| min(session.remaining_len(), scratch.len()))
                .unwrap_or(0);
            if want == 0 {
                return self.finish_ota_upload(now_ms);
            }

            let socket = self.sockets.get_mut::<tcp::Socket>(tcp_handle);
            if !socket.may_recv() {
                return self.abort_ota_upload(tcp_handle, OtaUploadError::ConnectionClosed, now_ms);
            }

            match socket.recv_slice(&mut scratch[..want]) {
                Ok(0) => break,
                Ok(n) => {
                    chunks = chunks.saturating_add(1);
                    if let Err(error) = self.stage_ota_upload_bytes(&scratch[..n]) {
                        return self.abort_ota_upload(tcp_handle, error, now_ms);
                    }
                    let _ = self.interface.poll(
                        SmoltcpInstant::from_millis(now_ms as i64),
                        &mut self.device,
                        &mut self.sockets,
                    );
                },
                Err(_) => {
                    return self.abort_ota_upload(
                        tcp_handle,
                        OtaUploadError::ConnectionClosed,
                        now_ms,
                    );
                },
            }
        }

        if self
            .ota_upload
            .as_ref()
            .is_some_and(|session| session.is_complete())
        {
            self.finish_ota_upload(now_ms)?;
        }
        Ok(())
    }

    fn stage_ota_upload_bytes(&mut self, bytes: &[u8]) -> Result<(), OtaUploadError>
    {
        let Some(session) = self.ota_upload.as_mut() else {
            return Err(OtaUploadError::NoSession);
        };
        session.stage_bytes(bytes)?;
        while session.received >= session.next_log_bytes
            && session.next_log_bytes <= session.content_len
        {
            esp_println::println!(
                "OTA: received {}/{} bytes",
                session.received,
                session.content_len,
            );
            let next_log_bytes = session
                .next_log_bytes
                .saturating_add(HTTP_OTA_PROGRESS_LOG_BYTES as u32);
            if next_log_bytes <= session.next_log_bytes {
                break;
            }
            session.next_log_bytes = next_log_bytes;
        }
        Ok(())
    }

    fn finish_ota_upload(&mut self, now_ms: u64) -> Result<(), HttpError>
    {
        let Some(mut session) = self.ota_upload.take() else {
            return Ok(());
        };
        let tcp_handle = session.tcp_handle;
        match session.finish() {
            Ok(()) => {
                esp_println::println!(
                    "OTA: verified {} bytes; boot target updated; rebooting",
                    session.content_len,
                );
                if self.pending_control.is_none() {
                    self.pending_control = Some(HttpControlCommand::OtaFinished);
                }
                let send_result = self.send_text_response(tcp_handle, "202 Accepted", b"", now_ms);
                if let Err(error) = send_result {
                    esp_println::println!("OTA: response send failed before reboot ({:?})", error);
                }
                self.drain_tcp_before_reset(tcp_handle, now_ms, HTTP_OTA_REBOOT_DRAIN_MS);
                esp_rom_sys::rom::software_reset()
            },
            Err(error) => self.abort_ota_upload(tcp_handle, error, now_ms),
        }
    }

    fn drain_tcp_before_reset(&mut self, tcp_handle: SocketHandle, now_ms: u64, drain_ms: u32)
    {
        let delay = Delay::new();
        let mut clock_ms = now_ms;
        for _ in 0..drain_ms {
            let _ = self.interface.poll(
                SmoltcpInstant::from_millis(clock_ms as i64),
                &mut self.device,
                &mut self.sockets,
            );
            let socket = self.sockets.get_mut::<tcp::Socket>(tcp_handle);
            if socket.send_queue() == 0
                && matches!(
                    socket.state(),
                    tcp::State::Closed | tcp::State::TimeWait | tcp::State::FinWait2
                )
            {
                break;
            }
            delay.delay_millis(1);
            clock_ms = clock_ms.saturating_add(1);
        }
    }

    fn abort_ota_upload(
        &mut self,
        tcp_handle: SocketHandle,
        error: OtaUploadError,
        now_ms: u64,
    ) -> Result<(), HttpError>
    {
        self.ota_upload = None;
        esp_println::println!("OTA: upload failed ({:?})", error);
        self.pending_control = Some(HttpControlCommand::OtaFinished);
        self.send_text_response(
            tcp_handle,
            "500 Internal Server Error",
            ota_error_response_body(&error),
            now_ms,
        )
    }

    fn read_body(
        &mut self,
        tcp_handle: SocketHandle,
        request: &[u8],
        content_len: usize,
        now_ms: u64,
    ) -> Result<AllocVec<u8>, HttpError>
    {
        if request_has_expect_continue(request) {
            self.send_all(tcp_handle, b"HTTP/1.1 100 Continue\r\n\r\n", now_ms)?;
        }

        let mut body = AllocVec::new();
        body.try_reserve_exact(content_len)
            .map_err(|_| HttpError::Render)?;

        let body_start = header_end_index(request).unwrap_or(request.len());
        if body_start < request.len() {
            let initial_len = min(content_len, request.len() - body_start);
            body.extend_from_slice(&request[body_start..body_start + initial_len]);
        }

        let delay = Delay::new();
        let mut idle_polls = 0_usize;
        let mut clock_ms = now_ms;
        let mut scratch = [0u8; HTTP_TCP_RX_BUFFER_BYTES];
        while body.len() < content_len {
            let want = min(content_len - body.len(), scratch.len());
            let socket = self.sockets.get_mut::<tcp::Socket>(tcp_handle);
            if !socket.may_recv() {
                return Err(HttpError::Send);
            }
            match socket.recv_slice(&mut scratch[..want]) {
                Ok(0) => {
                    idle_polls = idle_polls.saturating_add(1);
                    if idle_polls > HTTP_BODY_IDLE_POLLS {
                        return Err(HttpError::Send);
                    }
                    delay.delay_millis(1);
                    clock_ms = clock_ms.saturating_add(1);
                    let _ = self.interface.poll(
                        SmoltcpInstant::from_millis(clock_ms as i64),
                        &mut self.device,
                        &mut self.sockets,
                    );
                },
                Ok(n) => {
                    body.extend_from_slice(&scratch[..n]);
                    idle_polls = 0;
                    let _ = self.interface.poll(
                        SmoltcpInstant::from_millis(clock_ms as i64),
                        &mut self.device,
                        &mut self.sockets,
                    );
                },
                Err(_) => return Err(HttpError::Send),
            }
        }

        Ok(body)
    }

    fn send_text_response(
        &mut self,
        tcp_handle: SocketHandle,
        status: &'static str,
        body: &[u8],
        now_ms: u64,
    ) -> Result<(), HttpError>
    {
        self.send_response_bytes(
            tcp_handle,
            status,
            "text/plain; charset=utf-8",
            body,
            now_ms,
        )
    }

    fn send_response_bytes(
        &mut self,
        tcp_handle: SocketHandle,
        status: &'static str,
        content_type: &'static str,
        body: &[u8],
        now_ms: u64,
    ) -> Result<(), HttpError>
    {
        let mut header: heapless::String<256> = heapless::String::new();
        write!(
            header,
            "HTTP/1.1 {}\r\nContent-Type: {}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            status,
            content_type,
            body.len(),
        )
        .map_err(|_| HttpError::Render)?;
        self.send_all(tcp_handle, header.as_bytes(), now_ms)?;
        self.send_all(tcp_handle, body, now_ms)?;
        let socket = self.sockets.get_mut::<tcp::Socket>(tcp_handle);
        socket.close();
        // Drive smoltcp once more so the closing FIN + remaining tx
        // buffer actually go out on the wire before we return.
        let _ = self.interface.poll(
            SmoltcpInstant::from_millis(now_ms as i64),
            &mut self.device,
            &mut self.sockets,
        );
        Ok(())
    }

    /// Stream `payload` to the given TCP socket, interleaving
    /// `interface.poll` calls so the bytes actually leave the device.
    /// Without the interleaved poll, `send_slice` just stages bytes in
    /// the TX buffer and they never make it onto the wire.
    fn send_all(
        &mut self,
        tcp_handle: SocketHandle,
        mut payload: &[u8],
        now_ms: u64,
    ) -> Result<(), HttpError>
    {
        let delay = Delay::new();
        let mut idle_polls = 0_usize;
        let mut clock_ms = now_ms;
        while !payload.is_empty() {
            let socket = self.sockets.get_mut::<tcp::Socket>(tcp_handle);
            if !socket.may_send() {
                return Err(HttpError::Send);
            }
            match socket.send_slice(payload) {
                Ok(0) => {
                    idle_polls = idle_polls.saturating_add(1);
                    if idle_polls > HTTP_SEND_IDLE_POLLS {
                        return Err(HttpError::Send);
                    }
                    delay.delay_millis(1);
                    clock_ms = clock_ms.saturating_add(1);
                    let _ = self.interface.poll(
                        SmoltcpInstant::from_millis(clock_ms as i64),
                        &mut self.device,
                        &mut self.sockets,
                    );
                },
                Ok(n) => {
                    payload = &payload[min(n, payload.len())..];
                    idle_polls = 0;
                    let _ = self.interface.poll(
                        SmoltcpInstant::from_millis(clock_ms as i64),
                        &mut self.device,
                        &mut self.sockets,
                    );
                },
                Err(_) => return Err(HttpError::Send),
            }
        }
        Ok(())
    }
}

fn new_listen_socket<'buffer>(
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

struct OtaUploadSession
{
    tcp_handle:     SocketHandle,
    ota:            Ota<FlashStorage>,
    write_buffer:   AllocVec<u8>,
    content_len:    u32,
    received:       u32,
    next_log_bytes: u32,
}

impl OtaUploadSession
{
    fn new(
        tcp_handle: SocketHandle,
        content_len: u32,
        target_crc: u32,
    ) -> Result<Self, OtaUploadError>
    {
        let mut ota = Ota::new(FlashStorage::new()).map_err(OtaUploadError::Ota)?;
        ota.ota_begin(content_len, target_crc)
            .map_err(OtaUploadError::Ota)?;

        let mut write_buffer = AllocVec::new();
        write_buffer
            .try_reserve_exact(HTTP_OTA_WRITE_BUFFER_BYTES)
            .map_err(|_| OtaUploadError::BufferAlloc)?;

        Ok(Self {
            tcp_handle,
            ota,
            write_buffer,
            content_len,
            received: 0,
            next_log_bytes: HTTP_OTA_PROGRESS_LOG_BYTES as u32,
        })
    }

    const fn remaining_len(&self) -> usize
    {
        self.content_len.saturating_sub(self.received) as usize
    }

    const fn is_complete(&self) -> bool
    {
        self.received >= self.content_len
    }

    fn stage_bytes(&mut self, mut bytes: &[u8]) -> Result<(), OtaUploadError>
    {
        if bytes.len() > self.remaining_len() {
            bytes = &bytes[..self.remaining_len()];
        }

        while !bytes.is_empty() {
            let space = HTTP_OTA_WRITE_BUFFER_BYTES.saturating_sub(self.write_buffer.len());
            if space == 0 {
                self.flush_buffer()?;
                continue;
            }
            let take = min(space, bytes.len());
            self.write_buffer.extend_from_slice(&bytes[..take]);
            self.received = self.received.saturating_add(take as u32);
            bytes = &bytes[take..];
            if self.write_buffer.len() == HTTP_OTA_WRITE_BUFFER_BYTES {
                self.flush_buffer()?;
            }
        }
        Ok(())
    }

    fn finish(&mut self) -> Result<(), OtaUploadError>
    {
        if !self.is_complete() {
            return Err(OtaUploadError::IncompleteBody);
        }
        self.flush_buffer()?;
        self.ota.ota_flush(true, false).map_err(OtaUploadError::Ota)
    }

    fn flush_buffer(&mut self) -> Result<(), OtaUploadError>
    {
        if self.write_buffer.is_empty() {
            return Ok(());
        }
        self.ota
            .ota_write_chunk(self.write_buffer.as_slice())
            .map_err(OtaUploadError::Ota)?;
        self.write_buffer.clear();
        Ok(())
    }
}

#[derive(Debug)]
enum OtaUploadError
{
    BufferAlloc,
    ConnectionClosed,
    IncompleteBody,
    NoSession,
    Ota(OtaError),
}

fn ota_error_response_body(error: &OtaUploadError) -> &'static [u8]
{
    match error {
        OtaUploadError::BufferAlloc => b"ota buffer allocation failed\n",
        OtaUploadError::ConnectionClosed => b"ota connection closed\n",
        OtaUploadError::IncompleteBody => b"ota body incomplete\n",
        OtaUploadError::NoSession => b"ota session missing\n",
        OtaUploadError::Ota(OtaError::OtaPartitionTooSmall) => b"ota partition too small\n",
        OtaUploadError::Ota(OtaError::NotEnoughPartitions) => b"ota partitions missing\n",
        OtaUploadError::Ota(OtaError::OtaNotStarted) => b"ota not started\n",
        OtaUploadError::Ota(OtaError::FlashRWError) => b"ota flash write failed\n",
        OtaUploadError::Ota(OtaError::WrongCRC) => b"ota crc mismatch\n",
        OtaUploadError::Ota(OtaError::WrongOTAPArtitionOrder) => b"ota partition order invalid\n",
        OtaUploadError::Ota(OtaError::OtaVerifyError) => b"ota verify failed\n",
        OtaUploadError::Ota(OtaError::CannotFindCurrentBootPartition) => {
            b"ota current boot partition unknown\n"
        },
    }
}

fn request_is_authorized(request: &[u8], config: &GatewayConfig<MAX_TELEMETRY_PRODUCERS>) -> bool
{
    let Some(http) = config.http.as_ref() else {
        return true;
    };
    if http.tokens.is_empty() {
        return true;
    }
    // Look for `Authorization: Bearer <token>` header — case-sensitive
    // match for simplicity (HTTP/1.1 allows case-insensitive header
    // names; tighten later if hosts misbehave).
    let prefix = b"Authorization: Bearer ";
    let Some(start) = find_subsequence(request, prefix) else {
        return false;
    };
    let after = &request[start + prefix.len()..];
    let end = after
        .iter()
        .position(|c| *c == b'\r' || *c == b'\n')
        .unwrap_or(after.len());
    let presented = &after[..end];
    http.tokens
        .iter()
        .any(|token| token.as_str().as_bytes() == presented)
}

fn method_path_is(request: &[u8], method: &[u8], path: &[u8]) -> bool
{
    request.starts_with(method)
        && matches!(request.get(method.len()), Some(b' '))
        && request
            .get(method.len() + 1..)
            .is_some_and(|suffix| suffix.starts_with(path))
        && matches!(
            request.get(method.len() + 1 + path.len()),
            Some(b' ' | b'?' | b'\r' | b'\n')
        )
}

fn request_query_for<'a>(request: &'a [u8], method: &[u8], path: &[u8]) -> Option<&'a [u8]>
{
    if !method_path_is(request, method, path) {
        return None;
    }
    let query_start = method.len() + 1 + path.len();
    if request.get(query_start) != Some(&b'?') {
        return None;
    }
    let query = &request[query_start + 1..];
    let query_end = query
        .iter()
        .position(|byte| matches!(byte, b' ' | b'\r' | b'\n'))
        .unwrap_or(query.len());
    Some(&query[..query_end])
}

fn parse_ota_crc32(request: &[u8]) -> Option<u32>
{
    header_value(request, b"x-ota-crc32")
        .and_then(parse_u32_auto)
        .or_else(|| {
            request_query_for(request, b"POST", b"/ota")
                .and_then(|query| {
                    query_value(query, b"crc32").or_else(|| query_value(query, b"crc"))
                })
                .and_then(parse_u32_auto)
        })
}

fn query_value<'a>(query: &'a [u8], key: &[u8]) -> Option<&'a [u8]>
{
    let mut remaining = query;
    while !remaining.is_empty() {
        let pair_end = remaining
            .iter()
            .position(|byte| *byte == b'&')
            .unwrap_or(remaining.len());
        let pair = &remaining[..pair_end];
        if let Some(eq) = pair.iter().position(|byte| *byte == b'=') {
            let name = &pair[..eq];
            if name == key {
                return Some(&pair[eq + 1..]);
            }
        }
        if pair_end == remaining.len() {
            break;
        }
        remaining = &remaining[pair_end + 1..];
    }
    None
}

fn parse_u32_auto(value: &[u8]) -> Option<u32>
{
    let value = ascii_trim(value);
    let (digits, radix) = if value.starts_with(b"0x") || value.starts_with(b"0X") {
        (&value[2..], 16)
    } else if value
        .iter()
        .any(|byte| matches!(*byte, b'a'..=b'f' | b'A'..=b'F'))
    {
        (value, 16)
    } else {
        (value, 10)
    };
    parse_u32_radix(digits, radix)
}

fn parse_u32_radix(digits: &[u8], radix: u32) -> Option<u32>
{
    if digits.is_empty() {
        return None;
    }
    let mut value = 0_u32;
    for byte in digits {
        let digit = match *byte {
            b'0'..=b'9' => u32::from(*byte - b'0'),
            b'a'..=b'f' => u32::from(*byte - b'a' + 10),
            b'A'..=b'F' => u32::from(*byte - b'A' + 10),
            _ => return None,
        };
        if digit >= radix {
            return None;
        }
        value = value.checked_mul(radix)?.checked_add(digit)?;
    }
    Some(value)
}

fn content_length(request: &[u8]) -> Option<usize>
{
    let value = header_value(request, b"content-length")?;
    let digits_end = value
        .iter()
        .position(|byte| !byte.is_ascii_digit())
        .unwrap_or(value.len());
    if digits_end == 0 {
        return None;
    }
    core::str::from_utf8(&value[..digits_end])
        .ok()?
        .parse()
        .ok()
}

fn request_has_expect_continue(request: &[u8]) -> bool
{
    header_value(request, b"expect")
        .is_some_and(|value| ascii_trim(value).eq_ignore_ascii_case(b"100-continue"))
}

fn header_value<'a>(request: &'a [u8], name: &[u8]) -> Option<&'a [u8]>
{
    let header_end = header_end_index(request).unwrap_or(request.len());
    let mut remaining = &request[..header_end];
    while !remaining.is_empty() {
        let line_end = remaining
            .iter()
            .position(|byte| *byte == b'\n')
            .unwrap_or(remaining.len());
        let mut line = &remaining[..line_end];
        if line.ends_with(b"\r") {
            line = &line[..line.len() - 1];
        }
        if let Some(colon) = line.iter().position(|byte| *byte == b':') {
            let header_name = ascii_trim(&line[..colon]);
            if header_name.eq_ignore_ascii_case(name) {
                return Some(ascii_trim(&line[colon + 1..]));
            }
        }
        if line_end == remaining.len() {
            break;
        }
        remaining = &remaining[line_end + 1..];
    }
    None
}

fn ascii_trim(mut input: &[u8]) -> &[u8]
{
    while let Some((first, tail)) = input.split_first()
        && first.is_ascii_whitespace()
    {
        input = tail;
    }
    while let Some((last, head)) = input.split_last()
        && last.is_ascii_whitespace()
    {
        input = head;
    }
    input
}

fn header_end_index(request: &[u8]) -> Option<usize>
{
    find_subsequence(request, b"\r\n\r\n")
        .map(|index| index + 4)
        .or_else(|| find_subsequence(request, b"\n\n").map(|index| index + 2))
}

fn parse_time_query(query: &[u8], default_offset_minutes: i16) -> Option<TimeSettings>
{
    let query = core::str::from_utf8(query).ok()?;
    let mut settings = TimeSettings {
        utc_offset_minutes: default_offset_minutes,
        unix_time_seconds:  None,
    };
    let mut saw_field = false;

    for pair in query.split('&') {
        if pair.is_empty() {
            continue;
        }
        let mut parts = pair.splitn(2, '=');
        let key = parts.next()?;
        let value = parts.next().unwrap_or_default();
        match key {
            "unix" | "unix_time_seconds" => {
                settings.unix_time_seconds = Some(value.parse::<u64>().ok()?);
                saw_field = true;
            },
            "offset" | "utc_offset_minutes" => {
                settings.utc_offset_minutes = value.parse::<i16>().ok()?;
                saw_field = true;
            },
            _ => {},
        }
    }

    (saw_field && settings.is_valid()).then_some(settings)
}

fn parse_time_json(body: &[u8], default_offset_minutes: i16) -> Option<TimeSettings>
{
    let text = core::str::from_utf8(body).ok()?;
    let wire: WireHttpTimeSync = serde_json::from_str(text).ok()?;
    let unix_time_seconds = wire.unix_time_seconds.or(wire.unix);
    let utc_offset_minutes = wire
        .utc_offset_minutes
        .or(wire.offset)
        .unwrap_or(default_offset_minutes);
    let settings = TimeSettings {
        utc_offset_minutes,
        unix_time_seconds,
    };
    (settings.is_valid()
        && (settings.unix_time_seconds.is_some()
            || wire.utc_offset_minutes.is_some()
            || wire.offset.is_some()))
    .then_some(settings)
}

#[derive(Debug, Deserialize)]
struct WireHttpTimeSync
{
    unix_time_seconds:  Option<u64>,
    unix:               Option<u64>,
    utc_offset_minutes: Option<i16>,
    offset:             Option<i16>,
}

fn find_subsequence(haystack: &[u8], needle: &[u8]) -> Option<usize>
{
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}
