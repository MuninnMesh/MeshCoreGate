# Bifrost Gate ESP32-C6 LCD + E22P-915M30S

Bifrost Gate is the second concrete Muninn Gate board target. It uses a
Waveshare ESP32-C6-LCD-1.69 module as the MCU/UI carrier and the EBYTE
E22P-915M30S as the LoRa front end. The LoRa module is not soldered yet,
so milestone 1 is MCU + display + WiFi only; LoRa runtime is disabled.

## Firmware Wiring

- Board feature: `bifrost-c6`
- Board variant: `muninn_gate_board_bifrost_c6::BifrostC6Gate`
- Platform family: ESP32-C6 (RISC-V, single core)
- Platform crate: `muninn-gate-platform-esp32c6`
- Flash layout: 16 MB W25Q128JVSIQ (NOR flash, soldered)
- Output interfaces (milestone 1): USB-Serial/JTAG logging, color LCD,
  WiFi station
- LoRa: not wired (placeholder; E22P-915M30S integration is a later
  milestone)

## Hardware Inventory

- ESP32-C6 (32-bit RISC-V, WiFi 6, BT 5, IEEE 802.15.4)
- 1.69 inch 240×280 IPS LCD, 262K colors, ST7789 controller, SPI
- W25Q128JVSIQ 16 MB SPI NOR flash
- PCF85063 RTC (I2C)
- QMI8658 6-axis IMU (I2C)
- ETA6098 Li-ion charger IC
- MX1.25 2P battery header (3.7 V Li-ion, charge + discharge)
- USB-C using ESP32-C6 native USB-Serial/JTAG
- Onboard PCB antenna
- PWR button (power management; single/double/multi/long press)
- BOOT button (GPIO9)
- RST button (hardware reset)
- NS4150B 3.0 W class-D audio amp
- ES8311 mono audio codec
- Microphone
- MX1.25 speaker header

The E22P-915M30S is an EBYTE SX1262-based 915 MHz module with integrated
PA/LNA/FEM and 30 dBm conducted TX. SPI control, 3.3 V logic, 2.5–5.5 V
supply, ~650 mA TX peak.

References:

- Product page: <https://www.cdebyte.com/products/E22P-915M30S>
- Datasheet: <https://www.cdebyte.com/pdf-down.aspx?id=4270>
- User manual: <https://www.fr-ebyte.com/pdf-down.aspx?id=5937>

## Confirmed Pinout

Source: Waveshare official Arduino demo
(`Arduino/examples/{02_button_example, 03_battery_example, 04_es8311_example, 05_gfx_helloworld}.ino`
from `https://files.waveshare.com/wiki/ESP32-C6-LCD-1.69/ESP32-C6-LCD-1.69-Demo.zip`).

### LCD (ST7789, 240×280, IPS)

| Signal | GPIO |
| --- | --- |
| SCLK | 1 |
| MOSI (DIN) | 2 |
| DC | 3 |
| RST | 4 |
| CS | 5 |
| Backlight (BL) | 6 |

LCD-specific notes:

- Controller: ST7789-class. The Waveshare driver passes
  `width=240`, `height=280`, `col_offset_1=0`, `row_offset_1=20`,
  `col_offset_2=0`, `row_offset_2=20`, and `IPS=true`. The 20-row offset
  is required because the 280-tall frame buffer is mapped into the upper
  20 rows of an unused area of the controller's RAM window.
- SPI bus: 4-wire, no MISO. Use SPI2 or SPI3 (peripheral selection is a
  platform-crate decision).
- Backlight is active-high; driving GPIO6 LOW turns the screen off.

### Buttons

| Signal | GPIO | Notes |
| --- | --- | --- |
| BOOT | 9 | Active low, internal pull-up. Reused as USER button at runtime. |
| PWR | 18 | Active low. Supports single/double/multi/long press via debounce policy in firmware. |
| RST | n/a | Hardware reset; tied to EN. |

### Battery monitoring

| Signal | GPIO | Notes |
| --- | --- | --- |
| BAT_ADC | 0 | ADC1_CH0. Voltage divider scales VBAT to MCU-safe range; multiply ADC mV by 3 to recover battery voltage. |
| BAT_EN | 15 | Enable line for the divider. Drive HIGH before sampling; can be driven LOW to save standby current. |

The ETA6098 charger STAT/CHG line is not exposed on a confirmed GPIO in
the Waveshare examples. The schematic shows ETA6098 wiring but the
charging-status pin connection still needs to be verified before the
"charging" indicator can be driven from a discrete pin. As a fallback, the
firmware can infer charging by sampling battery voltage trend while USB
is present (USB-bus voltage detection itself is implicit: USB-Serial/JTAG
enumeration succeeded means USB is plugged).

> TODO: confirm ETA6098 STAT GPIO from the schematic. Until confirmed, the
> charging icon must use a heuristic (USB present + battery voltage
> rising) rather than a direct pin read.

### I2C bus (RTC + IMU + Codec)

| Signal | GPIO |
| --- | --- |
| SDA | 8 |
| SCL | 7 |

Shared by PCF85063 RTC, QMI8658 IMU, and ES8311 codec.

### Audio I2S (out of scope for milestone 1)

| Signal | GPIO |
| --- | --- |
| MCK | 19 |
| BCK | 20 |
| LRCK | 22 |
| DOUT | 23 |
| DIN | 21 |

## Toolchain

The workspace already uses the espup `esp` toolchain
(`rust-toolchain.toml: channel = "esp"`). That toolchain's `rustc`
already exposes `riscv32imac-unknown-none-elf` in `--print target-list`,
and the workspace's `[unstable] build-std = ["core", "alloc"]` config in
`.cargo/config.toml` builds the core/alloc sysroot from source for any
target. **No separate toolchain is needed for ESP32-C6** — same channel,
different `--target`.

Sysroot precompiled binaries are not installed for RISC-V; build-std
handles it transparently. First C6 build will compile core/alloc from
source (slower one-time cost).

The C6 target uses upstream Rust's standard linker path (`rust-lld` with
esp-hal's `linkall.x`). It does NOT need `ldproxy` or
`xtensa-esp32s3-elf-gcc` like the Heltec target.

## Build And Flash

After Phase 2 of the implementation order is merged:

```sh
cargo build -p muninn-gate-firmware \
  --no-default-features --features bifrost-c6 \
  --target riscv32imac-unknown-none-elf --release

cargo espflash flash \
  --chip esp32c6 --flash-size 16mb \
  --package muninn-gate-firmware --bin muninn-gate \
  --no-default-features --features bifrost-c6 \
  --target riscv32imac-unknown-none-elf --monitor
```

USB device enumerates as Espressif USB JTAG/serial debug unit
(`303a:1001`) on `/dev/ttyACM*`. The board exposes USB-Serial/JTAG
natively; no UART bridge is involved.

## E22P-915M30S Future Integration

Out of scope for milestone 1. When the module is soldered:

- The SX126x register/command driver in `muninn-mesh-sx126x` applies
  directly.
- The `muninn-mesh-radio` orchestration layer applies, with a new
  `MeshRadioConfig` shape that accounts for the module's integrated
  PA/LNA. The "selected TX power level → conducted dBm" table is the main
  per-board policy this gateway target must own.
- E22P exposes TXEN/RXEN FEM-control lines that must be driven by the
  board crate's `RadioSwitch` implementation. GPIO assignments depend on
  the carrier wiring chosen at solder time and must be captured here
  before bring-up.
- Module supply must source ~650 mA at peak TX. The board's existing
  battery boost path needs verification at that current before 30 dBm
  enablement.

Until the module is wired and verified:

- Boot log emits `Radio: not wired (E22P-915M30S placeholder)`.
- LCD main screen footer shows `LoRa: not wired`.
- No `MeshRadio`, `MeshcoreClient`, or `PollScheduler` runtime is started
  on Bifrost. The C6 platform crate does not depend on
  `muninn-mesh-radio` or `muninn-mesh-sx126x`.
- `GatewayCapabilities` is unchanged: no `lora` field is added in
  milestone 1. The serial banner, LCD footer, and this board document
  are the placeholder. A real capability field would be a premature core
  change with ripples through every existing board crate; add it later
  if and when the runtime actually needs to gate behavior on it.

## Open Questions

- ETA6098 charging-status GPIO connection (see TODO above).
- Whether the PWR button on GPIO18 supports a wake-from-deep-sleep
  pattern.
- Whether the W25Q128 layout supports the same `muninn_cfg` partition
  approach as Heltec (16 MB matches; bootloader offset of `0x10000` is
  the C6 default per esp-hal docs). Should be a drop-in.

## Power Caution For Future LoRa Bring-Up

This is **not** a 22 dBm radio path. Once the E22P is soldered:

- Verify supply at the module pins under TX load.
- Verify USB and battery sources separately under 30 dBm TX.
- Confirm antenna installed before any transmit; an unloaded PA at 30 dBm
  can damage the module.
- Compliance (region, duty cycle, antenna gain, feedline loss) is a
  separate workstream.
