# Bifrost Gate — ProS3 + SSD1327 + Seeed Wio-SX1262

Bifrost Gate is a Muninn Gate target built from an Unexpected Maker
**ProS3 / ProS3[D]** (ESP32-S3-WROOM-1 N16R8, 16 MB flash, 8 MB PSRAM), an
Adafruit **SSD1327** 1.5 inch 128×128 4-bit grayscale I2C OLED, and a
**Seeed Studio Wio-SX1262** 915 MHz LoRa module (Semtech SX1262 + on-module
TCXO + RF switch + u.FL antenna, ~22 dBm peak conducted output).

> **Radio history**: the original plan called for an EBYTE
> **E22P-915M30S** (SX1262 + integrated PA at 30 dBm). We swapped to the
> Seeed Wio-SX1262 to simplify the supply path (no high-current buck) and
> trade peak TX power for a smaller footprint + single 3V3 rail. The chip
> is the same SX1262; the host-side interface (SPI + BUSY + DIO1 + RESET)
> is unchanged. The change frees up `RXEN`/`TXEN` (the on-module RF
> switch is driven internally via the SX1262's `DIO2`).

> **Display history**: the original plan called for an Adafruit 1673 SSD1351
> 128×96 color OLED over SPI. While we wait on the EYESPI cable, the
> firmware switched to the SSD1327 128×128 grayscale module wired over the
> STEMMA QT connector. The SSD1351 path is paused; the typed
> `DisplayVariant::Oled128x96Color` variant remains in core for future
> revival.

The current firmware milestone is **USB provisioning + flash-persistent
config + WiFi association + DHCP + animated UI + RGB phase indicator**.
The LoRa runtime is gated on `ui_state.lora_available` flipping `true`
once the SX1262 driver lands; until then the status LED blinks fast red
once WiFi reaches Online (= "radio not wired" critical state).

## Firmware Wiring

- Board feature: `bifrost-pros3`
- Board variant: `muninn_gate_board_bifrost_pros3::BifrostProS3Gate`
- Platform family: ESP32-S3 / Xtensa (`xtensa-esp32s3-none-elf`)
- MCU board: Unexpected Maker ProS3 / ProS3[D]
- Display: Adafruit 4741 / 5634 family — **SSD1327 128×128 4-bit grayscale**, I2C
- Flash layout: 16 MB QSPI, default `partitions.csv` (reuses Heltec layout)
- PSRAM: 8 MB QSPI PSRAM on-board; **not** enabled in this runner yet
- Output interfaces: USB Serial/JTAG (native, `303a:1001` after first flash),
  SSD1327 grayscale OLED, esp-wifi STA (associate + DHCP)
- LoRa: Seeed Wio-SX1262 — pinout documented below; driver bring-up in
  progress (`bifrost_pros3/lora.rs`)

## References

- ProS3 product page: <https://esp32s3.com/pros3.html>
- Unexpected Maker ESP32-S3 repo: <https://github.com/unexpectedmaker/esp32s3>
- Unexpected Maker pinout cards: <https://help.unexpectedmaker.com/docs/documentation/pinout-cards/>
- Adafruit SSD1327 product page: <https://www.adafruit.com/product/4741>
- Adafruit SSD1351 product page (paused): <https://www.adafruit.com/product/1673>
- Seeed Wio-SX1262 wiki: <https://wiki.seeedstudio.com/wio_sx1262_xiao_esp32s3_kit/>
- Semtech SX1262 datasheet: <https://www.semtech.com/products/wireless-rf/lora-connect/sx1262>
- E22P-915M30S product page (paused / superseded): <https://www.ebyte.com/product/2617.html>
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

### Seeed Studio Wio-SX1262

- Semtech SX1262 LoRa transceiver, 915 MHz ISM band.
- ~22 dBm peak conducted TX (~120 mA peak draw on 3V3). No external PA.
- On-module 32 MHz TCXO; chip drives TCXO power via internal `DIO3`.
- On-module RF switch driven by the chip's internal `DIO2` — no host
  TXEN/RXEN required.
- Host interface: SPI (MOSI, MISO, SCK, NSS) + `BUSY` + `DIO1` + active-low
  `RESET`. 7 signals, single 3V3 rail.
- u.FL / IPEX MHF1 antenna connector on-module.

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
| Poll button | 15 | Active-low input; wire momentary button to GND |
| Active buzzer | 16 | Output; active module IO pin. Module VCC=3V3, GND=GND |
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

### Seeed Wio-SX1262 (LoRa peripheral)

The control signals are routed through the GPIO matrix to free pins — the
SX1262 maxes out at 16 MHz SPI, so the GPIO-matrix routing penalty (vs.
SPI2 IOMUX) is irrelevant.

> **⚠️ Octal-PSRAM pin conflict (root cause of a multi-day bring-up
> failure):** the original plan put SPI2 on **GPIO 35/36/37**. The ProS3
> is an **ESP32-S3-WROOM-1 N16R8** — the **R8 = 8 MB *octal* PSRAM**,
> which is bonded inside the module package to **GPIO 33–37**
> (SPIIO4–SPIIO7 + SPIDQS). Those pins are **not usable as general I/O**
> even with PSRAM disabled in firmware — the PSRAM die loads/contends the
> nets, giving intermittent-then-dead SPI (reads return `0x00`/`0xFF`,
> writes silently dropped). SCK/MOSI/MISO were moved to **GPIO 12/13/14**
> (confirmed-free). NSS/BUSY/DIO1/RESET (38/39/40/41) and RF_SW (21) were
> never affected — none are PSRAM pins.

| Signal | ProS3 GPIO | Direction | Notes |
| --- | --- | --- | --- |
| SCK | 12 | output | SPI2 SCK (was 36 — octal-PSRAM pin, unusable) |
| MOSI | 14 | output | SPI2 MOSI (was 35 — octal-PSRAM pin, unusable) |
| MISO | 13 | input  | SPI2 MISO (was 37 — octal-PSRAM pin, unusable) |
| NSS | 38 | output | active-low CS |
| BUSY | 39 | input | host MUST poll low before every SPI write |
| DIO1 | 40 | input/IRQ | rising-edge IRQ (rx done / tx done / timeout) |
| RESET | 41 | output | active-low; pulse ≥ 100 µs at boot |
| RF_SW (module pin 1) | 21 | output | **must drive HIGH before reset** — gates on-module RF switch + TCXO supply; leaving it floating prevents the chip from completing boot calibration. Originally tried GPIO 34, but on the ProS3[D] revision GPIO 34 drives the on-board antenna mux and holding it HIGH kills WiFi association. |
| VCC | 3V3 | power | ProS3 3V3 LDO, ≤ 200 mA budget (peak TX ~120 mA) |
| GND | GND | ground | star-ground at module pad if possible |
| ANT | u.FL → 915 MHz whip | RF | **install antenna BEFORE first TX** |

Free GPIOs after this allocation: 1, 2, 3, 6, 7, 42, 43, 44. GPIO 3 is a
strap (avoid for outputs that toggle at boot); GPIO 43/44 are UART0 TX/RX
(leave free as fallback console). On the ProS3[D] revision **avoid GPIO
34** as a general output — it drives the on-board antenna mux.

### Wiring notes

- **Bundle the SPI bus**: SCK / MOSI / MISO / NSS together. Run them as a
  single 4-wire ribbon (or twisted set) ≤ 10 cm. Keep return-current paths
  short — solder GND at both ends of the ribbon if the run is long.
- **Module decoupling**: the Wio-SX1262 ships with its own on-board
  decoupling (visible bulk + bypass caps on the silkscreen). No extra
  caps required on the host side unless the VCC wire is unusually long
  (>10 cm) and you observe rail droop under TX.
- **`RF_SW` is mandatory**: the firmware drives GPIO 34 HIGH at probe
  time. The chip needs `RF_SW` HIGH to power its TCXO; leaving the wire
  off prevents boot calibration from completing and `BUSY` never drops.
- **RESET pull-up**: optional — 10 kΩ from `RESET` to `3V3` so the host
  never accidentally floats the line low and re-holds the chip in reset.
- **DIO1 noise**: the line is an interrupt-driven input. If the ribbon
  picks up SPI clock crosstalk, add a 22 pF cap from `DIO1` to `GND`
  near the host — usually unnecessary at ≤ 10 cm.
- **Antenna trace**: keep the u.FL→antenna run as short as possible.
  Every extra 2-3 cm of unshielded coax at 915 MHz costs ~0.5 dB. A
  hand-wired 50 Ω whip cut to ¼-wave (~8 cm) works for bench testing.
  Don't TX into open air without an antenna — return loss can fry the PA.

## Firmware Architecture

The Bifrost path is intentionally isolated from `run_gateway` (which is
hard-wired to the Heltec V4 GPIO / SX1262 / SSD1306 map). Code lives in
`crates/muninn-gate-platform-esp32/src/bifrost_pros3/`:

```
bifrost_pros3/
├── antenna.rs     — AntennaSwitch + AntennaPath (WiFi RF mux on GPIO 11)
├── battery.rs     — Max17048 + BatterySample + BatteryError
├── board.rs       — BoardServices facade (owns every peripheral handle)
├── display.rs     — SSD1327 driver (Gray4 DrawTarget, 8 KiB framebuffer)
├── i2c_bus.rs     — Shared blocking I2C bus (RefCell + RefCellDevice)
├── lora.rs        — Wio-SX1262 driver (SPI2 + GPIO + reset; in progress)
├── power.rs       — Ldo2Rail
├── status_led.rs  — StatusLed + Rgb + LedDriver phase machine
├── ui.rs          — Public render() + Screen enum + luma + fonts
├── ui/boot.rs     — Boot splash + Braille-default spinner pick
├── ui/connecting.rs — Connecting / Acquiring-IP / Error body screens
├── ui/header.rs   — Header chrome (title + battery icon + WiFi bars)
├── ui/operational.rs — Online screen + LoRa-not-wired warning panel
├── ui/provisioning.rs — Configuration-Required screen + AP list
├── ui/spinner.rs  — Pluggable spinner pack (QR/Braille/Arc/etc.)
├── ui/state.rs    — UiState, NetworkPhase, WifiAp, AuthLabel
├── ui/wifi.rs     — Reusable AP-list renderer + dedicated scan screen
└── wifi.rs        — WifiScanner (associate + DHCP via smoltcp)
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

## Boot + Runtime Behaviour

Boot order on a populated board (OLED + optional battery + WiFi, LoRa
in progress):

1. `esp_hal::init` at max CPU clock; heap allocated for esp-wifi DMA +
   the OLED framebuffer.
2. **Antenna**: GPIO 11 driven LOW (`Onboard3D`, currently required for
   reliable WiFi association — external u.FL needs an antenna attached).
3. **LDO2**: GPIO 17 driven HIGH; 50 ms rail settle.
4. **Shared I2C bus**: I2C0 @ 800 kHz on GPIO 8 / GPIO 9, parked in a
   `'static RefCell`.
5. **MAX17048**: quick-start sent (write `0x4000` to MODE), 1.5 s settle.
6. **SSD1327**: init command sequence + clear to background.
7. **RGB LED**: WS2812 via RMT, latched OFF; later driven by
   `LedDriver::current_color(now_ms)`.
8. **WiFi controller**: `esp-wifi` init → `set_configuration(default)`
   → `start()` → `wait_for_started` → `disable_power_save` →
   `set_max_tx_power(20 dBm)` (see [`wifi_common`](../../crates/muninn-gate-platform-esp32/src/wifi_common.rs)).
9. **LoRa** (when driver lands): SPI2 init on GPIO 35–38, GPIO 41 pulse,
   SX1262 version probe. On success, `ui_state.lora_available = true`
   and the LED override flips from blinking RED to solid BLUE.
10. **Stored config**: read `muninn_cfg` partition; if present, prefill
    UI state with name/SSID/producers.
11. USB banner printed.
12. **Splash** (800 ms): "Booting…" headline + Braille spinner; RGB does
    a one-shot RED → PURPLE → CYAN sweep (200 ms each).
13. WiFi `try_connect`: pre-scan + BSSID/channel pin + `connect()`.
14. **Connecting screen**: spinner + SSID + footer; LED solid ORANGE.
15. On association: inline DHCP via smoltcp (~2-5 s); LED blink BLUE.
16. **Operational screen**: IP:port headline + (future) producer list;
    LED solid BLUE — or fast-blink RED while `lora_available` is false.

Main poll loop (1 Hz, except `fast_poll_until_associated` which polls at
100 ms during initial WiFi up):

- Read MAX17048 (`voltage_mv` + `soc_percent`).
- USB-provisioning poll (re-uploadable while in `Connecting`/`Online`).
- Render the current screen (Provisioning / Connecting / Operational).
- Drive the RGB based on `(network_phase, config_loaded, lora_available)`
  via [`LedDriver`](../../crates/muninn-gate-platform-esp32/src/bifrost_pros3/status_led.rs).
- Heartbeat status line on USB Serial/JTAG every 5 ticks.

## Open Questions

- Exact ProS3 vs ProS3[D] SKU and schematic revision.
- Whether ProS3[D] antenna switch should remain `Onboard3D` (current,
  needed for WiFi reliability) or be surfaced as a UI/config toggle.
- Final SSD1351 + SPI bring-up once the EYESPI cable arrives (the
  `Oled128x96Color` variant remains in core for that path).
- Whether to share SPI2 with another peripheral later (a microSD card,
  for example) — would need to multiplex `NSS` and arbitrate around the
  SX1262's `BUSY` window.
- Safe TX power ceiling for the Wio-SX1262 on the bare 3V3 rail before
  voltage sag becomes visible on `VCC` under sustained TX.
