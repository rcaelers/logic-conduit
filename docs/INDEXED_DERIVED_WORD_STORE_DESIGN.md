# Indexed Derived Word Store - Design and Implementation Plan

Status: implementation complete (Steps 1-8); the aspirational 50-second full-cache target remains
open, while the live-decoding requirement is met at 3.79x real time.

Remaining validation, operability, optimization, and cleanup work is tracked in Section 19.

Primary implementation areas:

- `crates/signal_processing/src/runtime/derived_word_store/` (new)
- `crates/signal_processing/src/nodes/sinks/viewer_sink.rs`
- `crates/signal_processing/src/runtime/derived_index.rs`
- `crates/widgets/logic_analyzer_viewer/src/draw/derived.rs`
- `crates/widgets/logic_analyzer_viewer/src/cursor.rs`
- `crates/widgets/logic_analyzer_viewer/src/channel.rs`
- `crates/logic_analyzer_graph/src/compiler/viewer.rs`
- `crates/logic_analyzer_graph/src/compiler/mod.rs`

Related documents:

- [LOGIC_ANALYZER_VIEWER_DESIGN.md](LOGIC_ANALYZER_VIEWER_DESIGN.md)
- [PARALLEL_DECODER_PERFORMANCE_PLAN.md](PARALLEL_DECODER_PERFORMANCE_PLAN.md)
- [PARALLEL_DECODER_PARALLELISM_PLAN.md](PARALLEL_DECODER_PARALLELISM_PLAN.md)
- [PIPELINE_DESIGN.md](PIPELINE_DESIGN.md)

---

## 1. Problem Statement

The optimized parallel decoder can process the 255.6 second reference capture in
approximately 30-40 seconds. During the long active interval it can produce roughly
12 million eight-bit words per capture second, or about three billion words for the
complete recording.

Keeping every word as the current in-memory `Annotation` representation is not viable:

```text
Annotation {
    start_ns: u64,
    end_ns:   u64,
    value:    u64,
}

minimum payload size = 24 bytes/word
3 billion words       = 72 GB before Vec capacity and allocator overhead
```

Bounded retention prevents the memory growth, but discarding old annotations also removes
the exact values and word boundaries needed after panning to an older part of the capture.
The current `ChunkedMipmap` preserves only a coarse presence summary; it cannot reconstruct
the original word value or snap a cursor to an evicted boundary.

The viewer needs a derived-data equivalent of the raw capture cache and waveform index:

1. A compact, append-only store containing every decoded value and timestamp.
2. A small directory that finds the encoded block containing a timestamp.
3. Restart points inside each encoded block so a query never decodes from the beginning.
4. A multi-resolution index answering whether data exists in a time interval and how dense
   it is without decoding values.
5. A bounded decoded-block cache for repeated zoom, pan, render, and cursor operations.

---

## 2. Goals

The first production version must provide all of the following:

- Preserve every decoded `Word` value, timestamp, and explicit duration.
- Keep resident memory bounded independently of recording duration.
- Show activity over the complete decoded timeline at overview zoom.
- Recover exact word boxes and values after zooming into any historical region.
- Snap cursors to exact historical word starts and ends.
- Support queries while decoding is still in progress.
- Keep file-writer output and other word consumers independent from viewer storage.
- Sustain more than 12 million eight-bit words per second on the reference machine.
- Avoid file I/O and VLQ decoding while holding the `DerivedLanes` UI lock.
- Detect and reject stale persistent caches after capture or graph changes.
- Fail without corrupting the primary decode or file-writer branch.

### 2.1 Performance targets

Release-build targets on the development machine:

| Operation | Target |
| --- | ---: |
| Append eight-bit words | at least 20 million words/s |
| Full reference decode plus cache | less than 50 s |
| Peak application RSS | less than 500 MiB |
| Cold exact query, one viewport | less than 50 ms |
| Warm exact query, one viewport | less than 5 ms |
| Nearest-boundary query, warm | less than 2 ms |
| UI work under `DerivedLanes` lock | less than 1 ms per publication |
| Stop/cancel latency | less than 100 ms |

The append target intentionally exceeds the 12 MHz live word rate so viewer caching does not
become the source of backpressure.

---

## 3. Non-Goals for the First Version

- A general database for editing decoded words.
- In-place mutation or deletion of individual words.
- One cache shared by unrelated captures or graph configurations.
- Compression tuned for every protocol or value distribution.
- A B-tree or LSM tree before a flat sorted block directory is measured and found lacking.
- Persistent caching on wasm.
- Replacing the normal `Word` stream used by file writers and other pipeline nodes.
- Materializing exact values at overview zoom where several million words map to one pixel.

---

## 4. Ownership Decision

### 4.1 The store belongs to the viewer lane

The indexed store should be generic to every `Word` lane displayed by a `ViewerSink`. It
should not be embedded directly in `ParallelDecoder`.

Reasons:

- SPI, UART, plugin, and future decoders can produce word lanes with the same scaling issue.
- A decoder whose output is connected only to a file writer should not create a multi-GB
  display cache.
- Viewer cache lifecycle, retention, error display, and cleanup are UI/storage concerns.
- The existing batched `Word` stream is already fast enough to feed both file and viewer
  branches.
- The graph remains explicit: the viewer connection is what requests display storage.

`ParallelDecoder` remains responsible for finding triggers, sampling values, preserving
ordering, and producing batches. `ViewerSink` materializes those batches into an
`IndexedAnnotationStore` for each word lane.

### 4.2 Optional future decoder hint

A later optimization may let a word producer attach a `WordStorageHint` to its output schema:

```rust
pub struct WordStorageHint {
    pub value_bits: Option<u8>,
    pub timestamp_quantum_ns: Option<u64>,
}
```

This is not required for version 1. The store can select the smallest value width per block
and encode timestamp deltas directly in nanoseconds. On the reference bus, 80-100 ns deltas
already fit in one VLQ byte.

---

## 5. Architecture

```text
ParallelDecoder
  |
  | ordered Vec<Word> batches
  +---------------------------> BinaryFileWriter
  |
  +----> BufferNode<Word> ----> ViewerSink word lane
                                  |
                                  | append_batch
                                  v
                         IndexedAnnotationStore
                           |              |
                           |              +-- in-memory committed directory
                           |              +-- time presence/count mipmap
                           |              +-- decoded-block LRU
                           |
                           +-- temporary append-only data file
                           +-- final data sidecar
                           +-- final index sidecar
                                  |
                                  v
                         AnnotationQuery handle
                                  |
                    +-------------+-------------+
                    |                           |
                    v                           v
           viewer render query        cursor boundary query
```

The pipeline node thread, not the egui thread, appends encoded blocks. The UI receives an
`Arc<dyn AnnotationQuery>` handle and performs bounded time-window queries. The handle
publishes only fully committed blocks.

During a run, the writer uses normal file writes and readers use `read_at`/`pread` against the
committed prefix. After a successful finish, the immutable files can be reopened with mmap.
This avoids repeatedly remapping a file that grows while decoding.

---

## 6. Logical Data Model

### 6.1 Stored word

The input remains the existing runtime type:

```rust
pub struct Word {
    pub value: u64,
    pub timestamp_ns: u64,
    pub duration_ns: u64,
}
```

Words must arrive in nondecreasing timestamp order. Equal timestamps are retained in arrival
order. An out-of-order timestamp is a store error and must identify the lane and offending
timestamps.

An instantaneous word has `duration_ns == 0`. Adjacent words within a decode burst meet at the
next word's start. If the next word arrives much later than the recent cadence, the visual end is
bounded to one prior period (and at most 1 ms), leaving gated-off or otherwise inactive intervals
empty. A non-zero duration is authoritative and must be stored exactly; this covers multi-cycle
and partial words.

### 6.2 Encoded block

One block contains at most 32,768 words. It may close earlier when any limit is reached:

- encoded payload exceeds 1 MiB;
- timestamp span exceeds a configurable maximum;
- the gap from the previous word exceeds `MAX_INTER_WORD_GAP_NS`;
- the lane is flushed, stopped, or completed.

Closing on a large gap keeps sparse regions outside block spans, so a block-level presence
record does not imply activity through a long idle interval.

The initial gap threshold should be 1 ms and must be benchmark-configurable. It affects only
block boundaries and summary precision, never exact data.

### 6.3 Value width

Each block stores every value using the smallest fixed width capable of representing the
largest value in that block:

| Maximum value | Stored bytes |
| --- | ---: |
| `0xff` | 1 |
| `0xffff` | 2 |
| `0xffff_ffff` | 4 |
| otherwise | 8 |

The reference eight-bit lane therefore uses one value byte per word without needing decoder-
specific metadata.

### 6.4 Timestamp encoding

The block header stores the first timestamp as an absolute `u64` nanosecond value. Each record
stores an unsigned VLQ delta from the previous timestamp. The first record has delta zero.

For example, dense DDR words at 80 ns and 100 ns intervals encode as:

```text
delta VLQ: 1 byte
value:     1 byte
total:     approximately 2 bytes/word
```

Long gaps naturally use additional VLQ bytes. Because large gaps normally start a new block,
they do not inflate all later records.

### 6.5 Duration encoding

Adding a tag byte to every record would increase the common eight-bit representation by 50%.
Durations are therefore stored in a sparse side stream:

```text
duration exception = VLQ(record_index_delta), VLQ(duration_ns)
```

Only records with `duration_ns != 0` appear in this stream. The block header records its
offset and exception count. An empty duration stream costs no per-word bytes.

---

## 7. On-Disk Files

Each cache generation consists of three siblings:

```text
<cache-key>.dwd       encoded word data blocks
<cache-key>.dwi       block directory and mipmap
<cache-key>.json      manifest and final commit marker
```

Temporary builds use `.tmp` suffixes. The manifest is renamed last and is the commit point.
Readers ignore data/index files without a valid matching manifest.

All binary integers are little-endian. Every structure includes an explicit version and size
so future readers can reject or skip incompatible layouts.

### 7.1 Data file header

Proposed magic: `DWRDDAT1`.

```text
DataHeader (64 bytes)
  magic                  [u8; 8]
  version                u32
  header_size            u32
  cache_key_prefix       [u8; 16]
  created_unix_ns        u64
  flags                  u64
  reserved               [u8; 16]
```

No final block count is required in this header. The index and manifest describe the committed
set.

### 7.2 Data block

Proposed block magic: `DWBL`.

```text
WordBlockHeader (72 bytes)
  magic                  [u8; 4]
  header_size            u16
  flags                  u16
  sequence               u64
  first_timestamp_ns     u64
  last_timestamp_ns      u64
  word_count             u32
  value_bytes            u8
  reserved_0             [u8; 3]
  record_payload_len     u32
  restart_count          u32
  restart_table_offset   u32   (relative to block start)
  duration_count         u32
  duration_table_offset  u32   (relative to block start)
  block_len              u32
  crc32c                 u32   (header with crc=0 plus all block payload)
  reserved_1             [u8; 4]
```

The block body is:

```text
record payload
restart table
duration exception table
padding to 8-byte alignment
```

The writer computes the full block in a reusable memory buffer, writes it with `write_all`,
and only then publishes its directory entry. A reader never observes a partial block through
the live in-memory directory.

### 7.3 Record payload

For each word in arrival order:

```text
VLQ(timestamp_delta_ns)
value[value_bytes]
```

VLQ is unsigned LEB128. Decoders must reject encodings longer than ten bytes, arithmetic
overflow, timestamps before the block start, and timestamps beyond the directory entry's
declared end.

### 7.4 Restart table

A fixed restart interval of 512 records is the measured default. Each entry is 16 bytes:

```text
RestartEntry
  timestamp_ns          u64
  payload_offset        u32
  record_index          u32
```

The first record always has a restart entry. To find a timestamp inside a block, binary-search
the restart timestamps and decode forward at most 511 records.

At 32,768 records per block this adds 1 KiB per full block. Three billion words produce about
92,000 block directory entries and around 91 MiB of restart entries, which is acceptable and
far smaller than a per-word index.

### 7.5 Block directory entry

Proposed index magic: `DWRDIDX1`.

```text
BlockDirectoryEntry (48 bytes)
  sequence               u64
  first_timestamp_ns     u64
  last_timestamp_ns      u64
  data_offset            u64
  block_len              u32
  word_count             u32
  value_bytes            u8
  flags                  u8
  reserved               [u8; 6]
```

Entries are sorted by `(first_timestamp_ns, sequence)`. The directory is small enough to keep
in memory during a live run and mmap after completion. With 46,000 blocks it occupies about
2.1 MiB, so a B-tree is unnecessary.

### 7.6 Index header and levels

```text
IndexHeader
  magic                  [u8; 8]
  version                u32
  header_size            u32
  cache_key              [u8; 32]
  block_count            u64
  total_word_count       u64
  first_timestamp_ns     u64
  last_timestamp_ns      u64
  directory_offset       u64
  level_directory_offset u64
  level_count            u32
  reserved               ...
```

The immutable index is written only at successful finish. The live store keeps the equivalent
directory and mipmap levels in memory.

---

## 8. Presence/Count Mipmap

The raw waveform index answers whether a signal toggled in a sample interval. A derived word
index answers whether one or more decoded words exist in a time interval.

### 8.1 Leaf record

One leaf corresponds to one occupied run within a committed word block:

```rust
pub struct WordSummaryRecord {
    pub start_ns: u64,
    pub end_ns: u64,
    pub word_count: u64,
    pub first_block: u64,
    pub block_count: u32,
}
```

`word_count > 0` means the interval contains decoded data. Instantaneous-word ends use the same
recent-cadence inference as exact annotations, so a long decoder-disabled interval splits one block
into multiple leaves. A leaf references its encoded block for exact and cursor queries. To keep the
index bounded for pathological sparse inputs, each block retains at most 256 runs by coalescing
across the smallest gaps first; the largest and most visible inactive intervals survive.

### 8.2 Higher levels

Every 64 adjacent records combine into one record at the next level:

```text
level 0: one record per occupied run
level 1: one record per 64 runs
level 2: one record per 4,096 runs
level 3: one record per 262,144 runs
```

Only the levels needed for the actual block count are stored. An incomplete tail is folded into
one temporary live record during queries, matching the existing derived mipmap behavior.

### 8.3 Viewport query semantics

The renderer divides the visible time range into at most one bucket per horizontal pixel. For
each bucket the mipmap reports:

```rust
pub struct WordPresenceBucket {
    pub start_ns: u64,
    pub end_ns: u64,
    pub word_count: u64,
}
```

The renderer draws an activity band only when `word_count > 0`. A combined record may cover
gaps between child blocks, so the query must descend whenever a record straddles a pixel-bucket
boundary. It must never paint a multi-pixel span merely because a coarse ancestor contains data
somewhere inside it.

This preserves the viewer's existing truthfulness rule: at coarse zoom the UI says that data
exists in a pixel interval, but does not invent individual word boundaries or values.

### 8.4 Exact-query crossover

The store returns exact annotations when the estimated word count in the visible window is no
more than an exact-detail budget. The initial budget should be:

```text
max(4,096, viewport_width_pixels * 4)
```

Above that budget, rendering uses presence buckets. Zooming in eventually crosses the budget
and switches to exact values. The decision uses mipmap counts and does not decode a block merely
to decide which path to take.

---

## 9. Public Query API

The store should expose a viewer-oriented trait rather than its file layout:

```rust
pub trait AnnotationQuery: Send + Sync {
    fn metadata(&self) -> AnnotationStoreMetadata;
    fn generation(&self) -> u64;

    fn presence_window(
        &self,
        start_ns: u64,
        end_ns: u64,
        target_buckets: usize,
    ) -> Result<Vec<WordPresenceBucket>>;

    fn exact_window(
        &self,
        start_ns: u64,
        end_ns: u64,
        max_words: usize,
    ) -> Result<ExactAnnotationWindow>;

    fn nearest_boundary(
        &self,
        timestamp_ns: u64,
        max_distance_ns: u64,
    ) -> Result<Option<u64>>;
}
```

```rust
pub struct ExactAnnotationWindow {
    pub annotations: Vec<Annotation>,
    pub complete: bool,
    pub generation: u64,
}
```

`complete == false` means the caller's word limit was reached. The renderer must then use the
presence path rather than draw a misleading partial exact window.

The generation increments after every committed block. Viewer sampling keys include it so a
live lane is resampled when new blocks arrive, but unchanged historical windows remain cached.

### 9.1 Nearest boundary

`nearest_boundary` considers:

- word start timestamps;
- `start + duration` for explicit non-zero durations;
- the next word's start for adjacent instantaneous words, or the cadence-bounded inferred end
  before a long decoding gap.

It binary-searches the block directory, checks neighboring blocks, uses restart entries to seek,
and decodes only the local records needed to select the closest boundary. Ties retain the
viewer's existing deterministic rule.

---

## 10. Decoded Block Cache

Exact queries decode VLQ records into a bounded LRU:

```rust
struct DecodedWordBlock {
    sequence: u64,
    annotations: Arc<[Annotation]>,
    memory_bytes: usize,
}
```

The default memory budget should be 64 MiB per application, shared by all indexed word lanes.
The cache key includes the store identity and block sequence. Eviction is by total decoded
bytes, not block count, because sparse and duration-heavy blocks can have different sizes.

The encoded file itself is not copied into an application cache. Completed persistent files are
mmap-backed and rely on the OS page cache. Live temporary files use positional reads bounded to
one committed block.

Repeated cursor queries and small pans should normally hit decoded blocks. A full-view render
must not populate the decoded-block cache because it uses only mipmap records.

---

## 11. Live Append and Commit Model

### 11.1 Append path

`ViewerSink` owns one `IndexedAnnotationWriter` per word lane. Its node thread already runs
outside the egui thread and receives batches. The initial implementation appends synchronously
on that node thread:

1. Validate timestamp ordering.
2. Add words to the current block builder.
3. Close blocks that hit a size, count, span, or gap boundary.
4. Encode the block into a reusable byte buffer.
5. Write the complete block to the temporary data file.
6. Publish its directory entry and mipmap leaf under the store's short metadata lock.
7. Increment the generation and request a repaint through the existing application path.

No file write or VLQ loop may run under the `DerivedLanes` lock.

If benchmarks show synchronous encoding cannot stay ahead of 12 million words/s, add a dedicated
store-writer thread with a bounded queue of existing `Vec<Word>` batches. Do not add this thread
before measurement; an extra copy or queue can cost more than the encoder.

### 11.2 Hot tail

Words in the not-yet-closed block are kept in a small in-memory tail. Query results merge:

1. committed block data;
2. the immutable snapshot of the current hot tail.

The hot tail contains at most 32,768 words and is the only exact word vector whose size is tied
to appending rather than the decoded-block LRU.

The writer publishes a cloned tail snapshot only at a bounded cadence, initially every 16,384
words or 50 ms, whichever comes first. It must not clone the tail for every input batch.

### 11.3 Finish

On normal completion:

1. Close the hot block.
2. Flush and `sync_data` the temporary data file.
3. Serialize the complete directory and mipmap to the temporary index file.
4. Flush and `sync_data` the temporary index file.
5. Rename data and index files to their final names.
6. Write and sync the manifest temporary file.
7. Rename the manifest last.

Only a valid final manifest makes a persistent cache discoverable.

### 11.4 Cancel, crash, and disk-full behavior

- A cancelled run closes the writer promptly and removes temporary files by default.
- A process crash leaves only `.tmp` files; the next startup may remove stale temporaries.
- CRC failure invalidates the affected cache generation, not the source capture.
- Disk-full or permission errors put the viewer lane into an error state and stop caching that
  lane. They must not stop the file writer or unrelated graph branches.
- The UI shows a lane warning and may fall back to the existing bounded in-memory tail plus
  coarse summary.

---

## 12. Viewer Data Model Changes

The current shape:

```rust
DerivedLaneData::Annotations(Vec<Annotation>)
```

must become a lane object capable of serving both memory and indexed storage:

```rust
pub enum AnnotationLaneSource {
    InMemory(InMemoryAnnotationLane),
    Indexed(IndexedAnnotationLane),
}

pub struct IndexedAnnotationLane {
    pub query: Arc<dyn AnnotationQuery>,
    pub status: AnnotationStoreStatus,
}
```

The exact names may change, but these properties are required:

- `DerivedLaneData` remains cloneable through `Arc` handles.
- Tests and wasm can keep the in-memory implementation.
- Native file/live pipelines use the indexed implementation by default for word lanes.
- Render and cursor code do not pattern-match directly on a raw `Vec` after migration.

### 12.1 Locking rule

Current rendering obtains a `DerivedLanes` read guard and draws while it is held. Indexed
queries may fault pages or decode blocks, so this must change:

1. Lock `DerivedLanes` briefly.
2. Clone the lane metadata and `Arc<dyn AnnotationQuery>`.
3. Release the lock.
4. Perform presence/exact queries.
5. Paint the result.

The same rule applies to cursor snapping. No indexed store method may be called while the lane
collection lock is held.

### 12.2 Viewer sampling cache

The viewer caches the last sampled annotation window by:

```text
lane identity
store generation
visible start/end
viewport width
exact/presence mode
```

Unchanged frames reuse the sampled result. A generation change invalidates a live window only
when newly committed data can intersect it; historical windows before the previous committed end
remain reusable.

### 12.3 Rendering

- Presence mode draws per-pixel activity/density bands.
- Exact mode uses the existing word-box renderer and value formatting.
- Partial and explicit-duration words use their stored end time.
- An instantaneous final word uses the existing previous-width/burst-cap fallback.
- Transitioning between presence and exact mode must not change the reported capture extent.

### 12.4 Cursor snapping

The cursor layer delegates annotation rows to `AnnotationQuery::nearest_boundary`. In-memory
lanes implement the same logical operation over their vector. This removes the current split
where only retained recent annotations can snap exactly.

---

## 13. Cache Location, Identity, and Invalidation

### 13.1 Version 1: temporary cache

The first implementation uses an application cache/temp directory and removes the cache when the
run or application closes. This delivers bounded memory and exact historical queries without
first solving graph identity.

Temporary cache names use a random run UUID and lane ID. They are never reused after restart.

### 13.2 Version 2: persistent cache

Persistent cache identity is a BLAKE3 hash over:

- store format version;
- source capture fingerprint;
- source sample rate and sample count;
- canonical serialization of the upstream subgraph feeding this viewer lane;
- every upstream node configuration value;
- decoder implementation/cache ABI version;
- lane output name and resolved variadic member ordering.

The current capture fingerprint based only on file length is not sufficient for a multi-GB
persistent derived artifact. It should include at least file length, modification time, and a
hash of the capture header. A stronger sampled-content or full-content hash can be added later.

Suggested location:

```text
<platform cache dir>/dsl/derived/<source-id>/<cache-key>/
```

Do not place multi-GB derived files beside a read-only capture by default. The UI may expose a
project-local cache location as an explicit option.

### 13.3 Lifecycle policy

The cache manager records last access, encoded size, source path, and completion state. It
supports:

- maximum total cache size;
- least-recently-used cleanup;
- manual "Clear derived cache";
- removal of incomplete temporaries;
- pinning a cache used by an open graph;
- reporting expected and current disk usage.

Persistent reuse is an optimization. Cache deletion must never affect the source capture or
file-writer output.

---

## 14. wasm and Non-Filesystem Fallback

wasm keeps an in-memory annotation lane and `ChunkedMipmap`. It cannot provide unlimited exact
history for an unbounded stream. The host must choose a bounded retention policy and the UI must
identify that older exact data is unavailable.

The common `AnnotationQuery` interface should still be used so render and cursor code do not
fork deeply by platform. `InMemoryAnnotationQuery` implements it over the retained vector and
summary.

Native cache creation failures use the same fallback.

---

## 15. Alternatives Considered

### 15.1 Keep every `Annotation` in memory

Rejected because the reference recording requires at least 72 GB for payload fields alone and
becomes progressively slower under memory pressure.

### 15.2 Keep only a bounded tail plus a mipmap

Useful as a fallback, but rejected as the complete solution because old word values and exact
boundaries cannot be reconstructed.

### 15.3 Put the store inside `ParallelDecoder`

Rejected for the first version because it duplicates viewer policy in one decoder, creates large
caches even when no viewer is connected, and does not solve other word-producing nodes.

### 15.4 Add a per-word fixed-size index

Rejected because three billion fixed index records would itself require tens of gigabytes.
Block directory plus restart points bounds exact lookup with roughly 180 MiB of restart data.

### 15.5 Use a B-tree immediately

Rejected because the expected directory is about 46,000 entries and binary search is trivial.
The immutable flat representation is simpler to validate and mmap.

### 15.6 Compress blocks with zstd immediately

Deferred. VLQ plus narrow values already targets about two bytes per reference word. General
compression may reduce disk size but competes directly with live decode CPU. Add optional LZ4 or
zstd only after measuring encode throughput and real disk usage.

### 15.7 Mmap the growing live file

Rejected initially because remapping on every growth/commit complicates pointer lifetimes and
cross-platform behavior. Use positional reads during a live run and mmap only immutable final
files.

---

## 16. Correctness Invariants

The implementation must maintain these invariants:

1. Directory entries describe complete, CRC-valid blocks only.
2. Block sequence numbers are contiguous and never reused within a generation.
3. Word timestamps are nondecreasing globally and within blocks.
4. Concatenating decoded blocks exactly reproduces input word order.
5. Explicit durations round-trip without modification.
6. Mipmap counts equal the number of committed words represented below each node.
7. Presence queries never report zero for a bucket containing a word.
8. Presence rendering never invents an exact boundary or value.
9. Exact queries either return every word intersecting the requested interval or set
   `complete == false`.
10. A published persistent manifest refers only to synchronized data and index files with the
    same cache key.
11. Store failure cannot change another consumer's word stream.
12. UI lane locks are never held across file I/O, mmap page faults, or VLQ decode loops.

---

## 17. Test Plan

### 17.1 Encoding unit tests

- VLQ values at 0, 127, 128, `u32::MAX`, and `u64::MAX`.
- Reject overlong, truncated, and overflowing VLQ values.
- One-, two-, four-, and eight-byte value blocks.
- Equal timestamps and long timestamp gaps.
- Empty and populated duration exception streams.
- Restart boundaries at records 0, 255, 256, 257, and 65,535.
- Partial final blocks.
- CRC corruption and truncated block rejection.

### 17.2 Randomized round-trip tests

Generate ordered word sequences with random:

- timestamp deltas;
- values across all widths;
- duplicate timestamps;
- explicit durations;
- block count/span/gap boundaries.

Append, finish, reopen, decode, and compare every tuple exactly.

### 17.3 Directory and query tests

- Query before the first word, between blocks, and after the last word.
- Query spanning one and many blocks.
- Query exactly at block boundaries.
- Exact limit sets `complete == false` without returning a misleading suffix.
- Nearest-boundary checks current, preceding, and following blocks.
- Instantaneous-word visual ends and explicit-duration ends.
- Decoded-block LRU hit, eviction, and memory-budget accounting.

### 17.4 Mipmap differential tests

For randomized word timelines, compare every presence bucket and count with a direct scan of the
source words. Cover:

- dense continuous data;
- isolated words;
- long empty gaps;
- incomplete 64-way groups at every level;
- arbitrary viewport bucket boundaries;
- live uncommitted tails.

### 17.5 Live concurrency tests

- Append while repeatedly querying old and current windows.
- Readers never see a partially written block.
- Generation changes only after commit.
- Stop while a block is open.
- Disk error while file writer continues.
- Viewer detach/restart releases handles and temporary files.
- UI lock timing instrumentation proves queries occur after the lock is released.

### 17.6 Viewer tests

- Full recording shows presence outside the hot tail.
- Zooming into an old region produces exact value boxes.
- Cursor snaps to starts and ends in an evicted/historical region.
- Partial words retain their exact width.
- Home-to-fit uses complete store extent.
- Presence/exact crossover does not create blank frames.
- Multiple word lanes have independent stores and shared LRU accounting.

### 17.7 Integration and performance tests

Use `parallel-decoder-bench` plus a store sink mode:

```text
--sink indexed-viewer
--cache temporary|persistent
--decoded-cache-mib 64
```

Report:

- samples/s and words/s;
- real-time factor;
- encoded bytes and bytes/word;
- block and restart counts;
- encoder CPU time;
- peak queue depth;
- peak RSS;
- cold/warm exact query latency;
- presence query latency;
- cache hit rate;
- final output fingerprint.

The opt-in full reference test must verify:

- all file-writer outputs remain byte-identical;
- indexed store word count matches decoder count;
- random exact windows match a direct retained/reference decode;
- random cursor queries match direct boundary search;
- full run finishes below the performance target.

---

## 18. Implementation Plan

### Step 1: Format and codec module

Status: implemented in `crates/signal_processing/src/runtime/derived_word_store/`.

The implementation includes the fixed data/block headers, unsigned LEB128 codec, narrow value
widths, sparse duration exceptions, restart entries, CRC32C validation, a reusable block builder,
randomized round-trip coverage, the dense-size guard, and a release-only throughput guard.

Create `runtime/derived_word_store` with:

- unsigned VLQ encoder/decoder;
- block builder and decoder;
- duration exception stream;
- restart table builder/search;
- fixed binary header serialization;
- CRC validation;
- randomized round-trip tests.

Exit criteria:

- An arbitrary ordered `Vec<Word>` round-trips exactly.
- The dense eight-bit fixture averages no more than 2.2 encoded bytes per word excluding restart
  and file headers.
- Block encoding alone exceeds 20 million words/s.

### Step 2: Append-only live store

Status: implemented in `crates/signal_processing/src/runtime/derived_word_store/store.rs`.

The native implementation owns a temporary data file, appends bounded encoded blocks, publishes
directory entries only after complete writes, exposes positional committed-block reads, maintains
generation/status metadata and bounded hot-tail snapshots, and provides distinct finish and
low-latency cancel paths. The file remains alive until the last writer/query handle is dropped.

Implement:

- temporary data file creation;
- current block/hot tail;
- committed in-memory block directory;
- positional block reads;
- generation counter;
- finish/cancel behavior;
- store status and error propagation.

Exit criteria:

- Concurrent readers observe committed blocks and hot-tail snapshots without partial data.
- Cancellation stays below 100 ms.
- Disk errors are isolated to the store.

### Step 3: Exact reader and decoded-block LRU

Status: implemented in `crates/signal_processing/src/runtime/derived_word_store/{query,cache,store}.rs`.

Exact queries binary-search the committed directory, use restart-bounded cold decoding at window
edges, merge the live hot tail, and return completeness plus store generation. Nearest-boundary
queries cover starts and explicit ends with deterministic earlier-boundary tie-breaking. Broad
decoded blocks use a process-wide 64 MiB byte-budgeted LRU keyed by store identity and sequence;
finished stores atomically switch their read backend from positional file reads to an immutable
mmap.

Implement:

- directory binary search;
- restart seek plus bounded forward VLQ decode;
- exact time-window queries;
- nearest-boundary queries;
- process-wide decoded-block cache with byte budget;
- immutable mmap reader for completed stores.

Exit criteria:

- Exact and nearest-boundary randomized differential tests pass.
- Query work is proportional to intersecting blocks plus at most one restart interval at each
  boundary.

### Step 4: Presence/count mipmap

Status: implemented in `crates/signal_processing/src/runtime/derived_word_store/presence.rs` and exposed through
`IndexedAnnotationStore`'s `AnnotationQuery` implementation.

Every committed word block contributes one or more gap-aware occupied-run leaves. Complete groups
fold into 64-way levels; bucket queries consume aligned full groups at the highest available level
and estimate only the leaf records intersecting bucket boundaries. Leaves retain their source block
sequence independently of their leaf index. The live hot tail is merged from its bounded snapshot.
Overview queries return no more than the requested bucket count and perform no encoded-block reads
or word decoding. Persistent index version 2 serializes the directory and run tables separately.

Implement the 64-way block summary levels and per-pixel presence query. Reuse general ideas from
`derived_index.rs`, but create bounded occupied runs from each block rather than storing one leaf
record per word.

Exit criteria:

- Presence/count differential tests pass for dense, sparse, and gapped timelines.
- Full-capture presence query returns at most a small multiple of viewport width.
- Overview queries perform no word-block decode.

### Step 5: ViewerSink integration

Status: implemented in `crates/signal_processing/src/nodes/sinks/viewer_sink.rs`.

Native `ViewerSink` word lanes now create one `IndexedAnnotationWriter`, append each drained
`Vec<Word>` directly, and publish an `IndexedAnnotationLane` containing the shared query handle,
metadata, and live/finished/cancelled/failed status. End-of-stream finishes the writer; dropping a
running sink cancels it. Store creation failure falls back to the existing in-memory annotation
lane, while an append/finish failure remains visible on that indexed lane without stopping signal,
trigger, or independent pipeline branches. wasm and the explicit `with_indexed_words(false)` mode
retain the in-memory implementation.

The native lane no longer stores a one-million-entry annotation tail in `DerivedLanes`; resident
lane state is the query/store handle plus the bounded hot tail, directory, presence index, and
decoded-block cache owned by the indexed store. The viewer benchmark obtains the complete word
count from indexed metadata instead of scanning a retained annotation vector. Rendering and cursor
queries intentionally remain Step 6, so this step only makes consumers recognize the new lane kind.

Release validation on 12 July 2026 used the first 200,000,000 samples of
`_captures/wipneus5.dsl` (4.0 capture seconds). The count and indexed-viewer sinks both produced
exactly 47,999,433 words. The indexed writer spent 0.893 seconds appending 733 batches, equivalent
to approximately 53.8 million words/s, and the complete viewer run took 0.985 seconds (4.06x real
time). Peak RSS was 274.7 MiB versus 254.8 MiB for the count sink despite indexing almost 48 million
words. The standalone codec guard measured 94.9 million words/s. This run also found and fixed a
release-only block-boundary bug where the retry append was incorrectly placed inside a
`debug_assert_eq!`; a release-mode multi-block regression test now passes.
The multi-billion-word/full-capture RSS acceptance run remains part of Step 8; this Step 5 run
validates the integrated path and bounded-memory trend without generating a multi-gigabyte cache.

Replace native indexed word-lane appends with `IndexedAnnotationWriter`:

- construct one store per word lane;
- append existing batches without scalarizing them;
- publish the query handle and status through `DerivedLanes`;
- keep in-memory lanes for wasm and fallback;
- remove the one-million-entry exact-tail dependency for indexed native lanes.

Exit criteria:

- Viewer caching sustains the live word rate.
- RSS stays bounded during a multi-billion-word synthetic run.
- File writer output remains unchanged with the viewer attached or detached.

### Step 6: Rendering and cursor integration

Status: implemented in `crates/widgets/logic_analyzer_viewer/src/{indexed_annotations,draw,cursor,viewer}.rs`.

The viewer owns a bounded per-lane viewport cache keyed by query identity, store generation,
visible nanosecond range, and pixel width. Sampling clones only the lane name and query `Arc` while
holding `DerivedLanes`; exact decoding and presence queries run after releasing that lock. A window
first requests at most two annotations per pixel. Complete results use the existing value-box
renderer, including explicit-duration partial words and open-final-word sizing. An incomplete dense
result switches to at most one presence bucket per pixel, so overview rendering neither decodes nor
retains the complete word stream. Stable finished viewports reuse their cached result, while live
generation changes refresh at the store's bounded publication cadence.

Gap-aware occupied-run summaries preserve long disabled intervals even when two bursts share one
encoded block and the view is too coarse for exact decoding. Presence queries additionally estimate
the visible word count from the summaries and, when it is no more than eight words per target pixel
(with a minimum budget of 32), perform a bounded exact query and rasterize annotation intervals into
presence buckets. Denser and full-capture views stay on the run-summary path and do not populate the
decoded-block cache.

Cursor snapping clones the same query handle under the lane lock and calls `nearest_boundary`
outside it with the existing eight-pixel snap distance converted to nanoseconds. Store metadata now
includes `extent_end_ns`, the maximum explicit end across committed summaries and the live hot tail;
Home-to-fit uses that extent when no capture metadata is available. Live indexed lanes request a
50 ms repaint cadence so hot-tail publications become visible without a separate UI notification.
Duration-bearing blocks carry a format flag and are fully decoded for exact/boundary queries when
needed. A prefix-max block-extent directory finds a long partial word even when its end lies several
encoded blocks after its start, without broad decoding for ordinary instantaneous-word blocks.

Tests cover exact retrieval from old committed blocks, explicit-duration preservation, bounded
dense presence results, internal gaps within one encoded block, live cache invalidation, indexed
start/end cursor snapping, long-word extent across later blocks, and Home-to-fit. The
200,000,000-sample release reference run still
indexed exactly 47,999,433 words, with 0.886 seconds in indexed appends (approximately 54.2 million
words/s). After the duration-block query flag was added, the same final-code run spent 0.905 seconds
in indexed appends (approximately 53.0 million words/s) with the identical word count.

Refactor derived annotation rendering and cursor snapping around `AnnotationQuery`:

- clone handles and release lane locks before querying;
- sample/caches presence or exact visible windows;
- draw historical presence bands;
- draw exact old and recent values identically;
- route cursor snap through `nearest_boundary`;
- use store metadata for full extent and Home-to-fit.

Exit criteria:

- No historical region is blank solely because it left memory retention.
- Exact old-region cursor and partial-word tests pass.
- UI frame time remains bounded during live appends.

### Step 7: Persistent publication and invalidation

Status: implemented in `crates/signal_processing/src/runtime/derived_word_store/{persistent,store}.rs`,
`crates/signal_processing/src/nodes/sinks/viewer_sink.rs`, and `crates/logic_analyzer_graph/src/compiler/`.

Completed stores publish immutable data and index files followed by a checksummed manifest. Reopen
validates the full 256-bit key, format versions, file lengths, metadata checksums, and block
directory before mmaping the data file. Invalid generations are removed; unfinished temporary
files have no manifest and are never discoverable. Cache cleanup accounts encoded bytes, removes
stale temporaries, preserves keys pinned by the open graph, and evicts unpinned entries by last
access above the default 50 GiB budget. Public accounting and clear-cache APIs are available for a
future settings surface.

The compiler derives BLAKE3 lane keys from a cache ABI version, canonical node configuration,
sorted upstream wiring and port kinds, resolved viewer member order, and the source capture's
canonical path, length, modification time, parsed header metadata, sample rate, sample count, and
probe names. Persistence is disabled for a file source whose filename is supplied by a runtime
edge because that source cannot be identified safely while compiling the graph.

On a cache hit, the Viewer sink opens the indexed lane directly and the execution graph removes
the cached viewer edge. Upstream nodes that no longer feed any live sink are pruned, so a decoder
used only for that viewer lane is not started. An upstream branch still runs when another sink,
such as a file writer, needs it. Tests cover stable and invalidated keys, variadic member order,
capture identity changes, corrupt manifest/index rejection, interrupted builds, LRU cleanup,
Viewer reopen without rewriting input, and a full two-run pipeline whose second run returns the
published words without starting its decoder.

Add:

- source/upstream cache-key construction;
- immutable index serialization;
- data/index/manifest atomic publication;
- persistent reopen and mmap;
- stale-cache rejection;
- cache directory accounting and cleanup.

Exit criteria:

- A second run can open a valid completed cache without decoding again.
- Any relevant capture, graph, port-order, or decoder-config change selects a different key.
- Interrupted builds never appear valid.

### Step 8: Full validation and tuning

Status: complete on the 12,782,165,248-sample (255.643 second) `wipneus5.dsl` reference capture.

The first full counter run exposed source-side RSS growing to 10.4 GiB. This was not retained Rust
payload: every raw-cache `SampleBlock` referenced a region of one 16 GiB mmap, so macOS kept all
touched page mappings resident until the capture reader closed. `RawBlockCache` now creates one
zero-copy mmap per immutable raw slot. The mapping is released with the last block view. A
one-billion-sample differential run reduced peak RSS from 1,125 MiB to 66 MiB with the identical
239,997,224 words and fingerprint; the complete counter rerun peaked at 100 MiB.

Final production-style results use Auto packed streaming, four scan workers, persistent indexed
publication, 32,768 words per encoded block, and a 512-record restart interval:

| Metric | Result |
| --- | ---: |
| Capture samples / duration | 12,782,165,248 / 255.643 s |
| Indexed words | 3,067,688,031 |
| Output fingerprint | `a8df111ff6f474dd` |
| Decode plus persistent publication | 67.393 s |
| Throughput / real-time factor | 189.666 MSamples/s / 3.793x |
| Viewer append time | 59.023 s |
| Encoded data | 6,237,981,960 bytes |
| Encoded bytes per word | 2.033 |
| Blocks / restart entries | 93,619 / 5,991,579 |
| Peak RSS during decode/publication | 117.8 MiB |
| Average CPU cores | 2.26 |
| Exact query median / p95 | 1.137 / 1.557 ms cold; 0.389 / 0.435 ms warm |
| Presence query median / p95 | 0.392 / 0.432 ms |
| Cursor query median / p95 | 1.198 / 1.546 ms |
| Full independent store readback/fingerprint | 153.457 s |

The counter authority and indexed-store readback contain exactly the same word count and
fingerprint. The full readback time is validation after the live run and is not included in the
67.393-second pipeline measurement. Stable UI frames are served by the viewer's viewport cache;
narrow store queries use restart-bounded range decoding, so the full decoded-block LRU recorded no
hits in this probe. Its zero hit rate is therefore not evidence of repeated UI-frame work.

The 32,768-word block default was selected from 16K, 32K, 64K, and 128K reference-prefix runs. It
roughly halves exact/cursor query latency relative to 64K while keeping append throughput and
two-byte encoding density. A 512-record restart interval halved restart metadata without a
measured query regression. Compression and an extra writer thread were not added: encoded density
is already close to two bytes per word, and ViewerSink already runs concurrently with the decoder.

The original less-than-50-second stretch target is not met. The implementation is nevertheless
well ahead of live input, bounded in memory, and responsive in measured query paths. Any further
work toward that stretch target should focus on the synchronous builder/encode/write append path,
which accounts for 59 of the 67 seconds, rather than decoder parallelism or query indexing.

Run the complete reference capture and record:

- decode/store wall time;
- real-time factor;
- output fingerprint;
- encoded size and bytes/word;
- peak RSS;
- presence and exact query latency;
- cursor latency;
- UI responsiveness while decoding.

Tune only measured bottlenecks:

- block word count;
- restart interval;
- maximum gap/span;
- decoded LRU size;
- optional store-writer thread;
- optional LZ4/zstd block compression.

Do not introduce a tree index or general compression without benchmark evidence.

---

## 19. Remaining Work Plan

The indexed-store implementation is production-capable. The list below contains the work that is
still open after Steps 1-8. Only R1 closes a remaining correctness-validation gap. R2 and R3 improve
operability and measured UI confidence. R4 is an optional stretch optimization because the current
pipeline is already 3.79x faster than real time. R5 is conditional cleanup and must remain last.

- [ ] R1: Record full file-writer differential validation (P0).
- [ ] R2: Add derived-cache management UI (P1).
- [ ] R3: Measure interactive UI responsiveness during a full decode (P1).
- [ ] R4: Investigate the less-than-50-second stretch target (P2, optional).
- [ ] R5: Remove obsolete native annotation retention paths (P3, conditional).

### R1: Record full file-writer differential validation (P0)

Status: not run after the persistent indexed-store integration. The release-only test already
exists as `compiler::tests::live_attach_detach_preserves_writer_output`; implementation work is not
expected unless it finds a failure.

Plan:

1. Run the ignored test in a release build against `_captures/wipneus5.dsl` from the repository
   root so the fixture cannot be silently skipped.
2. Run `compiler::tests::golden_compiled_graph_matches_reference` in the same environment to cover
   the compiled graph against its hand-built reference pipeline.
3. Record output file names, byte lengths, and content hashes for the uninterrupted reference and
   viewer attach/detach runs.
4. If output differs, localize the first byte and the corresponding input time window before
   changing code. Do not weaken the comparison or normalize output.
5. Add the command, elapsed time, and hashes to the Step 8 validation record.

Definition of done:

- Both ignored release tests pass on the complete reference capture.
- The writer output is byte-identical with the viewer attached, detached, and absent.
- No test leaves persistent cache or writer artifacts outside its temporary directory.

### R2: Add derived-cache management UI (P1)

Status: backend cleanup, LRU, pinning input, accounting results, and destructive clear functions
exist. There is no user-facing disk-usage or clear-cache surface yet.

Plan:

1. Add a read-only cache inventory API that reports entry count, total encoded bytes, configured
   budget, and last cleanup result without triggering eviction.
2. Keep the active compiled graph's cache keys pinned and define clear behavior for active entries.
   The UI must never unlink a cache currently mapped by an open viewer lane.
3. Add a compact derived-cache section to the application's existing settings/application menu
   surface. Show current usage and budget; provide `Clear Derived Cache...` with a confirmation
   dialog that states the amount to be removed.
4. Refresh the displayed usage after publication, cleanup, and clear. Report permission/I/O errors
   through the existing toast/error path without affecting the graph run.
5. Cover empty cache, populated cache, pinned active cache, cancellation, and failed deletion.

Definition of done:

- Users can see cache usage and clear all inactive derived caches from the desktop application.
- Active graph caches remain queryable throughout clear and are reported as retained.
- macOS native menus and the non-macOS application menu expose equivalent behavior.

### R3: Measure interactive UI responsiveness during a full decode (P1)

Status: store query latency and RSS are measured; actual egui update time and input latency during
the full live append have not been recorded.

Plan:

1. Add opt-in timing for egui update duration, indexed sampling duration, `DerivedLanes` lock hold
   time, repaint rate, and delayed pointer/key events. Keep instrumentation disabled by default.
2. Run the production desktop graph through the complete reference capture while continuously
   panning, zooming, moving a cursor, and using Home-to-fit in recent and historical regions.
3. Record median/p95/max update and sampling times, longest input delay, and RSS over time. Correlate
   any stall with store append, file sync, cache publication, or a lane lock.
4. Add a focused regression test for any reproduced stall. Avoid encoding machine-specific timing
   thresholds into ordinary unit tests.
5. Add the measured results and machine/build details to Step 8.

Definition of done:

- No store query or file operation occurs while the lane collection lock is held.
- Pointer, cursor, Home, pan, and zoom actions remain responsive during append and final publish.
- The recorded profile contains enough phase timing to localize any frame over 50 ms.

### R4: Investigate the less-than-50-second stretch target (P2, optional)

Status: current complete decode plus persistent publication is 67.393 seconds. Viewer append takes
59.023 seconds and is the measured limit; decoder parallelism, query indexing, and RSS are not the
next bottlenecks.

Plan:

1. Split append timing into builder ingestion, block finalization, VLQ/value encoding, restart and
   duration encoding, CRC, presence update, write syscalls, and final sync/publication.
2. Benchmark those phases on the 200-million-sample prefix and confirm that their sum explains the
   full-run append time.
3. Optimize only the dominant phase. First evaluate batch-aware builder ingestion and removal of
   redundant word passes. Consider parallel block encoding or buffered/asynchronous writes only if
   phase timing demonstrates that they can help.
4. Re-run 200-million-sample count/store fingerprints after every change, followed by one complete
   reference run for changes that improve prefix append time by at least 10%.
5. Reject changes that increase peak RSS beyond 256 MiB, worsen exact/cursor p95 beyond 5 ms, make
   cancellation exceed 100 ms, or complicate publication recovery without a material speedup.

Definition of done:

- Either the full cache run finishes below 50 seconds with the same
  `a8df111ff6f474dd` fingerprint, or measurements document why the added complexity is not justified.
- Encoded density remains at or below 2.1 bytes/word and live throughput remains above 1.1x.

### R5: Remove obsolete native annotation retention paths (P3, conditional)

Status: native production word lanes use `AnnotationQuery`, but in-memory annotations are still a
required wasm implementation and native error fallback. They must not be removed wholesale.

Plan:

1. Audit every `DerivedLaneData::Annotations` producer and consumer and classify it as wasm,
   explicit in-memory mode, tested fallback, plugin compatibility, or obsolete native path.
2. Migrate plugins and remaining native consumers to the common query interface where practical.
3. Delete only branches proven unreachable in native production and preserve a bounded fallback
   for store creation/write failures.
4. Run native, wasm, plugin, rendering, cursor, and failure-injection coverage after each removal.
5. Update the public viewer API documentation to state which platforms/modes retain exact history.

Definition of done:

- Native file/live word lanes have no duplicate unbounded `Vec<Annotation>` retention.
- wasm, explicit in-memory tests, and native storage-failure fallback remain functional.
- Render and cursor code depend on the common query interface rather than storage variants.

### Recommended execution order

1. R1, because it closes the remaining correctness-validation gap.
2. R2 and R3 independently; neither depends on the stretch optimization.
3. R4 only if reducing the already-real-time 67-second run is worth the implementation cost.
4. R5 after plugin/wasm compatibility is confirmed and the preceding behavior is stable.

---

## 20. Rollout Strategy

The work should land in independently testable increments:

1. Codec and store behind tests, unused by production.
2. Benchmark sink writing/querying temporary stores.
3. Native ViewerSink opt-in feature flag or configuration.
4. Rendering and cursor migration with in-memory compatibility.
5. Native indexed storage enabled by default after full-capture validation.
6. Persistent reuse enabled only after cache-key/invalidation coverage.
7. Remove obsolete raw annotation retention paths only after plugins, wasm, and tests use the
   common query interface.

At every stage the file-writer branch remains the correctness authority. A viewer storage error
must degrade the viewer, not the recorded output.

---

## 21. Open Decisions Resolved During Implementation

These choices were resolved through measurement and remain centralized tuning inputs:

| Decision | Current choice | Measurement |
| --- | --- | --- |
| Words per block | 32,768 | encode speed, query amplification |
| Restart interval | 512 | cold cursor latency, index size |
| Maximum gap | 1 ms | summary truthfulness, block count |
| Encoded payload cap | 1 MiB | allocation and write latency |
| Decoded LRU | 64 MiB shared | pan/zoom hit rate and RSS |
| Hot-tail publish | 16,384 words or 50 ms | live latency and clone cost |
| Writer threading | synchronous ViewerSink thread | append throughput |
| Persistent fingerprint | size + mtime + header hash + graph hash | stale-cache tests |
| Compression | none | disk bytes/word and CPU headroom |

The flat directory, VLQ time deltas, per-block narrow values, sparse duration stream, restart
table, and presence/count mipmap are baseline architectural decisions. The table parameters are
tuning inputs and should remain centralized constants or configuration.
