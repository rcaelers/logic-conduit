# Plugin-Extensible Collected Payload Design

## Current architecture

`PortKind` is an open runtime payload identity. A compile-time plugin implements `PortValue` for its
Rust payload type. A graph-node inventory submission carries any idempotent runtime channel setup
needed for a non-collected custom payload; collected payload capability registration performs the
same typed channel setup as part of its atomic submission.

`CollectedPayloadRegistry` records a durable, plugin-owned identity for each payload intended to
become collectable. `CollectedPayloadRegistration` inventory submissions atomically provide that
identity, typed channel setup, adapter factory, request configuration, persistence policy, and
default waveform presentation. `BuilderRegistry::standard()` applies submissions in stable-ID
order and rejects identity/type collisions before graph-node payload requirements are validated.

`DerivedDataCollector` schedules adapter-created lane ingestors beside its
other lanes. An adapter publishes its retained, type-erased query object through `DerivedLanes`,
so a later subscriber can discover it by stable payload identity and downcast only to its own
registered query type. Built-in payloads are registered through this same path and retain their
digital, indexed-word, marker, numeric, and text data behind their adapters.

`BuilderRegistry` owns one collection-subscription contract per subscribable payload. The contract
binds the open `PortKind` to its adapter descriptor, diagnostic name, default waveform
presentation, request configuration, and optional persistent-cache policy. Lowering obtains the
accepted kinds from these contracts. Materialization invokes the selected contract and adapter;
the generic data-collector builder contains no built-in payload list, type comparison, adapter
registry construction, or payload-specific request setup. Registering a payload identity or an
adapter alone therefore does not make it subscribable.

The built-in digital adapter publishes an opaque query with bounded exact-transition and dense
activity snapshots, cursor-boundary lookup, and timeline extent. Its viewer presentation consumes
those snapshots through the same renderer contract as a plugin payload.

The built-in trigger adapter provides the same query capabilities with exact marker timestamps or
dense marker-activity snapshots. Its presentation is likewise adapter-owned; neither the generic
collector nor the generic viewer interprets trigger values to render the normal subscribed row.

The built-in numeric and text adapters retain their respective `i64` and `String` payload values in
their queries. Their graph-owned presentation adapters format bounded values only while rendering;
the generic collector does not convert a payload into display text.

The built-in word adapter exposes `CollectedWordLaneQuery` as its concrete query contract. Generic
subscribers use its bounded waveform and table capabilities through `CollectedLaneQuery`; concrete
word diagnostics may downcast the query and inspect its indexed store without accessing generic
collector storage.

`CollectedLaneQuery` supplies an immutable snapshot only when its payload has waveform
semantics. The request is bounded by a visible time window and item limit. The viewer passes the
returned `OpaqueCollectedLaneSnapshot` to the payload's renderer only after it has released
retained-data locks; the renderer downcasts the snapshot only to its own registered type. An
opaque lane activates an explicit viewer group or a singleton default presentation registered for
its stable payload identity. Renderers also project bounded snapshots into payload-neutral level
and event transitions for hover measurement and event-row interaction. Cursor boundaries,
timeline extent, and live status are query capabilities. The generic viewer neither reads a
parallel built-in lane representation nor matches a payload type.

## Collected-payload adapters

A payload is *collectable* only when its owner registers explicit timeline and storage semantics.
This is intentionally narrower than being a `PortValue`: a payload such as a configuration command
may flow through a graph but has no useful retained timeline representation.

```text
plugin payload T
      │
      ├── PortValue + runtime channel registration
      │
      └── collected-payload adapter registration
                   │
                   ▼
           CollectedPayloadRegistry
                   │
                   ▼
           DerivedDataCollector
                   │
                   ▼
           retained query handle
             ├── waveform presentation
             ├── table presentation
             └── plugin panel
```

The collector owns no protocol, payload, viewer, table, or panel knowledge. It schedules a set of
object-safe lane ingestors. Each ingestor is constructed by the adapter registered for one payload
type.

### Stable identities

An adapter combines the registered process-local Rust `TypeId` and stable payload identifier such
as `"org.logicconduit.camera-frame/v1"` with its typed ingest and presentation contracts.

`TypeId` selects the typed channel and adapter while an application runs. Saved graphs, saved
panel state, persistent caches, and missing-plugin diagnostics use the stable identifier; they
never serialize `TypeId`.

Graph documents store a versioned `logic_analyzer_graph.payload_subscriptions` extension for every
explicit Viewer input and every `show_in_view` output. Each entry identifies its endpoint and the
payload owner's stable identifier. Socket indices and `show_in_view` remain in the generic graph
model, while the namespaced extension supplies the domain-specific compatibility contract. On
load, legacy built-in lanes are assigned their registered stable identities without changing
their connections, selection state, ordering, grouping, badge, or renderer. The application shows
a persistent compatibility warning when a saved payload, ingestion subscription, or presentation
registration is unavailable and retains the unresolved identity on subsequent saves.

The identity registry accepts an identical repeat registration, but rejects a Rust type assigned a
different identifier or an identifier assigned to a different Rust type. The future adapter
registration uses the same rule for its storage and presentation definition. This prevents two
plugins from silently assigning incompatible semantics to one payload.

### Runtime adapter contract

`CollectedPayloadAdapter` registration for `T` creates a typed lane ingestor while exposing only
an erased, object-safe interface to the collector.

```rust
trait ErasedLaneIngestor: Send {
    fn input_schema(&self, index: usize) -> PortSchema;
    fn ingest(&mut self, input: &InputPort) -> WorkResult<usize>;
    fn is_finished(&self) -> bool;
}
```

The typed factory captures `T` during plugin registration. It creates the correct `PortSchema`,
downcasts the `InputPort` only to `Receiver<T>`, and owns all append state. The generic collector
only schedules ingestors and applies backpressure and retention policy. The ingestor publishes its
query handle while it is created, allowing subscribers to appear after data has been collected.

`CollectedLaneQuery` exposes bounded visible-window snapshots. Snapshots are immutable and
type-erased at the generic boundary. Plugin-owned presentation adapters receive only the snapshot
type registered with their payload; the generic viewer never gives them access to mutable
collector state. Timeline extent, activity summaries, and boundary snapping are added as explicit
capabilities rather than inferred from a payload type.

### Presentation and panels

The waveform viewer owns a separate presentation registry keyed by the collected payload identity.
Its adapter supplies default group/badge metadata and a renderer for bounded snapshots. Rendering
occurs after the retained-lane lock is released. `ViewerLaneTheme` supplies current background,
foreground, muted, accent, and error roles. `ViewerLaneInteractionContext` supplies the bounded
visible range, item budget, hover state, and optional pointer time. Renderers receive these value
objects instead of viewer internals and can therefore respond to host theme and interaction state
without reaching into `LogicAnalyzerViewer`.

Table projection is optional adapter metadata. `CollectedLaneQuery::table_metadata` supplies a
revision and row count for cache invalidation, while `table_snapshot(max_rows)` supplies bounded
scalar rows with a format hint and a completeness flag. The decoder-table panel consumes this
contract through opaque lane handles and never reads a concrete payload's retained storage. An
adapter may expose rows and columns when that is meaningful; no table-specific behavior is
required for arbitrary payloads.

Extra panels are UI-owned plugin registrations, not graph or processing registrations. A panel
submits a stable identity, title, icon, minimum size, factory, and optional singleton constraint
through `UiPanelRegistration`. The application discovers those descriptors when it
builds the View menu and panel-layout catalog, so adding a panel does not add an application dispatch
branch. Each panel instance receives a restricted read-only context containing collected-lane
descriptors and query handles. It keeps versioned serializable state under its stable panel identity
and the layout instance identity. A panel can be opened after a capture finishes because it queries
retained data rather than receiving the live stream. The current contract intentionally exposes no
application mutation commands.

The out-of-tree example plugin proves the complete route with `CameraFrame`: a custom socket and
finite source produce timestamped RGB images, a custom adapter retains bounded frames, a viewer
renderer draws bounded thumbnail snapshots, and a Camera Frames panel queries the same retained
lane. The source reaches collection through an explicit Viewer connection; neither the collector,
viewer, application panel catalog, nor panel layout contains a CameraFrame-specific branch.

Contract tests cover identity and adapter collisions, missing registrations, typed channel
construction and negotiation, retention limits, bounded dense snapshots, timeline extent, renderer
lock release, and saved-panel-state diagnostics. Architecture tests reject built-in payload and
protocol checks in generic collection, compiler, and viewer paths. CI compiles the example plugin
on native targets as part of the workspace and explicitly on `wasm32-unknown-unknown`.

### Crate ownership

- `signal_processing` owns type-erased ingestion, retained query, snapshot, and storage contracts.
- `logic_analyzer_viewer` owns presentation adapters and drawing contracts for those snapshots.
- `logic_analyzer_graph` owns compiler negotiation: it accepts only registered collectable payloads
  for a data subscription and reports a targeted error for an unavailable subscription contract
  or adapter.
- `logic_analyzer_ui` owns panel factories, panel state, and the read-only panel data context.
- Application composition iterates the independent graph, payload, and panel inventories without
  making `signal_processing` depend on graph or UI crates. Enabled plugin crates are retained by a
  host symbol anchor; the web platform entry point invokes module constructors once before the
  first inventory iteration.

Compile-time Rust plugins are the initial extension mechanism. Runtime-loaded native plugins need
an additional ABI-stable boundary and are outside this design phase.
