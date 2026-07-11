# Parallel Decoder Parallelism Plan

## Objective

Increase dense parallel-bus decoding throughput by selecting the appropriate
input protocol and processing packed capture windows concurrently, while
preserving exact word values, timestamps, ordering, bounded backpressure, and
responsive cancellation.

The reference workload is a 50 MHz capture with a 6 MHz DDR clock (roughly
12 million sampling edges per second). Measurements made before this plan:

| Path | Throughput |
| --- | ---: |
| Indexed, decoder only | 150 MSamples/s |
| Packed stream, decoder only | 281 MSamples/s |
| Indexed with unlimited viewer | 98 MSamples/s |
| Packed stream with unlimited viewer | 231 MSamples/s |

The dense workload should therefore select packed streaming before adding
parallelism. Indexed queries remain important for sparse clocks where the
hierarchical index skips most capture blocks.

## Constraints

- Decoder output must be deterministic and ordered.
- CS, enable, strobe level, and partial word assembly cross window boundaries.
- A worker may not mutate `ParallelDecoder` runtime state directly.
- Queued windows and output fragments must remain bounded.
- Stopping or editing a live graph must not wait for the complete capture.
- The wasm implementation remains sequential.
- Worker count must be bounded; using all 20 cores is not automatically useful
  for an eight-bit, memory-bandwidth-heavy workload.
- Finite file viewers retain their complete timeline.

## Step 1: Benchmark and Correctness Baseline

Extend `parallel-decoder-bench` so every measured path reports:

- selected input protocol;
- setup and run wall time;
- processed samples and decoded words;
- a stable fingerprint of decoded `(value, timestamp_ns, duration_ns)` tuples;
- retained annotation count/fingerprint for viewer runs;
- worker count once parallel execution exists;
- peak memory and process CPU time in the recorded benchmark procedure.

Add two fixtures:

1. The existing dense DDR reference capture.
2. A deterministic sparse-clock capture small enough for normal tests.

The benchmark must support indexed, packed stream, and eventually auto mode.
Before proceeding, indexed and packed count runs must produce identical word
counts and fingerprints.

Status: complete. The benchmark now reports the selected protocol and a
stable, order-sensitive output fingerprint. `--mode both --sink count`
automatically fails when indexed and packed output counts or fingerprints
differ. On the first 50 million reference samples both produced 11,999,858
words with fingerprint `5872c3203a967271`. It also reports process user/system
CPU time, average cores used, and peak RSS. A generated sparse `.dsl` fixture
runs through the complete indexed and packed pipelines in normal tests.

## Step 2: Consumer-Aware Protocol Selection

Add a Binary Decoder input strategy:

- `Auto`
- `Packed stream`
- `Indexed`

Protocol negotiation currently lets producer ordering win. Extend the runtime
contract so a consumer can explicitly constrain or prefer a supported
protocol without changing producers or unrelated connections. The same logic
must be used by the static `Pipeline` and live `AppManager` paths.

Status: complete. `ParallelInputStrategy` constrains the decoder's raw
input schemas without changing global negotiation: Auto accepts both protocols,
Packed stream accepts only `Stream`, and Indexed accepts only `EdgeQuery`.
Level-triggered modes always stream. The desktop state and compiler expose the
same choice, with missing state from older graph files defaulting to Auto.
Static benchmark pipelines and the live manager both have end-to-end protocol
selection coverage. SPI and other query consumers are unchanged.

`Packed stream` must negotiate `Stream` for strobe, data, and CS block inputs.
`Indexed` must negotiate `EdgeQuery`. SPI and sparse query consumers retain
their current preference. Add negotiation tests for static construction, live
node addition/restart, and plugin builders.

## Step 3: Automatic Dense/Sparse Selection

Expose inexpensive per-channel transition-density metadata from the waveform
index. It should use root/index summaries and must not scan raw samples.

The Binary Decoder uses the strobe channel and its edge mode to estimate the
useful trigger density. Auto selection chooses:

- packed blocks when point-query work approaches a material fraction of all
  samples;
- indexed queries when hierarchical skipping avoids most raw blocks.

The threshold must be benchmark-derived and recorded in one place. Diagnostics
and benchmark output must state the chosen protocol and density estimate.
Explicit `Packed stream` and `Indexed` settings always override Auto.

Status: complete. `EdgeQuery::activity_ratio_hint` exposes the fraction
of active 64-sample index groups, and the file-backed implementation computes
it solely from the mmap'd waveform index. Synthetic tests measure below 1% for
isolated pulses and above 99% for a toggle every four samples. Static and live
runtime negotiation now presents all input candidates to the consumer at once;
the Parallel Decoder applies one strobe-derived decision to strobe, every data
input, and CS. The reference DDR capture reports ratio `1.000000`, selects
packed streaming, and matches the explicit indexed/packed count and fingerprint
(11,999,858 words, `5872c3203a967271` over 50 million samples). The generated
sparse capture selects indexed queries and also matches both explicit modes.

## Step 4: Window Fragment Refactor

Separate packed decoding into two phases.

### Parallel phase

Each immutable, aligned 65,536-sample window produces a `DecodeFragment`:

- monotonically increasing window sequence number;
- selected trigger positions;
- sampled bus values at those triggers;
- reset markers caused by CS or enable boundaries;
- first and last strobe/CS/enable observations needed for boundary repair;
- an exhausted/end-of-input marker.

Workers only read shared `SampleBlock` backing and write their private
fragment. They never emit `Word` values or update decoder fields.

### Ordered phase

The decoder consumes fragments by sequence number and performs the small
stateful portion sequentially:

- repair the first edge using the preceding window's last strobe value;
- apply CS and enable transitions at the boundary;
- carry or reset partial word assembly;
- construct `Word` values and timestamps;
- emit one ordered batch per merged fragment.

Sequential and fragment paths must share assembly helpers so their semantics
cannot drift.

Status: complete. Packed decoding is now split into an independent
`scan_stream_fragment` phase and an ordered `merge_stream_fragment` phase.
The scanner reads only immutable aligned `SampleBlock` backing plus its owned
reusable output buffers. For edge-triggered modes it records the first sample
as boundary metadata instead of depending on preceding decoder state; the
ordered merge repairs that edge from the previous fragment's final strobe
value. CS gating is represented by reset markers, while streamed or queried
enable state remains in the ordered phase. Word assembly, timestamps, output
batches, and decoder state are updated only by the merge. Release builds reject
out-of-sequence fragments instead of relying on a debug assertion.

The fragment differential test covers every split of a 137-sample packed
window for rising, falling, DDR, high-level, and low-level strobes, including
CS-gated spans and partial three-cycle words. The 50-million-sample reference
still produces 11,999,858 words with fingerprint `5872c3203a967271` in
indexed, explicit packed, and Auto modes. A controlled comparison against the
pre-Step-4 `HEAD` measured 115.6-118.9 MSamples/s before and 118.0-121.1
MSamples/s after for the count sink. The unlimited viewer measured 138.1
MSamples/s before and 137.0 MSamples/s after, with identical retained output
fingerprint `7b230a5c11e0818c`. Reusing fragment buffers avoids per-window
allocation and keeps the sequential refactor within the 5% regression gate.

## Step 5: Bounded Worker Pool

Use one shared native worker pool rather than spawning threads per decoder or
window. Start comparisons at 1, 2, 4, and 8 workers. The initial automatic
limit is `min(available_parallelism, 8)` and must remain configurable for the
benchmark.

Keep at most `2 * workers` windows in flight. A reorder buffer keyed by window
sequence handles out-of-order completion. Input acquisition stops when the
queue is full, preserving pipeline backpressure. Workers and the coordinator
check cancellation between bounded chunks.

Record queue depth and fragment memory. The default configuration must not add
more than 128 MiB of transient memory on the reference graph.

Status: complete. Native packed decoding now submits independent 65,536-sample
scan jobs to one process-wide compute pool. The pool has at most eight threads;
each decoder uses four by default and can request 1-8 through
`with_parallel_workers`. The benchmark exposes the same range through
`--workers`. wasm always takes the Step 4 sequential path. A decoder keeps at
most `2 * workers` fragments outstanding, receives completions through a
bounded channel, and stores early completions in a sequence-keyed reorder
buffer. End-of-input is propagated only after all queued fragments are merged.
Each scan job is itself the cancellation chunk, and dropped decoders disconnect
their completion channel so the bounded remainder exits without emitting.
Worker panics are caught and reported to the coordinator rather than leaving it
blocked on a missing completion.

Metrics now report effective workers, peak outstanding windows, peak reorder
depth, and a conservative fragment-buffer memory estimate. The final dense
Auto/viewer run used four workers, peaked at 8 outstanding and 3 reordered
fragments, and estimated 2.0 MiB of fragment buffers, well below the 128 MiB
limit. Reverse-order completion and multi-window sequential/parallel
differential tests cover reorder behavior and partial word assembly across
window boundaries. A missing `ProcessNode for Box<dyn ProcessNode>` forwarding
method was also fixed: without it static pipelines silently ignored the
decoder's Auto protocol override and selected indexed queries despite the dense
activity result.

Reference 50-million-sample scaling (MSamples/s):

| Sink | 1 worker | 2 workers | 4 workers | 8 workers |
| --- | ---: | ---: | ---: | ---: |
| Discard | 221.2 | 467.0 | 494.1 | 481.9 |
| Count + fingerprint | 118.1 | 121.4 | 119.9 | 116.9 |
| Unlimited viewer | 137.4 | 325.1 | 377.9 | 363.9 |

Four workers are the default because the real viewer path improves materially
through four while eight adds overhead. The final dense Auto/viewer validation
measured 382.3 MSamples/s (7.65x real time), retained all 11,999,858
annotations, and preserved fingerprint `7b230a5c11e0818c`.

## Step 6: Viewer Follow-Up

Profile again after packed protocol selection and parallel decoding. Only if
the viewer becomes the measured limit, replace the monolithic annotation
mipmap with chunked indexing:

- raw annotations remain one ordered logical timeline;
- immutable chunks are built independently from decoder batches;
- each chunk owns its local summary;
- a small top-level index locates intersecting chunks;
- the final open annotation remains patchable until its successor arrives.

Cursor snapping, partial-word rendering, Home-to-fit, and unlimited finite
retention must behave identically.

Status: complete. Opt-in `ViewerSinkMetrics` split channel-drain time from
store/index append time, and the benchmark gained a raw `Retain` sink to
isolate transport plus vector retention. On the 50-million-sample/four-worker
reference, raw retention reached 470.4 MSamples/s at 353 MiB, while the
pre-change viewer reached 368.8 MSamples/s at 638 MiB. Viewer channel drain
cost only 16 ms; annotation plus mipmap append cost 109 ms of 136 ms wall
time. The annotation mipmap was therefore both the measured throughput limit
and roughly half of viewer memory, satisfying the implementation gate.

`LaneSummary::Annotations` now uses a 4,096-entry `ChunkedMipmap`. Its active
chunk keeps exact leaf records in one reusable allocation. A completed chunk
folds to one immutable summary record, and an `AppendOnlyMipmap` over those
chunk summaries provides the small top-level window index. Raw annotations
remain one ordered `Vec`, so exact drawing, cursor snapping, partial/open word
handling, and Home-to-fit continue to use the unchanged timeline. The newest
instantaneous annotation still remains outside the summary until its successor
patches its end. Digital and marker summaries are unchanged.

Post-change runs reached 421-485 MSamples/s (8.4-9.7x real time), reduced
append time to 72-80 ms, and reduced a clean single-run peak RSS to 358-362
MiB. All modes retained 11,999,858 annotations with fingerprint
`7b230a5c11e0818c`. Chunk rollover/accounting and all viewer sink tests pass;
the logic-analyzer viewer's 37 rendering, cursor, sampling, and Home-to-fit
tests also pass.

## Step 7: Validation and Tuning

Correctness coverage:

- indexed versus packed versus parallel differential word tuples;
- every possible word-assembly offset across a window boundary;
- rising, falling, and DDR strobes;
- active-high, active-low, and disabled CS;
- streamed and queried enable signals;
- partial words at CS/enable boundaries and EOF;
- deliberately reordered worker completion;
- stop, restart, and live graph edits under load;
- viewer rendering and cursor snapping at fragment boundaries.

Performance gates on the reference machine:

- packed selection must not regress the measured 231 MSamples/s unlimited
  viewer baseline by more than 5%;
- parallel execution must show a material improvement through at least four
  workers before becoming the default;
- dense automatic selection must choose packed streaming;
- sparse automatic selection must choose indexed queries;
- sustained decoding must remain faster than real time;
- cancellation latency must remain below 100 ms;
- output count and fingerprint must match the sequential reference exactly.

Status: complete. The final defaults are four scan workers per native
Parallel Decoder, eight process-wide compute threads maximum, 65,536 samples
per scan fragment, `2 * workers` outstanding fragments, a 25% Auto activity
threshold, and 4,096 annotations per viewer-summary chunk. wasm remains
sequential. Explicit worker counts from 1-8 remain available to the benchmark
and decoder builder.

Sustained release validation on the dense reference capture:

| Workload | Samples | Throughput | Real time | Peak RSS |
| --- | ---: | ---: | ---: | ---: |
| Auto packed, discard | 1,000,000,000 | 565.6 MSamples/s | 11.31x | 176 MiB |
| Auto packed, unlimited viewer | 200,000,000 | 474.7 MSamples/s | 9.50x | 1,222 MiB |

The viewer retained all 47,999,433 annotations in the 200-million-sample run
with fingerprint `edbfc01f07fb1b1c`; its memory is dominated by the required
complete raw annotation timeline. Indexed, explicit packed, and Auto count
runs over the same 200 million samples all produced 47,999,433 words with
fingerprint `175831e21f59c527`. Auto used packed jobs for a generated dense
capture and indexed queries for a generated sparse capture through the real
pipeline, rather than merely reporting the density prediction.

Correctness coverage now includes every fragment split for all five strobe
modes, partial three-cycle words and CS resets, streamed and queried enable,
documented active-low and active-high CS levels in both input protocols,
deliberately reversed worker completion, sequential/parallel multi-window
differential output, viewer chunk rollover, cursor/rendering behavior, and
live-manager stop/restart/reconfigure/branch edits. The audit found and fixed
reversed Parallel Decoder CS predicates: active-low now decodes while low and
active-high while high, matching `CsPolarity`'s documented contract. A queued
parallel scan scheduler test passes the 100 ms stop-latency gate. Native
workspace and wasm compilation pass.

`cargo test -p dsl` still has twelve unrelated legacy `dsl_file` tests that
require an untracked `scan.dsl` in the working directory; that fixture is not
in the repository. The focused decoder, benchmark, viewer, derived-index, and
live-manager suites pass without it.

If a phase misses its performance gate, keep its correctness refactor only
when it simplifies the next measurement; otherwise revert that phase before
continuing.

## Delivery Order

1. Benchmark reporting and fingerprints.
2. Explicit protocol strategy.
3. Automatic density selection.
4. Window fragment refactor, sequential first.
5. Bounded worker pool and reorder buffer.
6. Viewer chunking only if profiling requires it.
7. Sustained validation, defaults, and documentation.
