# MAX17048 / MAX17049 — Single-Cell LiPo Fuel Gauge

Manufacturer: Analog Devices (originally Maxim Integrated)
Reference: [MAX17048 datasheet](https://www.analog.com/media/en/technical-documentation/data-sheets/MAX17048-MAX17049.pdf)

## What it is

A tiny (1.5 × 0.7 mm WLP) single-cell LiPo / Li-ion battery fuel gauge that
estimates state-of-charge (SOC) from cell voltage using the manufacturer's
ModelGauge™ algorithm — no current-sense resistor required.

Used on Muninn Gate's Unexpected Maker ProS3 (on-board), reachable over the
STEMMA QT I²C bus.

## Electrical

| Param         | Value                                            |
|---------------|--------------------------------------------------|
| Supply (VDD)  | 2.5 V – 4.5 V (typ. tied to VCELL)               |
| Quiescent     | 23 µA active, 4 µA sleep                         |
| Cell range    | 2.5 V – 5.0 V (MAX17049: 2.5 V – 10.0 V dual)    |
| Voltage LSB   | 78.125 µV                                        |
| SOC LSB       | 1/256 % (i.e. 0.00390625 %)                      |
| Update rate   | ~1 sample per second internal                    |
| Comms         | I²C, up to 400 kHz                                |

## I²C address

Fixed at **`0x36`** (7-bit). No address pins. The address is not
configurable. If you need two gauges on the same bus you must split
the bus or use an I²C mux.

## Register map (subset used here)

| Addr | Name      | Width | Notes                                                  |
|------|-----------|-------|--------------------------------------------------------|
| 0x02 | `VCELL`   | 16-bit| Cell voltage, 78.125 µV/LSB. `mV = raw * 78125 / 1e6`. |
| 0x04 | `SOC`     | 16-bit| State-of-charge, 1/256 %/LSB. `% = raw / 256`.         |
| 0x06 | `MODE`    | 16-bit| Write `0x4000` for quick-start.                        |
| 0x08 | `VERSION` | 16-bit| Read-only IC revision (use as a presence check).       |
| 0x0C | `HIBRT`   | 16-bit| Hibernate threshold config.                            |
| 0x14 | `CONFIG`  | 16-bit| RCOMP, alert thresholds, sleep, alerts.                |
| 0x18 | `VALRT`   | 16-bit| Min/max voltage alert thresholds.                      |
| 0x1A | `CRATE`   | 16-bit| Charge-rate %/hr (signed, 0.208%/hr per LSB).          |
| 0x1C | `VRESET`  | 16-bit| Reset comparator threshold and ID byte.                |
| 0xFE | `CMD`     | 16-bit| Write `0x5400` for full POR.                           |

All registers are big-endian on the wire: write `[addr, hi, lo]`, read
`[hi, lo]`. The driver in this crate handles that for you.

## Initialization

Bring up power, then send a **quick-start**: write `0x4000` to `MODE`
(`0x06`). The gauge re-derives SOC from the cell voltage it sees right
now instead of whatever stale state it powered up with. Wait ≥1 s
before treating subsequent SOC reads as settled. Allow ≥1.5 s on
Muninn Gate to align with the slower MeshCore housekeeping cadence.

If you skip the quick-start the first 30+ minutes of SOC reads can be
wildly off — the ModelGauge model has to converge from open-circuit
voltage on its own and there's no current measurement to help.

## Gotchas

- **Powered by the host rail you read**: on the ProS3 the gauge sits
  behind LDO2 (the STEMMA + RGB rail). Assert LDO2 enable (GPIO 17 on
  ProS3) and let it settle ~50 ms before the first I²C transaction.
- **Shared bus**: Bifrost's bus is shared with the SSD1327 OLED. Drive
  this gauge through an `embedded_hal_bus::i2c::RefCellDevice` (or
  equivalent) so the two drivers don't fight for the bus.
- **No current sense**: this part can't tell you charge/discharge
  current. If you need that, use a dedicated coulomb counter
  (e.g. LTC2944, INA226) or an INA3221 in series with the battery.
- **Voltage divider on host**: don't expose `VCELL` to an ADC that adds
  load — the gauge can be fooled by external pull-ups on the same
  battery node.
- **POR via 0xFE**: writing `0x5400` to `CMD` triggers a full power-on
  reset (every register goes back to defaults). Only do this on
  recovery from a hung state.

## See also

- [Adafruit MAX17048 breakout guide](https://learn.adafruit.com/adafruit-max17048-lipoly-liion-fuel-gauge-and-battery-monitor)
- [Analog Devices product page](https://www.analog.com/en/products/max17048.html)
