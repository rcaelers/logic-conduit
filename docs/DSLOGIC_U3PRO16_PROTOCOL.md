# DSLogic U3Pro16 logic-analyser USB protocol

This is a wire-level specification for implementing a DSLogic U3Pro16
logic-analyser client. It covers USB discovery, optional USB-controller
recovery, FPGA configuration, all logic-capture modes, triggers, acquisition,
data reception, status, and termination. Oscilloscope and analogue modes are
out of scope.

## 1. Conventions

- All multi-byte values are unsigned little-endian.
- USB control transfers use a 3,000 ms timeout. Bulk configuration transfers
  use a 1,000 ms timeout.
- A control read consists of two requests with a delay of at least 10 ms
  between them.
- `u8`, `u16`, `u32`, and `u64` below denote exact unsigned wire widths.
- `round_up(x, n)` rounds `x` upward to the next multiple of `n`.

## 2. USB identity and endpoints

| Item | Value |
| --- | --- |
| Vendor ID | `0x2a0e` |
| Product ID | `0x002a` |
| Configuration | `1` |
| Interface | `0` |
| Accepted link speeds | High Speed and SuperSpeed |
| Runtime manufacturer string prefix | `DreamSourceLab` |
| Runtime product string prefix | `USB-based DSL Instrument v2` |
| Bulk OUT endpoint | `0x02` |
| Bulk IN endpoint | `0x86` |
| Runtime firmware version | major version must be `2` |
| FPGA logic version | register `0x04` must be `0x0e` |

Open the device, select configuration 1 if needed, and claim interface 0.
Runtime identification requires both string prefixes above. A device that
cannot be opened must be reported as busy or inaccessible rather than assumed
compatible.

## 3. Image types and startup state

Two independent images may be needed.

| Image | Target | When used | Transport |
| --- | --- | --- | --- |
| `DSLogicU3Pro16.fw` | USB-controller CPU | only if the device does not expose the runtime strings | FX2 boot-loader request `0xa0` |
| `DSLogicU3Pro16.bin` | capture FPGA | when FPGA-configured status is clear, or when changing FPGA image | runtime control and bulk endpoint `0x02` |

The standard DSView bundle contains `DSLogicU3Pro16.bin`, but may not contain
`DSLogicU3Pro16.fw`. Do not substitute an image for another product. A normal
device starts with its USB-controller runtime already active, making the `.fw`
image unnecessary for ordinary operation.

### 3.1 Optional USB-controller firmware recovery

The `.fw` image is a raw, flat FX2 memory image. It has no container, checksum,
signature, relocation records, or Intel-HEX framing. The first bytes of known
images are 8051 code (`02 01 b9`).

Perform recovery only with the exact U3Pro16 firmware image:

1. Open the boot-mode device. On platforms requiring it, detach a kernel
   driver from interface 0.
2. Select configuration 1.
3. Send control OUT: `bmRequestType=0x40`, `bRequest=0xa0`,
   `wValue=0xe600`, `wIndex=0`, data `01`.
4. Starting at offset zero, send the file as consecutive chunks of at most
   4096 bytes. Each chunk is control OUT with `bmRequestType=0x40`,
   `bRequest=0xa0`, `wValue=offset`, `wIndex=0`, and the raw chunk as data.
5. Send control OUT `0x40/0xa0`, `wValue=0xe600`, `wIndex=0`, data `00`.
6. Close the handle, wait for USB re-enumeration, and restart runtime discovery.

`wValue` is 16-bit, so reject a firmware image that would write beyond offset
`0xffff`. Require every control transfer to report a full write.

### 3.2 Runtime FPGA configuration

The FPGA image is sent verbatim. Let `N` be its byte length; require
`1 <= N <= 0x00ffffff`.

1. Write command 3, data `fb` (program pin low).
2. Write command 5, data `fc` (both LEDs off).
3. Write command 3, data `04` (program pin high).
4. Poll command 2 until status bit 5 is set.
5. Write command 6, data `7f` (input-ready low).
6. Write command 10 with the three-byte little-endian value of `N`.
7. Bulk-write the complete `.bin` image to endpoint `0x02`.
8. Write command 6, data `80` (input-ready high).
9. Poll command 2 until status bit 7 is set.
10. Write command 6, data `7f`.
11. Poll command 2 until status bit 6 is set.
12. Write command 5, data `01` (green LED).
13. Write command 7, data `01`.

Use a finite deadline for every poll. A five-second deadline is reasonable.

## 4. Runtime control transport

Every runtime command uses `wValue=0` and `wIndex=0`.

```text
ControlHeader (4 bytes)
0: u8  command
1: u16 offset
3: u8  length
```

For a control write, send `ControlHeader` immediately followed by `length`
data bytes. The normal control payload maximum is 60 data bytes.

| Operation | bmRequestType | bRequest | OUT payload | IN payload |
| --- | ---: | ---: | --- | --- |
| Write | `0x40` | `0xb0` | header + data | none |
| Read phase 1 | `0x40` | `0xb1` | header | none |
| Read phase 2 | `0xc0` | `0xb2` | none | exactly `length` bytes |

For each read, send phase 1, wait at least 10 ms, then send phase 2. A
short control read is an error.

### 4.1 Runtime command numbers

| Command | Function |
| ---: | --- |
| 0 | read firmware version: two bytes, major then minor |
| 1 | revision information; reserved |
| 2 | read hardware status |
| 3 | FPGA program pin control |
| 4 | system control; reserved for direct use |
| 5 | LED control |
| 6 | input-ready handshake control |
| 7 | data-bus width control |
| 8 | start acquisition |
| 9 | stop acquisition |
| 10 | declare byte count for following bulk OUT payload |
| 11 | reserved |
| 12 | nonvolatile-memory read/write |
| 13 | oscilloscope command path; out of scope |
| 14 | write one FPGA-side register |
| 15 | read FPGA-side register or progress bytes |
| 16–21 | oscilloscope front-end controls; out of scope |
| 22 | waveform-generator path; reserved |
| 23 | probe-memory read |
| 24 | external-I²C path; reserved |

### 4.2 Status and register access

Command 2 returns one status byte.

| Bit | Meaning |
| ---: | --- |
| 7 | bulk engine complete |
| 6 | FPGA configured |
| 5 | FPGA initialization complete |
| 4 | streaming overflow |
| 3 | system-clear acknowledgement |
| 2 | system enable |
| 1 | red LED state |
| 0 | green LED state |

Command 14 writes one register: `offset=address`, `length=1`, followed by the
value. Command 15 reads a register using the two-phase read: `offset=address`,
`length=1`.

Registers required for logic acquisition:

| Address | Function |
| ---: | --- |
| `0x04` | FPGA logic version |
| `0x05` | hardware status mirror |
| `0x70` | acquisition control: `00` clear, `02` force ready, `04` force stop |
| `0x78` | input-threshold DAC code |

For a requested threshold `V` in volts, write
`floor(V / 3.3 * 1.5 / 2.5 * 255)` to register `0x78`. Validate and clamp `V`
in the API before conversion.

Command 12 provides direct nonvolatile-memory access with the same header:
the offset is the memory address and length is the byte count. Keep this behind
an explicit dangerous-operation interface. Normal logic capture does not need
to write nonvolatile memory.

## 5. Logic modes

The U3Pro16 has 16 physical logic inputs and 2 GiB aggregate capture memory.
The following modes are valid. Use only the low-numbered inputs up to the
listed valid-input count in a narrow mode.

| Link | Acquisition | Valid inputs | Rate range |
| --- | --- | ---: | ---: |
| High Speed | streaming | 16 | 100 kHz–20 MHz |
| High Speed | streaming | 12 | 100 kHz–25 MHz |
| High Speed | streaming | 6 | 100 kHz–50 MHz |
| High Speed | streaming | 3 | 100 kHz–100 MHz |
| High Speed | finite buffer | 16 | 1–500 MHz |
| High Speed | finite buffer | 8 | 1 MHz–1 GHz |
| SuperSpeed | streaming | 16 | 1–125 MHz |
| SuperSpeed | streaming | 12 | 1–250 MHz |
| SuperSpeed | streaming | 6 | 1–500 MHz |
| SuperSpeed | streaming | 3 | 1 MHz–1 GHz |
| SuperSpeed | finite buffer | 16 | 1–500 MHz |
| SuperSpeed | finite buffer | 8 | 1 MHz–1 GHz |

Only these discrete rates may be selected:

```text
10, 20, 50, 100, 200, 500 Hz
1, 2, 5, 10, 20, 40, 50, 100, 200, 400, 500 kHz
1, 2, 4, 5, 10, 20, 25, 50, 100, 125, 250, 500 MHz
1 GHz
```

Intersect that list with the selected mode's range. The selected mode's
maximum hardware rate is `M`; its pre-divider is always `P=5`.

```text
d0 = ceil(M / requested_rate)
div_high = ((P - 1) << 8) if d0 >= P, otherwise 0
d = ceil(d0 / P)
div_low = d & 0xffff
div_high |= d >> 16
```

An empty input mask is invalid. For `C = popcount(input_mask)`, available
per-input depth is `(2 GiB / C) & !1023` samples.

## 6. Capture-settings packet

Before a finite capture, round the requested limit upward:

```text
actual_samples = round_up(requested_samples, 1024)
actual_bytes   = actual_samples / 64 * C * 8
```

For streaming capture, retain the requested limit as a host-side stop limit;
the device remains active until stopped.

Declare a bulk payload length of 336 16-bit words with command 10, then poll
status bit 3 until set. Send the following 672-byte packet on endpoint `0x02`.
All unspecified reserved fields are zero.

| Offset | Field | Value |
| ---: | --- | --- |
| 0 | `sync` | `u32 0xf5a5f5a5` |
| 4 | header | `u16 0x0001` |
| 6 | mode | mode flags in section 6.1 |
| 8 | header | `u16 0x0102` |
| 10 | divider low | `div_low` |
| 12 | divider high | `div_high` |
| 14 | header | `u16 0x0302` |
| 16 | capture-count low | low 16 bits of `actual_samples >> 4` |
| 18 | capture-count high | high 16 bits of `actual_samples >> 4` |
| 20 | header | `u16 0x0502` |
| 22 | trigger position low | low 16 bits of aligned trigger position |
| 24 | trigger position high | high 16 bits of aligned trigger position |
| 26 | header | `u16 0x0701` |
| 28 | trigger global | `(C << 8) | stage_count` |
| 30 | header | `u16 0x0802` |
| 32 | sample-count low | low 16 bits of `actual_samples` |
| 34 | sample-count high | high 16 bits of `actual_samples` |
| 36 | header | `u16 0x0a02` |
| 38 | input mask low | 16-bit input mask |
| 40 | input mask high | zero |
| 42 | header | `u16 0x0c01` |
| 44 | digital gain | zero |
| 46 | header | `u16 0x40a0` |
| 48 | trigger mask plane 0 | 16 × `u16` |
| 80 | trigger mask plane 1 | 16 × `u16` |
| 112 | trigger value plane 0 | 16 × `u16` |
| 144 | trigger value plane 1 | 16 × `u16` |
| 176 | trigger edge plane 0 | 16 × `u16` |
| 208 | trigger edge plane 1 | 16 × `u16` |
| 240 | trigger logic plane 0 | 16 × `u16` |
| 272 | trigger logic plane 1 | 16 × `u16` |
| 304 | trigger count | 16 × `u32` |
| 368 | trailer | `u32 0xfa5afa5a` |

After the bulk write, write command 6 with data `80`, then require status bit
7. At High Speed, write command 7 with data `01` before declaring the settings
payload. At SuperSpeed, this pre-write is not required.

### 6.1 Mode flags

| Bit | Set when |
| ---: | --- |
| 0 | a logic trigger is enabled |
| 1 | external clock is selected |
| 2 | selected external-clock edge is active |
| 3 | run-length mode is enabled |
| 5 | selected rate is 500 MHz |
| 6 | selected rate is 1 GHz |
| 8 | one-sample input filter is enabled |
| 10 | `ceil(rate / 1000 * C / 8) < 1024` |
| 11 | serial trigger mode is selected |
| 12 | streaming mode is selected |
| 13 | memory loopback test is selected |
| 14 | external test is selected |
| 15 | internal test is selected |

Bits 4, 7, and 9 are zero for logic acquisition. The input-test options use
the ordinary logic data path; no fixed pattern or decoder is defined here.

### 6.2 Trigger position

For trigger percentage `p` in `0..=100` and requested sample limit `L`:

```text
position = max(floor(p / 100 * L), 64)
position = min(position, per_input_depth * limit_fraction)
position &= 0xffff << 6
```

`limit_fraction` is 10% in streaming mode and 90% in finite-buffer mode.

### 6.3 Trigger program

The packet contains two 16-input trigger planes and up to 16 stages. For each
input in a plane, encode these conditions in the corresponding mask, value,
and edge bits:

| Condition | mask | value | edge |
| --- | ---: | ---: | ---: |
| ignore | 1 | 0 | 0 |
| low | 0 | 0 | 0 |
| high | 0 | 1 | 0 |
| rising edge | 0 | 1 | 1 |
| falling edge | 0 | 0 | 1 |
| either edge | 1 | 0 | 1 |

Input number maps directly to bit number. Initialize unused stages with
mask=`0xffff`, value=`0`, edge=`0`, logic=`2`, and count=`0`.

For each active stage and plane:

```text
logic_word = (logical_operator << 1) | inversion_bit
```

The count array carries plane-0 count values. Plane-1 count values have no
wire field.

For a single-stage trigger, use stage 0, set `stage_count=0`, and initialize
stages 1–15 as unused. For a multi-stage trigger, use stages 0–15 and set
`stage_count` to the number of configured stages.

At 1 GHz, replicate each trigger mask, value, and edge word from its low byte:

```text
word = (word & 0x00ff) | ((word & 0x00ff) << 8)
```

In serial-trigger mode, do not replicate stage 3.

## 7. Acquisition sequence

1. Send command 9 with zero data to stop any prior acquisition.
2. Build, declare, and send the 672-byte capture-settings packet.
3. Queue one 1024-byte bulk-IN read for the trigger header and one or more
   bulk-IN data reads on endpoint `0x86`.
4. Send command 8 with zero data.
5. Process the trigger header, then process the data stream.

The trigger-header read must be queued before data reads and before command 8.

### 7.1 Trigger header

The trigger-header transfer is exactly 1024 bytes:

| Offset | Size | Field |
| ---: | ---: | --- |
| 0 | 4 | magic `0x55555555` |
| 4 | 4 | trigger position (`u32`) |
| 8 | 4 | capture-memory start address (`u32`) |
| 12 | 4 | remaining sample count, low 32 bits |
| 16 | 4 | remaining sample count, high 32 bits |
| 20 | 4 | status (`u32`); bit 0 means the trigger position is valid |
| 24 | 1000 | reserved |

Reject a short header or incorrect magic. Combine the remaining count as
`low | (u64(high) << 32)`. For a finite capture it must be strictly less than
the configured, 1024-sample-aligned extent. The delivered capture length is
then:

```text
actual_samples = (configured_samples - remaining_count) & !1023
actual_bytes   = actual_samples / 64 * C * 8
actual_samples = actual_bytes / C * 8
```

### 7.2 Logic data

Following the trigger header, endpoint `0x86` returns a continuous packed
logic-data stream. For a chunk of `N` bytes, its nominal sample count is
`N * 8 / C`. Samples are least-significant-bit first and input positions are
in increasing input-number order.

Data may end mid-sample when `C` is 3, 6, or 12. Preserve a bit carry between
USB transfers or expose chunk bit offsets. For finite capture, deliver at most
`actual_bytes`. For streaming capture, continue until the caller stops the
run. Run-length mode compresses capture memory inside the FPGA, but the upload
path expands it back into this ordinary interleaved sample representation.
Consumers therefore do not decode device-specific RLE bytes.

### 7.3 Read scheduling and overflow

Let `B = ceil(rate / 1000 * C / 8)` bytes/ms.

| Link | Stream buffer | Queued stream data reads |
| --- | --- | --- |
| SuperSpeed | `round_up(10 * B, 1024)` | `min(64, ceil(40 * B / buffer_size))` |
| High Speed | `round_up(20 * B, 512)` | `min(64, ceil(100 * B / buffer_size))` |

For finite capture, use a 1 MiB data-read buffer. A stream-transfer timeout is
`base + floor(base / 4)` ms, where `base = floor(total_queued_bytes / B)`;
a finite-transfer timeout is 20 ms. Process nonempty timed-out transfers as
data. Treat all other unsuccessful transfer statuses as errors.

If there are more than 16 consecutive event-loop polls without a completed
transfer while waiting for data, read four bytes using command 15 at offset 0.
They are progress bytes in this order: trigger hit, captured-count byte 3,
captured-count byte 2, captured-count byte 1. In streaming mode, also read
command 2; status bit 4 signals overflow.

## 8. Stop and close

To stop a live acquisition, write register `0x70 = 0x02`. Continue servicing
or cancelling all queued bulk-IN transfers until they are released. Then send
command 9 with zero data. Read register `0x05` if hardware-stop confirmation
is required. Finally release interface 0 and close the USB handle.

To force stop-and-upload behavior for a finite buffered run, write register
`0x70 = 0x04`, then continue receiving the trigger header and available data.

## 9. Rust implementation requirements

- Serialize every control header explicitly; do not rely on Rust structure
  layout.
- Serialize control requests and image configuration with one device-state
  lock. Never interleave another control command between the two phases of a
  control read.
- Represent a logic chunk with its channel count and bit offset so that narrow
  modes remain lossless across USB-transfer boundaries.
- Use bounded status polling and surface timeouts with the final status byte.
- Keep nonvolatile-memory writes and firmware recovery behind explicit opt-in
  APIs.
- Validate image sizes, USB return lengths, rate/mode combinations, nonempty
  input masks, trigger-stage count, and all arithmetic before narrowing values.
