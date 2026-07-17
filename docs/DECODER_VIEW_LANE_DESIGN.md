# Decoder View Lane Design

## Architecture boundary

Generic layers do not contain decoder-specific behavior.

`node_graph`, `logic_analyzer_viewer`, and generic graph compiler infrastructure remain
independent of UART, SPI, Binary Decoder, and all other concrete node types. Protocol behavior
belongs in:

- its feature directory in `crates/logic_analyzer_graph/src/nodes/`, which groups
  `definition.rs`, `builder.rs`, and protocol-specific presentation when needed;
- its runtime implementation in `crates/logic_analyzer_processing/src/nodes/`.

`signal_processing` remains UI-independent. It stores timestamped lane payloads and indexes, but
does not depend on egui or on a viewer renderer. `logic_analyzer_ui` only composes the graph and
viewer services; it does not define concrete renderers.

Saved-graph compatibility is handled at node restore/load boundaries and reported with
user-visible warnings. Presentation metadata is reconstructed from node builders when a graph is
compiled and is not inferred while loading a graph.

## Current lane model

Runtime nodes publish ordinary derived lanes with stable storage keys. `ViewerLaneRegistry`
explicitly assigns those payloads to displayed groups and tracks. The viewer renders digital,
annotation, indexed-annotation, and marker payloads using generic drawing and query paths.

UART bit and frame outputs carry explicit presentation metadata from `UartDecoderBuilder`. Their
grouping, value semantics, styling, geometry, and snap policy are implemented by the UART adapter
in `logic_analyzer_graph`; the viewer does not infer them from names or values.

## Lane presentation design

### Ownership and module layout

The reusable contract lives in `logic_analyzer_viewer`, because that crate owns the coordinate
system, drawing primitives, interaction model, and data-query integration. The contract is
public and protocol-neutral.

Concrete implementations live with their node feature:

```text
crates/logic_analyzer_graph/src/nodes/uart_decoder/
  definition.rs
  builder.rs
  presentation.rs
  mod.rs
```

`logic_analyzer_graph` depends on `logic_analyzer_viewer` and constructs these implementations.
This dependency direction does not form a cycle: the viewer depends only on generic runtime
data in `signal_processing`, while the graph crate depends on both.

A presentation module describes viewing and interaction rather than owning another kind of
derived data. Keeping it beside the definition and builder makes the complete concrete feature
discoverable in one directory.

### Two separate per-run stores

A run has two stores with the same lifetime:

1. `DerivedLanes` in `signal_processing` contains payloads, summaries, and indexed query handles.
2. `ViewerLaneRegistry` in `logic_analyzer_viewer` contains immutable row/group presentation
   objects and explicit references to the payload lanes they present.

`CompileCtx` owns both stores. `logic_analyzer_ui` gives clones to the viewer before compilation,
just as it currently does for `DerivedLanes`. Compiling Viewer sinks fills both stores. Starting a
new run swaps both stores, so data and presentation cannot leak across runs.

The presentation registry is not placed inside `DerivedLanes`: doing so would make the
UI-independent `signal_processing` crate depend on egui-facing trait objects.

### Identities, groups, and tracks

The contract has three opaque identities:

- `DerivedLaneId` identifies one runtime payload lane;
- `ViewerLaneGroupId` identifies one displayed row;
- `ViewerLaneTrackId` identifies a semantic track within that row.

They are identifiers, not display strings. A group also has a display label and badge metadata,
but changing those strings does not change identity or behavior.

A `ViewerLaneGroup` contains:

- a group ID, label, badge, and renderer object;
- an ordered list of tracks;
- for each track, its track ID, `DerivedLaneId`, and relative height.

Viewer-sink and producer node identities automatically namespace group and track keys during
graph lowering. This lets two UART nodes use the same local keys without colliding. It also lets
the same output feed several Viewer nodes without merging their rows.

A singleton group is the default for an ordinary digital, word, or marker lane. A compound group
is only created from explicit metadata. Missing optional tracks are valid: for example, a UART
renderer can present only its frame track when its bit-detail output is not connected.

Row ordering, renaming, height, drawing, hit-testing, and snapping use the group ID. A payload
lane that belongs to a compound group is not also inserted as an independent row.

### Registration during graph lowering

`RuntimeBuilder` provides a protocol-neutral hook that returns optional viewer-output presentation
metadata for one output socket. The UART builder implements the hook by delegating to the UART
adapter in `viewer_lanes::uart`. Generic lowering carries the opaque metadata, producer node ID,
and output socket identity in `ResolvedInput`; it never examines their values.

When `ViewerBuilder` creates a payload lane for a resolved input, it wraps the lane's stable
storage key in an explicit `DerivedLaneId` and associates that ID with the resolved presentation
metadata. It then groups tracks by the namespaced group key and registers the resulting
`ViewerLaneGroup`. Inputs without presentation metadata are registered through the viewer's
default singleton-group constructor.

This keeps the Viewer graph node generic. It negotiates `Sample`, `Word`, and `Trigger`, creates
payload lanes, and forwards opaque presentation contracts. It does not branch on producer node
type, output label, protocol, or annotation value.

The hook is part of the public builder/plugin contract. A plugin can provide its own renderer and
track metadata without modifying the viewer or the standard builder registry.

### Renderer and viewer facilities

`ViewerLaneRenderer` is an object-safe, immutable `Send + Sync` trait held behind `Arc`. Its
operations supply the concrete semantics needed by the generic row drawing path:

- compute row metrics from the base lane height and available tracks;
- resolve an annotation value to its label and visual style;
- select the explicitly registered tracks eligible for snapping at a pointer position.

The trait receives group/track metadata and fully generic default annotation visuals rather than
`LogicAnalyzerViewer` or runtime lane storage. Before a renderer runs, the viewer prepares a
bounded `ViewerLaneFrame` snapshot for the visible window. Sparse annotation snapshots contain
exact values up to a viewport-derived limit; dense snapshots contain activity only and do not
invoke value formatters. Indexed handles are queried after the lane lock is released.

The viewer itself owns the protocol-neutral facilities:

- visible time range, time-to-screen transforms, clipping, colors, and track rectangles;
- digital waveform, marker, and annotation-track drawing;
- angled annotation boxes, dense presence bands, and indexed/in-memory sampling;
- configurable annotation labels and visual style supplied by the renderer;
- nearest transition, marker, and annotation-boundary queries with a caller-provided tolerance.

Exact and indexed lanes therefore use the same drawing rules, dense rendering remains bounded,
and cursor snapping uses the same storage/query rules for every renderer. A concrete adapter
chooses composition and semantics through its output metadata and renderer: track ordering,
relative heights, labels, colors, and which generic boundaries are eligible for snapping.

Renderer implementations perform no direct I/O and retain no references to a locked lane store.
The viewer clones query handles and prepares the minimal bounded frame before invoking renderer
code or any query that can touch storage. This preserves the existing rule that derived-lane
locks are not held across storage I/O and prevents plugin renderer code from running under a lane
lock.

### Default presentation

`logic_analyzer_viewer` supplies default singleton renderers for the generic payload families:

- digital levels;
- decoded words/annotations, including a generic numeric format;
- event markers.

Knowing these payload families is part of the generic viewer contract. Knowing that a value is a
UART start marker, an SPI transfer direction, or a decoder error is not.

The default annotation renderer owns the angled-box geometry, fitting, clipping, dense fallback,
and generic number formatting. Concrete renderers reuse it with a value presenter that can map
protocol values to labels and styles.

### UART presentation

The UART builder explicitly contributes one compound group with bit-detail and frame tracks. The
UART renderer:

- chooses track order and relative heights;
- formats bit values and frame values;
- interprets UART start, stop, and error sentinel values;
- assigns UART-specific error styling;
- selects the relevant track or tracks for cursor snapping.

The viewer sees only group/track IDs, geometry, generic payload references, and calls to its own
drawing/query facilities. It contains no `uart`, `Bits`, `Data`, start/stop, or UART sentinel
logic.

SPI and Binary Decoder outputs use the default word presentation.

### Sampling overlays

Clocked nodes can also contribute a protocol-neutral sampling-overlay descriptor. The descriptor
identifies a clock input definition, sampled input groups, an electrical edge rule (rising,
falling, or both), and optional active-level qualifiers. Concrete builders derive that descriptor
from their node state. Generic lowering resolves its input references to explicit capture-channel
origins supplied by capture source builders; it never parses socket labels or runtime port names.

Capture-backed qualifiers are evaluated directly from their channel value at each candidate clock
edge. When a qualifier is produced by processing rather than a capture channel, lowering provides
the runtime node with a generic, shared activity timeline. The runtime publishes active intervals
to that timeline while it processes data. The timeline stores only level boundaries rather than a
marker for every clock edge. This permits derived enable conditions without introducing protocol
knowledge into the viewer or storing sampling overlays as derived words.

The application exposes each resolved descriptor as a host-contributed node context action and
keeps at most one selected node. Selection is presentation state rather than node state. The
viewer receives only the selected clock channel, sampled channel indices, edge rule, qualifiers,
and activity timelines. It draws directional markers and sampled-value circles only on exact
visible clock edges for which every sampling condition is active. Marker rendering is bounded by
viewport density and is suppressed when the indexed window contains only unresolved activity
summaries.

Sampling descriptors and resolved channel origins are reconstructed from node definitions during
lowering and are not serialized in graph files. Native and wasm use the same descriptor and
rendering path.

### Proposed future adapters

If SPI needs transfer framing or combined MOSI/MISO presentation, an SPI adapter can register a
compound group through the same contract without changing generic viewer code.

### Saved graphs and migration

The presentation registry is derived from the current node definition and is not serialized.
The UART socket schema remains stable, so existing graphs load without a presentation migration.
The hidden legacy UART `Words` output remains a normal singleton word lane when connected.

If a later adapter requires socket or state changes, the concrete node supplies explicit
deserialize/default or load-migration handling and a user-visible warning. Generic viewer and
compiler code do not recognize legacy node names or repair protocol wiring.

### Platform behavior

The registry, IDs, group model, and renderer trait have one platform-neutral shape. Native and
wasm use the same presentation contract. Storage capability differences remain behind the
existing derived-word-store platform boundary; renderer code does not add target-gated fields,
variants, or match arms.

### Validation invariants

The implementation preserves these invariants:

1. `logic_analyzer_viewer` contains no concrete node names, protocol labels, or protocol sentinel
   values.
2. `node_graph` and generic lowering never inspect presentation keys or renderer types.
3. Every visible derived row has an explicit group; default singleton groups are explicit too.
4. Every group track refers to a registered payload lane of a compatible generic family.
5. A payload lane appears in at most one displayed group for one Viewer sink.
6. Group behavior is unchanged by node-title edits, row renames, translated labels, or duplicate
   lane-name suffixes.
7. Drawing and snapping remain bounded for indexed and dense in-memory lanes.
8. Derived-lane locks are not held across indexed storage queries or renderer-controlled work.
9. Native and wasm compile against the same group and renderer APIs.
10. Plugins can register a concrete renderer without editing generic crates.
