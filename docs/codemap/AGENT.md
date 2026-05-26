# Codemap

Muninn Gate is split so platform-independent gateway logic, board support, firmware entrypoints, radio control, and MeshCore data types can evolve independently. Keep dependency direction strict: higher-level firmware crates may depend on lower-level crates, but low-level protocol and driver crates must not learn about a specific board.

## Workspace Crates

The root `Cargo.toml` owns shared dependency versions and workspace path dependencies through `[workspace.dependencies]`. Individual crate manifests should use `workspace = true` and only add crate-specific features, optional flags, or dependency aliases.

### `crates/muninn-gate-core`

Platform-independent gateway logic.

Owns:

- gateway, telemetry producer, HTTP/WiFi, radio, display, and MeshCore configuration models
- reusable `TelemetryMetrics` sensor superset, producer/channel telemetry data models, and fixed-capacity in-memory telemetry store
- diagnostic event model and collector trait
- gateway health metric semantics used by Prometheus, serial JSON, WiFi UI, and display code
- provisioning/runtime state for displays, health, and serial diagnostics
- polling scheduler and `MeshcoreClient` integration trait
- Prometheus metrics rendering
- feature-gated serial JSON rendering
- platform service traits such as `Clock`, `TelemetryStore`, `MetricsRenderer`
- `GateFirmwareVariant`, implemented by board crates and selected by the firmware binary
- board capability and TX power level mapping types

Rules:

- Keep `no_std` by default.
- Do not depend on Espressif SDK runtime crates, Embassy ESP, nRF HAL, WiFi, USB, concrete storage backends, or a concrete async runtime.
- Prefer fixed-capacity types and explicit error returns over heap allocation.

### `crates/muninn-gate-firmware`

Thin firmware binary crate.

Owns:

- MCU `main` function
- panic/backtrace and bootloader entry dependencies
- Cargo feature selection for the board variant

Current behavior:

- default feature `heltec-v4`
- `esp32s3` compatibility alias maps to `heltec-v4`
- dispatches to `muninn_gate_board_heltec_v4::WifiLora32V4x::run()`

Rules:

- Keep this crate thin.
- Do not put board-specific business logic here.
- Add future board features by depending on a `muninn-gate-board-*` crate and dispatching to that crate's `run()` entrypoint.

### `crates/muninn-gate-ui`

Platform-independent local display UI renderer.

Owns:

- compact text-frame rendering for 128x64 and 128x128 OLED-style displays
- reusable frame, formatting, bar, battery, and animation primitives
- generic `embedded-graphics` monochrome drawing helpers that are not tied to a controller or MCU
- local gate/node dashboard plus config, WiFi, and producer pages
- display-safe formatting from `GatewayConfig`, `GatewayRuntimeState`, and `TelemetrySnapshot`
- producer row formatting with RSSI signal indicator, ellipsized name, last-heard age, and configured value from the producer default `TelemetryMetrics` view

Rules:

- Keep this crate `no_std`.
- Do not depend on ESP, nRF, OLED controller drivers, USB, WiFi, or storage.
- Keep graphics helpers generic over `embedded-graphics` traits; board/platform crates own concrete controllers and framebuffers.
- Do not render passwords, bearer token values, MeshCore private keys, shared secrets, or full credential material.
- Board/platform crates own display controller setup, fonts, framebuffers, and refresh timing.

### `crates/muninn-gate-board-heltec-v4`

Concrete WiFi LoRa 32 V4.x, ESP32S3 + SX1262 LoRa Node board variant.

Owns:

- `WifiLora32V4x` implementation of `GateFirmwareVariant`
- board name and ESP32-S3 platform selection
- capability declaration for WiFi, HTTP, USB serial, and display
- `Esp32BoardVariant` implementation selecting ESP32 radio hardware settings
- WiFi LoRa 32 V4.x TX power level mapping helpers

Current behavior:

- exposes `WifiLora32V4x::run()`, which dispatches to `muninn_gate_platform_esp32::run_gateway::<WifiLora32V4x>()`
- clamps the requested TX power level to the current radio driver's accepted range
- exposes level-to-output lookup data for UI, telemetry, and display surfaces

Rules:

- Board-specific policy belongs here, not in `muninn-gate-firmware`.
- Keep reusable ESP32 services in `muninn-gate-platform-esp32`.
- Keep radio-family register mechanics in the radio driver crate.

### `crates/muninn-gate-platform-esp32`

Reusable ESP32-S3 platform crate.

Owns:

- `esp-hal` platform initialization
- ESP32-S3 clock, heap, diagnostics, and platform service hooks
- ESP32-S3 task placement policy, with LoRa/MeshCore reserved on APP CPU
- ESP WiFi driver initialization on the PRO CPU before the APP CPU radio owner starts
- retained ESP32-S3 board resources for Heltec V4.x radio/display bring-up
- `Esp32BoardVariant`, the platform-specific extension point for ESP32 board radio hardware settings
- gateway-to-radio configuration mapping for the current SX1262 path
- WiFi startup
- HTTP `/metrics`, `/logs`, and `/poll` server integration
- 128x64 OLED transport for the current ESP32 board
- display screen selection and SSD1306 transport for the current ESP32 board
- serial logging/provisioning hooks
- config storage backend
- PRO CPU scheduler runtime for due polls, `/poll` requests, serial heartbeats, and display refreshes
- concrete wiring from gateway core to radio and MeshCore services

Current behavior:

- `no_std` ESP32-S3 platform startup
- APP CPU radio owner entry with Heltec V4.x board resources
- `esp-wifi` startup through its `esp-alloc` internal allocation hooks
- split ESP32 internal heap: `.dram2_uninit` primary segment plus regular-DRAM spillover
- ESP WiFi station/DHCP bring-up before APP CPU radio startup when HTTP is configured
- USB serial provisioning when no valid config is stored
- boot-time USB config replacement after provisioning, before WiFi starts
- raw flash A/B config document storage in the dedicated `muninn_cfg` data partition
- sanitized boot-time config summary on USB serial
- shared telemetry store initialized from `GatewayConfig`
- Prometheus rendering from the shared telemetry store
- serial JSON rendering of gateway metrics, producer default metrics, and per-channel `TelemetryMetrics`
- WiFi station startup with DHCP
- blocking `smoltcp` HTTP listener for `/metrics`, `/logs`, and `/poll`
- `PollScheduler` runtime integration that consumes `/poll` requests and runs active producer polling
- MeshCore queue adapter that sends the startup gateway advert, sends companion telemetry requests directly, logs into repeaters before telemetry requests, matches encrypted responses, and also records passive producer observations
- SSD1306 display startup and ESP32 screen refresh for WiFi LoRa 32 V4.x
- OLED header indicators for gateway battery state and WiFi/USB status
- initial, update-driven, and heartbeat serial JSON output from the shared telemetry store
- small RAM-backed diagnostic ring exposed by `/logs`
- interface capability validation for the selected board variant
- radio host callback registration and `GatewayConfig.radio` plus ESP32 board hardware settings to `MeshRadioConfig` mapping
- gateway battery metric plumbing from the radio host callback into the shared telemetry snapshot
- WiFi LoRa 32 V4.x battery ADC sampling on the radio owner task

Rules:

- ESP-specific code belongs here, not in `muninn-gate-core`.
- Do not hardcode one ESP32 board here; board crates select capabilities and policy.
- The crate should provide services to core traits and then let core logic drive scheduling/rendering.

### `crates/muninn-mesh-sx126x`

Low-level generic SX126x driver crate for the current radio path.

Owns:

- SPI command encoding
- SX126x register addresses
- reset, busy, DIO, IRQ, FIFO, packet type, modulation, TCXO, RX/TX command primitives
- chip-level error/status response types

Rules:

- Keep this crate protocol-agnostic.
- Do not add MeshCore defaults, board policy, WiFi, scheduler, or gateway concepts.
- Driver configuration should be supplied by callers through parameters.

### `crates/muninn-mesh-radio`

MeshCore LoRa radio orchestration crate.

Owns:

- generic SX126x radio setup sequence
- LoRa PHY configuration supplied from gateway/platform configuration
- RX/TX state transitions
- SX126x errata and register workarounds under `src/radio/workarounds.rs`
- host callback registry under `src/radio/host.rs`
- public radio API types under `src/radio/types.rs`
- RSSI/SNR/noise and radio health counters
- lightweight MeshCore packet metadata extraction under `src/radio/packet.rs`
- conversion between raw LoRa payloads and MeshCore frame/message structures in `src/radio.rs`

Rules:

- Do not depend on ESP HAL, nRF HAL, board GPIO types, WiFi, USB, storage, or display code.
- Platform crates construct concrete SPI/GPIO/FEM/timing resources and pass them into `MeshRadio`.
- Keep radio-family-specific register mechanics in dedicated driver crates, currently `muninn-mesh-sx126x`.
- Keep generic MeshCore packet/event types in `muninn-mesh-meshcore-lib`.

### `crates/muninn-mesh-meshcore-lib`

Hardware-free MeshCore packet and event data crate.

Owns:

- MeshCore message, node, TX frame, and storage record data structures
- fixed-capacity packet/event metadata
- MeshCore payload kind constants and small display helpers

Rules:

- Keep `no_std`.
- Do not depend on radio, WiFi, ESP, nRF, USB, storage, or scheduler code.
- Keep parsing/encoding helpers hardware-independent.

## Dependency Direction

Expected direction:

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

Each arrow is a direct dependency from the crate above it. Avoid reverse dependencies. In particular, `muninn-gate-core`, `muninn-gate-ui`, `muninn-mesh-sx126x`, and `muninn-mesh-meshcore-lib` should not depend on ESP32-specific crates.

## Important Entry Points

- Firmware entrypoint: `crates/muninn-gate-firmware/src/main.rs`
- Workspace dependency catalog: `Cargo.toml`
- Upload-ready provisioning config: `config.example.json`
- Commented provisioning reference: `config.example.jsonc`
- Provisioning utility: `tools/cli.py`
- Board variant: `crates/muninn-gate-board-heltec-v4/src/lib.rs`
- WiFi LoRa 32 V4.x TX power level mapping: `crates/muninn-gate-board-heltec-v4/src/tx_power.rs`
- ESP32 platform runner: `crates/muninn-gate-platform-esp32/src/lib.rs`
- Core traits: `crates/muninn-gate-core/src/lib.rs`
- Local display UI renderer: `crates/muninn-gate-ui/src/lib.rs`
- Runtime state: `crates/muninn-gate-core/src/runtime.rs`
- Diagnostics model: `crates/muninn-gate-core/src/diagnostics.rs`
- ESP32 diagnostic collector: `crates/muninn-gate-platform-esp32/src/diagnostics.rs`
- Variant metadata: `crates/muninn-gate-core/src/variant.rs`
- Gateway config model: `crates/muninn-gate-core/src/config.rs`
- Poll scheduler: `crates/muninn-gate-core/src/poller.rs`
- Telemetry model, reusable sensor metrics, and store: `crates/muninn-gate-core/src/telemetry.rs`
- Metrics rendering: `crates/muninn-gate-core/src/metrics.rs`
- ESP32 WiFi/HTTP runtime: `crates/muninn-gate-platform-esp32/src/wifi.rs`
- ESP32 HTTP response rendering bridge: `crates/muninn-gate-platform-esp32/src/http_metrics.rs`
- ESP32 local input controls: `crates/muninn-gate-platform-esp32/src/input.rs`
- ESP32 display transport: `crates/muninn-gate-platform-esp32/src/display.rs`
- ESP32 USB provisioning parser: `crates/muninn-gate-platform-esp32/src/provisioning.rs`
- ESP32 boot-time USB config updates: `crates/muninn-gate-platform-esp32/src/config_update.rs`
- ESP32 radio config and owner entry: `crates/muninn-gate-platform-esp32/src/radio.rs`
- ESP32 shared telemetry store: `crates/muninn-gate-platform-esp32/src/telemetry_state.rs`
- Gateway metric semantics: `docs/design/firmware/7_GATEWAY_METRICS.md`
- Diagnostic event policy: `docs/design/firmware/8_DIAGNOSTICS.md`
- Display UI policy: `docs/design/firmware/9_DISPLAY_UI.md`
- ESP32 storage/serial/platform services: `crates/muninn-gate-platform-esp32/src/`
- Radio wrapper state machine: `crates/muninn-mesh-radio/src/radio.rs`
- Radio wrapper support modules: `crates/muninn-mesh-radio/src/radio/`
- Current SX126x driver: `crates/muninn-mesh-sx126x/src/`
- MeshCore data types: `crates/muninn-mesh-meshcore-lib/src/event.rs`
- WiFi LoRa 32 V4.x board notes: `docs/boards/heltec_v4.md`

## Verification Commands

Use the ESP Rust toolchain configured for this workspace unless explicitly testing host-only code.

```sh
cargo fmt --all --check
cargo check --workspace
cargo clippy --workspace -- -D warnings
RUSTDOCFLAGS='-D warnings' cargo doc -p muninn-gate-core --no-deps
```

Host tests for core require the host target because the workspace defaults to ESP32-S3:

```sh
cargo +stable test -p muninn-gate-core \
  --target "$(rustc +stable -vV | sed -n 's/host: //p')" \
  --config 'unstable.build-std=[]'
cargo +stable test -p muninn-gate-ui \
  --target "$(rustc +stable -vV | sed -n 's/host: //p')" \
  --config 'unstable.build-std=[]'
```
