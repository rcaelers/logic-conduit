# Application & UI — Design

Design of the desktop/wasm application: the `logic-analyzer-ui` crate
([crates/logic_analyzer_ui](../crates/logic_analyzer_ui)) — the application shell;
the `logic-analyzer-graph` crate ([crates/logic_analyzer_graph](../crates/logic_analyzer_graph)) —
node definitions and the graph→pipeline compiler; and the thin native and web application crates
([crates/app_native](../crates/app_native), [crates/app_web](../crates/app_web)). Companion docs:
[NODE_GRAPH_DESIGN.md](NODE_GRAPH_DESIGN.md) (the editor widget),
[PIPELINE_DESIGN.md](PIPELINE_DESIGN.md) (the runtime it compiles into),
[LOGIC_ANALYZER_VIEWER_DESIGN.md](LOGIC_ANALYZER_VIEWER_DESIGN.md) (the waveform view).

Layering rule: `node_graph` stays UI-generic, `signal_processing` stays generic and UI-free, and
`logic_analyzer_processing` owns concrete UI-independent runtime nodes. Concrete feature
integration lives under `logic-analyzer-graph/src/nodes/<feature>/`; shared lowering remains in
`logic-analyzer-graph/src/compiler/`. The application UI
consumes that integration and must not contain concrete node/compiler implementations.

---

## Application shells (`crates/app_native`, `crates/app_web`)

One window, split by a draggable horizontal splitter:

```text
┌──────────────────────────────────────────────┐
│ File menu (Load / Save / Save As / Quit)     │
├──────────────────────────────────────────────┤
│ LogicAnalyzerViewer         (top pane)       │
├━━━━━━━━ splitter ━━━━━━━━━━━━━━━━━━━━━━━━━━━━┤
│ Toolbar: ▶ Run / ⏹ Stop · status messages    │
├──────────────────────────────────────────────┤
│ NodeGraphWidget             (bottom pane)    │
└──────────────────────────────────────────────┘
```

- `App::build` creates the node-type registry (`nodes::build_registry()`), the builder
  registry (`compiler::BuilderRegistry::standard()`), runs plugin registration hooks, and
  installs a platform symbol font (menu glyphs).
- Graphs are saved/loaded as the editor's JSON document (`⌘O` / `⌘S` / `⇧⌘S`); the binary
  accepts a graph file argument (`dsl-ui graphs/spi_controlled_decode.json`). Example graphs live in
  [graphs/](../graphs).
- Every frame, the app asks the graph integration for an opaque pre-run capture presentation.
  Concrete source builders supply an indexed-capture factory, an in-memory preview, or a channel
  layout. The app and viewer do not identify node types or know what DSL and Sigrok paths mean.
  Runtime processing nodes remain compiler-owned and are created only when a run starts.
- `logic-analyzer-app-native` binary (named `dsl-ui`): clap CLI,
  `tracing_subscriber` with `RUST_LOG` env filter,
  and an eframe native window.
- `logic-analyzer-app-web` exports the wasm-bindgen `WebHandle` used by the browser shell.
  It runs the shared `App` with a demo UART graph pre-populated and the cooperative
  scheduler (below).
- File commands include New, Open, Open Recent, Save, and Save As. Destructive actions over
  an unsaved graph share one save/discard/cancel guard; recent paths are deduplicated and
  persisted.
- `eframe` storage retains the analyzer split, node-graph panel/minimap preferences, recent
  files, and dialog directory. The viewport uses eframe's own persisted geometry.
- Transient file/edit/live-run results use dismissible, self-expiring toasts; persistent run
  state remains in the toolbar. The toolbar also shows the active pane's contextual hint.
- Run and Stop are shared guarded commands used by toolbar and menus. Their shortcuts are
  `Cmd/Ctrl+R` and `Cmd/Ctrl+.` respectively.

## Socket styling: shape = time structure, color = payload

Two orthogonal axes, applied graph-wide via `node_graph`'s type identity table:

**Shape encodes how the value exists in time** — the property that decides which nodes can
consume it:

| Shape | Structure | Meaning |
|---|---|---|
| ■ Square | Static config | One value, fixed before the run (inline controls) |
| ● Circle | Level stream | Defined at *every* instant, transmitted as changes: can gate, can be read "now" |
| ◆ Diamond | Event stream | Timestamped occurrences, *undefined between events*: can only be reacted to |

**Color encodes the payload family**, identical across shapes (an `Int` config ■ and a
`Number` level ● are visibly kin):

| Socket type | Runtime stream | Look | Used by |
|---|---|---|---|
| `Signal` | `Sample` (or `SampleBlock`, negotiated) | green ● | channels, gates, latch Q, enables |
| `Words` | `Word` | orange ◆ | decoder outputs, matcher/writer/viewer inputs |
| `Trigger` | `Trigger` | amber ◆ | matcher out, latch set/reset, counter in |
| `Number` | `NumberSample` | blue ● | counter out, formatter in |
| `Text` | `TextSample` | rose ● | formatter out, writer filename |
| `Bool` / `Int` / `Float` / `Str` / `File` | static config | ■ (green/blue/violet/rose/tan) | inline controls |
| `Any` | wildcard | grey ● | reroutes |

Rules for extending: new structure of an existing payload keeps the hue and changes the
shape; a new payload family gets a new hue; red is reserved for error feedback; grey for
the wildcard. Colorblind robustness comes from the shape axis: hues that could collide
never share a shape.

## Node set (`crates/logic_analyzer_graph/src/nodes/`)

The `sources`, `decoders`, `logic`, and `sinks` directories mirror the processing-node families.
Each executable feature directory within them groups a `node_graph::NodeDef` in `definition.rs`
with its `RuntimeBuilder` in `builder.rs` and optional presentation metadata. Placement rule: the node body
carries sockets and the controls someone tweaks while reading the graph (matcher pattern,
gate op, template); everything else goes to the properties panel (SPI word size/CPOL/CPHA/bit
order, writer options, device settings). Viewer lane selection and presentation settings such as
decoder data format live in the separate View panel.

Sources: DSL File Source, DSLogic U3Pro16 (device settings in the panel — capture/signal
sections and a 16-channel enable grid with the channel-count↔rate constraint enforced in
`on_update`), UART Demo Source. Decoders: SPI, UART, Binary (parallel bus, SDR/DDR), and
an I2C placeholder (a `NodeDef` with no builder: editable, not runnable).
Logic: Word Matcher, SR Flip-Flop, Logic Gate (op enum retitles the node; NOT caps the
variadic group at one), Buffer, Counter, String Formatter. Sinks: File Writer (inline save
dialog while `Filename` is unconnected; a connected text stream hides it and wins), Text
File Writer, TGCK Recorder, Viewer (variadic input accepting
`Signal | Words | Trigger | Number | Text`).

The native application loads graphs selected by the user. The wasm application embeds and loads
`crates/app_web/data/wasm_decoder_demo.json`, a self-contained one-minute SPI-controlled
parallel-bus capture backed by `Demo Capture Source`. Programmatic graph construction is confined
to test fixtures in `nodes::test_graphs`. File-backed test fixtures live under the owning crate's
`tests/data/` directory. The editable examples in `graphs/` are not code dependencies.

## Graph → pipeline compiler

### Builders

Every executable node type registers its feature-local `RuntimeBuilder` in `nodes/catalog.rs`,
keyed by its `NodeDef::name()`. Generic lowering and runtime reconciliation live in `compiler/`:

```rust
pub trait RuntimeBuilder {
    fn is_source(&self) -> bool;                 // produces the time domain
    fn is_sink(&self) -> bool;                   // pruning root
    fn accepted_kinds(&self, socket, state) -> Vec<PortKind>;   // input side
    fn offered_kinds(&self, socket, state) -> Vec<PortKind>;    // output side, preference order
    fn input_port(&self, socket, member_index, state, kind) -> Option<String>;
    fn output_port(&self, socket, state, kind) -> Option<String>;
    fn input_required(&self, socket, state) -> bool;            // e.g. CS only while polarity ≠ Disabled
    fn input_buffer_override(&self, socket, state) -> Option<usize>; // Buffer node only
    fn build(&self, name, state, resolved, ctx) -> Result<Box<dyn ProcessNode>, String>;
    fn hot_config(&self, state) -> Option<NodeConfig>;          // live: apply without restart
}
```

`PortKind` is an open, `TypeId`-backed payload identity (`PortKind::of::<T: PortValue>()`,
[port_kind.rs](../crates/logic_analyzer_graph/src/compiler/port_kind.rs)) — the compiler-layer analogue of
`node_graph::SocketDef` and `signal_processing::register_type`, so plugin crates add payload types
without editing any compiler file.

**Kind negotiation** is per edge: `offered(producer) ∩ accepted(consumer)`; empty →
compile error; multiple → the producer's preference order wins. This resolves the one-UI-
socket/two-runtime-flavors split: a source offers `Signal` as `[SampleEdge, Block]`, the
SPI decoder accepts `[SampleEdge]`, the binary decoder `[Block]` — one UI wire fanning out
to both becomes two runtime connections from the two ports, legal and free. UI-only nodes
(frames, reroutes) have no builder; the compiler follows wires through reroutes.

### Two-stage construction

**Stage 1 — `lower(graph, registry) → CompiledGraph`**: a pure IR (nodes with canonical
state + resolved input kinds + runtime name `n{id}_{title_slug}`; edges with concrete port
names, negotiated kind, buffer size). Cheap to rebuild on every edit and cheap to diff —
exactly what live reconfiguration needs. Lowering prunes to nodes reachable from a sink,
then validates: every required input wired, hex props parse, **exactly one source node per
graph** (one time domain — timestamps from different sources are incomparable). Errors
carry `NodeId` so the editor badges the offending node.

**Stage 2 — materialize**: for each node `builder.build(…)`; for each edge a channel of
the negotiated kind. Offline this fills a `Pipeline`; live it feeds `NodeSpec`s to the
`PipelineManager` (native) or `CooperativeManager` (wasm).

### Buffer policy

Buffer size comes from the consumer edge's `PortKind` (`PortValue::buffer_size`):

| Kind | Buffer | Rationale |
|---|---|---|
| `Block` | 4 | each block ≈ 2 MB |
| `SampleEdge`, producer is a source | 10,000,000 | RLE edge bursts of fast raw channels |
| `SampleEdge`, control path | 1,000 | low rate |
| everything else (`Word`, `Trigger`, `Number`, `Text`, …) | 100 | sparse events |

Sizes reflect item characteristics only — never inter-branch skew; decoupling a slow branch
is the explicit `Buffer` node's job (its builder's `input_buffer_override` puts its
user-set capacity on its input edge). See the flow-control section of
[PIPELINE_DESIGN.md](PIPELINE_DESIGN.md).

## Run lifecycle & live editing

`start_app_run` lowers the current graph and starts a `LiveRun` — the app always runs
through the live machinery (a file replay is just a run whose source finishes).

Per frame, the app calls `run.pump(budget)` (no-op on the native threaded manager; on wasm
this is what executes node `work()`s) and requests ~16 ms repaints while running so derived
lanes fill visibly. Every 500 ms it:

1. Publishes per-node progress counters into node headers (`set_node_status`).
2. Re-lowers the edited graph and calls `run.apply(graph, builders)`, which diffs desired
   vs. running by `NodeId` and applies the cheapest edit class per difference:

| Edit | Action | Node threads touched |
|---|---|---|
| Add a tap (new branch on existing outputs) | materialize just the new nodes; subscribe; sticky levels prime | none |
| Remove a branch | unsubscribe + close its lists → branch-local shutdown cascade | removed ones |
| Hot prop change (matcher pattern, template, …) | `hot_config` → control-channel `Configure` | none |
| Prop change that can't hot-apply / rewire | restart-in-place | that node |
| Source changed / replaced | `NeedsFullRestart` — reported, run left untouched | — |

Mid-edit graphs are often momentarily invalid; compile errors during live sync are silently
ignored (the running pipeline continues; the diff retries once the graph is valid again).
`Disconnect` overflow events surface as warning badges ("can't keep up"). Stop = wind-down
via the manager; final progress counts stick. Each run swaps a fresh `DerivedLanes` store
into the viewer so stale lanes vanish atomically.

The correctness gate for the whole compile path is the golden test: the compiled startup
graph must produce byte-identical output to the hand-written pipeline example on a real
capture (`cargo test -p logic-analyzer-graph --release -- --ignored golden`), run through the live
machinery.

## Plugins

Compile-time plugin crates extend all three layers through one hook:
`App::new_with_plugins(cc, |ctx: &mut PluginContext| …)` where the context exposes
`register_payload::<T>()` (runtime type registry + `PortValue`),
`register_node::<T: NodeDef>()`, and `register_builder(name, Box<dyn RuntimeBuilder>)`.
A plugin depends on `logic-analyzer-graph`, so the registration call lives in the binary
crate that depends on both (`logic-analyzer-app-native`, behind the `example-plugin` Cargo
feature). [plugins/example-plugin](../plugins/example-plugin)
demonstrates a new payload type, socket type, node def, and builder
(`Pulse Measure`).

## wasm

The same `App` compiles to `wasm32-unknown-unknown`: no file dialogs/paths, no capture
files, no threads. The UART demo graph runs on the `CooperativeManager` pumped from the
frame loop; derived viewer lanes are the only viewer content. Cargo features/`cfg` gates
keep the native-only nodes (file source/writers, USB driver) out of the wasm registry.
