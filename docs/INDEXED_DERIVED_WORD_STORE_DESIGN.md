# Indexed Derived Word Store Design

The indexed derived-word store keeps decoded word lanes queryable without retaining every
annotation in viewer-owned memory. It provides exact values and cursor boundaries for narrow
time windows, presence summaries for overview rendering, and bounded decoded-block caching.

Primary code locations:

- `crates/signal_processing/src/derived_word_store/`;
- `crates/signal_processing/src/viewer_sink.rs`;
- `crates/widgets/logic_analyzer_viewer/src/draw/derived.rs`;
- `crates/widgets/logic_analyzer_viewer/src/cursor.rs`;
- `crates/widgets/logic_analyzer_viewer/src/channel.rs`;
- `crates/logic_analyzer_graph/src/nodes/viewer/builder.rs`.

Related documents:

- [LOGIC_ANALYZER_VIEWER_DESIGN.md](LOGIC_ANALYZER_VIEWER_DESIGN.md);
- [PIPELINE_DESIGN.md](PIPELINE_DESIGN.md);
- [WASM_STORAGE_PLATFORM_DESIGN.md](WASM_STORAGE_PLATFORM_DESIGN.md).

## Responsibilities

The store:

- preserves each decoded `Word` value, timestamp, and explicit duration;
- keeps viewer memory independent of recording duration on native builds;
- answers exact-window, presence-window, and nearest-boundary queries;
- supports queries while decoding is active;
- detects malformed blocks and stale or incomplete persistent caches;
- isolates storage failure from other consumers of the decoded word stream.

The store belongs to the viewer sink rather than to a decoder. Any decoder or plugin that emits
words can use the same storage path, while a decoder connected only to another sink does not
create a viewer cache. `ParallelDecoder` and other producers remain responsible for producing
ordered word batches; `ViewerSink` materializes those batches for display.

## Architecture

```text
word-producing runtime node
  |
  | ordered Word batches
  +------------------------------> other word consumers
  |
  +----> ViewerSink word lane
           |
           v
      IndexedAnnotationWriter
           |
           v
      IndexedAnnotationStore
        |       |        |
        |       |        +-- presence index
        |       +----------- committed block directory
        +------------------- bounded decoded-block cache (native)
           |
           v
      Arc<dyn AnnotationQuery>
        |                 |
        v                 v
      renderer       cursor snapping
```

The pipeline node appends blocks outside the egui thread. The viewer holds an
`Arc<dyn AnnotationQuery>` and performs bounded queries after releasing the derived-lane lock.
Only fully committed blocks are visible to readers.

## Platform model

`IndexedAnnotationStore`, `IndexedAnnotationWriter`, `AnnotationQuery`, configuration, status,
and viewer lane types exist on native and wasm.

- Native storage writes compact blocks to a temporary file. A completed store is mmap-backed and
  can be published to or reopened from the persistent cache.
- Wasm storage uses an in-memory ordered store with the same append and query semantics.

Platform selection is contained in `derived_word_store/platform/`. Generic viewer, sink, and
compiler code do not change lane shape by target. See
[WASM_STORAGE_PLATFORM_DESIGN.md](WASM_STORAGE_PLATFORM_DESIGN.md).

## Data model

The input is the runtime `Word` type:

```rust
pub struct Word {
    pub value: u64,
    pub timestamp_ns: u64,
    pub duration_ns: u64,
}
```

Words arrive in nondecreasing timestamp order. Equal timestamps retain arrival order. An
out-of-order timestamp is a store error. A non-zero duration is authoritative and round-trips
exactly. Instantaneous words use adjacent word starts or a cadence-bounded inferred end for
display and boundary queries, so long inactive intervals remain empty.

The public query surface is viewer-oriented and independent of the storage format:

```rust
pub trait AnnotationQuery: Send + Sync {
    fn metadata(&self) -> AnnotationStoreMetadata;

    fn presence_window(
        &self,
        start_ns: u64,
        end_ns: u64,
        target_buckets: usize,
    ) -> AnnotationQueryResult<Vec<WordPresenceBucket>>;

    fn exact_window(
        &self,
        start_ns: u64,
        end_ns: u64,
        max_words: usize,
    ) -> AnnotationQueryResult<ExactAnnotationWindow>;

    fn nearest_boundary(
        &self,
        timestamp_ns: u64,
        max_distance_ns: u64,
    ) -> AnnotationQueryResult<Option<u64>>;
}
```

An incomplete exact window causes the renderer to use the presence path; it is never drawn as if
it were a complete result. Store generations are part of the viewer sampling key so live windows
refresh when committed data changes.

## Block encoding

Native stores use append-only, versioned blocks. The default block configuration is centralized
in `BlockCodecConfig`:

| Setting | Default |
| --- | ---: |
| Maximum words | 32,768 |
| Restart interval | 512 words |
| Maximum encoded payload | 1 MiB |
| Maximum inter-word gap | 1 ms |
| Maximum timestamp span | unlimited |

A block closes when a configured count, payload, gap, or timestamp-span limit is reached, or when
the lane finishes. Gap-based closing prevents a block summary from implying activity across a
long idle interval.

Each block contains:

- an absolute first timestamp followed by unsigned VLQ timestamp deltas;
- fixed-width values using the smallest of one, two, four, or eight bytes for that block;
- sparse duration exceptions for words with non-zero duration;
- restart entries for bounded seeks within the variable-length record stream;
- a CRC32C checksum.

The file format is little-endian and versioned. Readers reject invalid magic, unsupported
versions, invalid reserved fields, overlong VLQ values, arithmetic overflow, truncated data, and
checksum mismatches.

## Presence index and exact queries

The presence index summarizes occupied time ranges and word counts at multiple resolutions.
Overview rendering requests no more buckets than the viewport needs and never invents exact
values or boundaries. Narrow views request exact annotations from the blocks intersecting the
time window.

Exact queries use the sorted block directory to find candidate blocks and restart entries to seek
within them. Native decoded blocks are shared through a memory-budgeted LRU keyed by store
identity and block sequence. Presence-only rendering does not populate that cache.

`nearest_boundary` considers word starts, explicit ends, and cadence-bounded inferred ends. It
checks neighboring blocks so snapping works at block boundaries and in older regions of a lane.

## Live publication

`ViewerSink` owns one writer for each indexed word lane. Appending:

1. validates ordering;
2. adds words to the active block builder;
3. encodes and writes complete blocks;
4. publishes directory and presence metadata;
5. increments the store generation and requests a repaint.

The active block is exposed through an immutable hot-tail snapshot. Publication is bounded by
`LiveStoreConfig` and defaults to 16,384 words or 50 ms. File writes, VLQ encoding, mmap page
faults, and block decoding never occur while the `DerivedLanes` lock is held.

Finishing closes the active block and marks the store complete. Cancelling discards unfinished
temporary state. Storage errors put the affected lane into an error state without changing the
word stream received by other graph branches.

## Persistent cache

A native persistent cache entry contains:

```text
words.dwd     encoded word blocks
words.dwi     block directory and presence index
manifest.dwm  cache identity, sizes, word count, and commit marker
```

The manifest is published last. A cache is discoverable only when its manifest, cache key, data
size, index size, directory, counts, and checksums validate. Completed data is immutable and
mmap-backed.

The compiler derives the cache key from source identity and the relevant graph configuration.
Cache entries are reusable optimizations: clearing or rejecting an entry never changes the source
capture or another sink's output. Native cache administration supports per-entry clearing and an
LRU size budget.

## Viewer integration

`DerivedLaneData` supports both ordinary in-memory annotations and
`IndexedAnnotations(IndexedAnnotationLane)`. `IndexedAnnotationLane` exposes query, metadata,
status, and platform-neutral store handles.

Rendering and cursor code follow the same locking rule:

1. acquire the derived-lane lock;
2. clone lane metadata and query handles;
3. release the lock;
4. perform store queries;
5. render or select a cursor boundary.

The viewer caches sampled windows by lane identity, store generation, visible time range,
viewport width, and query mode. Exact mode uses the ordinary annotation-box renderer; presence
mode renders summarized activity.

## Correctness invariants

1. Directory entries describe complete, checksum-valid blocks only.
2. Block sequence numbers are contiguous within a store generation.
3. Word timestamps are globally nondecreasing.
4. Concatenating decoded blocks reproduces input order and values exactly.
5. Explicit durations round-trip exactly.
6. Presence counts match the committed words represented by the index.
7. Presence queries do not report an empty bucket that contains a word.
8. Exact queries return every intersecting word or mark the result incomplete.
9. Persistent manifests refer only to synchronized data and index files with the same cache key.
10. Storage failure cannot alter another consumer's word stream.
11. Derived-lane locks are never held across storage I/O or block decoding.

## Validation

Native and wasm contract tests cover append, exact windows, presence windows, nearest-boundary
queries, finish, cancellation, and metadata semantics. Native tests additionally cover codec
round trips, corrupt and truncated data, persistent publication and reopening, cache invalidation,
decoded-block caching, live queries, and cursor behavior across blocks.

Large-capture performance and operational follow-ups are tracked in [TODO.md](../TODO.md).
