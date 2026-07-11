# Epson V500 CCD Data Stream Specification

Reverse-engineered from logic analyzer captures of the CCD board parallel bus,
decoded by `examples/ccd_viewer.rs` and `examples/spi_controlled_decode.rs`.

---

## Table of Contents

1. [Physical Signal Lines](#1-physical-signal-lines)
2. [SPI Control Protocol](#2-spi-control-protocol)
3. [Binary Capture File Format](#3-binary-capture-file-format)
4. [TGCK Line Boundary File](#4-tgck-line-boundary-file)
5. [Pixel Data Format](#5-pixel-data-format)
6. [Trilinear CCD — Channel Alignment](#6-trilinear-ccd--channel-alignment)
7. [Image Geometry](#7-image-geometry)
8. [Display / Reconstruction](#8-display--reconstruction)
9. [End-to-End Pipeline](#9-end-to-end-pipeline)
10. [AFE Register Map Summary](#10-afe-register-map-summary)

---

## 1. Physical Signal Lines

| Signal   | Channel | Wire   | Description                                      |
|----------|---------|--------|--------------------------------------------------|
| D0       | 24      | grey   | Parallel pixel data bit 0 (LSB)                  |
| D1       | 23      | brown  | Parallel pixel data bit 1                        |
| D2       | 22      | green  | Parallel pixel data bit 2                        |
| D3       | 21      | orange | Parallel pixel data bit 3                        |
| D4       | 19      | yellow | Parallel pixel data bit 4                        |
| D5       | 18      | blue   | Parallel pixel data bit 5                        |
| D6       | 17      | red    | Parallel pixel data bit 6                        |
| D7       | 16      | purple | Parallel pixel data bit 7 (MSB)                  |
| CS       | —       | white  | Chip select, active-low                          |
| ACDK     | 14      | red    | Pixel clock / parallel bus strobe                |
| TGCK     | 9       | brown  | Transfer Gate Clock — scan line sync             |
| SPI CS   | 8       | —      | SPI chip select (to AFE)                         |
| SPI CLK  | 7       | —      | SPI clock                                        |
| SPI MOSI | 6       | —      | SPI data (host → AFE)                            |

---

## 2. SPI Control Protocol

The host communicates with the AFE (Analog Front End) chip via 3-wire SPI.

- **Word length**: 24 bits
- **Format**: `[R/W : 1][REG : 7][DATA : 16]`
  - Bit 23 = `0` write, `1` read
  - Bits 22–16 = 7-bit register address (0x00–0x7F)
  - Bits 15–0 = 16-bit data value
- **Mode**: SPI Mode 0, MSB first

### Key commands

| SPI Command | Register | Value  | Meaning                                      |
|-------------|----------|--------|----------------------------------------------|
| `0x600080`  | 0x60     | `0080` | Set stream mode config (pre-start)           |
| `0x600081`  | 0x60     | `0081` | **Start** pixel stream on parallel bus       |
| `0x600000`  | 0x60     | `0000` | **Stop** pixel stream                        |
| `0x6A0000`  | 0x6A     | `0000` | Scan line reset / prepare next pass          |
| `0x680100`  | 0x68     | `0100` | Config-mode data bus                         |
| `0x68FFFF`  | 0x68     | `FFFF` | Re-enable 16-bit pixel data output           |
| `0x62xxxx`  | 0x62     | varies | RGB sequencer — per-line illumination timing |
| `0x630001`  | 0x63     | `0001` | Enter config mode (load timing registers)    |
| `0x630002`  | 0x63     | `0002` | Enter scan/acquisition mode                  |
| `0x630000`  | 0x63     | `0000` | Idle / exit current mode                     |

### Scan pass cycle

Each scan line acquisition follows this sequence:

```
6A0000    → Scan line reset
680100    → Config-mode data bus
62020E    → RGB sequencer setup
600080    → Stream mode config
6202CE    → Red channel timing
6200C6    → Green channel timing
620486    → Blue channel timing
          → (delay: motor moves to scan position)
600081    → START pixel stream
          → (CCD pixel data transfers over parallel bus)
600000    → STOP pixel stream
68FFFF    → Re-enable 16-bit data bus
6A0000    → Scan line reset for next pass
```

---

## 3. Binary Capture File Format

**Files**: `capture_NNNN.bin`

The parallel bus is sampled on every ACDK strobe edge while the pixel stream is active:
- SPI command `0x600081` has been received (stream enabled)
- CS is **inactive** (high, since CS is active-low)

Each byte in the file is the 8-bit value on D0–D7 at one strobe edge. There is **no in-band framing** — line boundaries are carried in the companion TGCK CSV file (§4).

A companion index file `captures.csv` records metadata for each binary file:

```
file_num, filename, bytes, width, start_time_us, end_time_us,
duration_us, start_pos, end_pos
```

---

## 4. TGCK Line Boundary File

**Files**: `capture_NNNN_tgck.csv`

### CSV columns

```
rising_byte_index,         rising_timestamp,
falling_byte_index,        falling_timestamp,
first_clock_rising_byte_index,  first_clock_rising_timestamp,
first_clock_falling_byte_index, first_clock_falling_timestamp
```

All `*_byte_index` values are byte offsets into the corresponding `.bin` file.  
All `*_timestamp` values are in nanoseconds.

### TGCK signal semantics

| Edge    | Meaning                                                                 |
|---------|-------------------------------------------------------------------------|
| Rising  | Transfer gate opens — beginning of CCD pixel readout for this scan line |
| Falling | End of transfer gate pulse; used as the pixel-aligned line boundary     |

The **falling edge** (`falling_byte_index`) is the preferred reference for line start because it lands consistently on a pixel boundary with no quantisation jitter. The rising edge can be off by ±1 byte.

### Computing line starts

```
line_starts[n] = tgck_falling_byte_index[n] + start_offset
```

`start_offset` defaults to **4 bytes** (adjustable interactively in the viewer with `[` / `]`).

### Computing line width

```
line_width_bytes = median(falling_byte_index[n+1] − falling_byte_index[n])
```

---

## 5. Pixel Data Format

### Layout

**6 bytes per pixel**, little-endian 16-bit per channel, channels in G → B → R order:

```
Byte  Content
  0   Green low byte   G[7:0]
  1   Green high byte  G[15:8]
  2   Blue  low byte   B[7:0]
  3   Blue  high byte  B[15:8]
  4   Red   low byte   R[7:0]
  5   Red   high byte  R[15:8]
```

### Reconstruction

```
pixel_offset = line_starts[row] + col * 6

G_raw = (data[pixel_offset + 1] << 8) | data[pixel_offset + 0]
B_raw = (data[pixel_offset + 3] << 8) | data[pixel_offset + 2]
R_raw = (data[pixel_offset + 5] << 8) | data[pixel_offset + 4]
```

The channel index used internally is: `0 = G`, `1 = B`, `2 = R`.

### Raw ADC depth

The raw 16-bit values do not span the full 0–65535 range. The default display gain
of `1/96` maps the useful signal range to 8-bit (0–255), which implies an effective
ADC depth of approximately **14 bits** (maximum raw value ~16383).

The AFE PGA gain (register `0x67`) and CDS gain (registers `0x30–0x37`) control the
analog signal level; the exact ADC bit depth depends on those settings.

---

## 6. Trilinear CCD — Channel Alignment

The V500 uses a **trilinear CCD**: three physically separated rows of photodiodes,
each covered with a different color filter (R, G, B). As the scan head moves along
the document, each row sees a different physical position — the same point on the
document is captured by each color row at a different scan line.

### Line delta model

```
b_row = row − bg_delta      ← Blue sensor leads Green by bg_delta lines
g_row = row                 ← Green is the reference
r_row = row + gr_delta      ← Red sensor lags Green by gr_delta lines
```

When `bg_delta > 0`, the Blue capture is `bg_delta` lines ahead of Green in the file.  
When `gr_delta > 0`, the Red capture is `gr_delta` lines behind Green in the file.

### Auto-detection algorithm

1. Sample ~64 evenly-spaced columns (skipping 5% margins on each side).
2. Build per-row mean signal for each channel pair.
3. Differentiate (row-to-row differences) to focus on edges rather than slow gradients.
4. Compute normalised cross-correlation for offsets in `[−max_delta, +max_delta]`.
5. Pick the offset that maximises correlation.
6. Apply sign convention: `bg_delta = −d_bg`, `gr_delta = d_gr`.

Search range is typically ±80 lines. Actual deltas depend on DPI and scan head geometry.

---

## 7. Image Geometry

```
pixel_width = line_width_bytes / 6
total_rows  = number_of_tgck_records − 1   (last record is a sentinel)

image_size  = pixel_width × total_rows pixels
```

### DPI reference values

| DPI  | Approx pixel_width | Notes                           |
|------|-------------------|---------------------------------|
| 800  | ~4,534            | Estimated from register 0x50/51 |
| 1600 | ~5,654            | Pixel window 0x1511–0x2B27      |
| 3200 | ~6,168            | Pixel window 0x1612–0x2E2A      |
| 6400 | ~9,067 (54,400 bytes/line ÷ 6) | Maximum resolution  |

---

## 8. Display / Reconstruction

### Grayscale (ITU-R BT.601 luma)

```
lum = 0.299·R + 0.587·G + 0.114·B
```

### Color with trilinear alignment

```rust
b_row = clamp(row − bg_delta, 0, total_rows − 1)
g_row = row
r_row = clamp(row + gr_delta, 0, total_rows − 1)

R = read_channel(r_row, col, ch=2)
G = read_channel(g_row, col, ch=0)
B = read_channel(b_row, col, ch=1)
```

### Gain and gamma

```
display_val = clamp(raw_u16 × gain, 0.0, 255.0)

// optional sqrt gamma for perceptual brightness:
display_val = sqrt(display_val / 255.0) × 255.0
```

Default gain: `1/96 ≈ 0.01042` (approximately a right-shift of ~6.6 bits).

---

## 9. End-to-End Pipeline

```
DSL capture file (.dsl ZIP archive, DSLogic format)
  │
  │  DslFileSource — streams 12 channels at 250+ MHz
  │
  ├──► SPI decoder (CS=ch8, CLK=ch7, MOSI=ch6, 24-bit, Mode 0)
  │        │
  │        └──► SpiCommandController
  │                  enable  = 0x600081
  │                  disable = 0x600000
  │                  │
  │                  └──► enable/disable signal (edge-based timestamps)
  │
  ├──► Parallel decoder
  │         Strobe : ACDK (ch14), AnyEdge
  │         Data   : D0–D7 (8 bits)
  │         CS     : active-low gate
  │         Enable : gated by SpiCommandController output
  │         │
  │         └──► capture_NNNN.bin  (raw bytes, 1 byte per strobe)
  │
  └──► TGCK edge detector (ch9)
           │
           └──► capture_NNNN_tgck.csv  (line boundary offsets + timestamps)


capture_NNNN.bin + capture_NNNN_tgck.csv
  │
  │  ccd_viewer
  │
  ├── Load TGCK CSV → falling_byte_index[] → line_starts[] (+ start_offset)
  ├── line_width_bytes = median(Δfalling_byte_index)
  ├── pixel_width = line_width_bytes / 6
  ├── For each (row, col): reconstruct G, B, R from 6-byte pixel
  ├── Apply trilinear alignment (bg_delta, gr_delta)
  ├── Scale by gain, optionally apply sqrt gamma
  └── Display / pan / zoom in framebuffer window
```

---

## 10. AFE Register Map Summary

See `REGISTERS.md` for the full register map. Key registers for the pixel stream:

| Register | Name             | Purpose                                      |
|----------|------------------|----------------------------------------------|
| `0x30`–`0x37` | CDS gain    | Correlated Double Sampling gain (constant across DPI) |
| `0x50`   | PIXEL_START_POS  | First active pixel column                    |
| `0x51`   | PIXEL_END_POS    | Last active pixel column                     |
| `0x60`   | Stream Control   | `0081`=start, `0000`=stop, `0080`=config     |
| `0x61`   | AFE Mode         | `A000`=normal, `8000`=early-init (4800 DPI)  |
| `0x62`   | RGB Sequencer    | Per-line R/G/B illumination timing (4 writes)|
| `0x63`   | System Mode      | `0001`=config, `0002`=scan, `0004`=bus test, `0000`=idle |
| `0x67`   | PGA Config       | Programmable Gain Amplifier setting          |
| `0x68`   | Data Bus Config  | `FFFF`=16-bit readout, `0100`=config mode    |
| `0x6A`   | Scan Line Ctrl   | `0000`=reset/prepare next pass               |
| `0x6B`   | VREF / Bias DAC  | Reference voltage bias (DPI-dependent)       |
| `0x78`–`0x7A` | RGB Gain/Offset | Per-channel gain+offset calibration (binary search) |
