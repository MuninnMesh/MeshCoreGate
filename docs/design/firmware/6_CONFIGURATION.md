# Configuration

Configuration is a persistent document loaded at boot. USB is the primary provisioning and recovery path; WiFi provisioning is an optional later layer over the same document.

## Target Boot Flow

1. Board starts and initializes minimal serial/log output.
2. Storage backend attempts to load a config document.
3. Config is parsed and validated into `GatewayConfig`.
4. The selected board variant validates requested outputs against its capabilities.
5. If config is missing, invalid, or unsupported by the board, enter provisioning mode.
6. Provisioning mode accepts config over USB serial.
7. After a valid config is received, write it to platform storage and start services.
8. If valid `http` config exists, start WiFi and HTTP metrics.
9. On provisioned boots, open a short USB config update window before WiFi starts.

USB provisioning should be available even when WiFi is absent or broken. It is the recovery channel.

The ESP32 bring-up path stores the validated provisioning document in flash using two A/B sectors inside the dedicated raw `muninn_cfg` data partition from [partitions.csv](../../../partitions.csv). A newly flashed board enters USB provisioning mode, shows a setup-required OLED page, and prints concise USB serial provisioning instructions. Valid uploaded config is persisted and starts services in the same boot.

After provisioning, the ESP32 runtime opens a short USB update window before WiFi starts. Uploading a new valid config in that window writes it to flash and returns `reboot_required`; the running WiFi, radio, scheduler, and MeshCore identity are not hot-swapped. The device reboots to apply the replacement config.

On every provisioned boot, the ESP32 runtime prints a sanitized config summary to USB serial. It includes public keys, producer kinds, routing, radio, polling, display, and HTTP shape, but redacts WiFi passwords, bearer tokens, MeshCore private keys, and producer passwords.

## USB Provisioning Protocol

The USB provisioning protocol accepts a complete raw config object or a command envelope:

```json
{"type":"set_config","config":{...}}
{"type":"help"}
{"type":"status"}
{"type":"poll_now"}
{"type":"reboot"}
```

Responses should also be newline-delimited JSON:

```json
{"type":"ok","request":"set_config"}
{"type":"ok","request":"set_config","action":"reboot_required"}
{"type":"error","request":"set_config","code":"invalid_config","message":"producer list is full"}
```

The current USB provisioning implementation supports:

- raw config JSON/JSONC upload
- `set_config`
- `help`
- `status`
- `poll_now`
- `reboot`
- validation before replacing the active config
- malformed config errors on USB serial and OLED
- boot-time config replacement over USB after provisioning, before WiFi starts

## Config Format

JSON is the most practical wire format for USB and WiFi provisioning. It maps directly to the existing core model and is easy to send from scripts, Home Assistant automations, or a local browser UI.

The core crate can keep serialization feature-gated so normal firmware builds only include what the selected board enables.

The repository root [config.example.json](../../../config.example.json) is the strict upload-ready example. [config.example.jsonc](../../../config.example.jsonc) is the commented JSONC-style reference for people editing the file. Runtime provisioning strips `//` comments before parsing, so either shape can be accepted over USB serial.

The host-side [cli.py](../../../tools/cli.py) helper fills common template values, can generate gateway MeshCore key material with the optional `cryptography` package, and uploads the final config over USB serial using throttled writes. Generated MeshCore identities use a 32-byte Ed25519 public key and MeshCore's 64-byte expanded private-key representation. Firmware remains the source of truth for schema validation.

Suggested strict JSON shape:

```json
{
  "name": "MC Telemetry Gate V4",
  "http": {
    "port": 80,
    "tls": false,
    "wifi_ssid": "example-ssid",
    "wifi_password": "example-password",
    "tokens": ["bearer-token"]
  },
  "display": {
    "enabled": true,
    "node_display": "temperature",
    "brightness_percent": 60
  },
  "polling": {
    "default_interval_secs": 600,
    "jitter_secs": 10
  },
  "time": {
    "utc_offset_minutes": -360
  },
  "radio": {
    "frequency_hz": 910525000,
    "bandwidth_hz": 62500,
    "spreading_factor": 7,
    "coding_rate": 5,
    "tx_power_level": 14,
    "sync_word": 5156,
    "preamble_len": 16,
    "iq_inverted": false,
    "tx_ramp_time_us": 200
  },
  "meshcore": {
    "public_key": "gateway-public-key",
    "private_key": "gateway-private-key",
    "routing": {
      "path_mode": 2
    }
  },
  "producers": [
    {
      "public_key": "companion-public-key",
      "kind": "companion",
      "password": null,
      "name": "handheld_node",
      "enabled": true,
      "route": "direct"
    },
    {
      "public_key": "repeater-public-key",
      "kind": "repeater",
      "password": null,
      "name": "roof_repeater",
      "enabled": false,
      "route": "direct"
    }
  ]
}
```

Set `http` to `null` or omit it for serial-only output. When `http` is present, WiFi is used to serve HTTP and serial output remains enabled on boards that support it.

## Validation

Validation rules:

- Gateway name must fit its 32-byte limit.
- MeshCore public key is the gateway ID and must be present.
- Producer list must fit `MAX_TELEMETRY_PRODUCERS`.
- Producer names, passwords, legacy route strings, HTTP/WiFi strings, tokens, and MeshCore material must fit fixed-capacity limits.
- Producer public keys are producer IDs and must be unique.
- Producer `kind` is `companion` or `repeater`. Companion producers receive direct encrypted telemetry requests. Repeater producers use MeshCore login before telemetry requests.
- Producer `route` is retained for older provisioning documents, but the ESP32
  firmware normalizes all producer routes to direct-only outbound MeshCore TX.
  Flood and explicit path strings must not change what Muninn Gate transmits.
- Producer `password` is only valid for `kind: "repeater"`; set it for repeaters that do not allow unauthenticated polling.
- Poll intervals are seconds. The current user-facing config exposes the gateway default interval; core also retains bounded producer-override support for future provisioning UI work.
- Defaults should bias toward low channel utilization. The baseline polling interval is 10 minutes, and multiple producers are staggered across that interval.
- Retry count is bounded; the default is five retries after the initial attempt and the current maximum is 10.
- Jitter should be large enough to avoid synchronized restarts; the default window is 10 seconds and the current maximum is 60 seconds.
- `time.utc_offset_minutes` is a fixed display offset from UTC in minutes. The host CLI injects `time.unix_time_seconds` during USB upload unless `--no-time-sync` is used; an RTC-capable board may persist/query that value through its board hook.
- MeshCore `routing.path_mode` mirrors MeshCore `path.hash.mode`: `0` means
  1-byte hashes, `1` means 2-byte hashes, and `2` means 3-byte hashes. The
  current ESP32 outbound path is direct-only, so this setting is retained for
  compatibility and diagnostics rather than flood/path transmission.
- Radio frequency, spreading factor, bandwidth, coding rate, TX power level, sync word, preamble length, IQ mode, and ramp time must be valid for the selected region and radio configuration.
- `http`, when present, must be supported by the selected board capabilities.
- `http.tokens` may be omitted, `null`, or empty; that disables bearer-header auth.
- Display `node_display`, when display is enabled, should be one of `temperature`, `humidity`, `soc`, `battery_voltage`, `pressure`, `luminosity`, `rssi`, or `latency`. These values currently read from the producer-level default `TelemetryMetrics` view. Channel-specific display selection is intentionally not in the config yet; it should be added only when the UI needs to distinguish equivalent channels such as INA3221 rail 1/2/3.
- Display `brightness_percent`, when present, is a 0-100 OLED brightness request. The platform maps it to the concrete display controller.

The config stores the requested TX power level. The selected board variant maps that request to the driver setting actually used by the radio path and may attach an estimated conducted output power. This is important for boards with a front-end module, where the configured level and conducted output power are not linear.

Board-owned radio details such as TCXO startup behavior are intentionally not in the provisioning document. ESP32 board crates provide them through `Esp32BoardVariant`.

HTTP access is intentionally simple: the config supplies the TLS policy and optional bearer tokens. These tokens are separate from MeshCore public keys and should not grow into a heavier auth system until there is a concrete threat model requiring it.

## Storage Backends

Persistent storage is platform-specific. On ESP32-S3,
`muninn-gate-platform-esp32::storage` stores the original validated JSON/JSONC
provisioning document in the raw `muninn_cfg` flash partition and parses it
again on boot. This is a small direct flash A/B record format with magic,
version, sequence, payload length, and CRC; it is not NVS. On nRF52 this may be
internal flash settings storage.

Core should only receive an already validated `GatewayConfig`.

## WiFi Provisioning

WiFi provisioning is a convenience layer over the same config document. It should not replace USB recovery.

If implemented, WiFi provisioning should be gated by a config flag or a physical/provisioning-mode trigger, and it should require authentication before accepting updates. A safe first version is read-only WiFi config display plus USB-only writes.
