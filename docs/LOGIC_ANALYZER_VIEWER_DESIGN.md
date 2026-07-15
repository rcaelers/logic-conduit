# Logic Analyzer Viewer — Design

Design of the waveform viewer for large DSLogic `.dsl` captures (multi-GB files) and live
pipeline output. The goal: zooming and panning stay realtime regardless of capture size,
indexing runs in the background, the UI thread never blocks on file I/O, ZIP decompression,
or raw sample scanning — and **every pixel is truthful at any timescale** (the renderer
never invents edge positions it doesn't know).

Implementation:

- egui widget: [crates/widgets/logic_analyzer_viewer](../crates/widgets/logic_analyzer_viewer)
  (`viewer.rs`, `channel.rs`, `cursor.rs`, `draw/`, `input.rs`, `sampling.rs`, `worker.rs`)
- Index build/query engine: [crates/signal_processing/src/waveform_index/](../crates/signal_processing/src/waveform_index)
  (`builder.rs`, `storage.rs`, `reader.rs`, `types.rs`)
- Raw block decompression cache: [crates/signal_processing/src/raw_block_cache.rs](../crates/signal_processing/src/raw_block_cache.rs)
- Capture reader / data source: [crates/logic_analyzer_processing/src/nodes/dsl_file.rs](../crates/logic_analyzer_processing/src/nodes/dsl_file.rs)
  (`DslCaptureReader`, `DslFileCaptureDataSource`)
- Common capture types / traits: [crates/signal_processing/src/capture.rs](../crates/signal_processing/src/capture.rs)
- Derived-lane store and summary index:
  [crates/signal_processing/src/viewer_sink.rs](../crates/signal_processing/src/viewer_sink.rs),
  [crates/signal_processing/src/derived_index.rs](../crates/signal_processing/src/derived_index.rs)

The widget's public API is documented in
[LOGIC_ANALYZER_VIEWER_API.md](LOGIC_ANALYZER_VIEWER_API.md).

---

## Three content sources, one row list

The viewer renders three independent kinds of rows:

1. **Capture channels** — sampled on demand from an indexed `.dsl` capture
   (`set_capture_path`). The viewer is generic over `CaptureDataSource`; the application
   supplies a closure that opens the concrete source, so the widget crate never depends on
   a file format.
2. **In-memory channels** — raw `(time, level)` transition lists handed in wholesale
   (`set_channels`), used for demo signals and any host-provided data.
3. **Derived lanes** — a shared `DerivedLanes` store (`Arc<RwLock<…>>`) that running
   pipeline `Viewer` nodes push into (`set_derived_lanes`); rendered live as digital,
   annotation (boxed word), marker, or labeled number/text level rows beneath the channels.

A single `row_order: Vec<RowKey>` is the only source of truth for display order across all
row kinds, reconciled every frame (stale rows dropped, new ones appended) before any
row-position math, so hit-testing, dragging, and layout always agree. Rows are reordered by
dragging their labels and renamed via double-click (rename maps live in the viewer, keyed by
channel index / lane name — the underlying data is untouched). Two color profiles (DSView
Tango-based, Classic muted) are selectable from the header bar.

On wasm there is no file access and no background worker: derived lanes and in-memory
channels are the only content.

---

## File Format (.dsl)

A `.dsl` file is a ZIP archive containing:

| Entry | Description |
|---|---|
| header/metadata | Sample rate, total sample count, channel list, block size |
| `L-{channel}/{block}` | Packed logic bits for one (channel, block) pair (deflate-compressed) |

Samples are divided into fixed-size **blocks** (`samples_per_block`, commonly `2^24 =
16,777,216` samples). Each `L-{channel}/{block}` ZIP entry holds one block's packed bits for
one channel.

---

## Architecture

```text
.dsl ZIP capture
  │
  ├─ DslFileCaptureDataSource            (path, header, sidecar path, fingerprint = file size)
  │    └─ DslCaptureReader               (ZipArchive + small LRU of decompressed blocks)
  │
  ├─ Waveform index (crates/signal_processing/src/waveform_index)
  │    ├─ IndexBuilder     — builds the sidecar `.dsl.idx` on worker threads
  │    ├─ IndexReader      — mmaps the sidecar, serves directory + leaf lookups
  │    └─ IndexSampler     — public query API: sampled_window() over a viewport
  │
  ├─ RawBlockCache                       — sidecar `.dsl.idx.raw`: decompressed raw blocks,
  │                                        populated lazily for deep-zoom exact reads
  │
  └─ LogicAnalyzerViewer (egui)          — UI widget
       ├─ background thread             — opens capture, builds/validates index, reports progress
       └─ UI thread                     — samples the visible window synchronously and paints it
```

`IndexSampler` is the single entry point used by the UI: it owns the `IndexReader` (mmapped
sidecar), a raw `BlockCaptureSource` reader, and an optional `RawBlockCache`. Opening it
builds the index if missing/stale, otherwise it just validates and mmaps the existing sidecar.
The viewer holds it as `Box<dyn CaptureIndex>` — the trait seam that keeps the widget crate
decoupled from the index implementation (and empty on wasm).

---

## Terminology

| Term | Meaning |
|---|---|
| Sample | A single 1-bit logic level reading at one point in time, on one channel |
| Block | The raw-capture unit; one `L-{channel}/{block}` ZIP entry, `samples_per_block` samples |
| Chunk / leaf | The serialized index payload for one (channel, block) pair: `valid_samples`, flags, and (if active) the L1/L2/L3 mipmap bitmaps |
| Directory entry | The per-(channel, block) directory record: chunk offset/length plus a duplicated top-level (L3) summary, so coarse queries never need to touch the payload |
| Sidecar index | The persistent `.dsl.idx` file: header + directory + chunk payloads |
| Raw block cache | The persistent `.dsl.idx.raw` file: lazily-populated decompressed raw blocks for exact/deep-zoom reads |

Every (channel, block) pair gets its own directory entry and chunk; the directory entry's
embedded L3 summary is what makes the coarsest zoom level cheap without another index level.

---

## Mipmap Hierarchy (per block)

Each active (non-constant) block stores three toggle levels above the raw bits. A **toggle**
bit answers "did the signal change state at least once in this group of samples?" — not which
direction. Alongside each toggle word, a same-shaped **last-value** word records the signal
level at the end of each group, so a renderer can reconstruct level without touching raw data.

```text
L1  4096 × u64   1 bit = any transition in   64 raw samples   (covers 64^2 = 4,096 samples/word)
L2    64 × u64   1 bit = any transition in 4,096 samples      (covers 64^3 = 262,144 samples/word)
L3     1 × u64   1 bit = any transition in 262,144 samples    (covers the whole 2^24-sample block)
```

`l1_last` / `l2_last` / `l3_last` are bitmaps of identical shape to their toggle counterparts,
each bit holding the signal value at the end of that group.

Memory per active block:

```text
l1_toggle = 4096 × 8 B = 32,768 B      l1_last = 4096 × 8 B = 32,768 B
l2_toggle =   64 × 8 B =    512 B      l2_last =   64 × 8 B =    512 B
l3_toggle =    1 × 8 B =      8 B      l3_last =    1 × 8 B =      8 B
total = 66,576 B ≈ 65 KiB per active block
```

**Constant blocks** (no transitions) store none of this: only `valid_samples`, `first`, and
`last` are kept, and the directory's `toggle` flag is cleared. This makes long idle regions
essentially free.

### Boundary transitions

A block's own samples may look constant while a transition actually falls exactly on the
boundary with the previous block (previous block's last sample differs from this block's
first sample). `IndexBuilder::apply_boundary_transition` detects this using the previous
block's last value and, if needed, synthesizes L1/L2/L3 toggle bits (allocating summaries for
an otherwise-constant block) so no edge is lost at block boundaries.

---

## Sidecar Index File Format

Magic `CAPIDX06`, built by `IndexWriter` / read by `IndexReader` in
[storage.rs](../crates/signal_processing/src/waveform_index/storage.rs):

```text
┌─────────────────────────────────────────────────────┐
│  HEADER  (96 bytes, offset 0)                        │
│    magic              [u8; 8]  = b"CAPIDX06"         │
│    version             u32     = 6                   │
│    header_size         u32     = 96                  │
│    source_revision     u64     (source file size)    │
│    total_samples       u64                            │
│    total_blocks        u64                            │
│    samples_per_block   u64                            │
│    samplerate_bits     u64  (f64::to_bits of Hz)      │
│    total_channels      u32                            │
│    blocks_per_channel  u32                            │
│    dir_offset          u64  = 96                      │
│    payload_offset      u64  = 96 + channels*blocks*40 │
│    _padding            to fill 96 bytes               │
├─────────────────────────────────────────────────────┤
│  DIRECTORY  (channels × blocks × 40 bytes)           │
│  channel-major order; one entry per (channel, block) │
│    offset     u64  (byte offset of chunk in file)    │
│    len        u64  (byte length of chunk)            │
│    flags      u8   bit0=toggle bit1=first bit2=last  │
│    _padding   [u8; 7]                                │
│    l3_toggle  u64  (duplicated top-level toggle word)│
│    l3_last    u64  (duplicated top-level last word)  │
├─────────────────────────────────────────────────────┤
│  PAYLOAD  (all chunks, any order; see directory)     │
│  Each chunk covers one (channel, block) pair:        │
│    valid_samples  u32                                │
│    flags          u8  bit0=first bit1=last bit2=active│
│    _padding       [u8; 3]                            │
│    [only when active:]                               │
│      l1_toggle  [u64; 4096]   l1_last  [u64; 4096]   │
│      l2_toggle  [u64;   64]   l2_last  [u64;   64]   │
│      l3_toggle  u64           l3_last  u64           │
└─────────────────────────────────────────────────────┘
```

The index is opened via `Mmap`; leaf chunks are read zero-copy directly out of the mapping (no
application-level chunk cache — the OS page cache manages residency). The directory itself is
read into a `Vec<Vec<RootDirEntry>>` at open time, so the coarsest-level query (`load_root_summary`)
never touches the mmap.

Validity: the header records `source_revision` (the source file's byte length) plus
`total_samples`/`total_blocks`/`samples_per_block`/`samplerate_bits`/`total_channels`. On open,
`IndexReader::is_valid` rejects a stale sidecar so a changed capture rebuilds its index instead
of serving mismatched data. The writer builds into a `.idx.tmp` sibling and atomically renames
it into place on `finish()`; a dropped, unfinished writer removes the temp file.

---

## Raw Block Cache (`.dsl.idx.raw`)

[raw_block_cache.rs](../crates/signal_processing/src/raw_block_cache.rs) keeps a sparse sidecar with
one fixed-size slot per (channel, block), used only by the **exact** (deep-zoom / raw-scan)
query path — it is not part of the mipmap index.

- Layout: 64-byte header, a validity bitmap (one bit per slot), then slots in block-major order
  (all channels of one block adjacent), starting page-aligned.
- A slot is filled the first time its block is decompressed and is served zero-copy from a
  shared `Mmap` afterwards, so disk usage grows only with regions actually inspected at sample
  resolution.
- The validity bitmap is written back only on clean drop, after `sync_data` on the slot writes,
  so a set bit on disk always refers to fully-written data; a crash only loses cache entries
  (fully re-derivable from the archive).
- Reads go through the map while writes use the file descriptor, which is coherent on Unix
  targets sharing a page cache between `write()` and `MAP_SHARED`.

---

## Index Building

`IndexBuilder::build` ([builder.rs](../crates/signal_processing/src/waveform_index/builder.rs)) runs
on a background thread (spawned by the viewer's worker, see below):

1. Enumerate every `(channel, block)` job (`total_probes × total_blocks`).
2. Spawn `index_worker_count()` worker threads (`CAPTURE_INDEX_THREADS` / `DSL_INDEX_THREADS`
   env override, else `available_parallelism()`, capped to the job count). Each worker opens
   its own `BlockCaptureSource` reader and pulls jobs from a shared queue.
3. Each worker reads the packed block, then `build_leaf_summary` computes `first`, `last`, and
   the L1/L2/L3 toggle/last bitmaps in one pass (allocating `BlockLevels` on the heap to avoid a
   large stack frame). A block with no internal toggles yields `levels: None`.
4. Results are streamed back through an `mpsc` channel to a single collector, which reorders
   them per-channel (a small `HashMap` reorder buffer, not the whole index) so each leaf can be
   patched for boundary transitions against its immediate predecessor before being written.
5. `IndexWriter::write_block` appends the chunk to the payload and records its directory entry;
   `finish()` writes the header + directory and atomically renames the temp file into place.

Progress is reported as `CaptureIndexProgress { completed_roots, total_roots }` (one unit per
completed (channel, block) job).

---

## Runtime Querying — `IndexSampler`

`IndexSampler::open_data_source_with_progress` builds the index if the sidecar is
missing/invalid, opens the (optional) `RawBlockCache`, mmaps the index, and opens a raw
`BlockCaptureSource` reader for exact reads.

### `sampled_window(channels, start_sample, end_sample, target_points)`

This is the single query the viewer calls every time the visible window or viewport size
changes.

1. Clamp `[start_sample, end_sample)` to `[0, total_samples)` and compute
   `sample_step = ceil(samples / target_points)`.
2. **Exact path**: if `samples <= exact_window_sample_limit(target_points)` (at least
   `target_points × 64` samples, i.e. at least one L1 bit per rendered point, floor 4096),
   scan the raw packed bits directly (`exact_sampled_channel`) and return individual
   `CaptureTransition`s. This keeps short pulses from being widened by index summaries once the
   viewport is zoomed in close to 1:1.
3. **Indexed path**: otherwise pick the coarsest summary granularity that still resolves to
   roughly one group per rendered point:

   | `sample_step` | Group size used |
   |---|---|
   | `>= samples_per_block` | one whole block |
   | `>= 262,144` (L3) | L3 groups |
   | `>= 4,096` (L2) | L2 groups |
   | else | L1 groups |

   For each rendered point, `indexed_display_range_summary` walks the blocks overlapping that
   point's sample range and merges their `first`/`toggle`/`last` (falling back to the coarser
   directory-only `load_root_summary` when the whole block is covered or the group size is at
   least L3; otherwise `load_leaf` mmaps the chunk's L1/L2 bitmaps). `append_pixel_waveform`
   then turns each point's summary into one `CaptureWaveformSegment`:
   - `Activity { first, last }` if any toggle occurred in the point's range,
   - `Level { value }` if the point continues the previous level unchanged,
   - `Edge { before, after }` followed by a `Level` if the point's value differs from the
     previous point's exit value without an internal toggle.

The exact path returns `transitions` (empty `waveform`); the indexed path returns `waveform`
(empty `transitions`). `CaptureSampledWindow.sample_step` records which granularity was used.

### Raw block reads

Both the exact path and the raw-cache-backed reads for the UI's hover measurement (below) go
through `cached_packed_block`, which prefers the `RawBlockCache` slot map and falls back to
`raw_reader.read_packed_block` (decompressing from the ZIP), storing the result back into the
cache.

---

## UI Widget — `LogicAnalyzerViewer`

Per-frame flow in `show()`:

1. `process_worker_responses()` — drain the background worker's channel (native only).
2. `ensure_row_order()` — reconcile the row list against current channels + derived lanes.
3. Row-label input (rename double-click, drag reorder), cursor input, fit-to-view
   (double-click / `F`), then pan/zoom input.
4. `sample_visible_window()` — recompute `(start_sample, end_sample, target_points)` for the
   current view/viewport; if unchanged since last frame, skip the query. Otherwise call
   `sampled_window` synchronously on the UI thread and convert the result into
   `LogicChannel`s. What is drawn is therefore always exactly the current view — there is no
   separate asynchronous refinement pass that could disagree with it.
5. `sample_hover_measurement()` — refresh the pulse measurement under the pointer.
6. `draw()` — header, ruler, row labels, channel waveforms, derived lanes, pointer marker,
   measurement tooltip, time cursors; then the color-profile selector overlay.
7. Repaint scheduling: while opening (no `CaptureInfo` yet) repaint every ~16 ms; while
   indexing or waiting for the sampler, every ~100 ms. Otherwise egui's normal
   repaint-on-input applies.

### Background worker

`set_capture_path` spawns one thread (`spawn_capture_worker`) per opened capture:

1. Send `WorkerResponse::Opened` with `CaptureMetadata` as soon as the header is parsed (lets
   the UI show placeholder channels immediately).
2. Send a `Status` message ("Building waveform index…").
3. Build/validate the index, forwarding `IndexProgress` messages (throttled to once per
   ~100 ms or every 64 completed jobs, plus always the first and last).
4. Send `IndexReady` or `Error`.

`process_worker_responses` ignores messages for a stale `path` (superseded by a newer
`set_capture_path` call) and, on `IndexReady`, opens a fresh sampler on the UI-owned struct so
subsequent `sampled_window` calls run synchronously on the UI thread.

### Channel data model

```rust
struct LogicChannel {
    index: usize,
    name: String,
    initial: bool,
    transitions: Vec<Transition>,     // exact path: individual toggles
    waveform: Vec<WaveformSegment>,   // indexed path: per-point summaries
}

enum WaveformSegmentKind {
    Level { value: bool },
    Edge { before: bool, after: bool },
    Activity { first: bool, last: bool },
}
```

`draw_channel_waveform` draws from `waveform` when present (indexed/coarse view), otherwise
from `transitions` (exact view). `Activity` segments wider than ~3 px render as a solid filled
band — a truthful "something toggled here" signal, since drawing invented edge positions would
visibly jump on refinement; narrower activity segments draw a first/last step plus a center
tick.

### Derived lanes

Derived display uses two per-run stores:

- `DerivedLanes` in `signal-processing` maps stable storage keys to payloads, summaries, and
  indexed query handles;
- `ViewerLaneRegistry` in `logic-analyzer-viewer` maps explicit group and track identities to
  those payloads and supplies protocol-neutral renderer objects.

`DerivedLaneData` has these generic payload families:

- `Digital(Vec<Sample>)` — rendered like a channel waveform;
- `Annotations(Vec<Annotation>)` — `(start_ns, end_ns, label)` boxes (decoded words,
  formatted at render time);
- `Markers(Vec<u64>)` — zero-width event ticks (triggers).

Every visible payload belongs to an explicit group. Ordinary payloads use default singleton
groups; concrete producer builders can register compound groups and renderer objects through
opaque `ViewerOutputPresentation` metadata. Row identity, labels, height, drawing, hit-testing,
and snapping use group/track IDs rather than display names.

Before concrete renderer code runs, the viewer prepares a bounded `ViewerLaneFrame` while holding
the payload lock and then releases it. Sparse annotation frames contain exact values; dense frames
contain activity only. Indexed queries likewise clone their handles before storage access. No
renderer/plugin code or indexed I/O runs while `DerivedLanes` is locked.

Lanes are **uncapped** — a truncated decode is a wrong decode; backpressure, not dropping, is how
a slow viewer is handled (see [PIPELINE_DESIGN.md](PIPELINE_DESIGN.md)). Rendering windows raw
vectors via `partition_point`; dense digital and annotation windows fall back to bounded
per-pixel activity bands. Hover/snap queries (`channel_at_row`) go through each lane's
`AppendOnlyMipmap` — an incremental, append-only multi-resolution summary built alongside
the raw data by the same append calls — so hover cost stays bounded even when the visible
window spans millions of entries.

### Pulse measurement (hover)

`sample_hover_measurement` measures the high/low run under the pointer. Because the visible
`waveform` may only carry per-point summaries at low zoom, measurement always re-queries the
index directly around the pointer (`exact_transitions_around`) rather than reusing the drawn
data, then resolves any open boundary by searching outward (`prev_transition_at_or_before`,
`next_transition_after`) so width/period/duty-cycle are exact and independent of zoom level or
query-window size. In-memory channels (no sampler) measure from their `transitions` directly.

### Cursors

DSView-style vertical time cursors are added by double-clicking the ruler, dragged by their
flag or line, and numbered with freed numbers reused so a cursor's color (derived from its
number) stays stable while others come and go. Cursor drag/hover suppresses view panning and
ruler double-click suppresses fit-to-capture for the same event.

### Interaction summary

| Input | Effect |
|---|---|
| Drag (primary button, not on a cursor/label) | Pan the view |
| Scroll X | Pan the view |
| Scroll Y | Zoom, pivoted on the pointer's sample position |
| Double-click (not on ruler or a row label) / `F` | Fit whole capture to view |
| Double-click ruler | Add a time cursor |
| Drag a cursor flag/line | Move that cursor |
| Double-click a row label | Rename the row |
| Drag a row label | Reorder rows |
| Header-bar combo (right) | Switch color profile (DSView / Classic) |

---

## Properties at a Glance

| Concern | Mechanism |
|---|---|
| Multi-GB file, limited RAM | Index chunks are mmapped, not resident; only touched pages are faulted in |
| Zoom to full view | One block per rendered point at coarsest zoom; directory-only `l3_toggle`/`l3_last` avoids touching chunk payloads |
| Zoom to single sample | Exact path scans raw packed blocks (via `RawBlockCache` or ZIP decompression) once the viewport is within one L1 group per point |
| Viewing during index build | `Opened`/`Status`/`IndexProgress` messages let the UI show metadata and a progress bar before the sampler exists |
| Constant / idle signals | No L1/L2/L3 payload stored; directory `toggle` bit cleared; reconstructed from `first`/`last` alone |
| Boundary transitions | Patched into an otherwise-constant block's summaries by `apply_boundary_transition` |
| Live decode output | Derived lanes: uncapped shared store + incremental `AppendOnlyMipmap` summaries, repainted while the pipeline runs |
| Render time | Bounded by viewport width (`target_points`) and available index/raw data |
| Measurement accuracy | Always resolved via direct index queries, independent of the zoom level currently drawn |
