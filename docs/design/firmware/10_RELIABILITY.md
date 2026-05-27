# Reliability Notes

Muninn Gate is built for unattended local telemetry. The reliability target is
not raw throughput; it is staying reachable on the LAN, preserving radio timing,
and recovering cleanly after WiFi, DHCP, TCP, or MeshCore edge cases.

## Availability Model

The source of truth for HTTP availability should be an external LAN probe that
requests `/metrics` every few seconds from the same network segment as normal
clients. Device-side counters explain why an outage happened, but they cannot
prove client-visible availability by themselves.

For a 99.9% monthly target, the total unavailable budget is about 43 minutes per
30 days. A single DHCP or reconnect event is acceptable; repeated long recovery
windows are not.

## WiFi Station Policy

ESP32-S3 WiFi is managed by `esp-wifi` in the ESP32 platform loop. The current
reliability baseline keeps WiFi, HTTP, smoltcp, scheduler, display, serial, and
LoRa service in one cooperative loop instead of running an independent APP-core
radio task beside the proprietary WiFi runtime.

- Power save is explicitly disabled with `PowerSaveMode::None`.
- ESP-IDF country configuration is reapplied after `esp_wifi_start()`, matching
  the stable Valhalla pattern for `esp-wifi 0.15.x`.
- WiFi TX power is requested through `esp_wifi_set_max_tx_power` and the
  applied value is read back. The current default is `80` quarter-dBm units
  (about 20 dBm) because field testing showed the lower 10 dBm setting could
  fail association on this AP.
- The applied TX power is read back and logged at boot/reconnect.
- Startup scans for the configured SSID, selects the strongest candidate, and
  pins the station config to that BSSID/channel when the AP accepts the pinned
  config. This avoids repeated all-channel scans on a fixed gateway.
- Connection attempts are retried at a controlled cadence rather than in a
  tight loop.
- Initial association failure is not terminal. Startup keeps retrying instead
  of entering `Status: error` for transient `AUTH_EXPIRE`, `CONNECTION_FAIL`,
  or `EspErrWifiConn` churn.
- When association does not complete for 45 seconds, the firmware performs a
  deeper WiFi cycle: disconnect, stop the controller, reapply station
  configuration, start the controller, reapply reliability settings, and retry.
- Disconnect reason codes are logged with symbolic names where known, including
  `2 = AUTH_EXPIRE`, `8 = ASSOC_LEAVE`, and `39 = TIMEOUT`.

The TX power setting is empirical, not a universal rule. Maximum ESP32 WiFi TX
power can improve association margin, but it also increases current spikes and
can destabilize marginal board power or RF conditions. On Heltec V4-class
boards, prefer the lowest WiFi TX power that keeps association and HTTP service
reliable, and always log the readback value.

## DHCP Recovery

DHCP is handled through smoltcp. The firmware keeps DHCP recovery aggressive but
bounded:

- The main wait loop polls networking every 25 ms while waiting for an address.
- DHCP discover and request retry timers are shortened for a local LAN.
- The DHCP socket is reset on WiFi reconnect and network reset.
- DHCP `Deconfigured` clears IPv4 state and tears down HTTP sockets.
- IP address changes tear down HTTP sockets before relistening.

This avoids the common failure where smoltcp remains in an old DHCP backoff
state after the WiFi link has already recovered.

## HTTP Socket Policy

smoltcp TCP sockets do not provide a listen backlog. A single listening socket
can make the whole server unavailable if a client stalls or disconnects in an
unhelpful TCP state. The ESP32 HTTP server therefore uses a small fixed socket
pool:

- `HTTP_SOCKET_COUNT = 16`
- `HTTP_TCP_RX_BUFFER_BYTES = 512`
- `HTTP_TCP_TX_BUFFER_BYTES = 1024`
- `HTTP_REQUEST_BUFFER_BYTES = 1024`
- `HTTP_CHUNK_BUFFER_BYTES = 2048`

The current pool favors availability over large per-socket buffers. Sixteen
sockets keep the listener resilient when Prometheus, manual `curl`, browsers,
and half-closed clients overlap. Per-socket buffers are intentionally small, and
the large arrays are initialized with `StaticCell::init_with` so they do not
create temporary stack copies during boot. The static HTTP RX/TX/request buffers
are about 40 KiB total.

Each socket:

- has a 5 second TCP timeout,
- has 5 second TCP keepalive,
- accumulates request bytes until HTTP headers are complete or the request
  buffer is full,
- closes after one response,
- is aborted on send stalls, malformed socket state, WiFi disconnect, DHCP loss,
  DHCP address change, or reconnect.

Metrics are streamed with HTTP chunked transfer. The firmware no longer tries to
render the complete Prometheus response into one fixed socket-sized buffer; this
keeps `/metrics` usable when producer channel data grows beyond a few kilobytes.

## HTTP And LAN Watchdog

The firmware now treats "WiFi reports connected but HTTP is no longer reachable"
as a recoverable data-path failure. This covers cases where the ESP32 remains
associated and continues LoRa polling, but peer LAN clients can no longer open
`/metrics`.

The watchdog observes:

- main-loop gaps longer than normal network polling,
- scheduler ticks that block the HTTP loop,
- active versus listening HTTP TCP sockets,
- accepted HTTP requests and completed responses,
- socket-pool exhaustion,
- long HTTP idle periods after the endpoint has already served at least one
  client.

Recovery actions:

- If an HTTP request reaches the firmware but the response cannot be sent, the
  firmware records `http_send_error` and aborts that TCP socket. Three send
  errors in a 120 second window are required before the watchdog treats the
  WiFi/IP data path as wedged and reconnects WiFi.
- If all HTTP sockets are active and no socket is listening for 30 seconds, the
  firmware records `socket_pool_stalled`, aborts TCP/DHCP state, disconnects
  WiFi, and reacquires DHCP.
- If the endpoint has previously served a client and then no HTTP request
  reaches the device for 10 minutes, the firmware records `http_idle`, aborts
  TCP/DHCP state, disconnects WiFi, and reacquires DHCP.
- Forced recoveries are rate-limited to at most once per minute.

The HTTP-idle watchdog is intentionally based on inbound HTTP activity. The
device cannot prove client-visible reachability by itself if no peer is probing
it. In production, keep an external scraper or prober hitting `/metrics`; then a
missing request stream is a useful signal that the LAN data path has wedged.

New metrics:

- `muninn_gate_http_requests_total`
- `muninn_gate_http_success_total`
- `muninn_gate_http_send_error_total`
- `muninn_gate_http_socket_abort_total`
- `muninn_gate_http_active_sockets`
- `muninn_gate_http_listening_sockets`
- `muninn_gate_http_sockets{state="..."}`
- `muninn_gate_http_oldest_socket_age_ms{state="..."}`
- `muninn_gate_http_last_request_age_ms`
- `muninn_gate_http_last_success_age_ms`
- `muninn_gate_wifi_connect_started_age_ms`
- `muninn_gate_wifi_connected_age_ms`
- `muninn_gate_wifi_connect_request_total`
- `muninn_gate_wifi_connect_request_error_total`
- `muninn_gate_dhcp_started_age_ms`
- `muninn_gate_dhcp_configured_age_ms`
- `muninn_gate_dhcp_last_acquire_ms`
- `muninn_gate_network_startup_to_serving_ms`
- `muninn_gate_network_recovery_total`
- `muninn_gate_smoltcp_poll_total`
- `muninn_gate_smoltcp_ms_since_last_poll`
- `muninn_gate_smoltcp_poll_gap_max_ms`
- `muninn_gate_smoltcp_poll_delay_miss_total`
- `muninn_gate_smoltcp_poll_bad_gap_total`
- `muninn_gate_lora_service_last_ms`
- `muninn_gate_lora_service_max_ms`
- `muninn_gate_http_service_last_ms`
- `muninn_gate_http_service_max_ms`
- `muninn_gate_main_loop_gap_last_ms`
- `muninn_gate_main_loop_gap_max_ms`
- `muninn_gate_scheduler_tick_last_ms`
- `muninn_gate_scheduler_tick_max_ms`

When the `serial-json` feature is enabled, serial JSON includes the same
counters using compact field names such as `http_requests_total`,
`network_recovery_total`, `main_loop_gap_max_ms`, and
`scheduler_tick_max_ms`. The default firmware build keeps telemetry JSON off;
boot/status logs still print over USB serial.

## HTTP Handler Boundaries

HTTP handlers must remain cheap:

- `/metrics` reads a telemetry snapshot and streams Prometheus text with
  `Transfer-Encoding: chunked`. Gateway and producer lines are emitted through a
  bounded staging buffer so large sensor/channel output does not require one
  monolithic response allocation.
- `/logs` renders retained diagnostics.
- `/poll` records an on-demand poll request and returns `202 Accepted`.
- HTTP must not directly poll producers or touch radio hardware.
- Authentication remains bearer-token based when `http.tokens` is non-empty.

The scheduler consumes `/poll` requests outside the socket handler. This keeps
HTTP work independent from LoRa timing and prevents slow radio operations from
holding TCP sockets open.

In the ESP32 HTTP loop, socket servicing runs before scheduled producer polling.
If any HTTP socket is still active after the HTTP pass, the scheduler pass is
deferred until the next loop. This keeps a due LoRa poll from jumping ahead of a
client request that has already reached smoltcp. A client that arrives while a
radio poll is already in progress is handled cooperatively: MeshCore response
wait loops call back into the WiFi/HTTP and radio service path every wait step.
This is the blocking-loop equivalent of Valhalla's dedicated async network
runner and keeps ICMP, TCP handshakes, `/metrics`, and LoRa RX/TX service alive
while the scheduler waits for login or telemetry replies.

The `muninn_gate_scheduler_tick_max_ms` and `muninn_gate_main_loop_gap_max_ms`
metrics can still show long scheduler duration, but that no longer means smoltcp
was completely unpolled for that entire interval.

## MeshCore Direct-Only TX

Outbound MeshCore packets are direct-only by firmware policy:

- Startup gateway adverts use direct route bits.
- Repeater login requests use direct route bits.
- Telemetry requests use direct route bits.
- Legacy `route` config strings are accepted, but normalized to direct.
- The ESP32 radio TX queue rejects non-direct MeshCore route headers before the
  frame can reach the SX126x.

Incoming parsing remains tolerant of flood, transport, path, and response
packets so the gateway can still observe nearby traffic and decode relevant
responses. The direct-only rule applies to what Muninn Gate transmits.

Remote nodes control their own replies. Muninn Gate requests companion and
repeater telemetry with direct route bits and never emits flood/path requests,
but a third-party repeater firmware could still choose to transmit its response
using its own policy. If that happens, fix the producer/repeater configuration
or firmware; the gateway-side guarantee is that Muninn Gate does not originate
flood traffic.

## AP And LAN Assumptions

Firmware recovery cannot compensate for an AP policy that intentionally blocks
LAN reachability. For reliability testing, use a deterministic IoT SSID:

- 2.4 GHz only.
- WPA2-Personal/AES for the ESP32.
- 20 MHz channel width on 2.4 GHz.
- Client isolation disabled when LAN clients must scrape `/metrics`.
- DHCP reservation for the gateway MAC when possible.
- Avoid roaming/band-steering features for the ESP32 SSID during stability
  testing.

The firmware should still reconnect after normal AP loss or DHCP renewal, but
network policy determines whether peer LAN hosts can reach the HTTP server.

## Crash Triage

If a panic occurs inside symbols such as `ieee80211_mgmt_output`,
`ieee80211_sta_new_state`, or `ieee80211_timer_do_process`, treat it as WiFi
driver state corruption or internal-RAM pressure until proven otherwise. First
checks:

1. Review recent static and stack buffer growth.
2. Check for large `StaticCell::init` temporaries.
3. Review HTTP socket/buffer counts and confirm all large buffers are static.
4. Confirm WiFi TX power cap readback is logged.
5. Compare whether the crash happens with HTTP enabled versus serial-only.

Do not assume a WiFi-blob backtrace means the root cause lives only in the WiFi
driver. Memory pressure from unrelated firmware changes can surface inside the
proprietary WiFi task later.

## Verification Checklist

Before calling a reliability change done:

- `cargo fmt --check`
- `cargo check`
- `cargo check --release`
- `cargo build --release`
- `cargo +stable test -p muninn-gate-core --target aarch64-apple-darwin`
- `git diff --check`
- hardware boot log includes WiFi power cap readback,
- DHCP reaches `Status: serving`,
- `/metrics` survives repeated scrapes,
- `/poll` does not block `/metrics`,
- serial telemetry continues while HTTP is being scraped,
- no WiFi disconnect loop or WiFi-blob panic during a soak run.

Rust `cargo test` is not currently a full verification path for the no_std
firmware target because the selected target lacks the standard Rust test
harness, allocator, and panic handler.
