# Graph Crate Responsibility Split

## Current boundary

`logic_analyzer_graph_api` owns the compile-time plugin contract through its `node` and
`node_support` namespaces. Graph-node and collected-payload inventory types are defined there;
inventory assembly is compiler-owned and consumes submissions without importing a node bundle.

`logic_analyzer_graph_nodes` owns the built-in node definitions, builders, migrations, socket
types, payload presentations, and inventory submissions. `logic_analyzer_graph` owns compiler and
host services. Its feature-gated `GraphCompiler` test constructors are the narrow integration seam
for isolated built-in-node tests.
`logic_analyzer_capture_export` owns native streaming capture export without depending on a graph
crate. `logic_analyzer_test_support` owns deterministic capture providers shared by cross-crate
tests. Native and web application composition link the built-in bundle before constructing the host
compiler.

## Architecture

The graph domain is divided into crates whose dependency edges follow the direction of their
contracts:

```text
node_graph                 signal_processing
    ^                            ^
    |                            |
    +---- logic_analyzer_graph_api ---- logic_analyzer_viewer
                    ^
          +---------+----------+
          |                    |
logic_analyzer_graph   logic_analyzer_graph_nodes
          ^                    ^
          |                    |
 logic_analyzer_ui          plugins

logic_analyzer_capture_export ---> signal_processing
logic_analyzer_test_support  ---> signal_processing
```

`logic_analyzer_graph` and `logic_analyzer_graph_nodes` both depend on
`logic_analyzer_graph_api`. The compiler crate never depends on the built-in-node crate. A plugin
depends on the API crate and the lower-level domains required by its own implementation; it does
not depend on the compiler or application UI.

### Graph API crate

`logic_analyzer_graph_api` is the supported compile-time extension contract. It has two public,
directory-backed namespaces and no application-host operations.

`node` contains contracts implemented or submitted by a graph-node feature:

- `RuntimeBuilder`;
- `GraphNodeRegistration`;
- `CollectedPayloadRegistration`;
- `LiveCaptureFeature`;
- `CaptureGraphSourceFactory`.

`node_support` contains data and restricted services supplied to those implementations:

- `PortKind`, `PortValue`, `ResolvedInput`, and `ResolvedInputs`;
- `NodeBuildContext`;
- state decoding at the node-owned error boundary;
- capture identity and presentation descriptions;
- default waveform and decoder-table column presentations;
- sampling overlay and qualifier descriptions;
- trigger configuration, simple-trigger channels, and live-capture edits.

The two namespaces are not convenience aliases. An implementer imports traits from `node` and
supporting values from `node_support`. The API crate does not re-export either namespace at its
root.

### Node build context

`NodeBuildContext` is the narrow service contract passed to `RuntimeBuilder`. It replaces
`CompileCtx` in every plugin-visible signature. It exposes only operations required while a
concrete node is materialized, including derived-lane access, retention and persistent-cache
configuration, waveform/table presentation registration, and runtime sampling activity lookup.

The compiler owns the concrete context state and implements `NodeBuildContext`. Host-only result
operations, such as taking resolved sampling candidates or publishing the final presentation
registries, remain on `CompileCtx`, which is exposed only through the compiler's `host` namespace.
A plugin cannot receive or import that concrete context through the graph-node API.

### Graph compiler and host facade

`logic_analyzer_graph` owns graph lowering, validation, discovery, execution, cache planning, and
saved-graph synchronization. Its only public namespace is `host`, consumed by `logic_analyzer_ui`,
native and web composition, headless hosts, and integration tests. Graph-node contracts are
imported directly from `logic_analyzer_graph_api` rather than forwarded through the compiler crate.

`GraphCompiler` owns the inventory-derived builder registry and provides these application-facing
operations:

- lower and validate a graph;
- discover capture and trigger features;
- apply a feature edit through the owning node;
- resolve sampling overlays and cache plans;
- synchronize saved payload subscriptions;
- start or update an application run and live analysis.

Compiler result types belong to `host`: `CompiledGraph`, `CompiledNode`, `CompiledEdge`,
`CompileError`, `ApplyError`, `LiveRun`, discovered feature wrappers, compatibility warnings, and
resolved sampling candidates. The crate root does not flatten the host facade.

Node-supplied descriptions and host-resolved results remain distinct:

| Node API description | Host result |
| --- | --- |
| `SamplingOverlayDescriptor` | `SamplingOverlayCandidate` |
| `TriggerConfigurationFeature` | `DiscoveredTriggerConfiguration` |
| `CapturePresentation` | `DiscoveredCapturePresentation` |
| `DecoderTableColumnPresentation` | `DecoderTableSource` |
| `RuntimeBuilder` | `CompiledNode` |
| `LiveCaptureFeature` | `DiscoveredLiveCaptureFeature` |

### Built-in graph nodes

`logic_analyzer_graph_nodes` contains only built-in graph-node features and their atomic payload
capabilities. Each node directory owns its definition, state, migration, builder, presentation
metadata, inventory submission, and isolated test. Concrete node symbols do not leave their
directory facade.

Built-in socket types and built-in collected-payload presentations live with the built-in node
bundle. The bundle submits registrations defined by `logic_analyzer_graph_api`; it does not call
compiler registration functions. The application references a small linker anchor from every
enabled built-in or plugin crate before inventory is read on native and wasm.

### Capture export

`logic_analyzer_capture_export` owns streaming export of finalized capture storage. It depends on
`signal_processing` capture contracts and format libraries, not on graph compilation or graph
nodes. Native UI services call its explicit exporter interface. Unsupported targets exclude the
complete exporter implementation at the crate boundary.

### Test support

`logic_analyzer_test_support` owns deterministic acquisition providers used across crate
boundaries. It depends only on generic `signal_processing` capture contracts. Processing
conformance tests, compiler tests, built-in test nodes, and UI integration tests consume this owner
directly. Node-isolation mocks remain private to the built-in graph-node crate unless another crate
has a documented need for them.

### Inventory and composition

The inventory collection types are declared by `logic_analyzer_graph_api`. Built-in nodes and
plugins submit registrations there. `logic_analyzer_graph::host::GraphCompiler` reads those
registrations without importing any submitter.

Every enabled submission crate exposes an idempotent `link()` anchor. Native and web application
composition references those anchors before constructing `GraphCompiler`. The anchor exists only
to make linker retention explicit; registration remains inventory-driven and contains no manual
per-node list.

### Compatibility and saved graphs

Moving Rust symbols does not change stable graph-node IDs, payload IDs, builder names, serialized
node definitions, or namespaced graph extensions. Compatibility remains owned by each concrete
node migration. Generic API/compiler crates never translate concrete node names or state.

### Enforcement

Architecture checks enforce these dependency rules:

- graph API does not depend on the compiler, built-in nodes, processing implementations, UI, or
  capture export;
- compiler does not depend on built-in nodes, concrete processing nodes, UI, or export formats;
- built-in graph nodes and plugins do not depend on the compiler `host` namespace;
- UI does not import built-in node implementation paths or `logic_analyzer_processing` concrete
  nodes;
- capture export does not depend on graph crates;
- concrete graph-node facades do not re-export implementation symbols.

Native and wasm builds exercise the same inventory and public API surfaces. Target selection stays
at whole implementation-module and linker-composition boundaries.
