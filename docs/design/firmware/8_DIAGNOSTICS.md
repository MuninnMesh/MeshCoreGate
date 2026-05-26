# Diagnostics

Diagnostics are retained event records for operator debugging. They are not a replacement for Prometheus metrics. Metrics answer "what is the current health signal?" Diagnostics answer "what happened recently that explains this state?"

## Core Model

`muninn-gate-core` defines:

- `DiagnosticEvent`: one timestamped event with sequence, severity, subsystem, code, and message.
- `DiagnosticSnapshot<N>`: the last retained events plus a dropped-event counter.
- `DiagnosticCollector<N>`: the storage boundary used by platform code.
- `FixedDiagnosticRing<N>`: a simple RAM-backed implementation for tests and simple builds.
- `NullDiagnosticCollector`: an implementation that intentionally drops diagnostic history.

The trait is intentionally storage-neutral. ESP32 builds should be able to back it with raw flash, PSRAM, or a small RAM ring depending on board and durability requirements. The core model should not force fast internal RAM for diagnostic history.

The current ESP32 bring-up implementation uses a 32-event RAM-backed ring so `/logs` is useful immediately. It is intentionally small and replaceable; a persistent or PSRAM-backed collector should be used when diagnostics must survive reset or when a larger history is needed.

## Event Shape

Each diagnostic event contains:

- `sequence`: monotonic collector-assigned event sequence.
- `timestamp_ms`: monotonic milliseconds when the event occurred.
- `level`: `debug`, `info`, `warn`, or `error`.
- `subsystem`: `runtime`, `config`, `storage`, `wifi`, `http`, `serial`, `radio`, `meshcore`, `poller`, or `display`.
- `code`: stable short machine-readable event code.
- `message`: concise human-readable detail.

The `code` field should be stable enough for scripts and UI filters. The `message` field is for humans and can change when wording improves.

## Storage Policy

The configured collector should retain the last `N` events. If the collector is full, it should drop the oldest event, increment `dropped_total`, and keep accepting new events.

Storage choices are board/platform decisions:

- flash-backed collector for persistent diagnostics across reset
- PSRAM-backed collector for larger volatile history
- small RAM ring for early bring-up or boards without suitable storage
- null collector for production builds that disable diagnostic history

Flash-backed collectors must batch or rate-limit writes so a noisy subsystem does not cause excessive flash wear.

## HTTP Exposure

WiFi-capable builds expose retained diagnostics through a local endpoint:

```text
GET /logs
```

The endpoint returns compact JSON rendered from `DiagnosticSnapshot<N>`. It does not clear diagnostics. The current ESP32 MVP returns this shape from the RAM-backed platform collector.

The ESP32 metrics path also exports a compact diagnostics summary through the
gateway telemetry snapshot: retained event count, dropped event count, retained
error count, and latest retained error age. Use `/logs` for detailed
event code/message inspection.

Diagnostics are potentially sensitive because they may include SSIDs, endpoint names, route hints, or provisioning failures. Management, provisioning, and diagnostics endpoints must use the configured HTTP token/TLS policy and should be safe to disable.

## What To Log

Good diagnostic events:

- boot and runtime state transitions
- config load, validation, save, and provisioning failures
- WiFi connect/disconnect and IP assignment
- HTTP server start/stop/render failures
- serial provisioning commands and validation failures
- radio reset, configuration, IRQ error, timeout, and device error events
- MeshCore decode/auth/poll failures
- poll retries and final poll failure

Avoid logging one event for every normal packet or every successful poll at default verbosity. That data belongs in counters and telemetry. Diagnostics should explain unusual behavior, state changes, and operator-actionable failures.

## Relation To Metrics

Diagnostics and metrics should cross-reference by subsystem and timestamp, not duplicate each other. For example:

- `muninn_gate_gateway_poll_failure_total` tells a dashboard that failures are increasing.
- A `poller` diagnostic event with code `poll_failed` explains which producer failed and why.
- Radio timeout counters show frequency; radio diagnostic events show recent context.

Displays should only show the latest high-severity diagnostic summary. WiFi UI and serial tooling can show a fuller event list.
