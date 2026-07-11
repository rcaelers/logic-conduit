# Pipeline Runtime — Design

Design of the `dsl` crate ([crates/dsl](../crates/dsl)): the streaming engine that executes
decode pipelines. It is UI-free — the node-graph editor and the graph→pipeline compiler
live above it (see [APP_DESIGN.md](APP_DESIGN.md)).

---

## Architecture

Thread-per-node streaming with bounded channels:

- Every node implements `ProcessNode` and runs on its own OS thread (true parallelism;
  no locking in node logic).
- Nodes are connected by bounded crossbeam channels created from the graph description;
  one output can broadcast to any number of inputs.
- **Backpressure is automatic and genuine**: a full channel blocks the producer, and the
  stall propagates upstream. This is deliberate (see *Flow control philosophy* below).
- Shutdown is a cascade: when a source finishes (or is dropped), closed channels surface as
  `WorkError::Shutdown` in downstream `recv()`/`send()` calls, unwinding each node cleanly.

```text
┌────────────────┐      ┌──────────────┐      ┌────────────┐
│ DslFileSource  │─────▶│  SpiDecoder  │─────▶│ FileWriter │
│   (thread 1)   │      │  (thread 2)  │      │ (thread 3) │
└────────────────┘      └──────────────┘      └────────────┘
     Sample edges          Word events           files
```

### `ProcessNode`

The single node trait ([runtime/node.rs](../crates/dsl/src/runtime/node.rs)). Sources have
0 inputs, sinks 0 outputs, processors both.

- `work(&mut self, inputs: &[InputPort], outputs: &[OutputPort]) -> WorkResult<usize>` —
  process one batch; the scheduler loops it. Blocking `recv`/`send` inside `work()` is
  normal; `Err(WorkError::Shutdown)` ends the node.
- `input_schema()` / `output_schema()` — named, typed ports (`PortSchema` carries the
  `TypeId`); this is what makes graph wiring name-based and type-checked.
- `is_self_threading()` — a node that manages its own worker threads (e.g. `DslFileSource`
  spawns per-channel readers); the scheduler calls `work()` once to start it and then waits
  on `should_stop()`.
- `apply_config(&NodeConfig) -> ConfigOutcome` — optional hot reconfiguration
  (`Applied` / `NeedsRestart`), used by live editing (below).

### `Pipeline` (offline builder)

[runtime/pipeline.rs](../crates/dsl/src/runtime/pipeline.rs) /
[ports.rs](../crates/dsl/src/runtime/ports.rs): name-based graph construction —
`add_process("name", node)`, `connect("from", "port", "to", "port")` (or
`connect_with_buffer(…, size)`), then `build()` validates names and port `TypeId`s, creates
one bounded channel per edge via the **type registry** (`register_type::<T>()` — every
payload type that flows through a channel must be registered so the builder can create
channels type-erased), fans outputs out to their subscriber lists, injects watchdog context,
and hands everything to the `Scheduler`. Endpoints move into node threads; teardown is the
drop-cascade. `scheduler.wait()` joins all threads.

## Stream types and the level contract

All timestamps are **nanoseconds in one shared domain** — every value ultimately derives
its time from the same source position, which is what keeps decoded events, control levels,
and viewer lanes aligned (including across live-edit rejoins).

| Type | Kind | Meaning |
|---|---|---|
| `Sample` | level | Logic-level change (RLE edge): `value`, `start_time_ns` |
| `SampleBlock` | bulk | Packed raw bits of one channel block (bandwidth path) |
| `Word` | event | One decoded value ≤ 64 bits: `value`, `timestamp_ns`, `duration_ns` — the single word type every decoder emits |
| `Trigger` | event | Instantaneous occurrence (matcher hit), time only |
| `NumberSample` | level | Integer level change (counter output) |
| `TextSample` | level | Text level change (formatter output / filename) |

**The level-stream contract** ([runtime/events.rs](../crates/dsl/src/runtime/events.rs)):
every low-rate stream is a *stepped level* — defined at every instant, transmitted as
changes only. Every level producer emits its initial value at t=0 on its first `work()`
call, and consumers hold the last received value. Consequently any node can always answer
"what is your value now" (a gate can always combine, the writer always knows the current
filename) and **no node ever blocks waiting for a control value to exist** — only data can
lag. Only `Word` and `Trigger` are true events, undefined between occurrences: they can
only be reacted to.

This contract is the system's deadlock guard, and it extends to live editing: level
channels are *sticky* — a subscriber added mid-stream is immediately primed with the
current value.

`Sample` vs `SampleBlock` is a bandwidth concern, not a semantic one: a source exposes each
channel both as RLE edges (`d{i}`) and packed blocks (`b{i}`); consumers declare which they
take and the compiler picks per edge.

## Flow control philosophy

Minimal buffers, real backpressure. If a consumer is slow, its producer — and every branch
sharing that producer — genuinely slows down. That is correct behavior, not a bug, even
when it paces an otherwise-independent branch: a truncated or silently lossy decode is
worse than a slow one.

- Default buffer sizes are small and sized by *item characteristics* (block ≈ 2 MB → 4;
  raw RLE edges → millions for burst headroom; sparse events → 100), never to absorb
  inter-branch skew.
- When a branch must be deliberately decoupled from a slower sibling (e.g. a decoder
  feeding both a file writer and the viewer), the mechanism is an explicit **`Buffer`
  node** (`BufferNode<T>`, [nodes/logic/buffer.rs](../crates/dsl/src/nodes/logic/buffer.rs))
  with a user-visible, user-configured capacity — not a bigger invisible default.
- `ViewerSink` drains its lanes in bounded batches, so a producer that outruns the viewer
  really does block on `send()` rather than the sink racing to keep the channel empty.
- The watchdog (below) is the diagnostic net for genuinely-too-small buffers.

## Live supervision

Live capture must keep running while the graph is edited. The offline `Pipeline::build`
forgets its endpoints; the live path inverts ownership so partial change is possible.

### `SharedSenders` ([runtime/sender.rs](../crates/dsl/src/runtime/sender.rs))

Two broadcast flavors coexist. Offline: static destination lists moved into threads. Live:
a supervisor-owned subscriber list per output — a node thread exiting does *not* close
downstream channels; subscribers can be added/removed mid-stream; **sticky level priming**
replays the last value (re-stamped) to late joiners. Each subscription carries an
`OverflowPolicy`:

- `Block` — lossless flow control; the default for every edge, viewer taps included.
- `Lossy` — never block; coalesce to the newest value. Suitable only for pure-display
  level taps; not used by default.
- `Disconnect(deadline)` — block up to the deadline, then unsubscribe the laggard and
  report it (`take_disconnected`) so the editor can badge the branch instead of silently
  corrupting a live capture.

### `PipelineManager` ([runtime/manager.rs](../crates/dsl/src/runtime/manager.rs))

Owns, per running node: its thread, its state, a control channel, its output subscriber
lists, and its input subscription ids. Operations, cheapest first:

| Operation | Mechanism |
|---|---|
| **Add a tap** | Materialize the new nodes; subscribe their receivers into existing lists; sticky levels prime them. Untouched nodes never notice. |
| **Remove a branch** | Unsubscribe its roots, close its own lists → the ordinary shutdown cascade, confined to the branch; join its threads. |
| **Reconfigure (hot)** | `NodeCommand::Configure` on the control channel, checked by the scheduler loop between `work()` calls → `apply_config`. |
| **Restart in place** | Unsubscribe the node's inputs → its next `recv()` sees `Shutdown` → thread exits cleanly (the normal unwind is the kill mechanism). A fresh instance is spawned with new subscriptions and the *same* output lists (generation + 1). Downstream just sees a quiet channel. |

Resynchronization after a restart is each node's normal startup behavior: decoders wait for
the next protocol boundary, gates/latches get primed levels. The shared timestamp domain
keeps a rejoined branch time-aligned — correct from now on, never misaligned. A node
exiting *naturally* closes its own lists on the way out, so end-of-run propagates exactly
like the offline drop-cascade. Deferred start (`add_node_deferred` + `start_all_deferred`)
lets sources snapshot complete subscriber lists before the first block. The manager also
accumulates per-node progress counters (items returned by `work()`, kept across
restarts-in-place) that the UI draws in node headers.

### `CooperativeManager` ([runtime/cooperative_manager.rs](../crates/dsl/src/runtime/cooperative_manager.rs))

Single-threaded sibling for `wasm32` (no `std::thread`). Drives the same `NodeSpec`s and
subscriber-list machinery — live add/remove/restart/reconfigure and sticky priming behave
identically — but never blocks: `pump(budget)` (driven from the UI frame loop) only calls a
node's `work()` when every input is ready **and** no output would block
(`SharedSenders::would_block`). A blocked-downstream node is skipped for that pump cycle and
retried once the consumer drains. This relies on one invariant: on the cooperative backend a
node performs at most one send per output per `work()` call.

## Node library

| Category | Nodes |
|---|---|
| Sources | `DslFileSource` (file replay; per channel `d{i}` edges + `b{i}` blocks), `DsLogicU3Pro16Source` (live USB capture; `LogicAnalyzerSource<DsLogicU3Pro16>` behind the driver-neutral `LogicAnalyzer`/`LogicCaptureConfig` interface — see [DSLOGIC_U3PRO16_PROTOCOL.md](DSLOGIC_U3PRO16_PROTOCOL.md)), `UartDemoSource` (synthetic; the wasm demo) |
| Decoders | `SpiDecoder` (modes 0–3, 1–64-bit words, MSB/LSB, CS polarity; two `Word` outputs mosi/miso), `ParallelDecoder` (strobe SDR/DDR/level, 1–64 data bits, optional CS/Enable, multi-cycle word assembly with endianness), `UartDecoder` (single-line, derived bit clock, parity/framing error triggers) |
| Logic / control | `WordMatcher` (pattern & mask, comparison ops, trigger at word start/end), `SrLatch`, `LogicGate` (NOT/AND/NAND/OR/NOR/XOR/XNOR over variadic inputs), `TriggerCounter`, `TextFormatter` (template substitution, up to 4 inputs merged in timestamp order), `BufferNode` (explicit decoupling) |
| Sinks | `BinaryFileWriter` (filename-level–driven file rollover; never blocks on `Filename`), `TextFileWriter`, `TgckRecorder` (per-capture line-boundary CSV), `ViewerSink` (pushes lanes into the shared `DerivedLanes` store; see [LOGIC_ANALYZER_VIEWER_DESIGN.md](LOGIC_ANALYZER_VIEWER_DESIGN.md)) |

Two merge disciplines exist, chosen deliberately per node:

- **Strict timestamp merge** (`LogicGate`): keeps every input's current level plus one
  pending edge, applies the globally earliest edge, and blocks on an input whose next edge
  is unknown. Safe for levels (an input either advances or closes) and *required* for
  correctness: input arrival skew is unbounded, and an event-driven merge would consume the
  fast input far past the slow one and corrupt the output timeline. Cost is lag, not
  deadlock — the accepted-lag model.
- **Event-driven merge** (`SrLatch`): processes whichever input has data, in
  `(timestamp, input)` order among what's available. A strict merge would starve here —
  sparse trigger streams carry no "nothing happened" information, so a set couldn't be
  emitted until the *next* reset arrived. A late-arriving opposite event is clamped to the
  last emitted timestamp and logged (defensive; protocol-scale gaps make it practically
  impossible).

Both stateful level nodes emit their initial state at t=0 (the level contract).

## Observability

Structured logging via `tracing`; every node logs under its module path, so `RUST_LOG`
filters per node type:

```bash
RUST_LOG=info                                          # everything at info
RUST_LOG=info,dsl::nodes::decoders::spi_decoder=debug  # one decoder at debug
RUST_LOG=info,dsl::nodes=debug                         # all nodes at debug
```

The **watchdog** ([runtime/watchdog.rs](../crates/dsl/src/runtime/watchdog.rs)) monitors
blocking channel operations transparently: enabled via `Pipeline::with_watchdog()` (and
always on in the managed/live path), it wraps every port so `recv`/`send` register guarded
operations, and reports any operation blocked longer than ~5 s with node name, port, and
direction:

```text
WARN dsl::runtime::watchdog: ⚠️  BLOCKED: [spi_decoder] recv on port 'clk' for 5.2s
```

This pinpoints which node of a stalled pipeline is stuck, on which port — the safety net
for the minimal-buffer philosophy above. Node implementations need no watchdog code; the
ports handle it.

## Waveform index

The capture-file mipmap index (`runtime/waveform_index/`), raw block cache, and the
incremental derived-lane index (`runtime/derived_index.rs`) are documented in
[LOGIC_ANALYZER_VIEWER_DESIGN.md](LOGIC_ANALYZER_VIEWER_DESIGN.md) — they live in this
crate but exist to serve the viewer.
