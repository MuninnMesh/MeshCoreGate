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

ESP32-S3 WiFi is managed by `esp-wifi` on the PRO CPU. The firmware applies a
conservative station policy:

- Power save is explicitly disabled with `PowerSaveMode::None`.
- WiFi TX power is capped at `60` quarter-dBm units, about 15 dBm.
- The applied TX power is read back and logged at boot/reconnect.
- Connection attempts are retried at a controlled cadence rather than in a
  tight loop.
- Disconnect reason codes are logged with symbolic names where known, including
  `2 = AUTH_EXPIRE` and `39 = TIMEOUT`.

The TX power cap is intentional. Maximum ESP32 WiFi TX power can improve range,
but it also increases current spikes and can destabilize marginal board power or
RF conditions. On Heltec V4-class boards, prefer the lowest WiFi TX power that
keeps the link reliable.

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

- `HTTP_SOCKET_COUNT = 2`
- `HTTP_TCP_RX_BUFFER_BYTES = 1024`
- `HTTP_TCP_TX_BUFFER_BYTES = 2048`
- `HTTP_REQUEST_BUFFER_BYTES = 1024`

The two-socket pool is a deliberate compromise. It provides one spare socket for
a second scraper or a stale client while keeping static internal RAM pressure
low enough for the ESP WiFi blob. A previous four-socket pool used about 36 KB
of static HTTP buffers and correlated with WiFi-blob `LoadProhibited` crashes;
the current pool uses about 8 KB.

Each socket:

- has Nagle disabled for small telemetry responses,
- has a 5 second TCP timeout,
- has 5 second TCP keepalive,
- accumulates request bytes until HTTP headers are complete or the request
  buffer is full,
- closes after one response,
- is aborted on send stalls, malformed socket state, WiFi disconnect, DHCP loss,
  DHCP address change, or reconnect.

Large static buffers must be initialized with `StaticCell::init_with`, not
`StaticCell::init`. `init` may create a large temporary on the stack before
moving it into static storage, which is unsafe for ESP32 firmware with WiFi
enabled.

## HTTP Handler Boundaries

HTTP handlers must remain cheap:

- `/metrics` reads a telemetry snapshot and streams Prometheus text.
- `/logs` renders retained diagnostics.
- `/poll` records an on-demand poll request and returns `202 Accepted`.
- HTTP must not directly poll producers or touch radio hardware.
- Authentication remains bearer-token based when `http.tokens` is non-empty.

The scheduler consumes `/poll` requests outside the socket handler. This keeps
HTTP work independent from LoRa timing and prevents slow radio operations from
holding TCP sockets open.

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
3. Reduce HTTP socket/buffer counts before adding new recovery logic.
4. Confirm WiFi TX power cap readback is logged.
5. Compare whether the crash happens with HTTP enabled versus serial-only.

Do not assume a WiFi-blob backtrace means the root cause lives only in the WiFi
driver. Memory pressure from unrelated firmware changes can surface inside the
proprietary WiFi task later.

## Verification Checklist

Before calling a reliability change done:

- `cargo fmt -p <changed package> --check`
- `cargo check`
- `cargo build`
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
