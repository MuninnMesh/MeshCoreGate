# Bifrost Gate — ProS3 + SSD1327 + (future) E22P-915M30S

Bifrost Gate is a Muninn Gate target built from an Unexpected Maker
**ProS3 / ProS3[D]** (ESP32-S3-WROOM-1 N16R8, 16 MB flash, 8 MB PSRAM), an
Adafruit **SSD1327** 1.5 inch 128×128 4-bit grayscale I2C OLED, and a
future EBYTE **E22P-915M30S** LoRa module (SX1262 + integrated PA/LNA, 915
MHz, 30 dBm peak).

> **Display history**: the original plan called for an Adafruit 1673 SSD1351
> 128×96 color OLED over SPI. While we wait on the EYESPI cable, the
> firmware switched to the SSD1327 128×128 grayscale module wired over the
> STEMMA QT connector. The SSD1351 path is paused; the typed
> `DisplayVariant::Oled128x96Color` variant remains in core for future
> revival.

The current firmware milestone is **USB logging + I2C OLED UI + WiFi
station scan-only + battery telemetry + RGB status LED**. The E22P module
is not soldered yet — LoRa runtime is intentionally disabled.

## Firmware Wiring

- Board feature: `bifrost-pros3`
- Board variant: `muninn_gate_board_bifrost_pros3::BifrostProS3Gate`
- Platform family: ESP32-S3 / Xtensa (`xtensa-esp32s3-none-elf`)
- MCU board: Unexpected Maker ProS3 / ProS3[D]
- Display: Adafruit 4741 / 5634 family — **SSD1327 128×128 4-bit grayscale**, I2C
- Flash layout: 16 MB QSPI, default `partitions.csv` (reuses Heltec layout)
- PSRAM: 8 MB QSPI PSRAM on-board; **not** enabled in this runner yet
- Output interfaces: USB Serial/JTAG (native, `303a:1001` after first flash),
  SSD1327 grayscale OLED, esp-wifi STA (scan-only at this milestone)
- LoRa: E22P-915M30S placeholder; pins documented, runtime not started

## References

- ProS3 product page: <https://esp32s3.com/pros3.html>
- Unexpected Maker ESP32-S3 repo: <https://github.com/unexpectedmaker/esp32s3>
- Unexpected Maker pinout cards: <https://help.unexpectedmaker.com/docs/documentation/pinout-cards/>
- Adafruit SSD1327 product page: <https://www.adafruit.com/product/4741>
- Adafruit SSD1351 product page (paused): <https://www.adafruit.com/product/1673>
- E22P-915M30S product page: <https://www.ebyte.com/product/2617.html>
- Reference firmware for ProS3[D]: `/run/media/vz/Code/mesh/apps/polygon-pros3d/`

## Board Inventory

### ProS3 / ProS3[D]

- ESP32-S3 dual-core, up to 240 MHz.
- 16 MB QSPI flash, 8 MB QSPI PSRAM.
- USB-C with native USB-Serial/JTAG (`303a:1001` after we flash; ships as
  `303a:80d4` from CircuitPython factory).
- 2.4 GHz WiFi (802.11 b/g/n) + BLE 5.
- 2x 700 mA 3.3 V LDOs (LDO1 = core, LDO2 = STEMMA + RGB rail).
- STEMMA QT connector powered by LDO2.
- LiPo charging + MicroBlade LiPo connector.
- On-board MAX17048 I2C fuel gauge at `0x36`.
- WS2812 RGB LED on GPIO 18.
- ProS3[D] **only**: on-board 3D antenna + u.FL connector switchable via
  GPIO 11 (LOW = onboard, HIGH = external).

### Adafruit SSD1327 128×128 Grayscale OLED

- 1.5 inch diagonal, 128×128 pixels, **4-bit grayscale (16 levels)**.
- SSD1327 controller, I2C or SPI; we run it over I2C from STEMMA QT.
- On-board 3.3 V LDO + 12 V boost + level shifters → safe with 3 V or 5 V
  logic; we feed 3 V3.
- I2C addresses: default `0x3D`; solder jumper to `0x3C`.
- 8 KiB framebuffer (128 × 128 × 4 bits / 8) lives in `'static`, not on
  the runner stack.
- No backlight — OLED pixels emit directly. Mind burn-in: keep brightness
  conservative and let the panel idle off when not actively displaying.

### EBYTE E22P-915M30S (future)

- SX1262-based 915 MHz LoRa module with integrated PA/LNA/FEM.
- Max conducted TX 30 dBm (~650 mA peak at 30 dBm).
- 32 MHz TCXO on-module.
- SPI control + BUSY/DIO1/NRST. FEM typically driven internally via SX1262
  DIO2; RXEN/TXEN pads usually NC.
- Module footprint ~38.5 × 24 mm.

## Pin Plan

Confirmed via `arduino-esp32 variants/um_pros3/pins_arduino.h` and the
working `mesh/apps/polygon-pros3d` reference firmware.

### ProS3 Board Services

| Function | GPIO | Notes |
| --- | --- | --- |
| USB D+/D− | 19 / 20 | Native USB-Serial/JTAG; do not reuse |
| BOOT button | 0 | Strap; input-only post-boot |
| I2C SDA | 8 | STEMMA QT — also reaches MAX17048 fuel gauge |
| I2C SCL | 9 | STEMMA QT |
| VBAT sense | 10 | ADC, on-board 1:3 divider |
| VBUS sense | 33 | HIGH when USB 5 V present |
| LDO2 enable | 17 | Drive HIGH to power STEMMA QT + RGB LED |
| WS2812 RGB | 18 | RMT channel 0, 80 MHz source, divider=1 (12.5 ns/tick) |
| Antenna RF switch | 11 | ProS3[D] only. LOW = onboard 3D, HIGH = external u.FL |

### SSD1327 OLED — connected via STEMMA QT

| Signal | ProS3 GPIO | Notes |
| --- | --- | --- |
| SDA | 8 | Shared I2C0 bus |
| SCL | 9 | Shared I2C0 bus |
| VIN | 3V3 (STEMMA pin) | On-board 12 V boost handles the panel itself |
| GND | GND | |
| Address | `0x3D` | Solder jumper moves it to `0x3C` |

### Future E22P-915M30S (peripheral, not soldered yet)

| Signal | ProS3 GPIO | Notes |
| --- | --- | --- |
| SCK | 36 | Dedicated SPI2 bus (separate from I2C STEMMA) |
| MOSI | 35 | SPI2 |
| MISO | 37 | SPI2 |
| NSS | 38 | Dedicated CS |
| BUSY | 39 | Input |
| DIO1 | 40 | IRQ input |
| NRST | 41 | Driver-controlled |
| RXEN | 42 | Only if E22P exposes external FEM control |
| TXEN | 21 | Only if E22P exposes external FEM control |
| VCC | see "Power notes" below | 3.3 V, 650 mA peak at 30 dBm |
| GND | GND | Thick traces / multiple pads for current return |
| ANT | external SMA / stamp-hole | **Install antenna BEFORE first TX** |

Free GPIOs still available: 1, 2, 3, 6, 7, 12, 13, 15, 16, 43, 44. GPIO 3 is
a strap; avoid for outputs that flip at boot. GPIO 43/44 are UART0 TX/RX —
leave free for a fallback console.

### Power notes for E22P

ProS3 LDO1 is 700 mA and ProS3 itself draws ~200 mA in WiFi TX, so the
3V3 rail cannot hold 30 dBm. Options, safest first:

1. **Separate buck** from VBUS or VBAT feeding only E22P VCC (Adafruit 4711
   or AP3429). **Recommended for any path that wants 30 dBm.**
2. **Bulk decoupling**: 220 µF low-ESR cap as close to E22P VCC as
   possible, plus 100 nF. Cap TX at ≤ 22 dBm in firmware.
3. **Direct 3V3 + permanent 22 dBm cap**: cheapest, loses long-link gain.

Until option 1 is in place, firmware keeps `allow_high_power = false` and
caps TX at ≤ 22 dBm.

## Firmware Architecture

The Bifrost path is intentionally isolated from `run_gateway` (which is
hard-wired to the Heltec V4 GPIO / SX1262 / SSD1306 map). Code lives in
`crates/muninn-gate-platform-esp32/src/bifrost_pros3/`:

```
bifrost_pros3/
├── antenna.rs     — AntennaSwitch + AntennaPath
├── battery.rs     — Max17048 + BatterySample + BatteryError
├── board.rs       — BoardServices facade (owns every peripheral handle)
├── display.rs     — SSD1327 driver (Gray4 DrawTarget, 8 KiB framebuffer)
├── i2c_bus.rs     — Shared blocking I2C bus (RefCell + RefCellDevice)
├── power.rs       — Ldo2Rail
├── status_led.rs  — StatusLed + Rgb
├── ui.rs          — Public render() + Screen enum + luma + fonts
├── ui/header.rs   — Header chrome (title + battery icon + WiFi bars)
├── ui/provisioning.rs  — Current top-level screen
├── ui/status.rs   — Forward-looking online status screen
├── ui/wifi.rs     — Reusable AP-list renderer + dedicated scan screen
├── ui/state.rs    — UiState, NetworkPhase, WifiAp, AuthLabel
└── wifi.rs        — WifiScanner (esp-wifi STA scan-only)
```

`BoardServices::init()` consumes the HAL peripheral block once and produces
typed owning handles. Failed peripherals leave their `Option<_>` slot
`None`; the runner short-circuits work that depends on a missing
capability instead of panicking. Boot order is documented inline in
`board.rs`.

## Build And Flash

The Xtensa GCC must be on `PATH` so `ldproxy` finds the linker:

```sh
export PATH="$HOME/.rustup/toolchains/esp/xtensa-esp-elf/esp-15.2.0_20250920/xtensa-esp-elf/bin:$PATH"
```

Build (release, Xtensa target):

```sh
cargo build -p muninn-gate-firmware \
  --no-default-features --features bifrost-pros3 \
  --target xtensa-esp32s3-none-elf --release
```

Flash (do **not** pass `--monitor` — `espflash monitor` hangs in
non-interactive contexts; use the local `tools/monitor.py` instead):

```sh
espflash flash --chip esp32s3 --flash-size 16mb \
  --partition-table partitions.csv \
  --port /dev/ttyACM0 \
  target/xtensa-esp32s3-none-elf/release/muninn-gate
```

Monitor:

```sh
uv run python tools/monitor.py -p /dev/ttyACM0 -d 30 --no-color
```

The ProS3 enumerates as `303a:80d4` from the CircuitPython factory image
and switches to `303a:1001` (Espressif USB JTAG/serial debug unit) after
our firmware is flashed.

## Milestone 1 Runtime

Boot behaviour on a populated board (OLED + battery + WiFi):

1. `esp_hal::init` at max CPU clock; 70 KiB DRAM2 + 32 KiB DRAM internal
   heap allocated to host esp-wifi DMA and the OLED framebuffer.
2. **Antenna**: GPIO 11 driven HIGH (external u.FL).
3. **LDO2**: GPIO 17 driven HIGH; 50 ms rail settle.
4. **Shared I2C bus**: I2C0 @ 400 kHz on GPIO 8 / GPIO 9, parked in a
   `'static RefCell`.
5. **MAX17048**: quick-start sent (write `0x4000` to MODE), 1.5 s settle.
6. **SSD1327**: init command sequence + `clear` to background.
7. **RGB LED**: WS2812 via RMT, latched OFF.
8. **WiFi scanner**: `esp-wifi` init → STA `Client(default)` →
   `controller.start()`. No association attempted.
9. USB banner printed; poll loop entered.

Poll loop (1 Hz):

- Read MAX17048 (`voltage_mv` + `soc_percent`).
- Every 15 s: blocking WiFi scan, populate top 8 APs by RSSI.
- Render the **provisioning** screen (header chrome + "Configuration
  Required" prompt + top 3 APs with `ch{N}/{security}` and signal bars +
  bottom "Connect USB & Configure" hint).
- Drive the WS2812 to the SOC-mapped color (red 0 % → yellow 50 % → green
  100 %, dim blue when SOC unknown).
- Heartbeat status line on USB Serial/JTAG every 5 ticks.

LoRa stays disabled (no `MeshRadio`, no `MeshcoreClient`, no
`PollScheduler` constructed).

## Open Questions

- Exact ProS3 vs ProS3[D] SKU and schematic revision.
- WiFi credential source for first hardware test (config storage path for
  the Bifrost variant is still TBD; right now the title is hard-coded
  `[???]` until USB provisioning lands).
- Whether ProS3[D] antenna switch should remain `ExternalUfl` (current) or
  be surfaced as a UI/config toggle.
- Partition table reuse — the current `partitions.csv` works because both
  boards are ESP32-S3-WROOM-1 N16R8.
- Final SSD1351 + SPI bring-up once the EYESPI cable arrives (the
  `Oled128x96Color` variant remains in core for that path).
- E22P module supply path — buck vs decoupled rail vs direct (see "Power
  notes" above).
- Safe TX power table for E22P before radio enablement.
