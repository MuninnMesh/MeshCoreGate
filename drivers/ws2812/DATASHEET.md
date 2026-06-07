# WS2812 / WS2812B — Integrated Smart RGB LED

Manufacturer: Worldsemi
Reference: [WS2812B datasheet (rev 2017)](http://www.world-semi.com/Certifications/WS2812B.html)

## What it is

A 5050-package addressable RGB LED that integrates the controller and the
RGB die into one part. Each pixel takes a 24-bit GRB color word over a
single one-wire signal and passes any extra bytes downstream to the next
pixel in the chain. Muninn Gate's ProS3 has one on-board WS2812 on
GPIO 18 used as the status indicator.

## Electrical

| Param           | Value                                            |
|-----------------|--------------------------------------------------|
| Supply (VDD)    | 3.5 V – 5.3 V (3V3 fine, 5 V brighter)           |
| Logic high      | ≥ 0.7 × VDD (i.e. ~2.3 V at 3V3 — works on 3V3 IO) |
| Current per pixel | ≤ 60 mA full white                             |
| Quiescent       | ~1 mA per pixel (very dim "off" — clamp to OFF) |
| Refresh rate    | up to 30 kHz per pixel                           |
| Color depth     | 24-bit (8 bit × 3 channels)                      |

## Wire protocol (one-wire NRZ)

| Symbol | Description       | T_high (ns) | T_low (ns) | Tolerance |
|--------|-------------------|-------------|------------|-----------|
| T0H    | "0" high time     | 400         | —          | ±150 ns   |
| T0L    | "0" low time      | —           | 850        | ±150 ns   |
| T1H    | "1" high time     | 800         | —          | ±150 ns   |
| T1L    | "1" low time      | —           | 450        | ±150 ns   |
| RES    | reset / latch low | —           | ≥ 50 µs    | longer is fine |

This driver uses the **WS2812B "classic"** numbers (T0H 400, T0L 800,
T1H 800, T1L 400) which work with both the original WS2812 and the
newer WS2812B parts. The looser WS2812 v1 envelope (T0H 350, T1H 700)
is NOT used because some "WS2812B clones" in the wild only honor the
tighter spec.

## Color order

**GRB**, MSB first, per channel. So to send "pure red" you transmit
`0x00 0xFF 0x00` (G, R, B). This driver's `Rgb::to_grb()` packs the
channels into a `u32` in the expected order.

## Reset / latch

After the last data bit, the line must stay LOW for ≥ 50 µs for the
chain to latch and start showing the new colors. This driver appends
an empty `PulseCode` at the end of the buffer which the RMT peripheral
holds at LOW for the rest of the configured tx-end window — comfortably
more than 50 µs.

## ESP32 RMT timing

This driver runs RMT on an 80 MHz source clock with `clk_divider = 1`,
yielding **12.5 ns per RMT tick**. The 24 data bits + 1 reset symbol
fit in a single 25-entry burst that the channel transmits in ~30 µs.

Pre-computed tick counts:

| Symbol | Ticks | ns    |
|--------|-------|-------|
| T0H    | 32    | 400   |
| T0L    | 64    | 800   |
| T1H    | 64    | 800   |
| T1L    | 32    | 400   |

## Gotchas

- **Brightness is exponential**: a `dim(4)` (right-shift 4) caps each
  channel at 15/255 — that's already comfortable in a desk-level
  enclosure. Run full-bright only if the LED is behind a diffuser.
- **PSU droop**: at full white on a 3V3 rail with a marginal LDO the
  voltage can sag enough that the next-pixel logic-high threshold
  fails. Symptom: random pixels show wrong colors. Cap brightness or
  upgrade the rail.
- **Data line termination**: at trace lengths > 20 cm consider a 220 Ω
  series resistor at the source to dampen reflections. The on-board
  ProS3 LED is right next to the ESP32-S3 so this isn't needed.
- **Powered by gated rail on ProS3**: the on-board WS2812 sits behind
  LDO2 (the STEMMA + RGB rail) — assert GPIO 17 HIGH before the first
  `set_color` or you'll just toggle a dead pin.

## See also

- [Adafruit NeoPixel Überguide](https://learn.adafruit.com/adafruit-neopixel-uberguide)
- [esp-hal RMT module docs](https://docs.rs/esp-hal/latest/esp_hal/rmt/index.html)
