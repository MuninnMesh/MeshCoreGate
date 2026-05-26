# Display UI

The display UI is the local, operator-facing status surface. It should show enough state to answer "what board is this, is it provisioned, is it connected, and are producers polling?" without duplicating HTTP, serial, or Prometheus-specific logic.

## Crate Boundary

`crates/muninn-gate-ui` owns platform-independent page rendering. It consumes:

- `GatewayConfig` for board-facing config summaries.
- `GatewayRuntimeState` for provisioning, serving, and error state.
- `TelemetrySnapshot` for gateway metrics and producer polling status.

It returns fixed-capacity text frames. It does not own I2C/SPI pins, OLED controller setup, framebuffers, font selection, dirty-region updates, or refresh scheduling. Those are board/platform responsibilities.

The first board target has a 128x64 monochrome OLED. The renderer also supports a 128x128 frame so a later board can show more rows with the same page model.

Current public render entry points are `render_oled_128x64`, `render_oled_128x128`, and `render_page`. The output type is `TextFrame`, with `Oled128x64Frame` and `Oled128x128Frame` aliases for the built-in display sizes.

`muninn-gate-platform-esp32::display` owns the current SSD1306 transport for WiFi LoRa 32 V4.x: VEXT on `GPIO36`, reset on `GPIO21`, I2C0 SDA/SCL on `GPIO17`/`GPIO18`, and address `0x3C`.

## Primary Screen

The primary 128x64 screen is a compact dashboard:

- Top-right: gateway battery icon from `GatewayMetrics.battery_percent`,
  followed by WiFi signal bars when WiFi is configured, or `USB` for serial-only
  configurations.
- Header: gateway name and active HTTP endpoint or concise runtime state.
- Remaining rows: telemetry producers.

Provisioning screens use the framed notice style: firmware label, status, and one short detail line. Rejection screens show a compact reason such as `INVALID JSON`, `MISSING FIELD`, or `INVALID CONFIG`. USB serial remains the detailed log and JSON response channel.

WiFi startup and reconnect use a dedicated animated screen. It shows the
gateway label, SSID, connection phase (`Connecting WiFi`, `Getting IP`, or
`Reconnecting`), and an inline animated 5-bar WiFi indicator while the station
associates and waits for DHCP.

If WiFi cannot connect during boot, the display leaves the animation and shows a
framed `WIFI FAILED` page with a compact reason such as `CHECK SSID/PASS` or
`NO IP ADDRESS`.

The main OLED screen should not show HTTP route hints such as `/poll`. Those
belong in README, serial logs, and HTTP tooling.

Producer rows use this layout:

```text
<signal> <name> <last-heard> <value>
```

- `<signal>` is a graphical 0-3 RSSI bar indicator, or `ERR` when the latest attempt failed or RSSI is unavailable.
- `<name>` is the configured or discovered producer name truncated to 10 characters with `~`.
- `<last-heard>` is the age of the last telemetry heard from that producer, based on monotonic milliseconds.
- `<value>` is selected by config.

The current RSSI indicator thresholds are: `0` at `<= -125 dBm`, `1` at `<= -110 dBm`, `2` at `<= -95 dBm`, and `3` above `-95 dBm`.

The configured display value is `display.node_display`. Supported labels are `temperature`, `humidity`, `soc`, `battery_voltage`, `pressure`, `rssi`, and `latency`. `soc` prefers battery percentage and falls back to voltage. `latency` currently uses the latest gateway poll latency until per-producer latency is tracked. `display.brightness_percent` is a 0-100 request mapped by the platform display driver to the concrete OLED controller.

For decoded MeshCore/LPP values, the display currently reads
`ProducerTelemetry.metrics`, the producer-level default metric set. That default
is copied from the first channel that reports the selected metric type. This
keeps the 128x64 row compact, but it is not suitable for producers with
multiple equivalent channels such as multi-rail voltage monitors. Those
producers should eventually use explicit display mapping in config, for example
selecting `battery_voltage` from channel `2`.

## Secondary Pages

The renderer also keeps simple secondary pages for bring-up and debugging:

- `Config`: gateway name, MeshCore public-key identity, effective output path, polling policy, TX level, frequency, HTTP state, display state, and selected node display value.
- `Wifi`: HTTP off/station state, SSID, active HTTP endpoint if serving, token count, and effective output path.
- `Producers`: node list with the configured producer value.

## 128x64 And 128x128 Layout

The default text layout assumes roughly 21 columns on a 128-pixel-wide display with a compact 5x7 font.

- 128x64: 8 text rows, intended for one gate status row, endpoint or setup detail, and producer rows.
- 128x128: 16 text rows, intended for the same data with more producers visible.

The renderer clips long lines to the frame width and stops adding optional lines when the frame is full. It should remain useful even when gateway names, board names, or producer names are longer than the physical display can show.

## Privacy And Secrets

The display must not show WiFi passwords, bearer tokens, MeshCore private keys, or full credential material. It may show:

- gateway name and truncated MeshCore public-key identity
- board name
- SSID
- token count
- effective output path
- selected TX level and frequency
- producer names or compact IDs
- RSSI signal indicator, last-heard age, and the configured producer telemetry value

## Refresh Policy

Display refresh should be driven by the board/platform crate. A good starting policy is to redraw when one of these changes:

- runtime state changes
- WiFi endpoint changes
- telemetry snapshot changes after a producer poll
- diagnostics or config state changes
- a page rotation timer expires

On ESP32-S3, display refresh belongs with WiFi, HTTP, serial, storage, and provisioning work on the PRO CPU. LoRa/MeshCore stays on the APP CPU.

## Local Button

On WiFi LoRa 32 V4.x, the user/PRG button on GPIO0 requests an immediate
producer poll while the firmware is running. The OLED shows a framed `POLL
REQUESTED` acknowledgement for one second, then returns to the dashboard.

## Future Work

Likely additions:

- page rotation and button navigation policy
- icons or compact glyphs in the board display driver
- a diagnostics page backed by the diagnostic collector
- signal quality detail rows for RSSI/SNR
- board-specific battery ADC/fuel-gauge wiring where a variant exposes it
- heap/free memory and latency rows on larger displays
