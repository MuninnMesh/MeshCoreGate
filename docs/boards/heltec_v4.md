# WiFi LoRa 32 V4.x, ESP32S3 + SX1262 LoRa Node

WiFi LoRa 32 V4.x, ESP32S3 + SX1262 LoRa Node is the first concrete Muninn Gate board target. It is represented by `crates/muninn-gate-board-heltec-v4` and runs through the reusable ESP32-S3 platform layer in `crates/muninn-gate-platform-esp32`.

## Firmware Wiring

- Board feature: `heltec-v4`
- Board variant: `muninn_gate_board_heltec_v4::WifiLora32V4x`
- Platform family: ESP32-S3
- Flash layout: 16 MB flash with the project [partitions.csv](../../partitions.csv)
- Platform services: WiFi, HTTP, storage, serial, heap, and task startup from `muninn-gate-platform-esp32`
- Display class: 128x64 monochrome OLED
- Current radio class: SX1262
- Radio hardware: SX1262 path with TCXO enabled and a 20 ms startup delay
- Output interfaces: WiFi HTTP `/metrics` and USB/JTAG serial
- Task placement: LoRa/MeshCore on ESP32-S3 APP CPU; WiFi/HTTP/storage/serial on PRO CPU
- User button: GPIO0, active low, used for immediate poll requests after boot
- Battery sense: GPIO37 enables the divider and GPIO1 / ADC1_CH0 samples battery voltage

The local display should use the 128x64 frame from `muninn-gate-ui`. The board/platform display driver will own the concrete OLED controller, pins, font, and refresh policy.

The OLED sits behind the board Vext rail. Firmware holds Vext enabled, waits for
the rail to settle, and retries SSD1306 initialization because the panel can
occasionally miss the first reset/init cycle during USB-powered reboot.

Battery voltage is sampled by the APP CPU radio owner so the radio host callback,
gateway metrics, serial JSON, Prometheus, and OLED header all see the same
latest value.

Build the firmware with:

```sh
cargo check -p muninn-gate-firmware --features heltec-v4
```

Flash commands for this board should pass `--flash-size 16mb` and
`--partition-table partitions.csv` so the dedicated raw `muninn_cfg` config
partition is present.

## Board Nuances

The board uses an external front-end module, so the configured TX power level is not equal to conducted RF output. The board variant owns this policy through `GateFirmwareVariant::map_tx_power`.

The current policy clamps the requested level to the radio driver's accepted range and attaches the nearest measured conducted-output estimate. TX 18 to 20 is the practical efficient range; TX 22 is accepted but can cost significantly more current for little additional RF output depending on board and measurement conditions.

## TX Power Reference

These values are project reference measurements for board policy, not regulatory certification data. Actual compliance depends on region, frequency, duty cycle, antenna gain, feedline loss, and board variance.

| TX power level | Approx conducted output | Approx mW |
| --: | --: | --: |
| 1 | 7.0 dBm | 5 |
| 5 | 12.2 dBm | 17 |
| 10 | 20.3 dBm | 107 |
| 12 | 22.5 dBm | 179 |
| 14 | 24.3 dBm | 268 |
| 16 | 25.4 dBm | 349 |
| 18 | 27.2 dBm | 520 |
| 20 | 27.7 dBm | 593 |
| 22 | 27.2 dBm | 520 |

Practical guidance:

- Prefer the lowest setting that maintains the link.
- Treat TX 18 to 20 as the useful high-power range for this board.
- Avoid assuming the configured level is linear with output power.
- Revisit the mapping if board revisions or calibrated test data show a materially different curve.

## WiFi Reliability Notes

The ESP32-S3 WiFi radio is separate from the SX1262 LoRa TX power table above.
For WiFi, the platform firmware caps the ESP WiFi TX power request at about
15 dBm and logs the applied value reported by the driver. This is intentional:
maximum WiFi TX power can create larger current spikes and can destabilize the
ESP WiFi blob on small boards before it improves real LAN availability.

For stability testing, use a deterministic 2.4 GHz SSID with WPA2/AES, 20 MHz
channel width, and client isolation disabled when peers need to scrape
`/metrics`. A DHCP reservation for the gateway MAC is recommended.
