# Plugin-Extensible Collected Payload Design

## Current architecture

`PortKind` is an open runtime payload identity. A compile-time plugin implements
`PortValue` for its Rust payload type and calls `PluginContext::register_payload::<T>()` so the
generic processing runtime can create channels for `T`.

`CollectedPayloadRegistry` records a durable, plugin-owned identity for each payload that is
intended to become collectable. `BuilderRegistry::standard()` registers the five built-in retained
payload types, and `PluginContext::register_collected_payload::<T>(stable_id)` registers a plugin
payload with both the runtime channel factory and that identity registry. The registry rejects a
stable identifier assigned to multiple Rust types, and a Rust type assigned multiple identifiers.

`PluginContext::register_collected_payload_adapter::<T>(stable_id, adapter)` additionally records
an adapter factory. `DerivedDataCollector` schedules adapter-created lane ingestors beside its
built-in lanes. An adapter publishes its retained, type-erased query object through `DerivedLanes`,
so a later subscriber can discover it by stable payload identity and downcast only to its own
registered query type. Built-in payloads are registered through this same path while preserving
their existing digital, indexed-word, marker, and value storage representations behind their
adapters.

`CollectedLaneQuery` supplies an immutable snapshot only when its payload has waveform
semantics. The request is bounded by a visible time window and item limit. The viewer passes the
returned `OpaqueCollectedLaneSnapshot` to the payload's renderer only after it has released
retained-data locks; the renderer downcasts the snapshot only to its own registered type. An
opaque lane is sufficient to activate an explicitly registered viewer row, so a payload does not
need a parallel `DerivedLaneData` entry merely to be visualized.

`DerivedLaneData`, `CollectedDataKind`, and `LaneBuffer` remain the implementation of the
built-in fallback lanes. They are not part of the collected-payload contract and are being
replaced incrementally by adapter-owned query and snapshot storage.

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
occurs after the retained-lane lock is released.

Table projection is optional adapter metadata. `CollectedLaneQuery::table_metadata` supplies a
revision and row count for cache invalidation, while `table_snapshot(max_rows)` supplies bounded
scalar rows with a format hint and a completeness flag. The decoder-table panel consumes this
contract through opaque lane handles and never reads a concrete payload's retained storage. An
adapter may expose rows and columns when that is meaningful; no table-specific behavior is
required for arbitrary payloads.

Extra panels are UI-owned plugin registrations, not graph or processing registrations. A panel
receives a restricted read-only context containing collected-lane descriptors and query handles,
plus an explicit application-command boundary. It keeps versioned serializable state under its own
stable panel identifier. A panel can be opened after a capture finishes because it queries retained
data rather than receiving the live stream.

### Crate ownership

- `signal_processing` owns type-erased ingestion, retained query, snapshot, and storage contracts.
- `logic_analyzer_viewer` owns presentation adapters and drawing contracts for those snapshots.
- `logic_analyzer_graph` owns compiler negotiation: it accepts only registered collectable payloads
  for a data subscription and reports a targeted error for an unavailable adapter.
- `logic_analyzer_ui` owns panel factories, panel state, and application command capabilities.
- The application-level plugin registration entry point composes the lower-level registries without
  making `signal_processing` depend on UI crates.

### Migration path

1. Move the remaining built-in `CollectedDataKind`/`LaneBuffer` implementation behind the
   adapters and remove the collector's closed lane representation.
2. Replace `DerivedLaneData` with adapter-owned query handles and snapshots.
3. Make data-subscription negotiation registry-driven instead of listing built-in `PortKind`s.
4. Register built-in waveform and table adapters through the same mechanism as plugins.
5. Add UI panel registration and an end-to-end plugin payload such as `CameraFrame`.

Compile-time Rust plugins are the initial extension mechanism. Runtime-loaded native plugins need
an additional ABI-stable boundary and are outside this design phase.
