# Logic Analyzer Viewer Widget — API

How to embed the `logic-analyzer-viewer` crate
([crates/widgets/logic_analyzer_viewer](../crates/widgets/logic_analyzer_viewer)). For internals (index,
sampling, rendering) see
[LOGIC_ANALYZER_VIEWER_DESIGN.md](LOGIC_ANALYZER_VIEWER_DESIGN.md).

Public surface:

```rust
pub use viewer::{ChannelSignal, LogicAnalyzerViewer};
pub use lanes::{
    AnnotationVisual, DefaultViewerLaneRenderer, DerivedLaneId, OpaqueLaneDrawContext,
    ViewerLaneBadge, ViewerLaneInteraction, ViewerLaneInteractionContext, ViewerLaneTheme,
    ViewerLaneGroup, ViewerLaneGroupId, WaveformPresentationRegistry, ViewerLaneRenderer,
    ViewerLaneTrack, ViewerLaneTrackId, ViewerOutputPresentation,
};
```

Capture/lane data types come from the `signal-processing` crate: `CaptureDataSource`,
`CaptureIndexFactory`, and `DerivedLanes`. Concrete file sources remain outside the widget.

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

### 1. Indexed captures — `set_capture_factory`

```rust
viewer.set_capture_factory(identity, factory);
```

The viewer only knows the generic `signal_processing::CaptureIndexFactory`. Concrete graph-source
features create factories for their formats. Calling the method again with the same opaque
identity is a no-op; a new identity replaces the capture rows and moves all opening and index work
to a background worker. `set_capture_path` remains available to lower-level hosts that already own
a concrete `CaptureDataSource`.

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
let lanes = signal_processing::DerivedLanes::default();
viewer.set_derived_lanes(lanes.clone());       // viewer renders it live
// hand `lanes` to the pipeline's DerivedDataCollector nodes (the app's compiler does this)
```

Collection adapters publish stable payload descriptors and type-erased query handles into this
store. A lane appears when its payload has a registered singleton presentation or an explicit
group. Swap in a fresh store per run to clear the previous run's lanes atomically; existing
channel rows are never touched by a run.

Hosts pair the data store with a per-run presentation registry:

```rust
use logic_analyzer_viewer::WaveformPresentationRegistry;

let compile_ctx = logic_analyzer_graph::compiler::CompileCtx::default();
viewer.set_waveform_presentations(compile_ctx.waveform_presentations().clone());
```

The registry contains explicit `ViewerLaneGroup` and `ViewerLaneTrack` objects plus singleton
defaults keyed by stable payload identity. A group can combine several payload lanes in one
displayed row and supplies a `ViewerLaneRenderer` for row height, bounded drawing, optional
level/event interaction, annotation labels/styles, and snap-track selection. Drawing receives a
theme with semantic color roles and an interaction context containing the bounded window, item
budget, hover state, and pointer time. Concrete producer builders contribute
`ViewerOutputPresentation` through `RuntimeBuilder::viewer_output_presentation`; the generic
waveform-subscription builder performs registration without inspecting node names, socket labels,
or metadata values. The compiler independently materializes a neutral `DerivedDataCollector` for
the subscribed outputs.

Before invoking a renderer, the viewer asks the lane's query for an immutable snapshot bounded by
the visible time window and item budget, then releases the lane registry lock. Exact-versus-dense
snapshot semantics belong to the payload adapter. Renderer and plugin code therefore never
executes while the runtime lane store is locked.

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
| `Home` / `F` | Fit the whole capture |
| Double-click time canvas | Add a time cursor (drag its flag/line to move; numbered, stable colors) |
| Double-click row label | Rename the row (viewer-local; underlying data untouched) |
| Drag row label | Reorder rows |
| Hover a waveform | Pulse measurement tooltip (width/period/duty), exact at any zoom |
| Combo box, header right | Color profile: DSView (default) / Classic |

## wasm

The crate compiles for `wasm32`: `set_capture_path` and the worker do not exist there;
in-memory channels and derived lanes are the only content.
