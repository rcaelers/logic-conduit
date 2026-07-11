# Node Graph Editor — Design

Design of the `node-graph` crate ([crates/node_graph](../crates/node_graph)): a reusable,
Blender-style node editor widget for egui. The crate is UI-generic — it knows nothing about
logic analyzers or pipelines; the application (see [APP_DESIGN.md](APP_DESIGN.md)) defines the
node types and compiles the drawn graph into something executable.

For the widget's public API and how to define node types, see
[NODE_GRAPH_API.md](NODE_GRAPH_API.md).

---

## Layering

```text
crates/node_graph/src
├── model/     Serializable document: GraphState, Node, Socket, Connection, Frame
├── api/       Node-type definition API: NodeDef, InputDef/OutputDef, PropDef,
│              SocketDef, InlineControl, builtin socket/value types
├── runtime/   NodeTypeRegistry + type-erased per-node instances (TypedNode)
├── widget/    NodeGraphWidget: rendering, interaction, menus, panel, minimap
└── support/   View transform (pan/zoom), paint helpers
```

The dependency direction is strict: `model` depends on nothing, `api` produces `model`
sockets, `runtime` erases `api` defs into instances, `widget` orchestrates all three.

### Model vs. runtime split

`GraphState` is the *document*: plain serde-serializable data (nodes, sockets, connections,
frames) with no trait objects. Everything needed for rendering and compatibility checking
lives in the model, so a saved file round-trips without consulting any registry.

Each node additionally has a **runtime instance** (`Box<dyn NodeInstance>`, kept in a side
map on the widget): the typed node state plus its def's behavior (`on_update`, controls,
badge). Instances are rebuilt from the registry whenever a graph is loaded, undone, or
pasted (`restore_node`); the model's `node.state: serde_json::Value` is the single durable
representation of node state.

## The definition API

A node type is a `NodeDef` implementation: an associated serde `State` type plus static
descriptions of sockets, inline props, panel sections, and two hooks:

- `on_update(state, inputs, outputs)` — runs after any state edit or connect/disconnect;
  mutates socket visibility/styling (dynamic sockets).
- `badge(state)` — recomputed after every update; a validation/status message drawn under
  the node (`NodeBadge` with info/warning/error severity).

Socket types are `SocketDef` implementations (name, color, shape); `SocketWithControlDef`
additionally binds a value type that renders as an inline control while the socket is
unconnected. Builtins: `Bool`, `Int`, `Float`, `Str`, `File`, `Any`, with value types
(`IntValue`, `FloatValue`, `BoolValue`, `StringValue`, `FileValue`, `EnumValue`).
`EnumValue` persists by variant *name*, not index, so save files survive variant reorders.
`FileValue` can open a native save dialog (used by writer nodes).

`NodeTypeRegistry::register::<T>()` records the def and auto-collects every socket type it
mentions (inputs, outputs, `.accepts::<T>()`) into a **type identity table**
(`socket_types: name → (color, shape)`; first registration wins). That table is what
re-skins resolved sockets and wires; compatibility checking never consults it.

## Socket type system

Connection validity is decided **per node, not per type** — there is no global cast table.
Each input declares which types it accepts beyond its native one
(`InputDef::new::<Signal>("Threshold").accepts::<Float>()`); the node's own processing is
responsible for handling any accepted type.

All data lives in the serialized `Socket`:

```rust
pub struct Socket {
    pub name: String,
    pub type_name: String,             // native type
    pub color: Color32,                // idle look (def-controlled)
    pub shape: SocketShape,            // idle look
    pub allowed: Vec<String>,          // extra accepted type names
    pub resolved_type: Option<String>, // set while connected to a non-native type
    pub def_index: usize,              // which Input/OutputDef this socket came from
    pub variadic: Option<VariadicInfo>,
    pub visible: bool, pub hidden: bool, pub has_control: bool,
}
```

Key rules:

- **Compatibility**: `compatible(out_type, input) = out_type == "Any" || input.type_name ==
  "Any" || out_type == input.type_name || input.allowed.contains(out_type)` — see
  `Socket::accepts`. Checked at wire completion and drag snapping.
- **Resolution on connect**: connecting a non-native (but accepted) output type sets the
  input's `resolved_type`. Rendering then takes color/shape from the identity table, so the
  wire reads as one type end to end; disconnect clears it and the socket reverts to its
  idle look. `Socket::effective_type()` = `resolved_type` if set, else `type_name`.
- **Input-driven resolution only.** Outputs keep their concrete type; the only polymorphic
  output is the reroute node's `Any`.
- **No runtime adapter splicing.** The graph→pipeline compiler creates channels with the
  *resolved* type; the consuming node sets up the matching consumption path itself.

### Variadic (growing) input groups

`InputDef::variadic(max)` turns one def into a growing group: the group renders as N member
sockets ("D 1", "D 2", …) plus one trailing placeholder while members < max. Connecting to
the placeholder converts it into a member and spawns a new placeholder; disconnecting a
member removes it. Mechanics:

- `SocketId`/`Connection` are positional, so inserting/removing a socket rewrites the index
  of every stored connection above the change point (`GraphState` insert/remove helpers fix
  up `connections` atomically).
- `def_index` decouples sockets from def positions — required because controls and restore
  logic would otherwise zip sockets with defs by index.
- Variadic sockets carry no inline controls; placeholders are skipped by the compiler.

### Restore reconciliation

`restore_node` validates saved sockets structurally against the current defs (per
`def_index`: static defs 1:1, variadic defs any member count ≤ max with exactly one
placeholder unless full). A match keeps the saved sockets with per-def data refreshed
(accept lists, control presence, stale `resolved_type` cleared); any mismatch rebuilds the
sockets from the defs. Pre-variadic files (all `def_index == 0`) are upgraded positionally.

## Widget

`NodeGraphWidget` owns the graph, the runtime instances, the registry, and all interaction
state. `show(ui)` runs once per frame: build layout → allocate per-node/per-socket
responses → route input (hotkeys, menus, wire drags, selection, pan/zoom) → draw
(connections, nodes, frames, badges, minimap, panel) — a single immediate-mode pass with no
retained scene graph.

Interaction highlights:

- **Wire dragging** with live compatibility checking and snap-to-socket; a snap candidate
  previews the shape it would resolve to. Fast-render mode suppresses per-socket hit
  targets during heavy drags.
- **Reroute nodes** (`NodeKind::Reroute`) are model-level wire waypoints with a single
  `Any` in/out; the compiler follows wires through them. *Dissolve* removes a node and
  directly reconnects compatible in/out pairs.
- **Frames** group nodes visually (label, color, rename-in-place, membership editing);
  frames with no members are cleaned up automatically.
- **Node presentation**: collapse (header only) and hide-unconnected-sockets toggles;
  per-node title rename; selection (click, box select, shift-add).
- **Menus**: right-click context menu (add, cut/copy/paste, duplicate, delete, dissolve,
  frame ops, show/hide, undo/redo) and an `A` add-search popup with fuzzy matching over
  `category → name`.
- **Clipboard** is the system clipboard: selected nodes + their internal connections
  serialize to a JSON payload tagged `node_graph_clipboard_v1`, so copy/paste works across
  application instances. Paste remaps ids, offsets positions, selects the pasted set, and
  prunes `resolved_type` on inputs whose producer wasn't copied.
- **Undo/redo** snapshot the whole `GraphState` (cheap: plain data). Because sockets —
  including resolution and variadic growth — live in the model, everything undoes for free.
  Node state is synced from instances into the model before every snapshot.
- **Minimap** (toggle `M`): scaled-down node rectangles + viewport indicator, click/drag to
  navigate.

### Properties panel

A Blender-style N-panel docked to the right edge, rendered in **screen space** (regular
egui widgets, unaffected by graph zoom — which is what makes rich controls like channel
grids practical; inline node controls stay zoom-scaled). It shows the *active* node — the
most recently clicked/added one. A built-in *Node* section exposes rename and type/category
info; the def contributes `PanelSection`s of `PropDef`s. Panel edits mutate the same node
state and run through the same `on_update` path as inline controls, so visibility,
clamping, and badges react identically. A persistent tab strip on the right edge toggles
the panel (`N`); the panel body floats over the graph and claims pointer input only within
its bounds.

### External badges and statuses

The embedding application can attach its own per-node annotations, kept separate from
def-driven badges so neither clobbers the other:

- `set_node_badge(id, Option<NodeBadge>)` — compile errors, runtime warnings; takes
  precedence over the def's badge while present.
- `set_node_status(id, Option<String>)` — short live text in the node header (e.g. item
  counters while a pipeline runs).

## Persistence

The document is `GraphState` as pretty-printed JSON (`save_to_path` / `load_from_path`).
`save` first syncs every instance's state into the model. `load` parses, replaces the
graph, and rebuilds all runtime instances through the restore reconciliation above — a load
is exactly the programmatic `set_graph` path. New model fields use `#[serde(default)]` so
older files keep loading.
