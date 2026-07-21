# LogicConduit

A desktop application for capturing, decoding, and analyzing digital signals. You draw
a decode pipeline as a node graph — sources, protocol decoders, logic, file writers, a
waveform viewer — press **Run**, and watch the results appear live in the built-in logic
analyzer view. Graphs run on a streaming, thread-per-node engine and can be edited while
they run.

## Features

- **Node-graph editor** (Blender-style): searchable add menu, frames, reroutes, undo/redo,
  copy/paste, a properties panel, and per-node validation badges.
- **Logic analyzer view** for multi-GB `.dsl` captures: realtime pan/zoom at any scale
  (background indexing, never blocks the UI), time cursors, exact pulse measurement,
  and live lanes showing decoded output while a pipeline runs.
- **Protocol decoders**: SPI, UART, parallel/binary bus (SDR and DDR); plus matchers,
  gates, flip-flops, counters, formatters, and file writers for building
  trigger-and-capture logic out of nodes.
- **Live editing**: attach a new matcher or viewer lane, tweak a pattern, or remove a
  branch while the pipeline is running — the engine applies the smallest possible change.
- **Sources**: `.dsl` capture file replay and live DSLogic U3Pro16 USB capture.

## Quick start

```bash
# Build and start the editor (release mode recommended)
cargo run --release --bin logic-conduit

# Or open a graph directly
cargo run --release --bin logic-conduit -- graphs/spi_controlled_decode.json
```

The editor starts with an empty graph. Use **File ▸ Load** (`⌘O`/`Ctrl+O`) to open a saved
graph — example graphs are in [graphs/](graphs) — then press **▶ Run** in the toolbar.
If the graph contains a *DSL File Source* node, its capture file opens automatically in
the waveform view above the graph.

## The editor

The window is split by a draggable divider: the **logic analyzer view** on top, the
**node graph** below, with the Run/Stop toolbar between them.

### Editing the graph

| Action | How |
|---|---|
| Add a node | `A` or right-click ▸ Add / Search |
| Connect | Drag from a socket to a compatible socket (incompatible ones won't snap) |
| Disconnect | Drag a wire off its input |
| Select | Click; box-drag; shift-click to extend |
| Move | Drag node headers |
| Pan / zoom | Drag empty canvas / scroll |
| Cut, copy, paste | `⌘X` / `⌘C` / `⌘V` (works across app instances) |
| Duplicate | `⇧D` |
| Delete | `Delete`, `Backspace`, or `X` |
| Undo / redo | `⌘Z` / `⇧⌘Z` |
| Properties panel | `N` (or the tab strip on the right edge) — settings of the active node |
| Minimap | `M` |
| Frames (group nodes) | select ▸ `⌘J`; rename/recolor via right-click |
| Hide unconnected sockets | `⌘H`; collapse a node via right-click |
| Reroute wires | Add a *Reroute* node as a wire waypoint |

(macOS `⌘` = Ctrl on Linux/Windows.)

Socket shapes and colors tell you what fits where: **circles** are continuous signals
(logic levels, counts, text — anything with a value at every instant), **diamonds** are
events (decoded words, triggers), **squares** are fixed settings. Colors group payload
kinds (green = logic, orange = words, amber = triggers, blue = numbers, rose = text).
Nodes that can't compile show a badge explaining why.

### Running

**▶ Run** compiles the graph and starts it; the toolbar shows *Live* while data flows and
node headers show live item counts. You can keep editing while it runs — most changes
(new branches, removed branches, pattern/template tweaks) apply within half a second
without disturbing the rest of the pipeline; changes that need a full restart say so in
the toolbar. **⏹ Stop** winds the run down; output files are flushed and closed.

### The logic analyzer view

| Action | How |
|---|---|
| Pan / zoom | Drag, scroll horizontally / scroll vertically (zooms around the pointer) |
| Fit whole capture | Double-click or `F` |
| Time cursors | Double-click the ruler to add; drag a cursor's flag to move it |
| Measure a pulse | Hover it — width, period, and duty cycle, exact at any zoom |
| Rename a row | Double-click its label |
| Reorder rows | Drag labels |
| Colors | Profile selector (top right): DSView or Classic |

While a pipeline runs, *Viewer* nodes add live rows below the capture channels: digital
traces, decoded-word boxes, and trigger markers.

### Nodes

| Category | Nodes |
|---|---|
| Sources | DSL File Source · Sigrok File Source · DSLogic U3Pro16 (live USB capture) |
| Decoders | SPI Decoder · UART Decoder · Binary Decoder (parallel bus, SDR/DDR) · I2C Decoder (placeholder — editable but not yet runnable) |
| Logic | Word Matcher · SR Flip-Flop · Logic Gate (NOT/AND/OR/XOR/…) · Counter · String Formatter · Buffer |
| Sinks | File Writer · Text File Writer · TGCK Recorder · Viewer |

A typical trigger-and-capture graph: decode SPI commands, match start/stop words, drive an
SR flip-flop that gates a parallel-bus decoder, count captures into generated filenames,
and write each start/stop window to its own file — see
[graphs/spi_controlled_decode.json](graphs/spi_controlled_decode.json).

## Command line & logging

```bash
cargo run --release --bin logic-conduit -- <graph.json>   # open a graph at startup

# Logging via RUST_LOG (per-module filtering)
RUST_LOG=info cargo run --release --bin logic-conduit
RUST_LOG=info,logic_analyzer_processing::nodes::decoders::spi_decoder=debug cargo run --release --bin logic-conduit
```

If a pipeline appears stuck, the built-in watchdog logs which node is blocked on which
port after ~5 seconds.

## Building & testing

```bash
cargo build --release      # release strongly recommended for capture processing
cargo test                 # workspace tests
```

### Testing the browser app on macOS

Install the WebAssembly target and the `wasm-bindgen` CLI once. The CLI version must
match the version pinned by this workspace:

```bash
rustup target add wasm32-unknown-unknown
cargo install wasm-bindgen-cli --version 0.2.126 --locked
```

Build the browser application and serve its generated files over HTTP:

```bash
scripts/build_wasm_app.sh
python3 -m http.server 8000 --directory target/wasm-app/dist
```

Then open <http://localhost:8000> in Safari, Firefox, or Chrome. Keep the server running
while testing and press `Ctrl-C` in its terminal to stop it. Do not open `index.html`
directly from Finder: browser security rules require the JavaScript modules and WASM
file to be loaded over HTTP.

After changing Rust or web files, run `scripts/build_wasm_app.sh` again and refresh the
browser. For a compile-only check matching CI, run:

```bash
cargo check -p logic-analyzer-app-web --target wasm32-unknown-unknown
```

The repository is a Cargo workspace: `crates/signal_processing` (generic streaming runtime),
`crates/logic_analyzer_processing` (concrete decoders, processing nodes, and file/USB sources),
`crates/logic_analyzer_graph` (node catalog and graph compiler),
`crates/widgets/node_graph` (reusable node editor widget),
`crates/widgets/logic_analyzer_viewer` (waveform widget), `crates/logic_analyzer_ui`
(application UI), `crates/app_native` (desktop binary), `crates/app_web`
(browser entry point), and `plugins/example-plugin` (an example
compile-time extension: build with
`--features example-plugin`).

Loadable pipeline examples live in [graphs/](graphs). They include file-backed
SPI processing and direct DSLogic U3Pro16 capture graphs:

```bash
cargo run --release --bin logic-conduit -- graphs/spi_controlled_decode.json
```

The sole standalone Rust example is `ccd_viewer`, a native framebuffer utility
for inspecting captured CCD image data rather than a processing pipeline.

## Documentation

| Document | Contents |
|---|---|
| [docs/APP_DESIGN.md](docs/APP_DESIGN.md) | Application shell, node set, graph→pipeline compiler, live editing |
| [docs/NODE_GRAPH_DESIGN.md](docs/NODE_GRAPH_DESIGN.md) | Node editor architecture: model, socket type system, widget |
| [docs/NODE_GRAPH_API.md](docs/NODE_GRAPH_API.md) | Embedding the editor widget and defining node types |
| [docs/LOGIC_ANALYZER_VIEWER_DESIGN.md](docs/LOGIC_ANALYZER_VIEWER_DESIGN.md) | Waveform viewer: index format, sampling, rendering |
| [docs/LOGIC_ANALYZER_VIEWER_API.md](docs/LOGIC_ANALYZER_VIEWER_API.md) | Embedding the viewer widget |
| [docs/LIVE_CAPTURE_TRIGGER_DESIGN.md](docs/LIVE_CAPTURE_TRIGGER_DESIGN.md) | Live-capture foundation and staged plan for hardware triggering, capture, and replay |
| [docs/PIPELINE_DESIGN.md](docs/PIPELINE_DESIGN.md) | Streaming engine: nodes, channels, backpressure, live supervision |
| [docs/DSLOGIC_U3PRO16_PROTOCOL.md](docs/DSLOGIC_U3PRO16_PROTOCOL.md) | DSLogic U3Pro16 USB protocol (hardware reference) |
| [docs/REGISTERS.md](docs/REGISTERS.md) | Hardware register reference |
| [docs/CCD_DATA_STREAM.md](docs/CCD_DATA_STREAM.md) | The CCD data stream the example pipeline decodes |

## Development

This project was developed collaboratively with AI assistance (Codex/Claude/GitHub Copilot).

## License

MIT — see [LICENSE](LICENSE).
