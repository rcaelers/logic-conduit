# Parallel Decoder Performance Plan

## Objective

Decode a parallel DDR bus sampled at 50 MHz at better than real-time speed.
The current target signal has a 6 MHz clock and samples on both edges, so the
decoder must sustain about 12 million trigger positions per second. The first
performance target is 1.1x real time for the complete file-to-sink pipeline,
with additional decoder-only headroom for live capture.

## Acceptance Criteria

The optimized implementation must meet all of the following in a release
build on the development machine:

- Decoder plus counter sink: at least 60 MSamples/s on an active capture.
- File source plus decoder plus viewer sink: at least 55 MSamples/s for a
  bounded capture that fits the configured viewer retention policy.
- Live packed-block decoder: at least 60 MSamples/s without a waveform index.
- Stop latency: less than 50 ms while decoding a continuously active bus.
- File block transport: no packed payload copy between the file cache and the
  decoder.
- Steady-state memory: bounded independently of capture duration, except for
  sinks explicitly configured to retain the complete decoded result.
- Indexed and packed-stream paths produce byte-identical `Word` output.

All throughput targets are measured after pipeline setup. Index construction,
sidecar validation, and cold block materialization are reported separately.

## Current Architecture

`DslFileSource` and `ParallelDecoder` both advertise `EdgeQuery` before
`Stream`, so a file-backed graph normally selects the indexed path. The
parallel decoder then performs one `next_edge` query for each strobe edge and
one `value_at` query for each data, CS, and query-backed enable input.

The file implementation of `next_edge` currently asks `sampled_window` for an
exact 4,096-sample window. Because the requested target point count equals the
sample count, the waveform sampler always selects its exact raw-data path. It
finds and allocates every transition in the window but returns only the first.
For a DDR edge every 4-5 samples, most of that scan is repeated by the next
query.

The alternative `SampleBlock` path is already close to zero-copy after a file
block enters the shared cache: `SampleBlock` owns an `Arc<[u8]>`, and channel
messages only clone that `Arc`. Its decoder loop is still scalar, however, and
one call processes an entire 16,777,216-sample file block.

## Baseline Findings

The reference capture `_captures/wipneus5.dsl` contains:

- 12,782,165,248 samples at 50 MHz (about 255.6 seconds).
- 11 channels and 762 blocks.
- 16,777,216 samples (2 MiB packed) per full channel block.

The existing raw sidecar is approximately 15 GiB physically allocated. This
shows that point reads have materialized nearly every relevant channel block.
Warm reads use mmap-backed `BlockData`, but every scalar query still acquires
the shared sampler mutex and constructs a block view.

The existing two-mode differential test performs two 200-million-sample
passes. When invoked from a directory where the capture is actually visible,
it did not finish in 106.7 seconds and was interrupted. Normal `cargo test`
runs currently resolve `_captures` relative to `crates/dsl`, silently skip the
fixture, and therefore do not provide a performance or integration signal.

### Recorded Baseline

Recorded on 2026-07-11 with an optimized build and the already-materialized
`wipneus5.dsl` index/raw sidecars:

```sh
cargo run -p dsl --release --bin parallel-decoder-bench -- \
  _captures/wipneus5.dsl --samples 10000000 --mode both --sink count

cargo run -p dsl --release --bin parallel-decoder-bench -- \
  _captures/wipneus5.dsl --samples 10000000 --mode both --sink viewer
```

| Mode | Sink | Run time | MSamples/s | MWords/s | Real time | Words |
| --- | --- | ---: | ---: | ---: | ---: | ---: |
| indexed | count | 5.604 s | 1.784 | 0.428 | 0.036x | 2,399,972 |
| stream | count | 0.395 s | 25.341 | 6.082 | 0.507x | 2,399,972 |
| indexed | viewer | 6.019 s | 1.661 | 0.399 | 0.033x | 2,399,972 |
| stream | viewer | 3.980 s | 2.513 | 0.603 | 0.050x | 2,399,972 |

The identical word counts provide an initial correctness cross-check. The
indexed/count result isolates the query bottleneck, while the large difference
between stream/count and stream/viewer confirms that scalar word transport and
annotation retention are already a separate major cost.

## Step 1: Reproducible Baselines

Add an opt-in `parallel-decoder-bench` binary. It must:

- Require an explicit capture path; never silently skip a missing fixture.
- Run indexed and forced packed-stream modes independently or sequentially.
- Support a lightweight counter sink and the production viewer sink.
- Accept the sample limit and channel mapping on the command line.
- Report pipeline setup time separately from decoding time.
- Report samples processed, capture duration, words emitted, MSamples/s,
  MWords/s, and real-time factor.
- Run only when explicitly invoked, so normal unit tests remain fast.

Status: implemented. The first results are recorded above. Run indexed and
stream modes separately when measuring cold-cache behavior; `--mode both` is
intended for convenient warm-cache comparisons and output-count checks.

Initial benchmark matrix:

| Mode | Sink | Purpose |
| --- | --- | --- |
| stream | discard | Packed file read, scan, gating, and assembly without output transport |
| indexed | count | Edge-query and decoder cost plus minimal transport |
| stream | count | Packed file read, scalar scan, and minimal transport |
| indexed | viewer | Full current user-visible indexed path |
| stream | viewer | Full current user-visible packed path |

Use 10 million samples for the current iteration loop; it is long enough to
reduce startup noise without making the indexed baseline impractical. Use at
least 200 million samples for acceptance measurements once the optimized path
can complete that prefix promptly.

## Step 2: Hierarchical Next-Transition Search

Add a dedicated `IndexSampler::next_transition(channel, position, limit)`.
It must not call `sampled_window` or allocate a transition vector.

Status: implemented. `DslChannelEdgeIndex::next_edge` now delegates directly
to this primitive. Deterministic tests cover L1/L2/L3 boundaries, file-block
boundaries, an edge at the last sample of a partial block, exclusive limits,
and constant-block skipping without raw reads. A randomized differential test
compares thousands of arbitrary ranges with raw sample ground truth.

Search in this order:

1. Skip constant file blocks using the root directory summary.
2. Find the next active L3 group (262,144 samples).
3. Find the next active L2 group (4,096 samples).
4. Find the next active L1 group (64 samples).
5. Read one raw word and locate the exact edge with XOR and
   `trailing_zeros`.

Partial groups at the start and end of a query must be masked precisely.
Boundary transitions between groups and file blocks must retain the existing
strictly-after-`position` semantics.

Verification:

- Synthetic constant, dense, sparse, boundary, and final-partial-block tests.
- Differential comparison with the exact scanner over randomized bit data.
- SPI and parallel decoder differential tests over a real capture prefix.
- A microbenchmark for dense and sparse `next_transition` calls.

### Step 2 Result

The same warm-cache 10-million-sample benchmark used for Step 1 produced:

| Mode | Sink | Before | After | Improvement | Words |
| --- | --- | ---: | ---: | ---: | ---: |
| indexed | count | 1.784 MSamples/s | 12.022 MSamples/s | 6.74x | 2,399,972 |
| indexed | viewer | 1.661 MSamples/s | 1.836 MSamples/s | 1.11x | 2,399,972 |

The counter result demonstrates the intended query improvement. The viewer
result confirms that scalar word transport and annotation retention dominate
once edge discovery becomes cheaper. Batch edge/value queries remain necessary
to reach real time, while batched output remains independently necessary for
the viewer path.

## Step 3: Batch Edge and Value Queries

Extend `EdgeQuery` with object-safe batch operations. Exact names may change,
but the intended surface is:

```rust
fn next_edges(
    &self,
    position: u64,
    limit: u64,
    max_edges: usize,
    output: &mut Vec<CaptureTransition>,
) -> Result<()>;

fn values_at(&self, positions: &[u64], output: &mut Vec<bool>) -> Result<()>;
```

Default implementations may loop over the existing scalar methods, preserving
compatibility with computed query sources. `DslChannelEdgeIndex` must override
both methods:

- Hold the sampler mutex once per batch instead of once per edge.
- Reuse caller-owned vectors without per-call allocation.
- Group sorted positions by file block and acquire one `BlockData` view per
  block.
- Use the L3/L2/L1 hierarchy to skip inactive ranges.
- Scan active raw data in 64-bit words and append all requested edges.

Start with a 65,536-sample window and a 65,536-edge safety limit. Make both
limits constants so benchmarks can tune them without changing semantics.

Status: implemented. `EdgeQuery` now supplies backward-compatible scalar
defaults for `next_edges` and `values_at`. `DslChannelEdgeIndex` overrides both
methods, holding the shared sampler once for each batch. The indexed reader
keeps one leaf and packed block view while collecting transitions, and groups
point reads by file block. `ParallelDecoder` reuses batch allocations and
processes up to 65,536 trigger positions per call. Rising/falling modes request
twice that many raw transitions and filter by landing value.

The streamed enable state machine and CS/data/enable gating are still applied
in chronological trigger order. Existing mixed streamed/query enable tests and
word-assembly differential tests pass unchanged.

### Step 3 Result

Warm-cache 10-million-sample results:

| Mode | Sink | Step 2 | Step 3 | Improvement | Words |
| --- | --- | ---: | ---: | ---: | ---: |
| indexed | count | 12.022 MSamples/s | 22.225 MSamples/s | 1.85x | 2,399,972 |
| indexed | viewer | 1.836 MSamples/s | 3.510 MSamples/s | 1.91x | 2,399,972 |

Relative to the original indexed baseline, the counter path is now 12.46x
faster. It is still below the 50 MSamples/s real-time requirement and slightly
behind the scalar packed-stream counter path. The next input-side task is the
vectorized packed/live scanner. The viewer remains dominated by scalar `Word`
transport and permanent annotation insertion.

## Step 4: Batched Indexed Parallel Decoder

Refactor `ParallelDecoder::work_indexed` to operate on batches:

1. Fetch a strobe edge batch for one processing window.
2. Apply rising, falling, or both-edge filtering during edge discovery.
3. Batch-read CS and query-backed enable values at trigger positions.
4. Skip data reads for gated-off triggers.
5. Batch-read every data channel for the remaining positions.
6. Assemble and emit words in a tight loop.
7. Persist the window cursor and incomplete-word state before returning.

This changes the dominant cost from roughly one sampler lock per signal per
trigger to roughly one lock per signal per processing window.

Status: implemented. The decoder collects up to 65,536 trigger positions per
call, batch-reads CS and query-backed enable values, and filters gated
positions before reading any data channel. Eligible positions are then read
from each data channel in one batch. A reset marker per eligible position
preserves incomplete-word behavior when one or more gated triggers occur
between two retained positions; a trailing gate also clears assembly state
before the call returns.

The initial batched consumer landed with Step 3 because the new batch query
surface needed a real consumer for benchmarking. The final gating pass is
covered by focused tests which prove that a fully gated batch performs zero
data point reads and that a gated trigger breaks multi-cycle word assembly.

## Step 5: Vectorized Packed-Block Decoder

The live path cannot depend on a prebuilt index. Keep aligned input
`SampleBlock`s resident in decoder state and process bounded internal windows
without splitting or copying their backing allocation.

For each 64-bit strobe word:

```text
toggles = word XOR ((word << 1) OR previous_bit)
rising  = toggles AND word
falling = toggles AND NOT word
```

Mask partial words, then iterate matching bits with `trailing_zeros`. Read data
and CS directly from the resident blocks only at trigger positions. Return to
the scheduler after at most one configured sample/edge window to preserve stop
latency.

The same implementation handles file-backed `SampleBlock`s and live analyzer
blocks. Index summaries are an optional file optimization, not a requirement
for correctness or real-time operation.

Status: implemented. The streaming decoder retains one aligned strobe,
data, and optional CS block set in node state. Each `work()` call scans at most
65,536 samples, then persists its cursor without splitting or copying any
packed payload. Strobe words are loaded 64 bits at a time; edge and level
trigger masks are walked with `trailing_zeros`, and data/CS bits are read only
at selected positions. Acquired channels are checked for identical start
position, sample count, and timestamp step.

Tests cover every strobe mode across 64-bit boundaries, yielding across three
windows of one retained block, and `Arc::ptr_eq` identity between the source
payload and the resident decoder block. Existing streamed/query gating and
word-assembly comparisons remain unchanged.

### Step 5 Result

Warm-cache 10-million-sample results on the reference capture:

| Mode | Sink | Scalar baseline | Vectorized | Improvement | Words |
| --- | --- | ---: | ---: | ---: | ---: |
| stream | discard | not measured | 266.346 MSamples/s | n/a | unmeasured |
| stream | count | 25.341 MSamples/s | 28.195 MSamples/s | 1.11x | 2,399,972 |
| stream | viewer | 2.513 MSamples/s | 2.933 MSamples/s | 1.17x | 2,399,972 |

The discard result is the median of three warm 200-million-sample acceptance
runs (259.121 to 270.020 MSamples/s, 5.18x to 5.40x real time). It leaves the
decoder output unconnected, so it includes packed file delivery, 64-bit
strobe scanning, CS/enable gating, data-channel reads, and word assembly but
not `Word` channel transport. A one-data-bit comparison reached 789.420
MSamples/s versus 257.772 MSamples/s with eight data bits, confirming the
measurement scales with decoder data work rather than only timing source
startup. The benchmark reports the discard word count as `unmeasured`; count
mode and the differential tests remain the output-correctness checks.

The count result is the median of five consecutive warm 10-million-sample
runs (26.130 to 30.787 MSamples/s); the viewer result is one warm run. Their
exact word count matches the scalar baseline and indexed path. The isolated
packed/live decoder now exceeds its 60 MSamples/s acceptance target by more
than 4x, but the end-to-end count path still emits about 2.4 million
individual `Word` channel messages for 10 million input samples. Step 7's
batched word transport is therefore confirmed as required for complete
real-time pipeline operation rather than further Step 5 input-side tuning.

## Step 6: Zero-Copy Backing and Bounded File Flow

The current cache-to-`SampleBlock` handoff clones only an `Arc`, but cold ZIP
reads still use intermediate allocations and the shared cache is unbounded.

Planned changes:

- Replace the unbounded block map with a bounded cache or direct ownership
  transfer when there is only one destination.
- Use a small connection capacity (initially 2-4 blocks) for packed streams.
- Keep all required channels in block lockstep so one channel cannot queue a
  complete capture while another is still decompressing.
- Represent packed bytes with shared backing plus an offset/range. This permits
  10,000-100,000 sample subviews without copying the 2 MiB file block.
- Prefer an ownership type that can adopt a decompressed `Vec<u8>` without
  copying its payload and can also reference mmap-backed storage.
- Remove the extra per-channel packed-vector clone in the live analyzer demux.
- Measure cold-cache writes separately; if materialization remains important,
  extend the block reader so DSL data can decompress directly into its final
  cache slot.

## Step 7: Batched Word Transport and Viewer Retention

At 12 million cycles per second, scalar `Word` channel traffic can become the
next limit after input decoding is fixed. Profile before changing the runtime,
then add a batch transport if needed. Preferred options are a `WordBatch`
payload or a batch variant in the channel envelope that receivers can flatten
for compatibility.

The viewer currently appends every decoded annotation permanently. Continuous
live operation at this rate requires an explicit policy:

- Rolling in-memory retention for live viewing, or
- File-backed/paged exact annotations with the existing mipmap retained for
  overview rendering.

Decoder, transport, and viewer throughput must be reported separately so a
downstream retention limit is not mistaken for an input-scanning regression.

## Delivery Order

1. Benchmark harness and recorded baseline.
2. Hierarchical scalar transition search.
3. Batch `EdgeQuery` operations.
4. Batched indexed decoder.
5. Vectorized packed/live decoder.
6. Bounded zero-copy file flow.
7. Batched output and live retention, if confirmed by profiling.

Each step must preserve differential output tests and add a focused benchmark
that demonstrates the intended improvement before moving to the next layer.
