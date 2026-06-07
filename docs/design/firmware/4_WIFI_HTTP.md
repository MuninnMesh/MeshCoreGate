# WiFi And HTTP

On ESP32-S3, WiFi and HTTP are platform services owned by `muninn-gate-platform-esp32`. A board crate enables them through `GateFirmwareVariant` capabilities.

## WiFi Startup

The current ESP32 flow is:

1. Load gateway configuration from platform storage.
2. If no config is available, stay in USB provisioning mode until a valid JSON/JSONC config is uploaded.
3. On provisioned boots, briefly open the USB config replacement window, then release the USB Serial/JTAG peripheral before WiFi starts.
4. If `http` config is present, create the `esp-wifi` station controller.
5. Connect to the configured SSID.
6. Obtain an IPv4 address with DHCP.
7. Start the cooperative LoRa/MeshCore radio owner.
8. Start a fixed-pool `smoltcp` TCP listener on the configured HTTP port.
9. Serve HTTP endpoints while continuing to service smoltcp, DHCP, scheduler,
   display, serial, button input, and LoRa in the same loop.

The ESP32 reliability baseline is cooperative single-loop servicing. Earlier
APP-core radio isolation was removed from the normal path after field testing
showed WiFi-blob panics when an independent second-core radio loop ran beside
`esp-wifi`. HTTP-enabled and serial-only boots both use the cooperative radio
owner; HTTP boots additionally keep smoltcp serviced before, during, and after
scheduled LoRa polls.

WiFi uses the `esp-wifi` station controller directly with the crate's
`builtin-scheduler`, `smoltcp`, and `esp-alloc` features. Muninn Gate stores
its own validated config document in the raw `muninn_cfg` flash partition, not
NVS. The current partition table intentionally has no `nvs` partition. The
`esp-wifi` driver is configured for RAM-backed driver state and receives
internal allocations through the shared `esp-alloc` heap.

The ESP32 heap is split across internal memory regions: a primary heap segment
in `.dram2_uninit` and a smaller regular-DRAM spillover segment. This keeps the
normal `.bss`/stack region from carrying all firmware and WiFi allocation
pressure. Heap statistics are currently omitted from telemetry because
`esp_alloc` stats use an internal `RefCell` and can re-enter allocator state
during WiFi startup.

The build config keeps WiFi queue/buffer counts small and avoids esp-wifi
scan/retry environment knobs that can touch unimplemented NVS callback paths in
the no_std OS adapter. The device serves low-rate local telemetry, not
high-throughput networking.

The runtime also applies reliability-oriented WiFi settings: modem power save is
disabled, country policy is applied after WiFi start, WiFi TX power is requested
and read back, the strongest scanned BSSID/channel is pinned when accepted, and
reconnect attempts are paced. DHCP is reset on link recovery and IP state
changes so smoltcp does not remain in an old discovery backoff. Details and the
operational checklist live in [10_RELIABILITY.md](10_RELIABILITY.md).

The DHCP client sends option 12 Host Name using a sanitized form of
`config.name` so consumer router apps can display the gateway by name. The
sanitizer keeps ASCII letters/digits, converts spaces/underscores/hyphens into
single hyphens, drops other characters, trims edge hyphens, and falls back to
`muninn-gate` when the configured name has no usable hostname characters.

WiFi state management belongs in the ESP32 platform crate. `muninn-gate-core` should not depend on the WiFi stack or know whether the board uses WiFi, USB, Ethernet, or serial-only output.

## HTTP `/metrics`

The HTTP handler should not poll producers directly. It should take a fast read-only snapshot from the telemetry store and pass it to `muninn_gate_core::metrics::render_prometheus`.

This keeps the HTTP response independent of radio timing. If the radio is busy or a producer is not responding, `/metrics` still returns the last known values plus freshness metrics such as `last_poll_ms` or age derived from current uptime.

The initial endpoint surface should stay small:

- `GET /metrics`: Prometheus text format.
- `GET /logs`: compact JSON for retained diagnostic events.
- `GET /poll`: operator request for an immediate producer poll.
- Optional later: provisioning/config endpoints only when explicitly enabled.

Prometheus and diagnostic JSON rendering live in core because they are platform-independent string formatting. Socket setup, WiFi state, HTTP request parsing, authentication, and response writing live in the ESP32 platform crate.

The HTTP server intentionally only parses simple `GET` requests and closes each
connection after one response. It is enough for Prometheus scraping and manual
bring-up with `curl`.

smoltcp has no listen backlog, so the ESP32 server uses a small fixed pool of
HTTP sockets rather than a single socket. The current pool uses sixteen sockets
with small per-socket RX/TX buffers, TCP timeout/keepalive, per-socket request
accumulation, and abort-on-stall behavior. All HTTP sockets are aborted when
WiFi disconnects, DHCP is deconfigured, the IP address changes, or WiFi
reconnects.

The current `/metrics` response streams the shared ESP32 telemetry store with
HTTP chunked transfer. Configured producers receive live values when the ESP32
MeshCore adapter matches received frames to those producers. The current
`/logs` response uses the core diagnostic JSON shape and reads from the ESP32
platform diagnostic collector. `/poll` records an operator poll request and
returns `202 Accepted`; the scheduler consumes that request outside the socket
handler so HTTP work does not directly touch the radio.

Gateway telemetry is rendered beside producer telemetry. It includes selected
TX power level/output, poll success rate, poll latency, radio counters,
heap/free memory when available, and uptime so WiFi UI, Prometheus, serial
consumers, and display code share one status model. Producer readings use the
shared `TelemetryMetrics` model; top-level producer metrics are a compact
default view, while channel-specific metrics are exposed with a `channel` label
for multi-channel sensors. The metric contract is defined in
[GATEWAY_METRICS.md](7_GATEWAY_METRICS.md).

Configuration/provisioning endpoints are not exposed over HTTP yet. USB serial is the recovery and write path. When HTTP config endpoints are added, they must use the configured HTTP token and TLS policy.

HTTP bearer auth is active when `http.tokens` contains one or more tokens. Empty, omitted, or `null` tokens disable bearer-header auth for protected-lab bring-up.

## Serial JSON

Serial output is separate from the HTTP server. Boards with USB/JTAG serial
always emit boot/status logs and support the USB config upload path. Telemetry
JSON events are feature-gated behind `serial-json`; the default firmware build
keeps that feature off so WiFi/HTTP can be tested with less formatting and
serial-output load. If `http` is omitted, serial logs and optional serial JSON
are the only output path.

Serial JSON rendering can use the feature-gated helper in `muninn-gate-core`.
It emits gateway metrics, top-level producer metrics, poll status, RSSI/SNR, and
a `channels` array when MeshCore/LPP channel data is present. The platform crate
owns the actual USB/JTAG serial transport and buffering policy; the selected
board variant decides whether that interface is exposed.

Serial diagnostics should use the same `DiagnosticSnapshot` model as HTTP diagnostics. Serial may emit individual events as they happen, while HTTP should return the retained event snapshot.

## Output Behavior

Output selection is intentionally implicit:

- `http` present: start WiFi and HTTP metrics, and keep serial enabled when the board supports it.
- `http` omitted or `null`: use serial only.

The selected board variant validates that the requested interfaces are supported by its capabilities before starting services. nRF52 support should not assume WiFi; its board crate can enable serial, USB, or another output while using the same telemetry store and rendering boundaries.
