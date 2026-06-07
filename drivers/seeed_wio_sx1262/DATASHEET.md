# Seeed Wio-SX1262 — 868/915 MHz LoRa Module

Manufacturer: Seeed Studio
Reference: [Wio-SX1262 Kit for XIAO ESP32S3 wiki](https://wiki.seeedstudio.com/wio_sx1262_xiao_esp32s3_kit/)

## What it is

A small SX1262-based LoRa daughterboard intended for the Seeed XIAO socket
(plug a XIAO ESP32-S3 board on top, get LoRa). The module exposes the
SX1262 host interface plus power on castellated pads so it can be hand-wired
to any ESP32-class host — that's how Muninn Gate's Bifrost ProS3 uses it
(no XIAO plugged in; ProS3 talks to the same pads directly).

Used on the Bifrost ProS3 board variant.

## Electrical

| Param            | Value                                            |
|------------------|--------------------------------------------------|
| Supply           | 5 V via `5V` pad, on-board LDO → 3.3 V to chip   |
| Logic level      | 3.3 V                                             |
| Peak TX current  | ~120 mA at +22 dBm                                |
| TCXO supply      | Gated by SX1262 internal DIO3 (configurable)     |
| RF switch        | On-module, driven internally by SX1262 DIO2      |
| Antenna          | u.FL / IPEX MHF1 connector                       |
| Frequency band   | 868 MHz / 915 MHz (depends on regional variant)   |

## Pin map (silkscreen → SX1262 function)

The board has two castellated rows. Some pads are aliased — the same
electrical net is exposed under both an XIAO-socket name (e.g. `D0`) and
the SX1262 functional name (e.g. `RST`).

| Silkscreen | SX1262 function     | Notes                                  |
|------------|---------------------|----------------------------------------|
| `RST`      | NRST (active-low)   | Pulse LOW ≥ 100 µs to reset.           |
| `BUSY`     | BUSY                | HIGH while booting / executing.        |
| `NSS`      | NSS                 | Active-low chip select.                |
| `DIO1`     | DIO1                | Rising-edge IRQ (RX done / TX done / …)|
| `MOSI`     | SPI MOSI            |                                        |
| `MISO`     | SPI MISO            |                                        |
| `SCK`      | SPI SCK             |                                        |
| `RF_SW`    | TCXO supply gate    | **Must be HIGH for the chip to finish boot calibration.** See "Gotchas". |
| `5V`       | LDO input           | 5 V from host or USB.                  |
| `3V3`      | LDO output          | Do not drive — measure-only.            |
| `GND`     | GND                  |                                        |
| `D0`–`D10` | XIAO socket aliases | Same nets as the SX1262 pins above.    |
| `TX`/`RX`  | XIAO UART aliases   | Unused unless a XIAO is plugged in.     |

## Wiring on Bifrost ProS3

Confirmed via [`docs/boards/bifrost_pros3.md`](../../docs/boards/bifrost_pros3.md)
and end-to-end multimeter continuity. The exact GPIO map lives in
[`crates/muninn-gate-board-bifrost-pros3/src/lora.rs`](../../crates/muninn-gate-board-bifrost-pros3/src/lora.rs)
(`RawLoraPeripherals`).

| Module pad | ProS3 GPIO | Direction | Notes                              |
|------------|------------|-----------|------------------------------------|
| SCK        | 36         | output    | SPI2 SCK                            |
| MOSI       | 35         | output    | SPI2 MOSI                           |
| MISO       | 37         | input     | SPI2 MISO                           |
| NSS        | 38         | output    | active-low CS                       |
| BUSY       | 39         | input     | no internal pull (chip drives)      |
| DIO1       | 40         | input     | rising-edge IRQ                     |
| RST        | 41         | output    | idle HIGH, pulse LOW 1 ms at boot   |
| RF_SW      | 21         | output    | held HIGH continuously (TCXO supply) |
| 5V         | 5V         | power     | ProS3 5V rail (USB-derived)         |
| GND        | GND        | ground    | star-ground at module pad if possible |

## Boot sequence

1. Bring up the host 3V3 / 5V rails. The module's on-board LDO is up
   within microseconds of `5V` being valid.
2. Drive `RF_SW` HIGH **before** releasing reset. The SX1262 reads the
   TCXO-supply-enabled state during boot calibration; if the supply is
   off, `BUSY` will stay HIGH forever.
3. Pulse `RST` LOW for ≥ 100 µs (datasheet minimum; we use 1 ms for
   trace-capacitance margin).
4. Release `RST` HIGH; the chip runs internal boot calibration. `BUSY`
   should drop LOW within ~30 ms on this module (longer than the SX1262
   datasheet's typical ~3.5 ms because the chip is also waiting for the
   TCXO to stabilize via the `tcxo_delay_ms` setting).
5. Issue `SetDIO3AsTcxoCtrl(voltage, delay)` to formally tell the chip
   "TCXO is connected via DIO3 supply"; subsequent operations will
   honor the configured TCXO settle time.

## Module-specific RF wiring

- **RF switch is internal**: the on-module Skyworks SKY13373 (or
  equivalent) is wired to the chip's `DIO2`. No host TXEN/RXEN GPIO is
  needed — the SX1262 driver flips DIO2 internally between RX and TX.
- **`RF_SW` is NOT the antenna switch**: despite the silkscreen name,
  `RF_SW` on this Seeed module gates the TCXO supply path, not the
  antenna mux. We hold it HIGH continuously. Driving it LOW prevents
  boot calibration.
- **u.FL connector**: install a 50 Ω antenna *before* TX. Radiating
  into an unloaded connector reflects back into the PA and can damage it.

## Boot calibration gotcha (observed during Bifrost bring-up)

On the bench we measured **BUSY released after 29.6 ms** consistently —
nearly 10× the SX1262 datasheet's typical 3.5 ms. The delta is due to
the chip waiting for the TCXO supply to stabilize during the
`SetDIO3AsTcxoCtrl`-equivalent boot path. If you tighten
`BUSY_TIMEOUT_MS` below ~50 ms you'll start seeing spurious
"chip not detected" errors on cold boots — keep the budget at ≥ 100 ms.

## See also

- [Semtech SX1262 datasheet](https://www.semtech.com/products/wireless-rf/lora-connect/sx1262)
- [`muninn-mesh-sx126x` driver crate](../../crates/muninn-mesh-sx126x)
- [`muninn-mesh-radio` higher-level wrapper](../../crates/muninn-mesh-radio)
