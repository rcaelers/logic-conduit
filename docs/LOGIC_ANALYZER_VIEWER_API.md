# Logic Analyzer Viewer Widget — API

How to embed the `logic-analyzer-viewer` crate
([crates/widgets/logic_analyzer_viewer](../crates/widgets/logic_analyzer_viewer)). For internals (index,
sampling, rendering) see
[LOGIC_ANALYZER_VIEWER_DESIGN.md](LOGIC_ANALYZER_VIEWER_DESIGN.md).

Public surface:

```rust
pub use viewer::{ChannelSignal, LogicAnalyzerViewer};
pub use lanes::{
    AnnotationVisual, DerivedLaneId, ViewerLaneBadge, ViewerLaneFrame,
    ViewerLaneGroup, ViewerLaneGroupId, ViewerLaneRegistry, ViewerLaneRenderer,
    ViewerLaneTrack, ViewerLaneTrackFrame, ViewerLaneTrackId,
    ViewerOutputPresentation,
};
```

Capture/lane data types come from the `signal-processing` crate: `CaptureDataSource`,
`DslFileCaptureDataSource`, `DerivedLanes`.

---

## Getting started

```rust
use logic_analyzer_viewer::LogicAnalyzerViewer;

let mut viewer = LogicAnalyzerViewer::new();   // or ::default()

// per frame, inside your egui layout:
viewer.show(ui);                               // fills the available rect
```

`show` is self-contained: it drains background-worker messages, handles all interaction
(pan, zoom, cursors, row rename/reorder, hover measurement), samples the visible window,
paints, and schedules repaints while a capture is opening or indexing.

## Feeding data

The viewer renders three independent kinds of rows, freely combined; a single internal row
order spans all of them (user-reorderable by dragging labels).

### 1. Capture files — `set_capture_path`

```rust
viewer.set_capture_path(path, |path| {
    signal_processing::DslFileCaptureDataSource::open(path).map_err(|e| e.to_string())
});
```

The viewer only knows the generic `signal_processing::CaptureDataSource` trait (`open_reader`,
`metadata`, `fingerprint`, `index_path`, `display_name`); the `open` closure is the one
place that knows what a path means. Calling it again with the same path is a no-op;
a new path replaces the capture rows (derived lanes are untouched) and spawns a background
worker that parses the header (channels appear immediately), builds or validates the
sidecar waveform index (progress bar in the header), and finally hands the UI a synchronous
sampler. On open failure the viewer clears capture rows and shows the error in its status
line. Native only (`#[cfg(not(target_arch = "wasm32"))]`).

### 2. In-memory channels — `set_channels`

```rust
use logic_analyzer_viewer::ChannelSignal;

viewer.set_channels(vec![ChannelSignal {
    index: 0,
    name: "CLK".into(),
    initial: false,
    transitions: vec![(0.0, true), (12.5, false), (25.0, true)], // (time_us, level after)
}]);
```

Replaces the channel rows with data the host already has — independent of files and
pipelines. Transitions must be in increasing time order. Hover measurement works directly
from these transitions.

### 3. Live pipeline output — `set_derived_lanes`

```rust
let lanes = signal_processing::DerivedLanes::default();      // shared Arc<RwLock<…>> store
viewer.set_derived_lanes(lanes.clone());       // viewer renders it live
// hand `lanes` to the pipeline's ViewerSink nodes (the app's compiler does this)
```

Whatever the running pipeline pushes into the store appears as extra rows under the
channels, repainted live: digital lanes (rendered like channels), annotation lanes (boxed
decoded values), and marker lanes (event ticks). Swap in a fresh store per run to clear the
previous run's lanes atomically; existing channel rows are never touched by a run.

Without an explicit presentation registry, each payload becomes a default singleton row. Hosts
that compile concrete node presentations pair the data store with a per-run registry:

```rust
use logic_analyzer_viewer::ViewerLaneRegistry;

let presentations = ViewerLaneRegistry::new();
viewer.set_viewer_lanes(presentations.clone());
let mut compile_ctx = logic_analyzer_graph::compiler::CompileCtx::default();
compile_ctx.viewer_lanes = presentations; // same registry used by Viewer builders
```

The registry contains explicit `ViewerLaneGroup` and `ViewerLaneTrack` objects. A group can combine
several payload lanes in one displayed row and supplies a `ViewerLaneRenderer` for row height,
annotation labels/styles, and snap-track selection. Concrete producer builders contribute
`ViewerOutputPresentation` through `RuntimeBuilder::viewer_output_presentation`; the generic
Viewer builder performs registration without inspecting node names, socket labels, or metadata
values.

Before invoking a renderer, the viewer prepares a bounded `ViewerLaneFrame` and releases the
derived-lane lock. Sparse annotation frames contain exact visible values; dense frames become
activity bands and skip per-value formatting. Renderer and plugin code therefore never executes
while the runtime lane store is locked.

## Threading & repaint behavior

- All queries happen synchronously on the UI thread against an mmapped index, so what is
  drawn always matches the current view; only opening/indexing runs on a worker thread.
- While opening, the viewer requests ~16 ms repaints; while indexing, ~100 ms. Once idle,
  normal egui repaint-on-input applies (a running pipeline's host should request repaints
  itself, as the app does).
- Stale worker messages (from a superseded `set_capture_path`) are ignored.

## Built-in interaction (for reference)

| Input | Effect |
|---|---|
| Drag waveform area | Pan |
| Scroll X / Scroll Y | Pan / zoom (pivoted on the pointer's time position) |
| Double-click waveform / `F` | Fit the whole capture |
| Double-click ruler | Add a time cursor (drag its flag/line to move; numbered, stable colors) |
| Double-click row label | Rename the row (viewer-local; underlying data untouched) |
| Drag row label | Reorder rows |
| Hover a waveform | Pulse measurement tooltip (width/period/duty), exact at any zoom |
| Combo box, header right | Color profile: DSView (default) / Classic |

## wasm

The crate compiles for `wasm32`: `set_capture_path` and the worker do not exist there;
in-memory channels and derived lanes are the only content.
