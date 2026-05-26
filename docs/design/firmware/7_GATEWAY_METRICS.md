# Gateway Metrics

Gateway metrics describe Muninn Gate itself. They are separate from producer telemetry and diagnostics. Producer telemetry answers "what did the remote MC-compatible device report?" Gateway metrics answer "is this gateway healthy, polling, reachable, and using the expected radio configuration?" Diagnostics answer "what recent events explain this state?"

## Source Of Truth

The source of truth is `TelemetrySnapshot.gateway`, represented by `GatewayMetrics` in `muninn-gate-core`.

All output paths should consume that same snapshot:

- Prometheus HTTP renders gateway and producer metrics from the snapshot.
- Serial JSON emits gateway telemetry and producer telemetry as event records.
- WiFi UI should read the same fields used by Prometheus and serial JSON.
- Display code should show a compact subset of the same state instead of defining a separate status model.

Platform crates update platform-owned fields such as heap/free memory when a
non-reentrant allocator stats API is available, gateway battery state, uptime,
and active interface state. The current ESP32 path intentionally omits
`free_heap_bytes` because `esp_alloc` heap stats can re-enter allocator state
during WiFi startup. The poller updates poll counters, freshness, and latency.
The radio owner updates radio counters. Board crates provide board-specific TX
power level mapping and output estimates.

## Metric Categories

### Runtime

Runtime metrics show whether the firmware is alive and how long this boot has been running.

Current fields:

- `uptime_ms`: monotonic milliseconds since boot.
- `free_heap_bytes`: platform-reported free heap, when available.
- `battery_voltage_mv`: gateway battery voltage in millivolts, when available.
- `battery_percent`: gateway battery charge percentage, when available.

Prometheus metrics:

- `muninn_gate_up`
- `muninn_gate_uptime_ms`
- `muninn_gate_free_heap_bytes`
- `muninn_gate_battery_voltage_mv`
- `muninn_gate_battery_percent`

`muninn_gate_up` is a liveness gauge emitted as `1` when the renderer is running. Startup/provisioning state is tracked separately by `GatewayRuntimeState`; display and serial diagnostics should use that state directly. A Prometheus state metric can be added later when the runtime state names are stable.

### Polling

Polling metrics describe scheduled producer polls at the gateway level and per producer.

Current gateway fields:

- `poll_success_total`: successful scheduled producer polls since boot.
- `poll_failure_total`: failed scheduled producer polls since boot.
- `poll_retry_total`: scheduled polls deferred for retry since boot.
- `poll_on_demand_total`: operator-requested immediate poll passes since boot.
- `last_poll_ms`: monotonic timestamp of the most recent scheduled poll attempt.
- `last_poll_latency_ms`: duration of the latest completed scheduled poll attempt.
- `avg_poll_latency_ms`: rolling average scheduled poll latency.

Prometheus gateway metrics:

- `muninn_gate_gateway_poll_success_total`
- `muninn_gate_gateway_poll_failure_total`
- `muninn_gate_gateway_poll_retry_total`
- `muninn_gate_poll_on_demand_total`
- `muninn_gate_gateway_poll_success_rate`
- `muninn_gate_last_poll_age_ms`
- `muninn_gate_last_poll_latency_ms`
- `muninn_gate_avg_poll_latency_ms`

Prometheus producer metrics:

- `muninn_gate_poll_success_total{node="..."}`
- `muninn_gate_poll_failure_total{node="..."}`
- `muninn_gate_poll_success_rate{node="..."}`
- `muninn_gate_node_last_poll_age_ms{node="..."}`

Counters are canonical. They reset on boot and must be monotonically increasing during one boot. The `*_poll_success_rate` gauges are convenience since-boot ratios for simple clients and displays. Prometheus dashboards should prefer windowed rates from counters, for example:

```promql
sum(rate(muninn_gate_gateway_poll_success_total[5m]))
/
(
  sum(rate(muninn_gate_gateway_poll_success_total[5m]))
  +
  sum(rate(muninn_gate_gateway_poll_failure_total[5m]))
)
```

A scheduled producer poll should count once after the retry policy reaches a final result. Lower-level retry traffic should appear in radio TX/RX counters, not as multiple producer poll successes or failures.

Core also stores `last_poll_success` per producer for local status surfaces. Prometheus can derive recent status from counters and freshness, but the display UI needs the latest attempt result directly so it can show a 0-3 signal indicator or `ERR` without guessing from totals.

### Radio

Radio metrics describe the gateway radio path, not producer-reported values.

Current fields:

- `radio_rx_total`: received radio packets since boot.
- `radio_tx_total`: transmitted radio packets since boot.
- `radio_rx_crc_error_total`: packets rejected by CRC since boot.
- `radio_rx_header_error_total`: packets rejected by header error since boot.
- `radio_rx_timeout_total`: RX timeout IRQ count since boot.
- `radio_device_error_bits`: latest SX126x device error bitmask.
- `radio_last_rssi_dbm`: RSSI from the latest decoded packet.
- `radio_last_snr_tenth_db`: SNR from the latest decoded packet.
- `radio_noise_floor_dbm`: rolling idle-RX noise floor estimate.
- `radio_rssi_inst_dbm`: latest instantaneous idle-RX RSSI sample.
- `radio_chip_mode`: latest sampled SX126x chip mode.
- `radio_last_applied_tx_power_dbm`: TX power most recently applied to the radio.
- `radio_last_tx_airtime_ms`: airtime of the latest successful TX.

Prometheus metrics:

- `muninn_gate_radio_rx_total`
- `muninn_gate_radio_tx_total`
- `muninn_gate_radio_rx_crc_error_total`
- `muninn_gate_radio_rx_header_error_total`
- `muninn_gate_radio_rx_timeout_total`
- `muninn_gate_radio_device_error_bits`
- `muninn_gate_radio_last_rssi_dbm`
- `muninn_gate_radio_last_snr_db`
- `muninn_gate_radio_noise_floor_dbm`
- `muninn_gate_radio_rssi_inst_dbm`
- `muninn_gate_radio_chip_mode`
- `muninn_gate_radio_last_applied_tx_power_dbm`
- `muninn_gate_radio_last_tx_airtime_ms`

Radio metrics should stay gateway-scoped unless they are truly associated with one producer. Producer RSSI/SNR belongs in producer telemetry because it describes the last link observation for that producer.

### TX Power

The configuration stores a requested TX power level, not guaranteed conducted dBm. A board variant maps that level to the radio driver setting and may attach an approximate conducted output estimate.

Current fields:

- `tx_power_level`: selected board/radio TX power level after board mapping.
- `tx_output_dbm_tenths`: board-estimated conducted TX output in tenths of dBm.
- `tx_output_milliwatts`: board-estimated conducted TX output in milliwatts.

Prometheus metrics:

- `muninn_gate_tx_power_level`
- `muninn_gate_tx_output_dbm`
- `muninn_gate_tx_output_milliwatts`

These output estimates are for UI, telemetry, and display context. They are not regulatory certification data. Compliance decisions must account for region, frequency, duty cycle, antenna gain, feedline loss, and board variance.

### Producer Telemetry

Producer telemetry is still rendered through the same snapshot, but it is not gateway telemetry. Producer metrics use a `node` label and represent last-known values from the remote telemetry producer.

Examples:

- `muninn_gate_node_battery_voltage{node="roof_repeater"}`
- `muninn_gate_node_rssi{node="roof_repeater"}`
- `muninn_gate_node_uptime_ms{node="roof_repeater"}`

MeshCore telemetry can contain multiple Cayenne LPP channels from one producer,
for example a three-channel INA3221 sensor. Both `ProducerTelemetry` and
`ProducerTelemetryChannel` reuse the same `TelemetryMetrics` struct for actual
sensor readings. It is a practical superset for the expected producer classes:

- BME680/BME688: temperature, humidity, pressure, gas resistance, and optional
  producer-side air-quality values.
- BMP280: temperature and pressure.
- SHT3x/SHT4x: temperature and humidity.
- GPS receivers: latitude, longitude, altitude, speed, heading, HDOP,
  satellites, and fix type.
- IMUs: acceleration, angular velocity, magnetic field, and attitude angles.
- INA219/INA228/INA3221: voltage, bus voltage, shunt voltage, current, power,
  and accumulated energy/charge when the monitor supports it.

`ProducerTelemetry.metrics` keeps the first seen value for each metric type as
the producer-level default so existing display rows and simple dashboards stay
compact. `ProducerTelemetryChannel.metrics` stores retained channel values
separately.

Current Prometheus output exposes the stable battery/environment fields plus
RSSI/SNR, uptime, poll freshness, and poll counters. Channel variants use an
added `channel` label:

- `muninn_gate_node_channel_battery_voltage{node="power_board",channel="1"}`
- `muninn_gate_node_channel_battery_voltage{node="power_board",channel="2"}`
- `muninn_gate_node_channel_battery_voltage{node="power_board",channel="3"}`

Serial JSON emits every populated `TelemetryMetrics` field at the producer
level and, when channel data is present, adds a `channels` array with the same
metric field names per channel. That makes JSON the richest current output for
new sensor classes while Prometheus naming stays conservative.

Channel values are the canonical source for decoded sensor readings. The
producer-level metrics are a denormalized convenience view only. They should not
be used when the channel identity matters, and future display/config work should
make the selected channel explicit instead of relying on "first channel wins".

The gateway should continue serving the last-known producer values when a producer is temporarily offline. Observation freshness is represented by `muninn_gate_node_last_heard_age_ms`; scheduled-poll freshness is represented by `muninn_gate_node_last_poll_age_ms`.

### Diagnostics Summary

Diagnostics remain available as structured JSON on `/logs`, but the
gateway snapshot also carries a small numeric summary so Prometheus can alert
without parsing event JSON.

Current fields:

- `diagnostic_events`: retained diagnostic event count.
- `diagnostic_dropped`: diagnostic events dropped by the collector.
- `diagnostic_errors`: retained diagnostic events with error severity.
- `last_error_ms`: monotonic timestamp of the latest retained error diagnostic.

Prometheus metrics:

- `muninn_gate_diagnostic_events`
- `muninn_gate_diagnostic_dropped_total`
- `muninn_gate_diagnostic_error_events`
- `muninn_gate_last_error_age_ms`

The latest error is exported as age in milliseconds rather than a string code.
Dashboards can alert on recent errors through a gauge while operators can use
`/logs` to inspect event code and message details.

## Naming And Units

Metric names follow these rules:

- All Prometheus names start with `muninn_gate_`.
- Monotonic counters end in `_total`.
- Time durations use `_ms`.
- Memory sizes use `_bytes`.
- Radio power estimates use `_dbm` or `_milliwatts`.
- Producer metrics use a `node` label.
- Producer channel metrics use both `node` and `channel` labels.
- Gateway metrics do not use a `node` label.

Optional fields are omitted when unknown. Do not emit fake zeroes for unknown TX output, heap, latency, or producer sensor values. Zero is a real value in Prometheus and serial JSON.

## Time Semantics

Core uses monotonic milliseconds, not wall-clock time. `last_poll_ms` is stored as a monotonic timestamp. Prometheus renders an age by subtracting it from `uptime_ms`; serial JSON can expose the raw monotonic timestamp for local consumers.

If wall-clock time is added later through NTP or provisioning, it should be an additional field. It should not replace monotonic freshness calculations.

## Output Surface Rules

Prometheus should expose stable raw counters and gauges. Derived gauges are allowed only when they are cheap, unambiguous, and useful for small clients.

Serial JSON should be compact and explicit. Gateway records use `type: "gateway_telemetry"` and producer records use `type: "producer_telemetry"`.

Display and WiFi UI should present a small operator-focused subset:

- runtime state and next action
- active interface such as IP/port or USB
- selected TX power level and output estimate when known
- last poll age
- since-boot success ratio
- latest or average poll latency
- heap/free memory when available

Display code should avoid parsing Prometheus text. It should consume `GatewayMetrics` and `GatewayRuntimeState` directly.

## Future Fields

Add fields when a concrete subsystem can update them accurately. Likely next additions:

- radio CRC error and timeout counters
- MeshCore decode failure counters
- HTTP request and render failure counters
- serial output drop counters
- config generation or saved-at monotonic timestamp
- queue depth or dropped event counters

Each new field should define its owner, unit, reset behavior, and whether unknown means omission or zero.
