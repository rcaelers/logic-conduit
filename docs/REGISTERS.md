# CCD AFE Register Map

Complete register map for the CCD analog frontend chip, reverse-engineered from SPI captures.

## SPI Protocol

- **24-bit** commands: `[R/W:1][REG:7][DATA:16]`
- Bit 23: `0` = write, `1` = read
- Bits 22–16: 7-bit register address (0x00–0x7F)
- Bits 15–0: 16-bit data value

## System Modes (Register 0x63)

The chip operates in distinct modes, selected by register 0x63:

| Command    | Mode                      | Description |
|------------|---------------------------|-------------|
| `630001`   | **Config mode**           | Load timing/config registers 0x00–0x3F. Values are latched on transition back to `630000`. |
| `630002`   | **Scan/acquisition mode** | Registers 0x00–0x3F are in a different bank (pulse counters). Write+readback used for verification. |
| `630004`   | **Bus test mode**         | All registers written with `0xFFFF` and read back. Bus integrity check. |
| `630000`   | **Idle / end mode**       | Exits current mode. Latches config if exiting from `630001`. |

---

## Register Groups

### 0x00–0x15: CCD Clock Phase Timing (16 registers)

Control the CCD charge-transfer clock waveform phases. Each pair of registers (even/odd) defines complementary clock phases.

| Register | Name | Description |
|----------|------|-------------|
| `0x00`–`0x01` | CCD_CLK_PHASE_00/01 | Clock phase pair 0 |
| `0x02`–`0x03` | CCD_CLK_PHASE_02/03 | Clock phase pair 1 |
| `0x04`–`0x05` | CCD_CLK_PHASE_04/05 | Clock phase pair 2 |
| `0x06`–`0x07` | CCD_CLK_PHASE_06/07 | Clock phase pair 3 |
| `0x08`–`0x09` | CCD_CLK_PHASE_08/09 | Clock phase pair 4 |
| `0x0A`–`0x0B` | CCD_CLK_PHASE_0A/0B | Clock phase pair 5 |
| `0x0C`–`0x0D` | CCD_CLK_PHASE_0C/0D | Clock phase pair 6 |
| `0x0E`–`0x0F` | CCD_CLK_PHASE_0E/0F | Clock phase pair 7 |
| `0x10`–`0x11` | CCD_CLK_PHASE_10/11 | Clock phase pair 8 |
| `0x12`–`0x13` | CCD_CLK_PHASE_12/13 | Clock phase pair 9 |
| `0x14`–`0x15` | CCD_CLK_PHASE_14/15 | Clock phase pair 10 |

**Config mode format** (`630001`): Upper byte is a bitmask, lower byte is phase timing.

| DPI   | Regs 0x00–0x15 | Regs 0x16–0x17 |
|-------|----------------|----------------|
| 1600  | `C0FF` (all)   | `C0F0`         |
| 2400  | `C5F5` (all)   | `C5F5`         |
| 3200  | `C5F5` (all)   | `C5F5`         |
| 4800  | Varies: `C0FF`→`C0F0`→`C000`→`C00F`→`C0FF`→`C0F0`→`C000` (progressive taper) |
| 6400  | `A6A6` (all)   | `A6A3`         |

**Scan mode values** (`630002`): Actual pulse counter values written in pairs, then read back for verification.

| Register pair | 1600 DPI   | 3200/6400 DPI |
|---------------|------------|---------------|
| 0x00/0x01     | `0000/0000` | `0000/0000` |
| 0x02/0x03     | `0028/31C0` | `0028/31C0` |
| 0x04/0x05     | `00C8/31C0` | `00C8/31C0` |
| 0x06/0x07     | `0190/1000` | `0190/2000` |
| 0x08–0x3F     | `FFFF` (all) | `FFFF` (all) |

Note: Register 0x07 = `1000` at 1600 DPI, `2000` at 3200/6400 DPI. All remaining registers are `FFFF` (unused/maximum).

### 0x16–0x17: CCD Clock Transition

| Register | Name | Description |
|----------|------|-------------|
| `0x16` | CCD_CLK_TRANS_A | Clock transition timing A |
| `0x17` | CCD_CLK_TRANS_B | Clock transition timing B |

Values follow the same pattern as clock phase registers per DPI.

### 0x18–0x22: Line / Integration Timing (11 registers)

Control per-line timing: integration period, readout timing, blanking intervals.

| Register | Name | Description |
|----------|------|-------------|
| `0x18`–`0x19` | LINE_TIMING_18/19 | Line timing pair |
| `0x1A`–`0x1B` | LINE_TIMING_1A/1B | Line timing pair |
| `0x1C`–`0x1D` | LINE_TIMING_1C/1D | Line timing pair |
| `0x1E`–`0x1F` | LINE_TIMING_1E/1F | Line timing pair |
| `0x20`–`0x21` | LINE_TIMING_20/21 | Line timing pair |
| `0x22`       | LINE_TIMING_22   | Line timing (unpaired) |

**Config mode values by DPI:**

| DPI   | Regs 0x18–0x22 |
|-------|-----------------|
| 1600  | `0000` (all zero) |
| 2400  | `0505` (all) |
| 3200  | `0505` (all) |
| 4800  | Varies: `C000`→`C00F`→`C0FF`→`C0F0`→`C0F0` (progressive ramp) |
| 6400  | `5353` (all) |

### 0x23–0x2D: Extended Timing (11 registers)

| Register | Name | Description |
|----------|------|-------------|
| `0x23`–`0x2D` | TIMING_23–TIMING_2D | Extended timing / unused region |

**Config mode values by DPI:**

| DPI   | Value |
|-------|-------|
| 1600  | `0000` (all zero) |
| 2400  | `0505` (all) |
| 3200  | `0505` (all) |
| 4800  | `0000` (all zero) |
| 6400  | `5353` (all) |

### 0x2E–0x2F: Configuration Flags

| Register | Name | Description |
|----------|------|-------------|
| `0x2E` | CONFIG_FLAGS_A | Configuration flags A |
| `0x2F` | CONFIG_FLAGS_B | Configuration flags B |

**Config mode values by DPI:**

| DPI   | 0x2E   | 0x2F   |
|-------|--------|--------|
| 1600  | `000F` | `000F` |
| 2400  | `050A` | `050A` |
| 3200  | `050A` | `050A` |
| 4800  | `000F` | `000F` |
| 6400  | `5353` | `5353` |

### 0x30–0x35: CDS Gain (6 registers)

Correlated Double Sampling gain configuration. **DPI-independent.**

| Register | Name | Value | Description |
|----------|------|-------|-------------|
| `0x30`–`0x35` | CDS_GAIN_30–35 | `4535` | CDS gain (identical across all DPIs) |

### 0x36–0x37: CDS Gain Alternate

| Register | Name | Value | Description |
|----------|------|-------|-------------|
| `0x36` | CDS_GAIN_ALT_A | `4539` | Alternate CDS gain A |
| `0x37` | CDS_GAIN_ALT_B | `4539` | Alternate CDS gain B |

### 0x38–0x3B: Clamp / Offset (4 registers)

**DPI-independent.**

| Register | Name | Value | Description |
|----------|------|-------|-------------|
| `0x38`–`0x3B` | CLAMP_OFFSET_38–3B | `4131` | Clamp/offset calibration |

### 0x3C–0x3F: Channel Configuration (4 registers)

**DPI-independent.**

| Register | Name | Value | Description |
|----------|------|-------|-------------|
| `0x3C`–`0x3F` | CHANNEL_CFG_3C–3F | `0101` | Channel enable/configuration |

---

### 0x44–0x47: Extended Configuration (4800 DPI only)

Only used in 4800 DPI mode during early initialization.

| Register | Name | Value | Description |
|----------|------|-------|-------------|
| `0x44`–`0x47` | EXT_CFG_44–47 | `0000` | Extended config (cleared) |

### 0x50–0x51: Pixel Window

Define the active pixel region for readout. **DPI-dependent.**

| Register | Name | Description |
|----------|------|-------------|
| `0x50` | PIXEL_START_POS | First active pixel position |
| `0x51` | PIXEL_END_POS | Last active pixel position |

| DPI   | Start (`0x50`) | End (`0x51`) | Pixel count (approx) |
|-------|----------------|--------------|----------------------|
| 1600  | `1511`         | `2B27`       | ~5,654 |
| 2400  | `1612`         | `2E2A`       | ~6,168 |
| 3200  | `1612`         | `2E2A`       | ~6,168 |
| 4800  | `1612`         | `2E2A`       | ~6,168 |
| 6400  | `1612`         | `2E2A`       | ~6,168 |

Note: 1600 DPI has a narrower pixel window. 2400+ DPI use identical pixel boundaries.

### 0x52–0x53: LED Exposure / Timing

**DPI-independent** (constant across all captures).

| Register | Name | Value | Description |
|----------|------|-------|-------------|
| `0x52` | LED_EXPOSURE | `0600` | LED exposure time/intensity |
| `0x53` | LED_TIMING | `0C06` | LED pulse timing |

### 0x56–0x57: LED Channel Control

On the CCD/AFE PCB — **not motor control** (motors are on the main PCB).

| Register | Name | Value | Description |
|----------|------|-------|-------------|
| `0x56` | LED_CTRL_A | `0000` | LED channel select / control A |
| `0x57` | LED_CTRL_B | `0000` | LED channel select / control B |

Always `0000` in all observed captures.

### 0x58–0x5C: Extended Config (4800 DPI only)

| Register | Name | Value | Description |
|----------|------|-------|-------------|
| `0x58`–`0x5C` | EXT_CFG_58–5C | `0000` | Extended config (cleared) |

---

### 0x60: Stream Control

Controls the pixel data stream from CCD to host.

| Value  | Meaning |
|--------|---------|
| `0081` | **Start** pixel stream |
| `0080` | Set stream mode config (pre-start) |
| `0000` | **Stop** pixel stream |

### 0x61: AFE Mode

Analog frontend operating mode. Written once during init.

| DPI   | Value  | Description |
|-------|--------|-------------|
| All   | `A000` | Normal AFE mode |
| 4800 (early init) | `8000` | Early-init AFE mode |

### 0x62: RGB Sequencer FIFO

**Not motor position** — this is the per-line RGB sequential illumination controller. Written as a 4-command sequence before each scan pass:

| Sequence | Value  | Meaning |
|----------|--------|---------|
| 1        | `020E` | Setup / configuration write |
| 2        | `02CE` | RGB color A timing (Red) |
| 3        | `00C6` | RGB color B timing (Green) |
| 4        | `0486` | RGB color C timing (Blue) |

These values are **constant across all DPIs and all scan captures**.

Typical scan-pass sequence:
```
6A0000    → Scan line reset
680100    → Set config-mode data bus
62020E    → Sequencer setup
600080    → Stream mode config
6202CE    → Red timing
6200C6    → Green timing
620486    → Blue timing
  ...     → (wait for motor positioning)
600081    → Start pixel stream
  ...     → (CCD data transfer)
600000    → Stop pixel stream
```

### 0x63: System Mode

See [System Modes](#system-modes-register-0x63) above.

### 0x64–0x66: Extended Config (4800 DPI only)

| Register | Name | Value | Description |
|----------|------|-------|-------------|
| `0x64`–`0x66` | EXT_CFG_64–66 | `0000` | Extended config (cleared) |

### 0x67: PGA Configuration

Programmable Gain Amplifier settings. **Partially DPI-dependent.**

| DPI   | Value  |
|-------|--------|
| 1600  | `0C88` |
| 2400  | `0C80` |
| 3200  | `0C80` |
| 4800  | `0C88` |
| 6400  | `0C80` |

### 0x68: Data Bus Configuration

| Value  | Meaning |
|--------|---------|
| `FFFF` | Enable full 16-bit pixel data output (normal readout mode) |
| `0100` | Config-mode data bus (set before sequencer writes) |

### 0x69: Extended Config (4800 DPI only)

| Register | Name | Value | Description |
|----------|------|-------|-------------|
| `0x69` | EXT_CFG_69 | `FFF0` | Extended config (4800 DPI only) |

### 0x6A: Scan Line Control

| Value  | Meaning |
|--------|---------|
| `0000` | Scan line reset / prepare for next pass |

Written before each scan pass and after each stream stop.

### 0x6B: VREF / Bias DAC

Reference voltage and bias current DAC. **DPI-dependent.**

| DPI   | Value  | Pattern |
|-------|--------|---------|
| 1600  | `D3D3` | Lower bias |
| 2400  | `D7D7` | Medium bias |
| 3200  | `D7D7` | Medium bias |
| 4800  | `D3D3` | Lower bias |
| 6400  | `DFDF` | Higher bias |

Both bytes are always equal (symmetric for + and − references).

### 0x6F: Extended Config (4800 DPI only)

| Register | Name | Value |
|----------|------|-------|
| `0x6F` | EXT_CFG_6F | `0000` |

### 0x70: Extended Config (4800 DPI only)

| Register | Name | Values |
|----------|------|--------|
| `0x70` | EXT_CFG_70 | `0000` then `000F` |

Written twice during 4800 DPI early init. Possibly enables extended register set.

### 0x72: Extended Config (4800 DPI only)

| Register | Name | Value |
|----------|------|-------|
| `0x72` | EXT_CFG_72 | `CC00` |

### 0x78–0x7A: RGB Gain & Offset Calibration

Per-channel gain and offset used for dark-level and analog calibration. Written/read repeatedly during the binary-search calibration loop.

| Register | Name | Format |
|----------|------|--------|
| `0x78` | RED_GAIN_OFFSET | `[gain:8][offset:8]` |
| `0x79` | GREEN_GAIN_OFFSET | `[gain:8][offset:8]` |
| `0x7A` | BLUE_GAIN_OFFSET | `[gain:8][offset:8]` |

**Read/write protocol**: Always accessed as read → write → read-verify triplets.

**Calibration sequence** (binary search on gain):
```
Initial:    gain=0x80, offset=0x00  (read existing, write with offset zeroed)
Pass 1-2:   offset converges quickly (e.g., 0x30, 0x2A, 0x2E for R/G/B)
Pass 3:     gain=0x80 → test
Pass 4:     gain=0xC0 → test  (step up by 0x40)
Pass 5:     gain=0xA0 → test  (step down by 0x20)
Pass 6:     gain=0x90 → test  (step down by 0x10)
Pass 7:     gain=0x88 → test  (step down by 0x08)
Pass 8:     gain=0x8C → test  (step up by 0x04)
...         (continues until converged)
```

Each channel converges independently. Final values depend on CCD dark current and LED intensity.

### 0x7E–0x7F: Extended Config (4800 DPI only)

| Register | Name | Value | Description |
|----------|------|-------|-------------|
| `0x7E` | EXT_CFG_7E | `00BC` | 4800 DPI early init only |
| `0x7F` | EXT_CFG_7F | `400B` | 4800 DPI early init only |

Written during 4800 DPI initialization before any other register. May enable extended CCD readout mode or dual-line interleaving.

---

## Complete Initialization Sequence

### Standard (1600/2400/3200/6400 DPI)

1. **RGB gain/offset init**: Read each channel (0x78–0x7A), write with offset=0x00, verify
2. **Config mode** (`630001`): Write all timing registers 0x00–0x3F
3. **Latch** (`630000`): Exit config mode, latches values
4. **Control registers**: Write 0x52, 0x53, 0x56, 0x57, 0x61, 0x6B, 0x67, 0x50, 0x51
5. **Scan mode** (`630002`): Write actual pulse values to 0x00–0x07, write+readback 0x00–0x3F
6. **Bus test** (`630004`): Write FFFF to all 0x00–0x3F, readback verify
7. **Idle** (`630000`)
8. **Calibration loop**: Binary search on gain via 0x78–0x7A with scan passes

### Extended (4800 DPI only)

Extra steps before the standard sequence:
1. `700000` → `7F400B` → `70000F` → `618000` → Read `618000`
2. `69FFF0` → `680100` → `6A0000` → `620486`
3. Initialize 0x78–0x7A with gain=0x80, offset=0x07
4. `7E00BC` → Clear 0x44–0x47, 0x58–0x5C, 0x64–0x66 → `72CC00` → `6F0000`
5. Continue with standard sequence

---

## Scan Pass Cycle

Each scan pass follows this pattern (repeats for every calibration/acquisition pass):

```
[RGB gain calibration: read/write/verify 0x78-0x7A]
6A0000     ← Scan line reset
680100     ← Config-mode data bus
62020E     ← RGB sequencer setup
600080     ← Stream mode config
6202CE     ← Red channel timing
6200C6     ← Green channel timing
620486     ← Blue channel timing
           ← (delay: motor moves to scan position)
600081     ← START pixel stream
           ← (CCD data transfer over parallel bus)
600000     ← STOP pixel stream
68FFFF     ← Re-enable 16-bit data bus
6A0000     ← Scan line reset for next pass
```

---

## DPI Configuration Summary

| Parameter | 1600 | 2400 | 3200 | 4800 | 6400 |
|-----------|------|------|------|------|------|
| Clock phases 0x00–0x15 | `C0FF` | `C5F5` | `C5F5` | varies | `A6A6` |
| Line timing 0x18–0x22 | `0000` | `0505` | `0505` | varies | `5353` |
| Config flags 0x2E/0x2F | `000F` | `050A` | `050A` | `000F` | `5353` |
| Pixel start 0x50 | `1511` | `1612` | `1612` | `1612` | `1612` |
| Pixel end 0x51 | `2B27` | `2E2A` | `2E2A` | `2E2A` | `2E2A` |
| PGA config 0x67 | `0C88` | `0C80` | `0C80` | `0C88` | `0C80` |
| VREF/bias 0x6B | `D3D3` | `D7D7` | `D7D7` | `D3D3` | `DFDF` |
| Scan-mode reg 0x07 | `1000` | `2000` | `2000` | `1000` | `2000` |
| Extra init regs | — | — | — | 0x44–0x47, 0x58–0x5C, 0x64–0x66, 0x69, 0x6F, 0x70, 0x72, 0x7E, 0x7F | — |
| Scan-mode readback | single | double | double | single | double |

**DPI groupings observed:**
- **1600 / 4800**: Similar PGA, VREF, scan-mode timing — likely 1× and 3× native CCD resolution
- **2400 / 3200**: Identical timing config — likely same CCD clock mode, different mechanical stepping
- **6400**: Unique timing values — likely 4× oversampled or dual-pass mode
