# SSD1327 — 128×128 4-bit Grayscale OLED Driver

Manufacturer: Solomon Systech Limited
Reference: [SSD1327 datasheet](https://www.solomon-systech.com/wp-content/uploads/2024/06/SSD1327-Datasheet.pdf)

## What it is

96 × 96 to 128 × 128 OLED segment/common driver with built-in 16-grayscale
LUT, supporting I²C and 8080/6800 parallel and 4-wire SPI. The Adafruit
4741 / 5634 module wires it as 128 × 128 over I²C with on-board 3V3 LDO,
12 V boost, and level shifters — host only needs a vanilla 3V3 supply
and an I²C bus.

Used on Muninn Gate's Bifrost ProS3 over the shared STEMMA QT bus.

## Electrical

| Param            | Value                                         |
|------------------|-----------------------------------------------|
| Logic supply     | 1.65 V – 3.5 V                                |
| Display supply   | 12 V (boost generated on Adafruit modules)    |
| Pixel grid       | 128 × 128                                     |
| Color depth      | 4-bit (16 grayscale levels) per pixel         |
| I²C clock        | ≤ 400 kHz documented, 800 kHz works in practice |
| Boot delay       | None mandatory; first command can land immediately after power-good |

## I²C address

Default **`0x3D`** (Adafruit 4741 / 5634); a solder jumper moves it to
**`0x3C`**. The driver `Ssd1327::new(...)` takes the address as a
parameter so both are supported.

## Framebuffer layout

The panel takes pixels packed two-per-byte horizontally:

```
byte[0] = (left-pixel-luma << 4) | right-pixel-luma
```

Total framebuffer at 128 × 128 × 4 bpp = `128 * 128 / 2 = 8192 bytes`.

This driver expects a `'static mut [u8; 8192]` slot passed by the
caller (`StaticCell` is the typical owner). Keeps the driver heap-free.

## I²C protocol

Every write is prefixed with a 1-byte control word:

- **`0x00`** → next bytes are command(s) + args
- **`0x40`** → next bytes are display RAM data

Multiple data bytes can follow a single `0x40` with no STOP in between.
The full-frame flush in this driver uses one `0x00` (to re-arm the
column/row window) followed by one `0x40` + 8 KiB of pixel data via
`embedded_hal::i2c::I2c::transaction`, amortizing the START + addr over
the whole frame instead of repeating it 512 × per chunk.

## Boot command sequence

Cribbed from the SSD1327 datasheet "recommended initialization" plus
the working `ssd1327-i2c` 0.2 crate (which targets embedded-hal 0.2; we
target 1.0 so we can use esp-hal directly).

| Cmd  | Args   | Purpose                                                |
|------|--------|--------------------------------------------------------|
| 0xFD | 0x12   | Command unlock (mandatory for write-protect-on parts). |
| 0xAE | —      | Display OFF.                                           |
| 0x15 | 0x00 0x3F | Column address range (0..=63 → 128 pixels @ 2/byte). |
| 0x75 | 0x00 0x7F | Row address range (0..=127).                        |
| 0x81 | 0x80   | Contrast (~50 %, comfortable bring-up value).          |
| 0xA0 | 0x51   | Remap: column + nibble + COM split odd/even.           |
| 0xA1 | 0x00   | Display start line 0.                                  |
| 0xA2 | 0x00   | Display offset 0.                                      |
| 0xA4 | —      | Normal display mode (not all-on / all-off / inverse).  |
| 0xA8 | 0x7F   | Multiplex ratio = 128 (full panel).                    |
| 0xB1 | 0x51   | Phase length.                                          |
| 0xB3 | 0x00   | Front clock divider / oscillator freq.                 |
| 0xAB | 0x01   | Function selection A: internal VDD regulator.          |
| 0xB6 | 0x04   | Second pre-charge period.                              |
| 0xBE | 0x0F   | VCOMH.                                                 |
| 0xBC | 0x08   | Pre-charge voltage.                                    |
| 0xD5 | 0x62   | Function selection B: enable second precharge + ext VSL. |
| 0xB9 | —      | Default linear grayscale table.                        |
| 0xAF | —      | Display ON.                                            |

## Gotchas

- **Don't skip 0xFD 0x12**. Some SSD1327 lots ship with command write
  protect ON; without the unlock you can never set contrast / window /
  enable.
- **OLED burn-in**: pixels emit light directly, so static UI elements
  (chrome, headers, fixed-position icons) fade unevenly over weeks of
  always-on use. Keep contrast modest, blank the panel on long idle.
- **Shared I²C bus**: this driver takes a generic `embedded-hal 1.0`
  `I2c` device. Drive it through `embedded_hal_bus::i2c::RefCellDevice`
  when sharing with another peripheral (e.g. MAX17048 on Bifrost).
- **Re-arm on every flush**: a previous partial-window write can leave
  the cursor mid-panel. This driver re-arms the column + row window on
  every flush so frame-level paints stay deterministic.

## See also

- [Adafruit 4741 product page](https://www.adafruit.com/product/4741)
- [Adafruit 5634 product page](https://www.adafruit.com/product/5634)
- [`ssd1327-i2c` reference impl on crates.io](https://crates.io/crates/ssd1327-i2c)
