# Design Overview

This directory describes Muninn Gate firmware architecture at the level needed to add board variants, wire hardware services, and keep protocol/radio responsibilities separated.

## Firmware

Firmware design docs live under [firmware/](firmware/):

- [firmware/1_OVERVIEW.md](firmware/1_OVERVIEW.md): main firmware overview and runtime diagram.
- [firmware/2_CODE_STRUCTURE.md](firmware/2_CODE_STRUCTURE.md): crate layout and dependency direction.
- [firmware/3_RADIO_MESHCORE.md](firmware/3_RADIO_MESHCORE.md): LoRa, radio, and MeshCore task boundaries.
- [firmware/4_WIFI_HTTP.md](firmware/4_WIFI_HTTP.md): WiFi, HTTP `/metrics`, and serial JSON output.
- [firmware/5_SCHEDULER.md](firmware/5_SCHEDULER.md): polling scheduler behavior.
- [firmware/6_CONFIGURATION.md](firmware/6_CONFIGURATION.md): config storage, USB provisioning, WiFi provisioning, and JSON format.
- [firmware/7_GATEWAY_METRICS.md](firmware/7_GATEWAY_METRICS.md): gateway health metrics, producer/channel telemetry, units, naming, and output semantics.
- [firmware/8_DIAGNOSTICS.md](firmware/8_DIAGNOSTICS.md): retained diagnostic events and storage/HTTP exposure policy.
- [firmware/9_DISPLAY_UI.md](firmware/9_DISPLAY_UI.md): local display pages, supported sizes, and renderer boundaries.
- [firmware/10_RELIABILITY.md](firmware/10_RELIABILITY.md): WiFi/HTTP, DHCP, socket, MeshCore TX, and hardware-soak reliability policy.

Board notes live under [../boards/](../boards/), starting with [../boards/heltec_v4.md](../boards/heltec_v4.md).

## Current Shape

Muninn Gate uses a small layered workspace:

- `muninn-gate-core` owns platform-independent gateway logic, including the reusable producer/channel `TelemetryMetrics` model.
- `muninn-gate-ui` owns platform-independent local display page rendering.
- `muninn-gate-firmware` owns the selected MCU `main` function.
- `muninn-gate-board-heltec-v4` owns the first concrete board variant and implements `GateFirmwareVariant`.
- `muninn-gate-platform-esp32` owns reusable ESP32-S3 services.
- `muninn-mesh-radio` owns platform-independent SX126x/MeshCore radio orchestration.
- radio-family driver crates, currently `muninn-mesh-sx126x`, own generic chip primitives.
- `muninn-mesh-meshcore-lib` owns hardware-free MeshCore packet/event data types.

The important design rule is that board variants select capabilities and board policy, platform crates provide MCU services, and `muninn-gate-core` keeps the application model and scheduling logic independent of any board, async runtime, storage backend, WiFi stack, or USB implementation.

## Device Variants And Interfaces

Device variants live in `crates/muninn-gate-board-*` and implement `GateFirmwareVariant`. Platform crates live in `crates/muninn-gate-platform-*` and provide common MCU services. ESP32 boards can expose WiFi HTTP plus USB serial; USB-only boards can expose serial without changing core scheduling or telemetry models.

## Configuration

Configuration is loaded by the selected board/platform runtime, validated into `GatewayConfig`, and then passed into core scheduling/output code. The ESP32 backend persists the validated provisioning document in flash and enters USB provisioning when no valid config is present. USB serial provisioning is the recovery path for missing or invalid config; WiFi provisioning can be added by WiFi-capable variants. See [firmware/6_CONFIGURATION.md](firmware/6_CONFIGURATION.md).

## Building A Board Variant

Build the top-level firmware crate with the feature for the target board. The current board is selected with `heltec-v4`:

```sh
cargo build -p muninn-gate-firmware --features heltec-v4 --release
cargo espflash flash \
  --flash-size 16mb \
  --partition-table partitions.csv \
  --package muninn-gate-firmware \
  --bin muninn-gate \
  --features heltec-v4 \
  --release \
  --target xtensa-esp32s3-none-elf \
  --chip esp32s3 \
  --monitor
```

Future boards should add their own feature, for example `heltec-v5` or `nrf52-usb`, and dispatch from `muninn-gate-firmware` to that board crate. The current `esp32s3` feature is only a compatibility alias for `heltec-v4`.
