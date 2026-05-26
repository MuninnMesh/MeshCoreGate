# LoRa, Radio, And MeshCore

The radio side has three layers: a radio-family driver, a generic radio owner, and a MeshCore client used by the gateway scheduler.

## Radio Driver Layer

`muninn-mesh-sx126x` is the current radio driver crate. Future radio families, such as LR1121 or LR2021, should use the same separation: a dedicated driver crate owns chip-specific command/register mechanics and does not know about MeshCore.

A radio driver exposes chip primitives such as:

- setting packet params
- setting modulation params
- reading IRQ status
- reading and writing buffers
- switching RX/TX/standby modes
- applying TCXO and PA configuration

MeshCore defaults and board policy should not be added to this crate.

## Radio Owner

`muninn-mesh-radio` is the platform-independent SX126x/MeshCore radio owner. The platform crate creates the SPI device, radio pins, RF switch, and clock/delay provider, then passes them into `MeshRadio`. The radio owner configures the selected SX126x for LoRa, applies chip workarounds, enters continuous RX, receives frames, extracts lightweight MeshCore metadata, and transmits `MeshTxFrame` payloads.

The crate is split for reviewability: `src/radio.rs` keeps the radio state machine, while `src/radio/types.rs`, `host.rs`, `config.rs`, `workarounds.rs`, and `packet.rs` hold API types, platform callbacks, SX126x config translation, errata handling, and packet metadata helpers.

It also owns radio health counters:

- RX frame count
- CRC errors
- timeouts
- RSSI and SNR
- instantaneous RSSI and noise floor
- device error bits
- last applied TX power level

`MeshRadioConfig` keeps the LoRa PHY policy above the raw driver. Frequency, sync word, spreading factor, bandwidth, coding rate, preamble length, IQ mode, and TX ramp are supplied as configuration instead of being embedded in a low-level radio driver.

ESP32 board variants supply radio hardware settings before the platform creates the radio owner. For example, WiFi LoRa 32 V4.x, ESP32S3 + SX1262 LoRa Node has an external FEM and TCXO, so the ESP32 platform owns FEM GPIO setup while the board crate provides TCXO timing and `GateFirmwareVariant::map_tx_power` clamps the requested TX power level to the accepted driver range while attaching an approximate conducted-output estimate.

## MeshCore Client Layer

The gateway task talks to MeshCore through the `MeshcoreClient` trait from `muninn-gate-core`. On ESP32, `muninn-gate-platform-esp32::meshcore` implements that trait over the radio owner queues.

The current adapter sends official MeshCore client packets through the radio
owner TX queue and consumes response frames from the RX queue. Outbound MeshCore
traffic is direct-only by firmware policy: startup adverts, repeater login
requests, and telemetry requests all use direct route bits. Legacy route config
strings are accepted for compatibility, but the ESP32 adapter normalizes all
producer contacts to direct. The radio TX queue also rejects any frame whose
MeshCore route header is not direct before it can reach the SX126x.

On startup the adapter queues a signed companion-style MeshCore advert built
from the configured gateway public/private key so nearby nodes can learn the
gateway identity when their contact policy allows it. Configured companion
producers receive an encrypted telemetry request type `0x03` directly.
Configured repeater producers receive a MeshCore login request first, using the
configured producer password when present, then the telemetry request. The
adapter matches encrypted responses by tag and normalizes returned Cayenne LPP
bytes into
`ProducerTelemetry`: decoded sensor values go into channel-specific
`TelemetryMetrics`, and the first value of each metric type is also copied into
the producer default metric set for compact outputs. It also passively consumes
received `MeshRxFrame` events, matches them to configured producers by
advertised public-key prefix or name, and records RSSI/SNR plus plain Cayenne
LPP telemetry fields when present.

Incoming parsing remains tolerant of flood, transport, path, and response
packets so the gateway can observe nearby traffic and decode relevant responses.
The direct-only rule applies to what Muninn Gate transmits. Remote producer
firmware controls its own replies; Muninn Gate can request direct replies by
sending direct login/request packets, but it cannot prevent a remote repeater
from using a different reply policy.

Poll failures should preserve the layer where the request failed. The ESP32 adapter records distinct labels for no post-TX LoRa RX, LoRa CRC/header errors, no MeshCore response frame, response addressed to another gateway, wrong producer source, MAC failure, request-tag mismatch, decode failure, auth failure, and unsupported routes. This keeps serial JSON and `/logs` useful while debugging RF settings, MeshCore keys, route behavior, and parser bugs.

The intended flow is:

1. The board variant and ESP32 platform map board hardware settings and gateway radio config into `MeshRadioConfig`.
2. A radio-owning task keeps the device in continuous RX when idle.
3. RX IRQ polling or interrupt handling feeds incoming packets to the MeshCore layer.
4. The ESP32 MeshCore adapter queues a signed gateway advert after radio startup.
5. The ESP32 MeshCore adapter enqueues telemetry request frames through the radio owner, with a login frame first only for configured repeaters.
6. The adapter drains queued RX frames, matches encrypted responses to the pending producer/tag, and updates the shared telemetry store.
7. Nonmatching frames are still considered for passive producer observations before they are discarded from the queue.

Only the radio-owning task should directly mutate the radio device. Other tasks should communicate with it through a small command/event boundary so SPI access, RX/TX state transitions, and FEM switching remain serialized.

On ESP32 this boundary can be an Embassy channel, a critical-section queue, or another local async primitive chosen by the board crate. Core should not depend on that choice.

## ESP32-S3 Core Assignment

ESP32-S3 builds must keep the LoRa/MeshCore path on the APP CPU. `muninn-gate-platform-esp32` owns this platform policy:

- APP CPU: LoRa radio owner, IRQ/event handling, RX/TX state transitions, and MeshCore packet path.
- PRO CPU: startup, storage, WiFi, HTTP, serial, provisioning, and display/UI work.

This split keeps WiFi/HTTP bursts, provisioning writes, and rendering work from delaying radio event handling. The APP CPU task should block on radio IRQs, radio command queues, or scheduler requests; it should not run HTTP, storage, JSON rendering, or display work.

For HTTP-enabled ESP32 boots, the platform crate initializes the ESP WiFi
scheduler/controller, connects the station, and waits for DHCP on the PRO CPU
before starting the APP CPU radio owner. Serial-only boots skip WiFi and start
the APP CPU radio owner directly. The APP CPU is started through `esp-hal`, and
the platform holds the core guard for the lifetime of the gateway. The APP CPU
radio owner entry receives Heltec V4.x board resources and the mapped
`MeshRadioConfig`, constructs the SPI device, SX126x pins, RF switch, and
clock, initializes `MeshRadio`, starts continuous RX, polls `poll_receive`,
queues received frames for the MeshCore adapter, drains queued TX frames,
spaces TX by airtime, reinitializes after repeated errors, and publishes RX/TX
packet health, CRC/header/timeouts, RSSI/SNR, noise floor, chip mode, device
errors, selected TX power, and last TX airtime.

## Radio State Model

Radio state should stay simple:

```text
boot and reset radio
configure LoRa PHY and packet params
enter continuous RX
periodically sample health while idle
on TX request:
  enter standby
  write payload
  set TX params
  transmit
  restore RX
on RX event:
  read payload
  clear IRQ
  restore RX if needed
  publish packet event
```

MeshCore-specific parsing that is hardware-free belongs in `muninn-mesh-meshcore-lib`. MCU-level details such as SPI host setup, GPIO ownership, and FEM controls belong in the platform or board crate. Board-specific details such as pin maps, display/radio variant selection, and TX power level policy belong in the board crate or the platform code it selects.
