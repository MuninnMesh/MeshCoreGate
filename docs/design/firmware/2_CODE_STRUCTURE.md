# Code Structure

The workspace is divided by responsibility.

```text
crates/
  muninn-gate-core/
  muninn-gate-ui/
  muninn-gate-firmware/
  muninn-gate-board-heltec-v4/
  muninn-gate-platform-esp32/
  muninn-mesh-radio/
  muninn-mesh-sx126x/
  muninn-mesh-meshcore-lib/
```

Shared crate versions and workspace path dependencies live in the root
`Cargo.toml` under `[workspace.dependencies]`. Crate manifests should reuse
those entries with `workspace = true` and declare only the features or optional
dependency flags they need locally.

## `muninn-gate-core`

`muninn-gate-core` is the platform-independent gateway crate. It owns the
configuration model, telemetry producer model, shared `TelemetryMetrics` sensor
superset, producer/channel telemetry records, metrics rendering, diagnostics
model, scheduler, serial JSON rendering helper, and service traits. It stays
`no_std` by default and must not depend on ESP HAL, nRF HAL, WiFi stacks, USB
stacks, concrete flash storage, or a specific async runtime.

Core traits define the boundaries between gateway logic and the board:

- `Clock` supplies monotonic time in milliseconds.
- `TelemetryStore` stores normalized producer telemetry and poll counters.
- `PollTelemetryStore` is the smaller sink required by the scheduler when a platform store cannot expose direct metric references.
- `DiagnosticCollector` records retained diagnostic events through a platform-selected backend.
- `MeshcoreClient` polls one configured telemetry producer and returns normalized telemetry.
- `MetricsRenderer` renders a telemetry snapshot.
- `GateFirmwareVariant` is implemented by board crates and selected by the firmware binary.

Telemetry decoding should write actual sensor values into `TelemetryMetrics`.
`ProducerTelemetryChannel.metrics` is the canonical channel-specific store for
MeshCore/LPP producers. `ProducerTelemetry.metrics` is a denormalized default
view copied from the first channel that reports each metric type, used by simple
display rows and existing top-level Prometheus/serial fields.

Core also owns `GatewayRuntimeState`, which is the status model for display, health, and serial reporting:

- `Unprovisioned`: no valid config is available.
- `Provisioned`: config is loaded, but outputs are not serving yet.
- `Serving`: one or more telemetry/provisioning interfaces are active.
- `Error`: startup or runtime work failed and needs operator action.

## `muninn-gate-ui`

`muninn-gate-ui` is the platform-independent local display crate. It renders fixed-capacity text frames from `GatewayConfig`, `GatewayRuntimeState`, and `TelemetrySnapshot`. It also owns reusable frame, formatting, signal bar, battery, truncation, and animation helpers so 128x64 and 128x128 displays share the same status model. It does not own display buses, controller drivers, fonts, framebuffers, or refresh timing.

The public render entry points are `render_oled_128x64`, `render_oled_128x128`, and `render_page`.

The primary display page is a compact gate/node dashboard:

- gate row with WiFi and LoRa state
- producer rows with signal indicator, 10-character ellipsized name, last-heard age, and a configured telemetry value from the producer default `TelemetryMetrics` view

Secondary pages expose config, WiFi, and producer details for bring-up. Board/platform display drivers should consume these text frames and decide how to paint them on the concrete screen.

## `muninn-gate-firmware`

`muninn-gate-firmware` is the thin binary crate. It owns the MCU entrypoint, boot descriptor, panic/backtrace integration, and Cargo feature selection. Today its default `heltec-v4` feature dispatches to `muninn_gate_board_heltec_v4::WifiLora32V4x::run()`.

Later, another board feature should dispatch to that board crate, such as `muninn_gate_board_foo::FooBoard::run()`. The binary crate should stay thin: it selects a variant and calls `run()`.

## `muninn-gate-board-heltec-v4`

`muninn-gate-board-heltec-v4` is the first concrete board variant. It implements `GateFirmwareVariant`, declares the WiFi LoRa 32 V4.x, ESP32S3 + SX1262 LoRa Node board name, ESP32-S3 platform family, WiFi/HTTP/USB capabilities, 128x64 OLED display class, ESP32 radio hardware settings, and board-specific TX power level mapping.

Board crates are where board policy belongs:

- display class and display initialization policy
- supported output interfaces
- radio hardware settings such as TCXO startup delay
- FEM and TX power level mapping
- board-specific task selection
- handoff to the correct platform crate

## `muninn-gate-platform-esp32`

`muninn-gate-platform-esp32` is the reusable ESP32-S3 platform crate. It initializes `esp-hal` and owns ESP32-specific storage, diagnostics, WiFi, HTTP, serial, heap, radio/display resource retention, and MCU-level wiring shared by ESP32 board variants.

This crate is where ESP32-S3 common services belong: WiFi implementation details, flash-backed config storage, USB/JTAG serial, HTTP server integration, and platform task startup. It also owns the ESP32-S3 task placement policy: LoRa/MeshCore runs on the APP CPU, while WiFi/HTTP/storage/serial stays on the PRO CPU. Concrete board crates decide which services to enable through `GateFirmwareVariant` and provide ESP32-specific radio hardware settings through `Esp32BoardVariant`.

The current ESP32 platform code enters USB provisioning when no config is
stored, persists validated JSON/JSONC config in the raw `muninn_cfg` flash
partition, offers a boot-time USB config replacement window after provisioning,
connects WiFi in station mode when `http` is present, obtains DHCP, starts the
APP CPU radio owner after the HTTP endpoint exists, serves `/metrics`, `/logs`,
and `/poll` with a small blocking `smoltcp` listener, emits serial JSON from
the shared telemetry store, drives the 128x64 OLED, retains a small RAM
diagnostic ring, maps `GatewayConfig.radio` plus ESP32 board hardware settings
into `MeshRadioConfig`, and runs the APP CPU radio owner entry with Heltec V4.x
board resources. The platform uses `esp-wifi` with `esp-alloc` and a split
internal heap, with the large heap segment in `.dram2_uninit` and a smaller
regular-DRAM spillover segment. The radio owner initializes the SX1262 path,
continuously polls RX, queues received frames, drains queued TX frames, spaces
TX by the last airtime, and publishes radio health. The PRO CPU scheduler
consumes `/poll` requests, sends official MeshCore login and telemetry requests
through the TX queue, matches encrypted response tags from the RX queue,
passively drains matched RX observations into telemetry, refreshes
serial/display output, and records poll latency/failure metrics.
Direct-then-flood fallback and explicit path support are still bring-up
integration points.

## Mesh And Radio Crates

`muninn-mesh-sx126x` is the generic SX126x driver crate for the current radio path. It owns chip-level command encoding, register addresses, reset/busy/DIO primitives, packet configuration, FIFO access, IRQ handling, TCXO control, and status/error response types. It should not contain MeshCore defaults, gateway scheduling, WiFi, storage, or board policy.

`muninn-mesh-radio` is the platform-independent SX126x/MeshCore radio wrapper. Its main `src/radio.rs` file owns the RX/TX state machine; focused support modules under `src/radio/` own public API types, host callbacks, SX126x configuration building, errata/workarounds, and lightweight MeshCore packet metadata extraction. Platform crates own concrete SPI/GPIO/FEM/timing setup and pass those resources into `MeshRadio`.

`muninn-mesh-meshcore-lib` is hardware-free MeshCore data. It owns fixed-capacity message, node, TX frame, and storage record structures plus MeshCore payload kind constants. It should remain `no_std` and independent of radio, WiFi, USB, scheduler, and storage code.

## Dependency Direction

```text
muninn-gate-firmware
  -> muninn-gate-core
  -> muninn-gate-board-heltec-v4

muninn-gate-board-heltec-v4
  -> muninn-gate-core
  -> muninn-gate-ui
  -> muninn-gate-platform-esp32

muninn-gate-ui
  -> muninn-gate-core

muninn-gate-platform-esp32
  -> muninn-gate-core
  -> muninn-gate-ui
  -> muninn-mesh-radio
  -> muninn-mesh-meshcore-lib
  -> muninn-mesh-sx126x

muninn-mesh-radio
  -> muninn-mesh-meshcore-lib
  -> muninn-mesh-sx126x
```

Each arrow is a direct dependency from the crate above it. The firmware binary should stay thin. Board crates should provide concrete services. Core should describe the application behavior.
