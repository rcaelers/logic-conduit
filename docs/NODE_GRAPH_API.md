# Node Graph Widget — API

How to embed and extend the `node-graph` crate ([crates/widgets/node_graph](../crates/widgets/node_graph)).
For the internal architecture see [NODE_GRAPH_DESIGN.md](NODE_GRAPH_DESIGN.md).

Public surface (crate root re-exports):

```rust
pub use api::{
    AnySocket, BoolSocket, BoolValue, EnumValue, FileSocket, FileValue, FloatSocket,
    FloatValue, InlineControl, InputDef, IntSocket, IntValue, NodeDef, OutputDef,
    PanelSection, PropDef, SocketDef, SocketTypeIdentity, SocketWithControlDef,
    StrSocket, StringValue,
};
pub use model::{
    BadgeSeverity, Connection, Frame, FrameId, GraphState, Node, NodeBadge, NodeId,
    NodeKind, Socket, SocketDirection, SocketId, SocketShape, VariadicInfo,
};
pub use runtime::{NodeTypeRegistry, SocketTypeStyle};
pub use widget::NodeGraphWidget;
```

---

## Getting started

```rust
use node_graph::{NodeGraphWidget, NodeTypeRegistry};

let mut registry = NodeTypeRegistry::new();
registry.register::<MyNode>();          // one call per NodeDef type
let mut widget = NodeGraphWidget::new(registry);

// per frame, inside your egui layout:
widget.show(ui);                        // fills the available rect
```

The widget is self-contained: it owns the graph document, undo/redo, clipboard,
interaction, context menus, minimap, and the properties panel.

## Defining a socket type

A socket type gives wires and sockets an identity (name, color, shape). Implement
`SocketDef` once per type; the registry auto-collects identities from every node def that
mentions the type (first registration wins), so the type looks identical graph-wide.

```rust
use node_graph::{SocketDef, SocketShape};

pub struct Words;
impl SocketDef for Words {
    type Value = u64;                                // carried value (host semantics)
    fn type_name() -> &'static str { "Words" }
    fn color() -> Color32 { Color32::from_rgb(215, 140, 60) }
    fn shape() -> SocketShape { SocketShape::Diamond } // default: Circle
}
```

`SocketWithControlDef` additionally binds a control type so an unconnected input can be
edited inline:

```rust
pub trait SocketWithControlDef: SocketDef {
    type Control: InlineControl;
}
```

Builtins: `BoolSocket`, `IntSocket`, `FloatSocket`, `StrSocket`, `FileSocket` (all with
controls) and `AnySocket` (the wildcard: compatible with everything; used by reroutes).
Their value types — `BoolValue`, `IntValue` (with optional range), `FloatValue` (range +
drag speed), `StringValue`, `FileValue` (optionally a save dialog with title and filters),
`EnumValue` (variant list; **persists by variant name**, so saved files survive reorders) —
are serde types you embed directly in node state.

Custom controls implement `InlineControl`:

```rust
pub trait InlineControl: Send + Sync + fmt::Debug {
    /// Draw into `rect` at the graph `zoom`; return true when the value changed.
    fn draw_widget(&mut self, ui: &mut Ui, label: &str, rect: Rect, zoom: f32,
                   clip_rect: Rect) -> bool;
}
```

## Defining a node type

A node type is a `NodeDef`: a serde `State` plus static socket/prop descriptions.

```rust
use node_graph::{InputDef, IntValue, NodeDef, OutputDef, PanelSection, PropDef};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CounterState { pub start: IntValue, pub step: IntValue }

pub struct Counter;
impl NodeDef for Counter {
    type State = CounterState;

    fn name() -> &'static str { "Counter" }       // unique; menu + serialization key
    fn category() -> &'static str { "Logic" }     // add-menu grouping
    fn color() -> Color32 { COLOR_LOGIC }         // header color

    fn inputs() -> Vec<InputDef<Self::State>> {
        vec![InputDef::new::<Trigger>("Trigger")]
    }
    fn outputs() -> Vec<OutputDef<Self::State>> {
        vec![OutputDef::new::<Number>("Count")]
    }
    fn state() -> Self::State {
        CounterState { start: IntValue::plain(0), step: IntValue::plain(1) }
    }
    fn panel() -> Vec<PanelSection<Self::State>> {
        vec![PanelSection::new("Options", vec![
            PropDef::control("start", "Start", |s| &mut s.start),
            PropDef::control("step",  "Step",  |s| &mut s.step),
        ])]
    }
}
```

### `InputDef` / `OutputDef` builders

| Constructor / method | Effect |
|---|---|
| `InputDef::new::<T>(label)` | Plain input of socket type `T` |
| `InputDef::control::<T>(label, accessor)` | Input whose unconnected state renders `T::Control` inline, bound to a state field |
| `.accepts::<U>()` | This input also accepts `U` — the node handles it itself (e.g. a constant on a stream input). Connecting a `U` sets the socket's `resolved_type`, restyling socket + wire to `U`'s identity |
| `.idle_style(color, shape)` | Override the unconnected look (resolved look always comes from the connected type) |
| `.variadic(max)` | Growing group: members "{label} 1…N" plus a trailing placeholder; connecting the placeholder adds a member. No inline controls |
| `OutputDef::new::<T>(label)` / `::control::<T>(…)` | Same for outputs (outputs never resolve; they keep their concrete type) |

### Props and the panel

- `props()` → `Vec<PropDef>` render in the **node body** (zoom-scaled, keep them few).
- `panel()` → `Vec<PanelSection>` render in the right-docked **properties panel**
  (screen-space, full-size widgets) when the node is active. `PropDef::panel_height(h)`
  requests a taller row (e.g. a channel grid).
- Both mutate the same `State` and trigger the same update path.

### Hooks

```rust
fn on_update(state: &mut State, inputs: &mut [Socket], outputs: &mut [Socket]) {}
fn badge(state: &State) -> Option<NodeBadge> { None }
```

`on_update` runs after every state edit, connect, disconnect, and restore — use it for
dynamic socket visibility (`socket.visible`), restyling (`color`/`shape`), and clamping
interdependent props. `badge` is recomputed after each update; return
`NodeBadge::info/warning/error(text)` to draw a status line under the node.

## `NodeGraphWidget` reference

| Method | Purpose |
|---|---|
| `new(registry)` | Create with a populated `NodeTypeRegistry` |
| `show(ui)` | Render + handle one frame in `ui`'s available rect |
| `graph()` / `graph_mut()` | Access the `GraphState` document (e.g. for compilation) |
| `add_node_at(name, pos) -> Option<NodeId>` | Programmatic add (`"Reroute"` adds a reroute) |
| `set_node_state(id, json) -> bool` | Replace a node's state and re-run its def (sockets, visibility, badge) |
| `set_graph(graph)` | Replace the document; rebuilds all runtime instances via restore reconciliation |
| `save_to_path(path)` / `load_from_path(path)` | JSON persistence (load leaves the current graph untouched on error) |
| `set_node_badge(id, Option<NodeBadge>)` | Externally owned badge (compile errors, runtime status); takes precedence over the def badge |
| `set_node_status(id, Option<String>)` / `clear_node_statuses()` | Short live text in node headers (e.g. item counters) |

`NodeTypeRegistry`: `new()`, `register::<T: NodeDef>()` (chainable), `category_of(name)`,
`socket_type_style(name) -> Option<SocketTypeStyle>`.

## Model types

`GraphState { nodes, connections, frames }` is plain serde data — safe to inspect and
(carefully) mutate. Useful pieces:

- `Node`: `id`, `title`, `pos`, `state: serde_json::Value`, `inputs`/`outputs:
  Vec<Socket>`, `kind` (`Regular`/`Reroute`), `collapsed`, `selected`, `badge`.
- `Socket`: `effective_type()` (resolved-or-native type name), `accepts(type_name)`
  (the compatibility rule), `is_variadic_member()`, `is_variadic_placeholder()`.
- `Connection { from: SocketId, to: SocketId }`; `SocketId { node, direction, index }` is
  positional per node side.
- `GraphState::is_input_connected(socket_id)`, `sorted_node_ids()`.

## Built-in interaction (for reference)

Right-click opens the context menu (add search, cut/copy/paste, duplicate, delete,
dissolve, frame operations, show/hide sockets, collapse, undo/redo). Keyboard: `A` add
search at pointer · `⌘X/C/V` cut/copy/paste (system clipboard, JSON payload) · `⇧D`
duplicate · `Delete`/`Backspace`/`X` delete · `⌘Z`/`⇧⌘Z` undo/redo · `⌘J` join in frame ·
`⌘H` hide unconnected sockets · `M` minimap · `N` properties panel · `Esc` cancel.
Hotkeys are suppressed while any widget holds keyboard focus.
