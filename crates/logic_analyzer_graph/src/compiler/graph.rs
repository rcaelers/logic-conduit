//! Graph → Pipeline compiler (`docs/APP_DESIGN.md`).
//!
//! Two stages: `lower()` turns the UI graph into a pure, diffable
//! `CompiledGraph` IR (prune to sink-reachable nodes, follow reroutes,
//! validate, negotiate per-edge stream kinds); `start_live()` materializes
//! it into a running [`LiveRun`], the supervisor-driven live path used
//! by both the app and its own tests — nothing builds an offline `Pipeline`
//! from this IR anymore; that's what `examples/*.rs` do directly against
//! `signal_processing::Pipeline` for headless/scripted captures.
//!
//! Kind negotiation: each edge picks `offered ∩ accepted`, producer
//! preference order winning. That is what maps one UI `Signal` socket onto
//! the source's dual `d{i}`/`b{i}` ports; every `Words` socket carries the
//! same `Word` runtime type regardless of which decoder produced it.

use std::collections::{BTreeSet, HashMap, HashSet};

use egui::{Color32, Pos2};
use serde_json::Value;

use logic_analyzer_viewer::{ViewerLaneRegistry, ViewerOutputPresentation};
use node_graph::{
    Connection, GraphState, Node, NodeId, NodeKind, Socket, SocketDirection, SocketId, SocketShape,
    VariadicInfo,
};
use signal_processing::{
    AppManager, DerivedLanes, DisconnectEvent, InputSub, NodeConfig, OverflowPolicy,
    PersistentStoreConfig, ProcessNode, SampleBlock, ViewerRetention,
};

use super::cache_platform;
use super::errors::{ApplyError, CompileError};
use super::port_kind::PortKind;

/// Shared resources handed to builders. A fresh `DerivedLanes` store per
/// run makes stale viewer lanes vanish atomically on re-run.
#[derive(Default)]
pub struct CompileCtx {
    pub derived_lanes: DerivedLanes,
    pub viewer_lanes: ViewerLaneRegistry,
    /// Storage policy selected by the graph's source. Finite sources retain
    /// their complete timeline; continuous sources can explicitly choose a
    /// bounded rolling window.
    pub viewer_retention: ViewerRetention,
    pub viewer_word_caches: Vec<Option<PersistentStoreConfig>>,
    pub persistent_cache_directory: Option<std::path::PathBuf>,
}

/// What one input edge settled on: the negotiated stream kind plus a
/// human-readable producer label (`"{node title}.{socket}"`, used for
/// viewer lane names).
#[derive(Debug, Clone)]
pub struct ResolvedInput {
    pub kind: PortKind,
    pub source: String,
    pub source_node: NodeId,
    pub source_node_title: String,
    pub word_display_format: Option<String>,
    pub viewer_presentation: Option<ViewerOutputPresentation>,
}

/// Per input socket, keyed `(def_index, member_index)`. Keys are
/// def-relative so variadic growth does not shift them.
#[derive(Debug, Clone, Default)]
pub struct ResolvedInputs(HashMap<(usize, usize), ResolvedInput>);

impl ResolvedInputs {
    pub fn kind(&self, def_index: usize) -> Option<PortKind> {
        self.0.get(&(def_index, 0)).map(|input| input.kind)
    }
    pub fn member_count(&self, def_index: usize) -> usize {
        self.0.keys().filter(|(def, _)| *def == def_index).count()
    }
    /// Members of a variadic group in port order.
    pub fn members(&self, def_index: usize) -> Vec<(usize, &ResolvedInput)> {
        let mut members: Vec<(usize, &ResolvedInput)> = self
            .0
            .iter()
            .filter(|((def, _), _)| *def == def_index)
            .map(|((_, member), input)| (*member, input))
            .collect();
        members.sort_by_key(|(member, _)| *member);
        members
    }
}

// ── Builder trait & registry ─────────────────────────────────────────────────

pub trait RuntimeBuilder {
    /// Produces the graph's time domain (exactly one per graph).
    fn is_source(&self) -> bool {
        false
    }
    /// Retention policy for exact viewer entries in this source's time
    /// domain. Summaries remain complete under bounded retention.
    fn viewer_retention(&self, _state: &Value) -> ViewerRetention {
        ViewerRetention::Unlimited
    }
    /// Terminal consumer; pruning keeps only nodes reachable from sinks.
    fn is_sink(&self) -> bool {
        false
    }
    /// Kinds this input socket can consume, in no particular order.
    fn accepted_kinds(&self, socket: &Socket, state: &Value) -> Vec<PortKind>;
    /// Kinds this output socket can produce, in preference order.
    fn offered_kinds(&self, socket: &Socket, state: &Value) -> Vec<PortKind>;
    /// Runtime port name once the edge kind is fixed. `member_index` numbers
    /// variadic group members (D 1 → 0, D 2 → 1, …).
    fn input_port(
        &self,
        socket: &Socket,
        member_index: usize,
        state: &Value,
        kind: PortKind,
    ) -> Option<String>;
    fn output_port(&self, socket: &Socket, state: &Value, kind: PortKind) -> Option<String>;
    /// Optional display metadata for a decoded-word output. Kept generic so
    /// the compiler never needs to identify a concrete decoder.
    fn word_display_format(&self, _socket: &Socket, _state: &Value) -> Option<String> {
        None
    }
    /// Optional protocol-neutral presentation contract for this output when
    /// it is connected to a Viewer. Generic lowering carries the value
    /// opaquely; concrete producer builders own its semantics.
    fn viewer_output_presentation(
        &self,
        _socket: &Socket,
        _state: &Value,
    ) -> Option<ViewerOutputPresentation> {
        None
    }
    /// Whether an unconnected input is a compile error (given the state:
    /// e.g. CS is only required while its polarity isn't Disabled).
    fn input_required(&self, _socket: &Socket, _state: &Value) -> bool {
        true
    }
    /// Overrides the policy-table buffer size (`docs/APP_DESIGN.md`) for this input's
    /// incoming edge. `None` (default, every built-in node) keeps today's
    /// `PortKind`-based sizing. Only a node whose buffer size is a
    /// user-visible property (the `Buffer` node) needs this.
    fn input_buffer_override(&self, _socket: &Socket, _state: &Value) -> Option<usize> {
        None
    }
    /// Instantiate the runtime node. `name` is the pipeline node name (used
    /// for thread naming/logs); `resolved` carries each input's kind so
    /// polymorphic consumers pick the matching concrete type.
    fn build(
        &self,
        name: &str,
        state: &Value,
        resolved: &ResolvedInputs,
        ctx: &mut CompileCtx,
    ) -> Result<Box<dyn ProcessNode>, String>;

    /// Runtime configuration for a *hot* state change, if this node type can
    /// apply the whole state without restarting (a hot prop change).
    /// `None` (default) means a state change restarts the node in place.
    fn hot_config(&self, _state: &Value) -> Option<NodeConfig> {
        None
    }
}

pub struct BuilderRegistry(HashMap<String, Box<dyn RuntimeBuilder>>);

impl BuilderRegistry {
    pub fn standard() -> Self {
        Self(crate::nodes::standard_builders())
    }

    /// Adds (or overwrites) one builder, keyed the same way `standard()`
    /// keys its own entries — the string must match the corresponding
    /// `NodeDef::name()`. Lets a plugin crate extend the registry `standard()`
    /// builds, without touching `standard()` itself.
    pub fn insert(
        &mut self,
        name: impl Into<String>,
        builder: Box<dyn RuntimeBuilder>,
    ) -> &mut Self {
        self.0.insert(name.into(), builder);
        self
    }

    pub(super) fn get(&self, def_name: &str) -> Option<&dyn RuntimeBuilder> {
        self.0.get(def_name).map(|b| b.as_ref())
    }
}

pub(crate) fn parse_state<T: serde::de::DeserializeOwned>(state: &Value) -> Result<T, String> {
    serde_json::from_value(state.clone()).map_err(|e| format!("invalid node state: {e}"))
}

pub(crate) fn parse_hex(text: &str) -> Result<u64, String> {
    let trimmed = text.trim();
    let digits = trimmed
        .strip_prefix("0x")
        .or_else(|| trimmed.strip_prefix("0X"))
        .unwrap_or(trimmed);
    u64::from_str_radix(digits, 16).map_err(|_| format!("'{text}' is not a hex value"))
}

// ── IR ───────────────────────────────────────────────────────────────────────

/// Pure description — no threads, no channels. Cheap to rebuild on every
/// edit and cheap to diff (live reconfiguration).
#[derive(Debug, Clone, Default)]
pub struct CompiledGraph {
    pub nodes: Vec<CompiledNode>,
    pub edges: Vec<CompiledEdge>,
    pub viewer_retention: ViewerRetention,
}

#[derive(Debug, Clone)]
pub struct CompiledNode {
    pub id: NodeId,
    /// `BuilderRegistry` key (the UI def name).
    pub builder: String,
    pub state: Value,
    /// Pipeline node name: `n{id}_{title_slug}`.
    pub runtime_name: String,
    pub resolved: ResolvedInputs,
    pub(super) viewer_word_caches: Vec<Option<PersistentStoreConfig>>,
}

#[derive(Debug, Clone)]
pub struct CompiledEdge {
    pub from: (NodeId, String),
    pub to: (NodeId, String),
    pub buffer: usize,
    pub kind: PortKind,
}

fn runtime_name(node: &Node) -> String {
    let slug: String = node
        .title
        .to_lowercase()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect();
    format!("n{}_{}", node.id.0, slug.trim_matches('_'))
}

// ── Stage 1: lower ───────────────────────────────────────────────────────────

/// A UI wire with reroutes and muted nodes collapsed away: both endpoints on
/// live (non-reroute, non-muted) regular nodes.
struct Wire {
    from: SocketId,
    to: SocketId,
}

enum WireSource {
    Found(SocketId),
    /// Upstream is missing entirely (e.g. an unplugged reroute) — silently
    /// drop the wire, matching pre-existing reroute behavior.
    Dangling,
    /// A muted node's output has no viable pass-through: either its own
    /// sockets have no type-compatible input at all (a type-transforming
    /// node like a decoder, or a source with nothing to pass through in the
    /// first place), or the one that would match isn't connected.
    MutedBlocked {
        output: SocketId,
    },
}

/// Chases `from` back through any run of Reroute and muted nodes to the
/// effective producing socket. At a muted hop, follows
/// `Node::mute_pass_through_pairs` — the node's own declared input/output
/// type pairing, independent of whatever is wired downstream (mirrors
/// Blender: a muted node only usefully bypasses through a same-typed
/// input/output pair; a type-transforming node has none, so its output has
/// nothing to splice to and just drops).
fn resolve_wire_source(graph: &GraphState, from: SocketId, hops: &mut usize) -> WireSource {
    *hops += 1;
    if *hops > graph.connections.len() + graph.nodes.len() + 1 {
        return WireSource::Dangling; // cycle guard
    }
    let Some(node) = graph.nodes.get(&from.node) else {
        return WireSource::Dangling;
    };
    if node.kind == NodeKind::Reroute {
        return match graph.connections.iter().find(|c| c.to.node == from.node) {
            Some(upstream) => resolve_wire_source(graph, upstream.from, hops),
            None => WireSource::Dangling,
        };
    }
    if node.muted {
        let Some(&(_, in_idx)) = node
            .mute_pass_through_pairs()
            .iter()
            .find(|(out_idx, _)| *out_idx == from.index)
        else {
            return WireSource::MutedBlocked { output: from };
        };
        let paired_input = SocketId {
            node: from.node,
            index: in_idx,
            direction: SocketDirection::Input,
        };
        return match graph.connections.iter().find(|c| c.to == paired_input) {
            Some(upstream) => resolve_wire_source(graph, upstream.from, hops),
            None => WireSource::MutedBlocked { output: from },
        };
    }
    WireSource::Found(from)
}

fn resolve_reroute_edges(graph: &GraphState) -> (Vec<Wire>, Vec<CompileError>) {
    let mut wires = Vec::new();
    let mut errors = Vec::new();
    let mut blocked: HashSet<SocketId> = HashSet::new();
    for connection in &graph.connections {
        let Some(to_node) = graph.nodes.get(&connection.to.node) else {
            continue;
        };
        if to_node.kind == NodeKind::Reroute || to_node.muted {
            // Handled when the wire *leaving* it is resolved.
            continue;
        }
        let mut hops = 0usize;
        match resolve_wire_source(graph, connection.from, &mut hops) {
            WireSource::Found(from) => wires.push(Wire {
                from,
                to: connection.to,
            }),
            WireSource::Dangling => {}
            WireSource::MutedBlocked { output } => {
                if blocked.insert(output) {
                    let output_name = graph
                        .nodes
                        .get(&output.node)
                        .and_then(|n| n.outputs.get(output.index))
                        .map(|s| s.name.as_str())
                        .unwrap_or("?");
                    let to_label = graph
                        .nodes
                        .get(&connection.to.node)
                        .and_then(|n| n.inputs.get(connection.to.index).map(|s| (n, s)))
                        .map(|(n, s)| format!("{}.{}", n.title, s.name))
                        .unwrap_or_else(|| "?".to_string());
                    errors.push(CompileError::on(
                        output.node,
                        format!(
                            "Muted: '{output_name}' has no type-matching input to pass through — '{to_label}' loses its input"
                        ),
                    ));
                }
            }
        }
    }
    (wires, errors)
}

/// Position of a variadic member within its group (0-based); 0 for plain
/// sockets.
fn member_index(node: &Node, socket_index: usize) -> usize {
    let Some(socket) = node.inputs.get(socket_index) else {
        return 0;
    };
    if !socket.is_variadic_member() {
        return 0;
    }
    node.inputs[..socket_index]
        .iter()
        .filter(|other| other.def_index == socket.def_index && other.is_variadic_member())
        .count()
}

/// Fixed id for the compiler-synthesized `Viewer` sink that gathers every
/// output checked in the node panel's generic "View" section
/// (`Socket::show_in_view`, `docs/APP_DESIGN.md`) without an explicit wire.
/// Kept constant (rather than derived from the graph's own ids) so
/// live-diffing sees the same node across `lower()` calls while the watched
/// set is unchanged, regardless of how many real nodes come and go.
const AUTO_VIEW_NODE_ID: NodeId = NodeId(u32::MAX);

/// If any output in `graph` is checked "Show in view", returns a clone with
/// a synthetic `Viewer` node wired to every one of them — the View panel's
/// checkboxes become lanes without the user dragging a wire. Reuses the
/// exact same pruning and edge-negotiation path an explicit Viewer
/// connection would take, so nothing downstream in `lower()` needs to know
/// this node isn't real.
fn with_auto_view_sink(graph: &GraphState) -> GraphState {
    let mut watched: Vec<(SocketId, String)> = graph
        .nodes
        .iter()
        .filter(|(_, node)| node.kind == NodeKind::Regular)
        .flat_map(|(&id, node)| {
            node.outputs
                .iter()
                .enumerate()
                .filter(|(_, output)| output.visible && output.show_in_view)
                .map(move |(index, output)| {
                    (
                        SocketId {
                            node: id,
                            index,
                            direction: SocketDirection::Output,
                        },
                        format!("{}.{}", node.title, output.name),
                    )
                })
        })
        .collect();
    // Protocol decoders can publish fine-grained annotations (UART Bits)
    // alongside a frame-level annotation (UART Data). Keep the detail lane
    // directly above its data lane, independent of its runtime port index.
    watched.sort_by_key(|(socket, label)| (socket.node.0, !label.ends_with(".Bits"), socket.index));

    let mut graph = graph.clone();
    if watched.is_empty() {
        return graph;
    }

    let inputs = watched
        .iter()
        .map(|(_, label)| Socket {
            name: label.clone(),
            type_name: "Signal".to_owned(),
            color: Color32::from_rgb(0, 205, 160),
            shape: SocketShape::Circle,
            allowed: vec![
                "Words".to_owned(),
                "Trigger".to_owned(),
                "Number".to_owned(),
                "Text".to_owned(),
            ],
            resolved_type: None,
            def_index: 0,
            variadic: Some(VariadicInfo {
                base: "In".to_owned(),
                max: watched.len(),
                placeholder: false,
            }),
            visible: true,
            hidden: false,
            has_control: false,
            show_in_view: false,
        })
        .collect();
    let mut auto_view = Node::blank(AUTO_VIEW_NODE_ID, "Viewer", Pos2::ZERO);
    auto_view.title = "Auto View".to_owned();
    auto_view.header_color = Color32::from_rgb(160, 80, 60);
    auto_view.inputs = inputs;
    auto_view.state = serde_json::json!({ "label": { "value": "" } });
    graph.nodes.insert(AUTO_VIEW_NODE_ID, auto_view);
    graph
        .connections
        .extend(
            watched
                .into_iter()
                .enumerate()
                .map(|(member, (from, _))| Connection {
                    from,
                    to: SocketId {
                        node: AUTO_VIEW_NODE_ID,
                        index: member,
                        direction: SocketDirection::Input,
                    },
                }),
        );
    graph
}

pub fn lower(
    graph: &GraphState,
    registry: &BuilderRegistry,
) -> Result<CompiledGraph, Vec<CompileError>> {
    let augmented = with_auto_view_sink(graph);
    let graph = &augmented;
    let (wires, mut errors) = resolve_reroute_edges(graph);

    // Prune: keep only what feeds a sink.
    let sinks: Vec<NodeId> = graph
        .nodes
        .values()
        .filter(|node| {
            node.kind == NodeKind::Regular
                && registry.get(node.def_name()).is_some_and(|b| b.is_sink())
        })
        .map(|node| node.id)
        .collect();
    if sinks.is_empty() {
        return Err(vec![CompileError::global(
            "Graph has no sink (add a File Writer)",
        )]);
    }
    let mut keep: HashSet<NodeId> = HashSet::new();
    let mut stack = sinks.clone();
    while let Some(id) = stack.pop() {
        if !keep.insert(id) {
            continue;
        }
        for wire in &wires {
            if wire.to.node == id && !keep.contains(&wire.from.node) {
                stack.push(wire.from.node);
            }
        }
    }
    let mut kept: Vec<NodeId> = keep.iter().copied().collect();
    kept.sort_by_key(|id| id.0);

    // Every kept node must have a runtime; exactly one source.
    let mut source_count = 0usize;
    let mut viewer_retention = ViewerRetention::Unlimited;
    for &id in &kept {
        let node = &graph.nodes[&id];
        match registry.get(node.def_name()) {
            None => errors.push(CompileError::on(
                id,
                format!("'{}' has no runtime implementation", node.def_name()),
            )),
            Some(builder) if builder.is_source() => {
                source_count += 1;
                viewer_retention = builder.viewer_retention(&node.state);
            }
            Some(_) => {}
        }
    }
    if source_count == 0 {
        errors.push(CompileError::global("Graph has no data source"));
    } else if source_count > 1 {
        for &id in &kept {
            let node = &graph.nodes[&id];
            if registry.get(node.def_name()).is_some_and(|b| b.is_source()) {
                errors.push(CompileError::on(
                    id,
                    "Multiple sources: a graph has exactly one time domain",
                ));
            }
        }
    }

    // Negotiate kinds and ports per edge.
    let mut resolved: HashMap<NodeId, ResolvedInputs> = HashMap::new();
    let mut edges: Vec<CompiledEdge> = Vec::new();
    let mut connected: HashMap<NodeId, HashSet<usize>> = HashMap::new();
    for wire in &wires {
        if !keep.contains(&wire.from.node) || !keep.contains(&wire.to.node) {
            continue;
        }
        let from_node = &graph.nodes[&wire.from.node];
        let to_node = &graph.nodes[&wire.to.node];
        let (Some(from_builder), Some(to_builder)) = (
            registry.get(from_node.def_name()),
            registry.get(to_node.def_name()),
        ) else {
            continue; // already reported above
        };
        let (Some(from_socket), Some(to_socket)) = (
            from_node.outputs.get(wire.from.index),
            to_node.inputs.get(wire.to.index),
        ) else {
            errors.push(CompileError::on(wire.to.node, "Dangling connection"));
            continue;
        };

        connected
            .entry(wire.to.node)
            .or_default()
            .insert(wire.to.index);

        let offered = from_builder.offered_kinds(from_socket, &from_node.state);
        let accepted = to_builder.accepted_kinds(to_socket, &to_node.state);
        let Some(kind) = offered.iter().copied().find(|k| accepted.contains(k)) else {
            errors.push(CompileError::on(
                wire.to.node,
                format!(
                    "'{}' cannot consume what '{}' produces on '{}'",
                    to_socket.name, from_node.title, from_socket.name
                ),
            ));
            continue;
        };

        let Some(out_port) = from_builder.output_port(from_socket, &from_node.state, kind) else {
            errors.push(CompileError::on(
                wire.from.node,
                format!("No runtime port for output '{}'", from_socket.name),
            ));
            continue;
        };
        let member = member_index(to_node, wire.to.index);
        let Some(in_port) = to_builder.input_port(to_socket, member, &to_node.state, kind) else {
            errors.push(CompileError::on(
                wire.to.node,
                format!("No runtime port for input '{}'", to_socket.name),
            ));
            continue;
        };

        resolved.entry(wire.to.node).or_default().0.insert(
            (to_socket.def_index, member),
            ResolvedInput {
                kind,
                source: format!("{}.{}", from_node.title, from_socket.name),
                source_node: wire.from.node,
                source_node_title: from_node.title.clone(),
                word_display_format: from_builder
                    .word_display_format(from_socket, &from_node.state),
                viewer_presentation: from_builder
                    .viewer_output_presentation(from_socket, &from_node.state),
            },
        );
        edges.push(CompiledEdge {
            from: (wire.from.node, out_port),
            to: (wire.to.node, in_port),
            buffer: to_builder
                .input_buffer_override(to_socket, &to_node.state)
                .unwrap_or_else(|| kind.buffer_size(from_builder.is_source())),
            kind,
        });
    }

    // Required inputs.
    for &id in &kept {
        let node = &graph.nodes[&id];
        let Some(builder) = registry.get(node.def_name()) else {
            continue;
        };
        let node_connected = connected.get(&id);
        for (index, socket) in node.inputs.iter().enumerate() {
            // Control-bearing sockets go through `input_required` like any
            // other: most are self-supplying config (their builders return
            // false), but one can be conditionally required — the writer's
            // Filename picker is required exactly while its value is empty.
            if !socket.visible {
                continue;
            }
            if socket.is_variadic_placeholder() {
                let has_member = node
                    .inputs
                    .iter()
                    .any(|s| s.def_index == socket.def_index && s.is_variadic_member());
                if !has_member && builder.input_required(socket, &node.state) {
                    errors.push(CompileError::on(
                        id,
                        format!("Input '{}' needs at least one connection", socket.name),
                    ));
                }
            } else if !socket.is_variadic_member()
                && !node_connected.is_some_and(|set| set.contains(&index))
                && builder.input_required(socket, &node.state)
            {
                errors.push(CompileError::on(
                    id,
                    format!("Input '{}' is not connected", socket.name),
                ));
            }
        }
    }

    // Cycle check (a cycle would deadlock the pipeline).
    if has_cycle(&kept, &edges) {
        errors.push(CompileError::global("Graph contains a cycle"));
    }

    if !errors.is_empty() {
        return Err(errors);
    }

    let nodes = kept
        .iter()
        .map(|&id| {
            let node = &graph.nodes[&id];
            CompiledNode {
                id,
                builder: node.def_name().to_owned(),
                state: node.state.clone(),
                runtime_name: runtime_name(node),
                resolved: resolved.remove(&id).unwrap_or_default(),
                viewer_word_caches: Vec::new(),
            }
        })
        .collect();
    let compiled = CompiledGraph {
        nodes,
        edges,
        viewer_retention,
    };
    let mut compiled = compiled;
    cache_platform::assign_viewer_caches(&mut compiled);
    Ok(compiled)
}

fn has_cycle(nodes: &[NodeId], edges: &[CompiledEdge]) -> bool {
    let mut indegree: HashMap<NodeId, usize> = nodes.iter().map(|&id| (id, 0)).collect();
    for edge in edges {
        *indegree.entry(edge.to.0).or_default() += 1;
    }
    let mut queue: Vec<NodeId> = indegree
        .iter()
        .filter(|entry| *entry.1 == 0)
        .map(|(&id, _)| id)
        .collect();
    let mut visited = 0usize;
    while let Some(id) = queue.pop() {
        visited += 1;
        for edge in edges.iter().filter(|e| e.from.0 == id) {
            let d = indegree.get_mut(&edge.to.0).expect("edge endpoints kept");
            *d -= 1;
            if *d == 0 {
                queue.push(edge.to.0);
            }
        }
    }
    visited != nodes.len()
}

// ── Live pipeline ───────────────────────────────────────────────────────

/// Producers-before-consumers order; `lower` already rejected cycles.
fn topo_order(compiled: &CompiledGraph) -> Vec<NodeId> {
    let mut indegree: HashMap<NodeId, usize> =
        compiled.nodes.iter().map(|node| (node.id, 0)).collect();
    for edge in &compiled.edges {
        *indegree.entry(edge.to.0).or_default() += 1;
    }
    let mut queue: Vec<NodeId> = compiled
        .nodes
        .iter()
        .map(|node| node.id)
        .filter(|id| indegree[id] == 0)
        .collect();
    queue.sort_by_key(|id| id.0);
    let mut order = Vec::with_capacity(compiled.nodes.len());
    while let Some(id) = queue.pop() {
        order.push(id);
        for edge in compiled.edges.iter().filter(|edge| edge.from.0 == id) {
            let degree = indegree.get_mut(&edge.to.0).expect("kept node");
            *degree -= 1;
            if *degree == 0 {
                queue.push(edge.to.0);
            }
        }
    }
    order
}

pub(super) fn compiled_node(compiled: &CompiledGraph, id: NodeId) -> &CompiledNode {
    compiled
        .nodes
        .iter()
        .find(|node| node.id == id)
        .expect("node in compiled graph")
}

pub fn derived_cache_configs_by_node(
    graph: &GraphState,
    registry: &BuilderRegistry,
) -> Result<HashMap<NodeId, Vec<PersistentStoreConfig>>, Vec<CompileError>> {
    cache_platform::cache_configs_by_node(graph, registry)
}

/// Input subscriptions for `id`, matched to the built node's input schema.
fn input_subs(
    compiled: &CompiledGraph,
    id: NodeId,
    built: &dyn ProcessNode,
    names: &HashMap<NodeId, String>,
) -> Result<Vec<Option<InputSub>>, String> {
    built
        .input_schema()
        .iter()
        .map(|schema| {
            let edge = compiled
                .edges
                .iter()
                .find(|edge| edge.to.0 == id && edge.to.1 == schema.name);
            match edge {
                None => Ok(None),
                Some(edge) => {
                    let from_node = names
                        .get(&edge.from.0)
                        .ok_or_else(|| format!("producer n{} not materialized", edge.from.0.0))?;
                    Ok(Some(InputSub {
                        from_node: from_node.clone(),
                        from_port: edge.from.1.clone(),
                        buffer: edge.buffer,
                        policy: OverflowPolicy::Block,
                    }))
                }
            }
        })
        .collect()
}

/// One live edit, in application order (removals reverse-topological,
/// additions topological, then hot configs and in-place restarts).
#[derive(Debug)]
enum LiveEdit {
    Remove(NodeId),
    Add(NodeId),
    Configure(NodeId, NodeConfig),
    Restart(NodeId),
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct ApplySummary {
    pub added: usize,
    pub removed: usize,
    pub configured: usize,
    pub restarted: usize,
}

impl ApplySummary {
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }
}

/// Wiring signature of a node's inputs, for diffing.
fn wiring_of(compiled: &CompiledGraph, id: NodeId) -> BTreeSet<(String, u32, String, usize)> {
    compiled
        .edges
        .iter()
        .filter(|edge| edge.to.0 == id)
        .map(|edge| {
            (
                edge.to.1.clone(),
                edge.from.0.0,
                edge.from.1.clone(),
                edge.buffer,
            )
        })
        .collect()
}

/// Classifies the difference between the running IR and the edited one
/// (the edit classes of `docs/APP_DESIGN.md`). Returns the edit list, or
/// the reason a full restart is needed.
fn diff(
    old: &CompiledGraph,
    new: &CompiledGraph,
    registry: &BuilderRegistry,
) -> Result<Vec<LiveEdit>, String> {
    let old_ids: HashSet<NodeId> = old.nodes.iter().map(|node| node.id).collect();
    let new_ids: HashSet<NodeId> = new.nodes.iter().map(|node| node.id).collect();
    let is_source = |compiled: &CompiledGraph, id: NodeId| {
        registry
            .get(&compiled_node(compiled, id).builder)
            .is_some_and(|builder| builder.is_source())
    };

    let mut edits: Vec<LiveEdit> = Vec::new();

    // Removals, consumers before producers.
    let mut removals: Vec<NodeId> = topo_order(old)
        .into_iter()
        .rev()
        .filter(|id| !new_ids.contains(id))
        .collect();
    for &id in &removals {
        if is_source(old, id) {
            return Err("the source node was removed".into());
        }
    }
    edits.extend(removals.drain(..).map(LiveEdit::Remove));

    // Additions, producers before consumers.
    for id in topo_order(new) {
        if old_ids.contains(&id) {
            continue;
        }
        for edge in new.edges.iter().filter(|edge| edge.to.0 == id) {
            if edge.kind == PortKind::of::<SampleBlock>() {
                return Err(
                    "new node consumes block channels; block subscriptions cannot join mid-stream"
                        .to_string(),
                );
            }
            if is_source(new, edge.from.0) {
                return Err(
                    "new connection directly to the source; source destinations are fixed at start"
                        .into(),
                );
            }
        }
        edits.push(LiveEdit::Add(id));
    }

    // Changed nodes: hot config, or restart in place.
    for id in topo_order(new) {
        if !old_ids.contains(&id) {
            continue;
        }
        let old_node = compiled_node(old, id);
        let new_node = compiled_node(new, id);
        let wiring_changed = wiring_of(old, id) != wiring_of(new, id);
        let state_changed = old_node.state != new_node.state;
        if !wiring_changed && !state_changed {
            continue;
        }
        if is_source(new, id) {
            return Err("the source node changed".into());
        }
        let builder = registry
            .get(&new_node.builder)
            .ok_or_else(|| format!("no builder for '{}'", new_node.builder))?;
        if !wiring_changed
            && state_changed
            && let Some(config) = builder.hot_config(&new_node.state)
        {
            edits.push(LiveEdit::Configure(id, config));
            continue;
        }
        // Restart in place: the node re-subscribes to its producers, which
        // is invisible to block streams and to source ports (their worker
        // threads snapshot destinations at start).
        for edge in new.edges.iter().filter(|edge| edge.to.0 == id) {
            if edge.kind == PortKind::of::<SampleBlock>() {
                return Err(format!(
                    "'{}' consumes block channels and cannot restart mid-stream",
                    new_node.runtime_name
                ));
            }
            if is_source(new, edge.from.0) {
                return Err(format!(
                    "'{}' is fed directly by the source and cannot restart mid-stream",
                    new_node.runtime_name
                ));
            }
        }
        edits.push(LiveEdit::Restart(id));
    }

    Ok(edits)
}

/// A pipeline running under the live supervisor: editable while it runs.
pub struct LiveRun {
    manager: AppManager,
    compiled: CompiledGraph,
    /// Supervisor key per UI node — assigned at add time and stable across
    /// title renames and in-place restarts.
    names: HashMap<NodeId, String>,
    lanes: DerivedLanes,
    viewer_lanes: ViewerLaneRegistry,
    /// Set by [`Self::stop`]: the wind-down has been signalled but node
    /// threads may still be finishing their current `work()` call.
    stop_requested: bool,
    cache_pruned: bool,
    persistent_cache_directory: Option<std::path::PathBuf>,
}

/// Lowers and materializes `graph` under an [`AppManager`] — real OS threads
/// natively, a cooperative single-thread runner on wasm.
pub fn start_live(
    graph: &GraphState,
    registry: &BuilderRegistry,
    ctx: &mut CompileCtx,
) -> Result<LiveRun, Vec<CompileError>> {
    let mut compiled = lower(graph, registry)?;
    cache_platform::configure_directory(&mut compiled, ctx.persistent_cache_directory.as_deref());
    ctx.viewer_retention = compiled.viewer_retention;
    let mut manager = AppManager::new();
    let mut names: HashMap<NodeId, String> = HashMap::new();

    let (execution, cache_pruned) = cache_platform::prepare_execution(&compiled, registry);

    for id in topo_order(&execution) {
        let node = compiled_node(&execution, id);
        let builder = registry.get(&node.builder).ok_or_else(|| {
            vec![CompileError::on(
                id,
                format!("unknown builder '{}'", node.builder),
            )]
        })?;
        ctx.viewer_word_caches.clone_from(&node.viewer_word_caches);
        let process = builder
            .build(&node.runtime_name, &node.state, &node.resolved, ctx)
            .map_err(|message| vec![CompileError::on(id, message)])?;
        let inputs = input_subs(&execution, id, process.as_ref(), &names)
            .map_err(|message| vec![CompileError::on(id, message)])?;
        manager
            .add_node_deferred(signal_processing::NodeSpec {
                name: node.runtime_name.clone(),
                node: process,
                inputs,
            })
            .map_err(|message| vec![CompileError::on(id, message)])?;
        names.insert(id, node.runtime_name.clone());
    }
    // All initial subscriptions exist; only now may threads start (a
    // self-threading source snapshots its subscriber lists on first work()).
    manager
        .start_all_deferred()
        .map_err(|message| vec![CompileError::global(message)])?;

    Ok(LiveRun {
        manager,
        compiled,
        names,
        lanes: ctx.derived_lanes.clone(),
        viewer_lanes: ctx.viewer_lanes.clone(),
        stop_requested: false,
        cache_pruned,
        persistent_cache_directory: ctx.persistent_cache_directory.clone(),
    })
}

impl LiveRun {
    /// Diffs the edited graph against what is running and applies the
    /// difference live. On any error the running pipeline is untouched
    /// (edits either fail up front in `diff`, or — for build failures midway
    /// — leave already-applied edits in place and report).
    pub fn apply(
        &mut self,
        graph: &GraphState,
        registry: &BuilderRegistry,
    ) -> Result<ApplySummary, ApplyError> {
        let mut new = lower(graph, registry).map_err(ApplyError::Compile)?;
        cache_platform::configure_directory(&mut new, self.persistent_cache_directory.as_deref());
        let edits = diff(&self.compiled, &new, registry).map_err(ApplyError::NeedsFullRestart)?;
        if edits.is_empty() {
            self.compiled = new;
            return Ok(ApplySummary::default());
        }
        if self.cache_pruned {
            return Err(ApplyError::NeedsFullRestart(
                "the running graph reused persistent viewer data; stop and rerun to apply edits"
                    .to_string(),
            ));
        }

        let mut ctx = CompileCtx {
            derived_lanes: self.lanes.clone(),
            viewer_lanes: self.viewer_lanes.clone(),
            viewer_retention: new.viewer_retention,
            viewer_word_caches: Vec::new(),
            persistent_cache_directory: self.persistent_cache_directory.clone(),
        };
        let mut summary = ApplySummary::default();
        for edit in edits {
            match edit {
                LiveEdit::Remove(id) => {
                    if let Some(name) = self.names.remove(&id) {
                        self.manager.remove_node(&name).map_err(ApplyError::Apply)?;
                    }
                    summary.removed += 1;
                }
                LiveEdit::Add(id) => {
                    let node = compiled_node(&new, id);
                    let builder = registry.get(&node.builder).ok_or_else(|| {
                        ApplyError::Apply(format!("no builder '{}'", node.builder))
                    })?;
                    ctx.viewer_word_caches.clone_from(&node.viewer_word_caches);
                    let process = builder
                        .build(&node.runtime_name, &node.state, &node.resolved, &mut ctx)
                        .map_err(ApplyError::Apply)?;
                    let inputs = input_subs(&new, id, process.as_ref(), &self.names)
                        .map_err(ApplyError::Apply)?;
                    self.manager
                        .add_node(signal_processing::NodeSpec {
                            name: node.runtime_name.clone(),
                            node: process,
                            inputs,
                        })
                        .map_err(ApplyError::Apply)?;
                    self.names.insert(id, node.runtime_name.clone());
                    summary.added += 1;
                }
                LiveEdit::Configure(id, config) => {
                    let name = self
                        .names
                        .get(&id)
                        .ok_or_else(|| ApplyError::Apply(format!("n{} not running", id.0)))?;
                    self.manager
                        .reconfigure(name, config)
                        .map_err(ApplyError::Apply)?;
                    summary.configured += 1;
                }
                LiveEdit::Restart(id) => {
                    let node = compiled_node(&new, id);
                    let name = self
                        .names
                        .get(&id)
                        .cloned()
                        .ok_or_else(|| ApplyError::Apply(format!("n{} not running", id.0)))?;
                    let builder = registry.get(&node.builder).ok_or_else(|| {
                        ApplyError::Apply(format!("no builder '{}'", node.builder))
                    })?;
                    ctx.viewer_word_caches.clone_from(&node.viewer_word_caches);
                    let process = builder
                        .build(&name, &node.state, &node.resolved, &mut ctx)
                        .map_err(ApplyError::Apply)?;
                    let inputs = input_subs(&new, id, process.as_ref(), &self.names)
                        .map_err(ApplyError::Apply)?;
                    self.manager
                        .restart_node(&name, process, inputs)
                        .map_err(ApplyError::Apply)?;
                    summary.restarted += 1;
                }
            }
        }
        self.compiled = new;
        Ok(summary)
    }

    pub fn is_finished(&self) -> bool {
        self.manager.is_finished()
    }

    /// Signals the wind-down and returns immediately — never joins node
    /// threads, so it is safe to call from the frame loop (a node may be
    /// mid-`work()` for a while yet; see `PipelineManager::request_stop`).
    /// [`Self::is_finished`] flips once every thread has exited.
    pub fn stop(&mut self) {
        self.stop_requested = true;
        self.manager.request_stop();
    }

    /// True from [`Self::stop`] until the run is dropped — used by the
    /// toolbar to show "Stopping…" while threads finish their current
    /// `work()` call.
    pub fn is_stopping(&self) -> bool {
        self.stop_requested
    }

    /// Drives up to `budget` `work()` calls forward. A no-op on the
    /// threaded native manager (its nodes run themselves); on wasm's
    /// cooperative manager this is what actually advances the run, so the
    /// UI frame loop must call it every frame regardless of target.
    pub fn pump(&mut self, budget: usize) {
        self.manager.pump(budget);
    }

    /// Blocks until the run completes naturally (tests / headless).
    #[allow(dead_code)]
    pub fn wait(&mut self) {
        self.manager.wait();
    }

    /// Items produced per UI node (sum of `work()` returns), for header
    /// progress display.
    pub fn progress(&self) -> Vec<(NodeId, u64)> {
        let by_name: HashMap<String, u64> = self.manager.progress().into_iter().collect();
        self.names
            .iter()
            .filter_map(|(id, name)| by_name.get(name).map(|items| (*id, *items)))
            .collect()
    }

    /// Consumers dropped by backpressure policy since the last call, mapped
    /// back to UI nodes where possible.
    pub fn take_disconnected(&self) -> Vec<(Option<NodeId>, DisconnectEvent)> {
        self.manager
            .take_disconnected()
            .into_iter()
            .map(|event| {
                let id = event.consumer.as_ref().and_then(|consumer| {
                    self.names
                        .iter()
                        .find(|(_, name)| *name == consumer)
                        .map(|(id, _)| *id)
                });
                (id, event)
            })
            .collect()
    }
}

pub type AppRun = LiveRun;

pub fn start_app_run(
    graph: &GraphState,
    registry: &BuilderRegistry,
    ctx: &mut CompileCtx,
) -> Result<AppRun, Vec<CompileError>> {
    start_live(graph, registry, ctx)
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use std::path::{Path, PathBuf};
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    use logic_analyzer_processing::BinaryFileWriter;
    use node_graph::{NodeDef, NodeGraphWidget};
    use signal_processing::{
        ConfigValue, CooperativeManager, DerivedLaneData, NodeSpec, Pipeline, Sample, Trigger, Word,
    };

    use super::*;
    use crate::nodes;

    fn startup_widget() -> NodeGraphWidget {
        let mut widget = NodeGraphWidget::new(nodes::build_registry());
        nodes::populate_startup(&mut widget);
        widget
    }

    fn uart_demo_widget() -> NodeGraphWidget {
        let mut widget = NodeGraphWidget::new(nodes::build_registry());
        nodes::populate_uart_demo(&mut widget);
        widget
    }

    fn binary_decoder_demo_widget() -> NodeGraphWidget {
        let mut widget = NodeGraphWidget::new(nodes::build_registry());
        nodes::populate_binary_decoder_demo(&mut widget);
        widget
    }

    fn run_cooperatively(widget: &NodeGraphWidget) -> (CompiledGraph, Vec<(String, u64)>) {
        let registry = BuilderRegistry::standard();
        let compiled = lower(widget.graph(), &registry).unwrap();
        let mut manager = CooperativeManager::new();
        let mut names = HashMap::new();
        let mut ctx = CompileCtx::default();

        for id in topo_order(&compiled) {
            let node = compiled_node(&compiled, id);
            let builder = registry.get(&node.builder).unwrap();
            ctx.viewer_word_caches.clone_from(&node.viewer_word_caches);
            let process = builder
                .build(&node.runtime_name, &node.state, &node.resolved, &mut ctx)
                .unwrap();
            let inputs = input_subs(&compiled, id, process.as_ref(), &names).unwrap();
            manager
                .add_node_deferred(NodeSpec {
                    name: node.runtime_name.clone(),
                    node: process,
                    inputs,
                })
                .unwrap();
            names.insert(id, node.runtime_name.clone());
        }

        manager.start_all_deferred().unwrap();
        for _ in 0..1_000 {
            manager.pump(256);
            if manager.is_finished() {
                break;
            }
        }
        assert!(
            manager.is_finished(),
            "unfinished: {:?}",
            manager.progress()
        );
        (compiled, manager.progress())
    }

    #[test]
    fn startup_graph_lowers() {
        let widget = startup_widget();
        let compiled = lower(widget.graph(), &BuilderRegistry::standard())
            .unwrap_or_else(|errors| panic!("lower failed: {errors:?}"));

        // Every saved processing node has a runtime, plus the generic viewer
        // sink synthesized from watched outputs.
        assert_eq!(compiled.nodes.len(), 11);
        assert_eq!(compiled.edges.len(), 29);

        // Viewer lanes resolve with per-lane kinds and producer labels.
        let viewer = compiled
            .nodes
            .iter()
            .find(|n| n.builder == "Viewer")
            .unwrap();
        let lanes = viewer.resolved.members(0);
        assert_eq!(lanes.len(), 6);
        assert!(
            lanes
                .iter()
                .any(|(_, input)| input.kind == PortKind::of::<Word>()
                    && input.source == "SPI Decoder.MOSI Words")
        );
        assert!(
            lanes
                .iter()
                .any(|(_, input)| input.kind == PortKind::of::<Trigger>()
                    && input.source == "Match Start.Match")
        );
        assert!(
            lanes
                .iter()
                .any(|(_, input)| input.kind == PortKind::of::<Word>()
                    && input.source == "Binary Decoder.Words")
        );

        // Kind negotiation spot checks: SPI clk reads edges, the binary
        // decoder reads blocks — both fed from the same UI sockets.
        let spi = compiled
            .nodes
            .iter()
            .find(|n| n.builder == "SPI Decoder")
            .unwrap();
        let decoder = compiled
            .nodes
            .iter()
            .find(|n| n.builder == "Binary Decoder")
            .unwrap();
        let edge_to = |node: NodeId, port: &str| {
            compiled
                .edges
                .iter()
                .find(|e| e.to.0 == node && e.to.1 == port)
                .unwrap_or_else(|| panic!("no edge into {port}"))
        };
        // The runtime port name no longer encodes which kind was picked
        // (both resolve to `ch{channel}` on a single collapsed port —
        // see `FileSourceBuilder::output_port`), so check the negotiated
        // kind directly via each node's `ResolvedInputs` instead of
        // sniffing a `d`/`b` prefix.
        assert_eq!(spi.resolved.kind(0), Some(PortKind::of::<Sample>())); // clk
        assert_eq!(
            decoder.resolved.kind(0),
            Some(PortKind::of::<SampleBlock>())
        ); // strobe
        assert_eq!(edge_to(decoder.id, "strobe").buffer, 2);
        assert_eq!(edge_to(spi.id, "clk").buffer, 10_000_000);
        assert_eq!(edge_to(decoder.id, "d7").from.1, "ch7");
        assert!(
            compiled
                .edges
                .iter()
                .any(|e| e.to.1 == "enable_signal" && e.buffer == 1_000)
        );
    }

    /// A lone source node, no explicit sink — the graph the "no wiring
    /// needed" View-panel feature exists to make compilable.
    fn source_only_widget() -> NodeGraphWidget {
        let mut widget = NodeGraphWidget::new(nodes::build_registry());
        widget
            .add_node_at(nodes::UartDemoSource::name(), egui::Pos2::ZERO)
            .expect("UART Demo Source is registered");
        widget
    }

    fn watch_first_output(widget: &mut NodeGraphWidget) -> NodeId {
        let id = *widget.graph().nodes.keys().next().unwrap();
        widget.graph_mut().nodes.get_mut(&id).unwrap().outputs[0].show_in_view = true;
        id
    }

    #[test]
    fn unwatched_source_has_no_sink() {
        let widget = source_only_widget();
        let errors = lower(widget.graph(), &BuilderRegistry::standard()).unwrap_err();
        assert!(errors.iter().any(|e| e.message.contains("no sink")));
    }

    #[test]
    fn watched_output_compiles_without_an_explicit_viewer_node() {
        let mut widget = source_only_widget();
        let source_id = watch_first_output(&mut widget);
        let source_title = widget.graph().nodes[&source_id].title.clone();

        let compiled = lower(widget.graph(), &BuilderRegistry::standard())
            .unwrap_or_else(|errors| panic!("lower failed: {errors:?}"));

        assert_eq!(compiled.nodes.len(), 2);
        let auto_view = compiled
            .nodes
            .iter()
            .find(|n| n.builder == "Viewer")
            .expect("a synthetic Viewer sink is added");
        let lanes = auto_view.resolved.members(0);
        assert_eq!(lanes.len(), 1);
        assert_eq!(lanes[0].1.source, format!("{source_title}.RX"));
    }

    #[test]
    fn counter_and_formatter_outputs_can_be_watched() {
        use signal_processing::{NumberSample, TextSample};

        let mut widget = NodeGraphWidget::new(nodes::build_registry());
        nodes::populate_binary_decoder_demo(&mut widget);
        for definition in [nodes::Counter::name(), nodes::StringFormatter::name()] {
            let node = widget
                .graph_mut()
                .nodes
                .values_mut()
                .find(|node| node.def_name() == definition)
                .unwrap_or_else(|| panic!("missing {definition}"));
            node.outputs[0].show_in_view = true;
        }

        let compiled = lower(widget.graph(), &BuilderRegistry::standard())
            .unwrap_or_else(|errors| panic!("lower failed: {errors:?}"));
        let viewer = compiled
            .nodes
            .iter()
            .find(|node| node.builder == "Viewer")
            .expect("synthetic viewer");
        let kinds: Vec<_> = viewer
            .resolved
            .members(0)
            .into_iter()
            .map(|(_, input)| input.kind)
            .collect();
        assert!(kinds.contains(&PortKind::of::<NumberSample>()));
        assert!(kinds.contains(&PortKind::of::<TextSample>()));

        let mut ctx = CompileCtx::default();
        let derived = ctx.derived_lanes.clone();
        let mut run = start_live(widget.graph(), &BuilderRegistry::standard(), &mut ctx)
            .expect("watched value levels should run");
        run.wait();
        let lanes = derived.read();
        for (suffix, expected_kind) in [
            (".Count", signal_processing::ViewerValueKind::Number),
            (".Text", signal_processing::ViewerValueKind::Text),
        ] {
            let lane = lanes
                .iter()
                .find(|lane| lane.name.ends_with(suffix))
                .unwrap_or_else(|| panic!("missing {suffix} viewer lane"));
            let signal_processing::DerivedLaneData::Values(values) = &lane.data else {
                panic!("{suffix} should be a value lane");
            };
            assert_eq!(values.kind, expected_kind);
            assert!(values.values.len() > 1, "{suffix} should contain changes");
        }
    }

    #[test]
    fn unwatching_the_only_output_drops_the_synthetic_viewer() {
        let mut widget = source_only_widget();
        let source_id = watch_first_output(&mut widget);
        let registry = BuilderRegistry::standard();
        assert!(lower(widget.graph(), &registry).is_ok());

        widget
            .graph_mut()
            .nodes
            .get_mut(&source_id)
            .unwrap()
            .outputs[0]
            .show_in_view = false;
        let errors = lower(widget.graph(), &registry).unwrap_err();
        assert!(errors.iter().any(|e| e.message.contains("no sink")));
    }

    #[test]
    fn synthetic_viewer_id_is_stable_across_relowers() {
        let mut widget = source_only_widget();
        watch_first_output(&mut widget);
        let registry = BuilderRegistry::standard();

        let first = lower(widget.graph(), &registry).unwrap();
        let second = lower(widget.graph(), &registry).unwrap();
        let viewer_id = |compiled: &CompiledGraph| {
            compiled
                .nodes
                .iter()
                .find(|n| n.builder == "Viewer")
                .unwrap()
                .id
        };
        assert_eq!(viewer_id(&first), AUTO_VIEW_NODE_ID);
        assert_eq!(viewer_id(&first), viewer_id(&second));
    }

    fn persistent_word_keys(compiled: &CompiledGraph) -> Vec<[u8; 32]> {
        compiled
            .nodes
            .iter()
            .flat_map(|node| node.viewer_word_caches.iter().flatten())
            .map(|config| config.cache_key)
            .collect()
    }

    #[test]
    fn persistent_viewer_key_is_stable_but_decoder_configuration_invalidates_it() {
        let mut widget = uart_demo_widget();
        let registry = BuilderRegistry::standard();
        let first = lower(widget.graph(), &registry).unwrap();
        let repeated = lower(widget.graph(), &registry).unwrap();
        let first_keys = persistent_word_keys(&first);
        assert!(!first_keys.is_empty());
        assert_eq!(first_keys, persistent_word_keys(&repeated));

        let decoder = widget
            .graph()
            .nodes
            .values()
            .find(|node| node.def_name() == "UART Decoder")
            .unwrap()
            .id;
        let mut state: nodes::UartDecoderState =
            serde_json::from_value(widget.graph().nodes[&decoder].state.clone()).unwrap();
        state.data_bits.value -= 1;
        widget.set_node_state(decoder, serde_json::to_value(state).unwrap());
        let changed = lower(widget.graph(), &registry).unwrap();
        assert_ne!(first_keys, persistent_word_keys(&changed));
    }

    #[test]
    fn cache_inventory_maps_a_lane_to_its_viewer_and_upstream_nodes() {
        let widget = uart_demo_widget();
        let registry = BuilderRegistry::standard();
        let compiled = lower(widget.graph(), &registry).unwrap();
        let viewer = compiled
            .nodes
            .iter()
            .find(|node| node.builder == "Viewer")
            .unwrap();
        let expected: Vec<_> = viewer
            .viewer_word_caches
            .iter()
            .flatten()
            .map(|config| config.cache_key)
            .collect();

        let inventory = derived_cache_configs_by_node(widget.graph(), &registry).unwrap();
        let actual = inventory[&viewer.id]
            .iter()
            .map(|config| config.cache_key)
            .collect::<Vec<_>>();
        let decoder = compiled
            .nodes
            .iter()
            .find(|node| node.builder == "UART Decoder")
            .unwrap();

        assert!(!expected.is_empty());
        assert_eq!(actual, expected);
        assert_eq!(
            inventory[&decoder.id]
                .iter()
                .map(|config| config.cache_key)
                .collect::<Vec<_>>(),
            expected
        );
    }

    #[test]
    fn persistent_viewer_key_includes_variadic_member_order() {
        let compiled = lower(uart_demo_widget().graph(), &BuilderRegistry::standard()).unwrap();
        let viewer = compiled
            .nodes
            .iter()
            .find(|node| node.builder == "Viewer")
            .unwrap();
        let edge = compiled
            .edges
            .iter()
            .find(|edge| edge.to.0 == viewer.id && edge.kind == PortKind::of::<Word>())
            .unwrap();
        assert_ne!(
            cache_platform::persistent_lane_key(&compiled, viewer.id, 0, edge),
            cache_platform::persistent_lane_key(&compiled, viewer.id, 1, edge)
        );
    }

    #[test]
    fn capture_file_identity_changes_when_source_file_changes() {
        use std::io::Write;

        let mut file = tempfile::NamedTempFile::new().unwrap();
        file.write_all(b"first").unwrap();
        file.as_file().sync_data().unwrap();
        let path = std::fs::canonicalize(file.path()).unwrap();
        let digest = |path: &Path| {
            let mut hasher = blake3::Hasher::new();
            cache_platform::hash_capture_file_identity(&mut hasher, path).unwrap();
            *hasher.finalize().as_bytes()
        };
        let first = digest(&path);
        file.write_all(b"-changed").unwrap();
        file.as_file().sync_data().unwrap();
        let second = digest(&path);
        assert_ne!(first, second);
    }

    #[test]
    fn persistent_cache_hit_prunes_decoder_used_only_by_cached_viewer_lane() {
        use signal_processing::{IndexedAnnotationWriter, LiveStoreConfig};

        let directory = tempfile::tempdir().unwrap();
        let registry = BuilderRegistry::standard();
        let mut compiled = lower(uart_demo_widget().graph(), &registry).unwrap();
        cache_platform::configure_directory(&mut compiled, Some(directory.path()));
        let cache = compiled
            .nodes
            .iter()
            .find(|node| node.builder == "Viewer")
            .unwrap()
            .viewer_word_caches
            .iter()
            .flatten()
            .next()
            .unwrap()
            .clone();
        let (mut writer, store) = IndexedAnnotationWriter::create(LiveStoreConfig {
            directory: directory.path().to_path_buf(),
            persistence: Some(cache),
            ..LiveStoreConfig::default()
        })
        .unwrap();
        writer.append(Word::new(0x48, 0)).unwrap();
        writer.finish().unwrap();
        drop((writer, store));

        let (execution, pruned) = cache_platform::prepare_execution(&compiled, &registry);

        assert!(pruned);
        assert!(
            execution
                .nodes
                .iter()
                .all(|node| node.builder != "UART Decoder")
        );
        assert!(execution.nodes.iter().any(|node| node.builder == "Viewer"));
        assert!(
            execution
                .edges
                .iter()
                .all(|edge| edge.kind != PortKind::of::<Word>())
        );
    }

    #[test]
    fn second_live_run_reuses_persistent_words_without_starting_decoder() {
        let directory = tempfile::tempdir().unwrap();
        let widget = uart_demo_widget();
        let registry = BuilderRegistry::standard();
        let decoder_id = widget
            .graph()
            .nodes
            .values()
            .find(|node| node.def_name() == "UART Decoder")
            .unwrap()
            .id;
        let mut first_ctx = CompileCtx {
            persistent_cache_directory: Some(directory.path().to_path_buf()),
            ..CompileCtx::default()
        };
        let mut first = start_live(widget.graph(), &registry, &mut first_ctx).unwrap();
        first.wait();
        assert!(first.names.contains_key(&decoder_id));
        drop((first, first_ctx));

        let mut second_ctx = CompileCtx {
            persistent_cache_directory: Some(directory.path().to_path_buf()),
            ..CompileCtx::default()
        };
        let lanes = second_ctx.derived_lanes.clone();
        let mut second = start_live(widget.graph(), &registry, &mut second_ctx).unwrap();
        assert!(!second.names.contains_key(&decoder_id));
        second.wait();

        let lanes = lanes.read();
        assert!(lanes.iter().any(|lane| {
            matches!(lane.data, signal_processing::DerivedLaneData::Annotations(ref words) if words.len() >= 6)
                || matches!(lane.data, signal_processing::DerivedLaneData::IndexedAnnotations(_))
        }));
    }

    #[test]
    fn uart_demo_graph_lowers() {
        let widget = uart_demo_widget();
        let compiled = lower(widget.graph(), &BuilderRegistry::standard())
            .unwrap_or_else(|errors| panic!("lower failed: {errors:?}"));

        assert_eq!(compiled.nodes.len(), 3);
        assert_eq!(compiled.edges.len(), 3);
        assert_eq!(compiled.viewer_retention, ViewerRetention::Unlimited);
        assert!(
            compiled
                .nodes
                .iter()
                .any(|n| n.builder == "UART Demo Source")
        );
        assert!(compiled.nodes.iter().any(|n| n.builder == "UART Decoder"));

        let viewer = compiled
            .nodes
            .iter()
            .find(|n| n.builder == "Viewer")
            .unwrap();
        let lanes = viewer.resolved.members(0);
        assert_eq!(lanes.len(), 2);
        assert_eq!(lanes[0].1.kind, PortKind::of::<Sample>());
        assert_eq!(lanes[1].1.kind, PortKind::of::<Word>());
    }

    #[test]
    fn uart_bits_view_completes_under_cooperative_runner() {
        let mut widget = uart_demo_widget();
        let decoder = node_by_def(&widget, "UART Decoder");
        let bits = output_index(&widget, decoder, "Bits");
        widget.graph_mut().nodes.get_mut(&decoder).unwrap().outputs[bits].show_in_view = true;

        let (compiled, _) = run_cooperatively(&widget);
        assert!(
            compiled
                .edges
                .iter()
                .any(|edge| edge.from == (decoder, "bits".to_owned()))
        );
    }

    #[test]
    fn binary_decoder_demo_decodes_both_protocols_cooperatively() {
        let widget = binary_decoder_demo_widget();
        let (compiled, progress) = run_cooperatively(&widget);
        let items_for = |builder_name: &str| {
            let runtime_name = &compiled
                .nodes
                .iter()
                .find(|node| node.builder == builder_name)
                .unwrap()
                .runtime_name;
            progress
                .iter()
                .find(|(name, _)| name == runtime_name)
                .unwrap()
                .1
        };

        assert_eq!(items_for("Demo Capture Source"), 60_000);
        assert_eq!(items_for("SPI Decoder"), 60);
        assert_eq!(items_for("Binary Decoder"), 96);
    }

    #[test]
    fn binary_decoder_demo_latch_follows_every_start_stop_pair() {
        let widget = binary_decoder_demo_widget();
        let mut ctx = CompileCtx::default();
        let lanes = ctx.derived_lanes.clone();
        let mut run = start_live(widget.graph(), &BuilderRegistry::standard(), &mut ctx).unwrap();
        run.wait();

        let lanes = lanes.read();
        let q = lanes
            .iter()
            .find(|lane| lane.name == "SR Flip-Flop.Q")
            .expect("latch output should be visible");
        let signal_processing::DerivedLaneData::Digital(samples) = &q.data else {
            panic!("latch output should be a digital lane");
        };
        assert_eq!(samples.len(), 25);
        assert!(
            samples
                .iter()
                .enumerate()
                .all(|(index, sample)| sample.value == !index.is_multiple_of(2))
        );
        assert!(
            samples
                .windows(2)
                .all(|pair| pair[0].start_time_ns <= pair[1].start_time_ns)
        );
    }

    #[test]
    fn uart_viewer_tracks_carry_explicit_presentation_metadata() {
        let compiled = lower(uart_demo_widget().graph(), &BuilderRegistry::standard()).unwrap();
        let viewer = compiled
            .nodes
            .iter()
            .find(|node| node.builder == "Viewer")
            .unwrap();
        let mut tracks = viewer
            .resolved
            .members(0)
            .into_iter()
            .filter_map(|(_, input)| {
                input
                    .viewer_presentation
                    .as_ref()
                    .map(|presentation| presentation.track_key.as_str())
            })
            .collect::<Vec<_>>();
        tracks.sort_unstable();

        // The demo connects only Data. Explicit grouping still produces a
        // valid partial compound group rather than relying on a Bits lane
        // being present or discoverable by name.
        assert_eq!(tracks, ["frame"]);
    }

    #[test]
    fn duplicate_and_renamed_decoders_keep_distinct_explicit_groups() {
        let mut widget = uart_demo_widget();
        let source = node_by_def(&widget, "UART Demo Source");
        let first_decoder = node_by_def(&widget, "UART Decoder");
        let viewer = node_by_def(&widget, "Viewer");
        let second_decoder = widget
            .add_node_at(nodes::UartDecoder::name(), Pos2::new(420.0, 420.0))
            .unwrap();
        for decoder in [first_decoder, second_decoder] {
            widget.graph_mut().nodes.get_mut(&decoder).unwrap().title = "Duplicate title".into();
        }
        let connect = |widget: &mut NodeGraphWidget, from: (NodeId, &str), to: (NodeId, &str)| {
            let from_index = output_index(widget, from.0, from.1);
            let to_index = input_index(widget, to.0, to.1);
            widget.graph_mut().add_connection(
                SocketId {
                    node: from.0,
                    index: from_index,
                    direction: SocketDirection::Output,
                },
                SocketId {
                    node: to.0,
                    index: to_index,
                    direction: SocketDirection::Input,
                },
            );
        };
        connect(&mut widget, (source, "RX"), (second_decoder, "RX/TX"));
        connect(&mut widget, (second_decoder, "Data"), (viewer, "In"));

        let build_groups = |widget: &NodeGraphWidget| {
            let builders = BuilderRegistry::standard();
            let compiled = lower(widget.graph(), &builders).unwrap();
            let viewer = compiled
                .nodes
                .iter()
                .find(|node| node.builder == "Viewer")
                .unwrap();
            let mut ctx = CompileCtx::default();
            builders
                .get("Viewer")
                .unwrap()
                .build(
                    &viewer.runtime_name,
                    &viewer.state,
                    &viewer.resolved,
                    &mut ctx,
                )
                .unwrap();
            let groups = ctx.viewer_lanes.read();
            groups
                .iter()
                .filter(|group| {
                    group
                        .tracks
                        .iter()
                        .any(|track| track.id.as_str() == "frame")
                })
                .map(|group| (group.id.as_str().to_owned(), group.label.clone()))
                .collect::<Vec<_>>()
        };

        let before = build_groups(&widget);
        assert_eq!(before.len(), 2);
        assert_ne!(before[0].0, before[1].0);
        assert!(before.iter().all(|(_, label)| label == "Duplicate title"));

        widget
            .graph_mut()
            .nodes
            .get_mut(&first_decoder)
            .unwrap()
            .title = "Renamed decoder".into();
        let after = build_groups(&widget);
        assert_eq!(
            before.iter().map(|(id, _)| id).collect::<Vec<_>>(),
            after.iter().map(|(id, _)| id).collect::<Vec<_>>()
        );
        assert!(after.iter().any(|(_, label)| label == "Renamed decoder"));
    }

    #[test]
    fn plugin_builder_can_contribute_a_lane_renderer() {
        use std::sync::Arc;

        use logic_analyzer_viewer::{
            DefaultViewerLaneRenderer, ViewerLaneBadge, ViewerLaneRenderer,
            ViewerOutputPresentation,
        };

        struct PluginBuilder;
        impl RuntimeBuilder for PluginBuilder {
            fn accepted_kinds(&self, _: &Socket, _: &Value) -> Vec<PortKind> {
                Vec::new()
            }

            fn offered_kinds(&self, _: &Socket, _: &Value) -> Vec<PortKind> {
                Vec::new()
            }

            fn input_port(&self, _: &Socket, _: usize, _: &Value, _: PortKind) -> Option<String> {
                None
            }

            fn output_port(&self, _: &Socket, _: &Value, _: PortKind) -> Option<String> {
                None
            }

            fn viewer_output_presentation(
                &self,
                _: &Socket,
                _: &Value,
            ) -> Option<ViewerOutputPresentation> {
                let renderer: Arc<dyn ViewerLaneRenderer> = Arc::new(DefaultViewerLaneRenderer);
                Some(ViewerOutputPresentation::new(
                    "plugin group",
                    "plugin track",
                    0,
                    1.0,
                    ViewerLaneBadge::new("P", Color32::WHITE),
                    renderer,
                ))
            }

            fn build(
                &self,
                _: &str,
                _: &Value,
                _: &ResolvedInputs,
                _: &mut CompileCtx,
            ) -> Result<Box<dyn ProcessNode>, String> {
                Err("not needed by presentation registration test".into())
            }
        }

        let mut node_types = nodes::build_registry();
        let mut builders = BuilderRegistry::standard();
        crate::compiler::PluginContext::new(&mut node_types, &mut builders)
            .register_builder("Plugin Presenter", Box::new(PluginBuilder));
        let widget = uart_demo_widget();
        let socket = &widget
            .graph()
            .nodes
            .values()
            .find(|node| node.def_name() == "UART Decoder")
            .unwrap()
            .outputs[3];
        let presentation = builders
            .get("Plugin Presenter")
            .unwrap()
            .viewer_output_presentation(socket, &Value::Null)
            .unwrap();

        assert_eq!(presentation.group_key, "plugin group");
        assert_eq!(presentation.track_key, "plugin track");
    }

    #[test]
    fn file_source_bounds_exact_viewer_entries() {
        let widget = startup_widget();
        let compiled = lower(widget.graph(), &BuilderRegistry::standard())
            .unwrap_or_else(|errors| panic!("lower failed: {errors:?}"));

        assert_eq!(
            compiled.viewer_retention,
            ViewerRetention::MaxEntries(signal_processing::DEFAULT_VIEWER_MAX_ENTRIES)
        );
    }

    #[test]
    fn missing_writer_input_is_reported() {
        let mut widget = startup_widget();
        // Cut the filename wire; the writer input becomes a compile error.
        let graph = widget.graph_mut();
        let writer = graph
            .nodes
            .values()
            .find(|n| n.def_name() == "File Writer")
            .unwrap()
            .id;
        let index = graph
            .connections
            .iter()
            .position(|c| c.to.node == writer && c.to.index == 1)
            .unwrap();
        graph.remove_connection_at(index);

        let errors = lower(widget.graph(), &BuilderRegistry::standard()).unwrap_err();
        assert!(
            errors
                .iter()
                .any(|e| e.node == Some(writer) && e.message.contains("Filename")),
            "expected filename error, got {errors:?}"
        );
    }

    /// A wired File socket on the DSL File Source builds the deferred
    /// variant (filename arrives at run start over the wire); unconnected
    /// keeps the build-time open, and unconnected + empty picker is
    /// required (a compile error).
    #[test]
    fn file_source_with_wired_filename_builds_deferred_source() {
        use signal_processing::TextSample;

        use crate::nodes::FileSourceBuilder;

        let builder = FileSourceBuilder;
        let state = serde_json::to_value(nodes::DslFileSourceState {
            file: node_graph::FileValue::new(""),
            channels: node_graph::IntValue::new(4, 1, 32),
        })
        .unwrap();

        let file_socket = Socket {
            name: "File".into(),
            type_name: "Text".into(),
            color: egui::Color32::WHITE,
            shape: node_graph::SocketShape::Circle,
            allowed: vec![],
            resolved_type: None,
            def_index: 0,
            variadic: None,
            visible: true,
            hidden: false,
            has_control: true,
            show_in_view: false,
        };
        assert_eq!(
            builder.accepted_kinds(&file_socket, &state),
            vec![PortKind::of::<TextSample>()],
            "the File socket accepts a Text filename wire"
        );
        assert!(!builder.input_required(&file_socket, &state));

        let mut resolved = ResolvedInputs::default();
        resolved.0.insert(
            (0, 0),
            ResolvedInput {
                kind: PortKind::of::<TextSample>(),
                source: "Formatter.Text".into(),
                source_node: NodeId(1),
                source_node_title: "Formatter".into(),
                word_display_format: None,
                viewer_presentation: None,
            },
        );
        let node = builder
            .build("src", &state, &resolved, &mut CompileCtx::default())
            .expect("wired filename must not require the file to exist at build");
        assert_eq!(
            node.num_inputs(),
            1,
            "expected the deferred source (one filename input)"
        );
    }

    /// The counterpart to `missing_writer_input_is_reported`: with the
    /// writer's static filename (save-dialog prop) set, an unconnected
    /// Filename input is fine — the graph compiles and the writer is built
    /// with the static path.
    #[test]
    fn static_filename_makes_writer_filename_input_optional() {
        let mut widget = startup_widget();
        let graph = widget.graph_mut();
        let writer = graph
            .nodes
            .values()
            .find(|n| n.def_name() == "File Writer")
            .unwrap()
            .id;
        let index = graph
            .connections
            .iter()
            .position(|c| c.to.node == writer && c.to.index == 1)
            .unwrap();
        graph.remove_connection_at(index);

        let mut state: nodes::FileWriterState =
            serde_json::from_value(graph.nodes[&writer].state.clone()).unwrap();
        state.filename = node_graph::FileValue::new_save("/tmp/capture.bin", "Save capture as");
        widget.set_node_state(writer, serde_json::to_value(state).unwrap());

        lower(widget.graph(), &BuilderRegistry::standard())
            .unwrap_or_else(|errors| panic!("expected the graph to compile: {errors:?}"));
    }

    #[test]
    fn buffer_node_kind_mismatch_is_rejected() {
        use egui::Pos2;
        use node_graph::{NodeDef, SocketDirection, SocketId};

        let mut widget = NodeGraphWidget::new(nodes::build_registry());
        let source = widget
            .add_node_at(nodes::UartDemoSource::name(), Pos2::new(0.0, 0.0))
            .unwrap();
        let buf = widget
            .add_node_at(nodes::Buffer::name(), Pos2::new(200.0, 0.0))
            .unwrap();
        let viewer = widget
            .add_node_at(nodes::Viewer::name(), Pos2::new(400.0, 0.0))
            .unwrap();

        // UartDemoSource offers `Sample` ("Signal"); set the buffer to
        // "Trigger" — no common kind on the source -> buffer edge, must be
        // a compile error (regardless of what the buffer -> viewer edge
        // downstream negotiates to).
        let mut state = nodes::Buffer::state();
        state.kind.select("Trigger");
        widget.set_node_state(buf, serde_json::to_value(state).unwrap());

        let connect = |widget: &mut NodeGraphWidget, from: (NodeId, &str), to: (NodeId, &str)| {
            let from_socket = SocketId {
                node: from.0,
                index: output_index(widget, from.0, from.1),
                direction: SocketDirection::Output,
            };
            let to_socket = SocketId {
                node: to.0,
                index: input_index(widget, to.0, to.1),
                direction: SocketDirection::Input,
            };
            widget.graph_mut().add_connection(from_socket, to_socket);
        };
        connect(&mut widget, (source, "RX"), (buf, "In"));
        // A dangling output is unreachable and gets pruned before kind
        // negotiation runs — give the buffer a sink so it stays reachable
        // and the mismatch on its input actually gets checked.
        connect(&mut widget, (buf, "Out"), (viewer, "In"));

        let errors = lower(widget.graph(), &BuilderRegistry::standard()).unwrap_err();
        assert!(
            errors.iter().any(|e| e.node == Some(buf)),
            "expected a compile error on the buffer node, got {errors:?}"
        );
    }

    #[test]
    fn muted_node_with_compatible_pass_through_lowers_to_a_direct_connection() {
        use egui::Pos2;
        use node_graph::{NodeDef, SocketDirection, SocketId};

        let mut widget = NodeGraphWidget::new(nodes::build_registry());
        let source = widget
            .add_node_at(nodes::UartDemoSource::name(), Pos2::new(0.0, 0.0))
            .unwrap();
        let buf = widget
            .add_node_at(nodes::Buffer::name(), Pos2::new(200.0, 0.0))
            .unwrap();
        let viewer = widget
            .add_node_at(nodes::Viewer::name(), Pos2::new(400.0, 0.0))
            .unwrap();

        let connect = |widget: &mut NodeGraphWidget, from: (NodeId, &str), to: (NodeId, &str)| {
            let from_socket = SocketId {
                node: from.0,
                index: output_index(widget, from.0, from.1),
                direction: SocketDirection::Output,
            };
            let to_socket = SocketId {
                node: to.0,
                index: input_index(widget, to.0, to.1),
                direction: SocketDirection::Input,
            };
            widget.graph_mut().add_connection(from_socket, to_socket);
        };
        connect(&mut widget, (source, "RX"), (buf, "In"));
        connect(&mut widget, (buf, "Out"), (viewer, "In"));
        widget.graph_mut().nodes.get_mut(&buf).unwrap().muted = true;

        let compiled =
            lower(widget.graph(), &BuilderRegistry::standard()).unwrap_or_else(|errors| {
                panic!("expected the muted buffer to splice through: {errors:?}")
            });

        assert!(
            compiled.nodes.iter().all(|n| n.id != buf),
            "muted node must be dropped from the compiled graph, got {:?}",
            compiled.nodes
        );
        assert_eq!(compiled.edges.len(), 1);
        let edge = &compiled.edges[0];
        assert_eq!(edge.from.0, source);
        assert_eq!(edge.to.0, viewer);
    }

    #[test]
    fn muted_node_without_compatible_pass_through_reports_a_targeted_error() {
        use egui::Pos2;
        use node_graph::{SocketDirection, SocketId};

        let mut widget = NodeGraphWidget::new(nodes::build_registry());
        let source = widget
            .add_node_at("UART Demo Source", Pos2::new(0.0, 0.0))
            .unwrap();
        let matcher = widget
            .add_node_at("Word Matcher", Pos2::new(200.0, 0.0))
            .unwrap();
        let flip_flop = widget
            .add_node_at("SR Flip-Flop", Pos2::new(400.0, 0.0))
            .unwrap();
        let viewer = widget.add_node_at("Viewer", Pos2::new(600.0, 0.0)).unwrap();

        let connect = |widget: &mut NodeGraphWidget, from: (NodeId, &str), to: (NodeId, &str)| {
            let from_socket = SocketId {
                node: from.0,
                index: output_index(widget, from.0, from.1),
                direction: SocketDirection::Output,
            };
            let to_socket = SocketId {
                node: to.0,
                index: input_index(widget, to.0, to.1),
                direction: SocketDirection::Input,
            };
            widget.graph_mut().add_connection(from_socket, to_socket);
        };
        // Word Matcher's only input is `Words`-typed and its outputs are
        // `Trigger`/`Signal` — none of those pairs share a type, so it has
        // no pass-through no matter what's wired to it. Connecting it from
        // the Signal-typed source (bypassing the editor's own connect-time
        // type check, as `buffer_node_kind_mismatch_is_rejected` does above)
        // just gives it something realistic to break.
        connect(&mut widget, (source, "RX"), (matcher, "Words"));
        connect(&mut widget, (matcher, "Match"), (flip_flop, "Set"));
        connect(&mut widget, (flip_flop, "Q"), (viewer, "In"));
        widget.graph_mut().nodes.get_mut(&matcher).unwrap().muted = true;

        let errors = lower(widget.graph(), &BuilderRegistry::standard()).unwrap_err();
        assert!(
            errors
                .iter()
                .any(|e| e.node == Some(matcher) && e.message.contains("Muted")),
            "expected a targeted error on the muted Word Matcher, got {errors:?}"
        );
    }

    #[test]
    fn muted_source_reports_the_break_and_prunes_its_branch() {
        // A source has no data input at all — no config property shares
        // its output's type either — so it can never have a pass-through
        // pair. Muting it is a hard break, not a silent no-op: the targeted
        // error should point at the source, and its downstream branch
        // should vanish from the compiled graph rather than dangling.
        let mut widget = uart_demo_widget();
        let source = node_by_def(&widget, "UART Demo Source");
        widget.graph_mut().nodes.get_mut(&source).unwrap().muted = true;

        let errors = lower(widget.graph(), &BuilderRegistry::standard()).unwrap_err();
        assert!(
            errors
                .iter()
                .any(|e| e.node == Some(source) && e.message.contains("Muted")),
            "expected a targeted error on the muted source, got {errors:?}"
        );
    }

    fn output_index(widget: &NodeGraphWidget, node: NodeId, name: &str) -> usize {
        widget.graph().nodes[&node]
            .outputs
            .iter()
            .position(|socket| socket.name == name)
            .unwrap_or_else(|| panic!("no output socket '{name}'"))
    }

    fn input_index(widget: &NodeGraphWidget, node: NodeId, name: &str) -> usize {
        widget.graph().nodes[&node]
            .inputs
            .iter()
            .position(|socket| socket.name == name && socket.visible)
            .unwrap_or_else(|| panic!("no input socket '{name}'"))
    }

    fn node_by_def(widget: &NodeGraphWidget, def: &str) -> NodeId {
        widget
            .graph()
            .nodes
            .values()
            .find(|node| node.def_name() == def)
            .unwrap_or_else(|| panic!("no '{def}' node"))
            .id
    }

    // ── diff classification ───────────────────────────────────────────

    #[test]
    fn diff_classifies_matcher_pattern_change_as_hot_config() {
        let registry = BuilderRegistry::standard();
        let mut widget = startup_widget();
        let old = lower(widget.graph(), &registry).unwrap();

        let matcher = widget
            .graph()
            .nodes
            .values()
            .find(|node| node.title == "Match Start")
            .unwrap()
            .id;
        let mut state: nodes::WordMatcherState =
            serde_json::from_value(widget.graph().nodes[&matcher].state.clone()).unwrap();
        state.pattern = node_graph::StringValue::new("0x600082");
        widget.set_node_state(matcher, serde_json::to_value(state).unwrap());

        let new = lower(widget.graph(), &registry).unwrap();
        let edits = diff(&old, &new, &registry).unwrap();
        assert_eq!(edits.len(), 1);
        match &edits[0] {
            LiveEdit::Configure(id, config) => {
                assert_eq!(*id, matcher);
                assert_eq!(config.get("pattern"), Some(&ConfigValue::U64(0x600082)));
            }
            other => panic!("expected Configure, got {other:?}"),
        }
    }

    #[test]
    fn diff_rejects_source_fed_restart() {
        let registry = BuilderRegistry::standard();
        let mut widget = startup_widget();
        let old = lower(widget.graph(), &registry).unwrap();

        // SPI word size has no hot config and the decoder is source-fed.
        let spi = node_by_def(&widget, "SPI Decoder");
        let mut state: nodes::SpiDecoderState =
            serde_json::from_value(widget.graph().nodes[&spi].state.clone()).unwrap();
        state.word_size = node_graph::IntValue::new(16, 1, 32);
        widget.set_node_state(spi, serde_json::to_value(state).unwrap());

        let new = lower(widget.graph(), &registry).unwrap();
        let error = diff(&old, &new, &registry).unwrap_err();
        assert!(error.contains("fed directly by the source"), "{error}");
    }

    /// Wires a new matcher onto the **binary decoder's** words — the one
    /// event branch that stays live for the whole run. The SPI control
    /// branch is index-driven (EdgeQuery) and decodes the entire capture
    /// in seconds, long before the block-streaming path produces its first
    /// capture file — a tap attached to it mid-run would join an
    /// already-closed stream and correctly observe nothing (event streams
    /// don't replay). Mask 0x0 matches every word, so the tap fires as
    /// soon as any enabled window streams data.
    fn attach_matcher_tap(widget: &mut NodeGraphWidget) -> NodeId {
        let matcher = widget
            .add_node_at("Word Matcher", egui::Pos2::new(620.0, 600.0))
            .unwrap();
        let mut state: nodes::WordMatcherState =
            serde_json::from_value(widget.graph().nodes[&matcher].state.clone()).unwrap();
        state.pattern = node_graph::StringValue::new("0x0");
        state.mask = node_graph::StringValue::new("0x0");
        widget.set_node_state(matcher, serde_json::to_value(state).unwrap());

        let decoder = node_by_def(widget, "Binary Decoder");
        let out_idx = |graph: &node_graph::GraphState, id: NodeId, name: &str| {
            graph.nodes[&id]
                .outputs
                .iter()
                .position(|s| s.name == name)
                .unwrap()
        };
        let input_idx = |graph: &node_graph::GraphState, id: NodeId, name: &str| {
            graph.nodes[&id]
                .inputs
                .iter()
                .position(|s| s.name == name && s.visible)
                .unwrap()
        };
        let graph = widget.graph_mut();
        let decoder_words = out_idx(graph, decoder, "Words");
        let matcher_in = input_idx(graph, matcher, "Words");
        graph.add_connection(
            SocketId {
                node: decoder,
                index: decoder_words,
                direction: node_graph::SocketDirection::Output,
            },
            SocketId {
                node: matcher,
                index: matcher_in,
                direction: node_graph::SocketDirection::Input,
            },
        );
        let matcher_out = out_idx(graph, matcher, "Match");
        graph.nodes.get_mut(&matcher).unwrap().outputs[matcher_out].show_in_view = true;
        matcher
    }

    #[test]
    fn diff_classifies_tap_attach_as_add_plus_viewer_restart() {
        let registry = BuilderRegistry::standard();
        let mut widget = startup_widget();
        let old = lower(widget.graph(), &registry).unwrap();

        let matcher = attach_matcher_tap(&mut widget);
        let new = lower(widget.graph(), &registry).unwrap();
        let edits = diff(&old, &new, &registry).unwrap();

        assert!(
            edits
                .iter()
                .any(|edit| matches!(edit, LiveEdit::Add(id) if *id == matcher)),
            "{edits:?}"
        );
        assert!(
            edits
                .iter()
                .any(|edit| matches!(edit, LiveEdit::Restart(id) if *id == AUTO_VIEW_NODE_ID)),
            "{edits:?}"
        );
        assert_eq!(edits.len(), 2, "{edits:?}");
    }

    #[test]
    fn diff_rejects_new_source_connections() {
        let registry = BuilderRegistry::standard();
        let mut widget = startup_widget();
        let old = lower(widget.graph(), &registry).unwrap();

        // New viewer lane fed straight from a source channel: the source's
        // worker threads snapshot destinations at start, so this cannot
        // join live.
        let source = node_by_def(&widget, "DSL File Source");
        // Ch 9 (TGCK) is otherwise unused. Watching it adds a new edge from
        // the already-running source to the synthetic viewer.
        widget.graph_mut().nodes.get_mut(&source).unwrap().outputs[9].show_in_view = true;

        let new = lower(widget.graph(), &registry).unwrap();
        let error = diff(&old, &new, &registry).unwrap_err();
        assert!(
            error.contains("source") || error.contains("block"),
            "{error}"
        );
    }

    fn repo_path(relative: &str) -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .join(relative)
    }

    /// Reference pipeline: the byte-exact Phase-1 wiring of
    /// `examples/spi_graph_decode.rs` (itself verified against the original
    /// `graphs/spi_controlled_decode.json`).
    fn run_reference(capture: &Path, out_dir: &Path) {
        use logic_analyzer_processing::nodes::decoders::{
            CsPolarity, ParallelDecoder, SpiDecoder, SpiMode, StrobeMode,
        };
        use logic_analyzer_processing::{
            DslFileSource, SrLatch, TextFormatter, TriggerCounter, WordMatcher,
        };

        let mut pipeline = Pipeline::new().with_default_buffer_size(10_000_000);
        pipeline
            .add_process("source", DslFileSource::new(capture, 11).unwrap())
            .unwrap();
        pipeline
            .add_process("spi", SpiDecoder::new(SpiMode::Mode0, 24, true, false))
            .unwrap();
        pipeline
            .add_process("start", WordMatcher::new(0x600081, u64::MAX))
            .unwrap();
        pipeline
            .add_process("stop", WordMatcher::new(0x600000, u64::MAX))
            .unwrap();
        pipeline.add_process("latch", SrLatch::new(false)).unwrap();
        pipeline
            .add_process("counter", TriggerCounter::new(0, 1))
            .unwrap();
        pipeline
            .add_process(
                "formatter",
                TextFormatter::new(format!("{}/capture_{{n:04}}.bin", out_dir.display())),
            )
            .unwrap();
        pipeline
            .add_process(
                "decoder",
                ParallelDecoder::new(8, StrobeMode::AnyEdge, CsPolarity::ActiveLow),
            )
            .unwrap();
        pipeline
            .add_process("writer", BinaryFileWriter::new().with_index_csv(true))
            .unwrap();

        pipeline.connect("source", "ch7", "spi", "clk").unwrap();
        pipeline.connect("source", "ch8", "spi", "cs").unwrap();
        pipeline.connect("source", "ch6", "spi", "mosi").unwrap();
        pipeline
            .connect_with_buffer("spi", "mosi_words", "start", "words", 1_000)
            .unwrap();
        pipeline
            .connect_with_buffer("spi", "mosi_words", "stop", "words", 1_000)
            .unwrap();
        pipeline
            .connect_with_buffer("start", "trigger", "latch", "set", 100)
            .unwrap();
        pipeline
            .connect_with_buffer("stop", "trigger", "latch", "reset", 100)
            .unwrap();
        pipeline
            .connect_with_buffer("latch", "q", "decoder", "enable_signal", 100)
            .unwrap();
        pipeline
            .connect_with_buffer("start", "trigger", "counter", "trigger", 100)
            .unwrap();
        pipeline
            .connect_with_buffer("counter", "count", "formatter", "value", 100)
            .unwrap();
        pipeline
            .connect_with_buffer("formatter", "text", "writer", "filename", 100)
            .unwrap();
        pipeline
            .connect_with_buffer("source", "ch10", "decoder", "strobe", 4)
            .unwrap();
        for bit in 0..8 {
            pipeline
                .connect_with_buffer(
                    "source",
                    &format!("ch{bit}"),
                    "decoder",
                    &format!("d{bit}"),
                    4,
                )
                .unwrap();
        }
        // Same channel 8 as `spi.cs` above, negotiated onto a *different*
        // SampleKind (Block, not Edge) for this destination — the mixed-
        // kind fan-out this whole change exists to collapse into one port.
        pipeline
            .connect_with_buffer("source", "ch8", "decoder", "cs", 4)
            .unwrap();
        pipeline
            .connect_with_buffer("decoder", "words", "writer", "data", 100_000)
            .unwrap();

        pipeline.build().unwrap().wait();
    }

    fn run_current_reference(capture: &Path, out_dir: &Path) {
        use logic_analyzer_processing::nodes::decoders::{
            CsPolarity, ParallelDecoder, SpiDecoder, SpiMode, StrobeMode,
        };
        use logic_analyzer_processing::{
            DslFileSource, GateOp, LogicGate, SrLatch, TextFormatter, TriggerCounter, WordMatcher,
        };

        let mut pipeline = Pipeline::new().with_default_buffer_size(10_000_000);
        pipeline
            .add_process("source", DslFileSource::new(capture, 11).unwrap())
            .unwrap();
        pipeline
            .add_process("spi", SpiDecoder::new(SpiMode::Mode0, 24, true, false))
            .unwrap();
        pipeline
            .add_process("start", WordMatcher::new(0x600081, u64::MAX))
            .unwrap();
        pipeline
            .add_process("stop", WordMatcher::new(0x600000, u64::MAX))
            .unwrap();
        pipeline.add_process("latch", SrLatch::new(false)).unwrap();
        pipeline
            .add_process("gate", LogicGate::new(GateOp::And, 2))
            .unwrap();
        pipeline
            .add_process("counter", TriggerCounter::new(0, 1))
            .unwrap();
        pipeline
            .add_process(
                "formatter",
                TextFormatter::new(format!("{}/capture_{{n:04}}.bin", out_dir.display())),
            )
            .unwrap();
        pipeline
            .add_process(
                "decoder",
                ParallelDecoder::new(8, StrobeMode::AnyEdge, CsPolarity::Disabled),
            )
            .unwrap();
        pipeline
            .add_process("writer", BinaryFileWriter::new().with_index_csv(true))
            .unwrap();

        pipeline.connect("source", "ch7", "spi", "clk").unwrap();
        pipeline.connect("source", "ch8", "spi", "cs").unwrap();
        pipeline.connect("source", "ch6", "spi", "mosi").unwrap();
        pipeline
            .connect("spi", "mosi_words", "start", "words")
            .unwrap();
        pipeline
            .connect("spi", "mosi_words", "stop", "words")
            .unwrap();
        pipeline
            .connect("start", "trigger", "latch", "set")
            .unwrap();
        pipeline
            .connect("stop", "trigger", "latch", "reset")
            .unwrap();
        pipeline.connect("source", "ch8", "gate", "in0").unwrap();
        pipeline.connect("latch", "q", "gate", "in1").unwrap();
        pipeline
            .connect("gate", "out", "decoder", "enable_signal")
            .unwrap();
        pipeline
            .connect("start", "trigger", "counter", "trigger")
            .unwrap();
        pipeline
            .connect("counter", "count", "formatter", "value")
            .unwrap();
        pipeline
            .connect("formatter", "text", "writer", "filename")
            .unwrap();
        pipeline
            .connect("source", "ch10", "decoder", "strobe")
            .unwrap();
        for bit in 0..8 {
            pipeline
                .connect("source", &format!("ch{bit}"), "decoder", &format!("d{bit}"))
                .unwrap();
        }
        pipeline
            .connect("decoder", "words", "writer", "data")
            .unwrap();
        pipeline.build().unwrap().wait();
    }

    fn bin_files(dir: &Path) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(dir)
            .unwrap()
            .filter_map(|entry| {
                let name = entry.unwrap().file_name().into_string().unwrap();
                (name.starts_with("capture_") && name.ends_with(".bin")).then_some(name)
            })
            .collect();
        names.sort();
        names
    }

    /// captures.csv rows with the filename column reduced to its basename,
    /// so runs into different directories compare equal.
    fn normalized_csv(dir: &Path) -> Vec<String> {
        std::fs::read_to_string(dir.join("captures.csv"))
            .unwrap()
            .lines()
            .map(|line| {
                line.split(',')
                    .map(|field| field.rsplit('/').next().unwrap_or(field))
                    .collect::<Vec<_>>()
                    .join(",")
            })
            .collect()
    }

    /// Startup graph pointed at `capture` with the writer template in
    /// `out_dir`.
    fn golden_widget(capture: &Path, out_dir: &Path) -> NodeGraphWidget {
        let mut widget = startup_widget();
        let source_id = node_by_def(&widget, "DSL File Source");
        let formatter_id = node_by_def(&widget, "String Formatter");
        widget.set_node_state(
            source_id,
            serde_json::to_value(nodes::DslFileSourceState {
                file: node_graph::FileValue::new(capture.display().to_string()),
                channels: node_graph::IntValue::new(11, 1, 32),
            })
            .unwrap(),
        );
        widget.set_node_state(
            formatter_id,
            serde_json::to_value(nodes::StringFormatterState {
                template: node_graph::StringValue::new(format!(
                    "{}/capture_{{n:04}}.bin",
                    out_dir.display()
                )),
            })
            .unwrap(),
        );
        widget
    }

    /// The live-tap gate: attach a matcher tap mid-run and detach it
    /// again; the untouched writer branch must produce byte-identical
    /// output to an uninterrupted reference run, and the tap must actually
    /// have collected data while attached.
    #[test]
    #[ignore = "runs the full wipneus5.dsl capture; use --release"]
    fn live_attach_detach_preserves_writer_output() {
        let capture = repo_path("_captures/wipneus5.dsl");
        assert!(capture.exists(), "capture not found: {}", capture.display());

        let tmp = tempfile::tempdir().unwrap();
        let graph_dir = tmp.path().join("graph");
        let ref_dir = tmp.path().join("reference");
        std::fs::create_dir_all(&graph_dir).unwrap();
        std::fs::create_dir_all(&ref_dir).unwrap();

        // The reference pipeline is a second, entirely independent full pass
        // over the same multi-billion-sample capture (own process, own
        // output dir) — nothing about it depends on the live-graph run
        // below, so it runs concurrently on its own thread instead of
        // afterward, roughly halving this test's wall-clock time on a
        // machine with room for both.
        let reference_handle = {
            let capture = capture.clone();
            let ref_dir = ref_dir.clone();
            std::thread::spawn(move || run_reference(&capture, &ref_dir))
        };

        let registry = BuilderRegistry::standard();
        let mut widget = golden_widget(&capture, &graph_dir);
        let mut ctx = CompileCtx::default();
        let lanes = ctx.derived_lanes.clone();
        let mut run = start_live(widget.graph(), &registry, &mut ctx)
            .unwrap_or_else(|errors| panic!("compile failed: {errors:?}"));

        // Wait until the pipeline demonstrably produces output, then attach.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(900);
        while bin_files(&graph_dir).is_empty() {
            assert!(!run.is_finished(), "run finished before any capture file");
            assert!(
                std::time::Instant::now() < deadline,
                "no capture file within deadline"
            );
            std::thread::sleep(std::time::Duration::from_millis(200));
        }

        let matcher = attach_matcher_tap(&mut widget);
        let summary = run.apply(widget.graph(), &registry).expect("attach tap");
        assert_eq!(summary.added, 1, "{summary:?}");
        assert_eq!(summary.restarted, 1, "{summary:?}"); // viewer rewired

        // Let the tap observe at least one window, then detach it — poll
        // instead of a fixed sleep so this only takes as long as it
        // actually needs to.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(120);
        loop {
            let observed = lanes.read().iter().any(|lane| {
                lane.name.contains("Word Matcher.Match")
                    && matches!(&lane.data, signal_processing::DerivedLaneData::Markers(markers) if !markers.is_empty())
            });
            if observed {
                break;
            }
            assert!(
                !run.is_finished(),
                "run finished before the tap observed anything"
            );
            assert!(
                std::time::Instant::now() < deadline,
                "tap never observed a trigger within deadline"
            );
            std::thread::sleep(std::time::Duration::from_millis(200));
        }
        widget.graph_mut().remove_node(matcher);
        let summary = run.apply(widget.graph(), &registry).expect("detach tap");
        assert_eq!(summary.removed, 1, "{summary:?}");
        assert_eq!(summary.restarted, 1, "{summary:?}");

        run.wait();
        reference_handle.join().expect("reference run panicked");

        // The writer branch never noticed any of it.
        let graph_files = bin_files(&graph_dir);
        let ref_files = bin_files(&ref_dir);
        assert!(!ref_files.is_empty());
        assert_eq!(graph_files, ref_files, "different file sets");
        for name in &ref_files {
            let a = std::fs::read(graph_dir.join(name)).unwrap();
            let b = std::fs::read(ref_dir.join(name)).unwrap();
            assert_eq!(a, b, "{name} differs");
        }
        assert_eq!(normalized_csv(&graph_dir), normalized_csv(&ref_dir));

        // The tap collected triggers while attached.
        let lanes = lanes.read();
        let tap_lane = lanes
            .iter()
            .find(|lane| lane.name.contains("Word Matcher.Match"))
            .expect("tap lane registered");
        match &tap_lane.data {
            signal_processing::DerivedLaneData::Markers(markers) => {
                assert!(!markers.is_empty(), "tap never fired while attached");
            }
            other => panic!("expected marker lane, got {other:?}"),
        }
    }

    /// Measures the live compiled graph without the golden test's concurrent
    /// reference pass or multi-gigabyte byte-for-byte comparison.
    #[test]
    #[ignore = "runs the full wipneus5.dsl capture; use --release"]
    fn benchmark_compiled_graph_runtime() {
        let capture = repo_path("_captures/wipneus5.dsl");
        assert!(capture.exists(), "capture not found: {}", capture.display());

        let output = tempfile::tempdir().unwrap();
        let widget = golden_widget(&capture, output.path());
        let mut ctx = CompileCtx::default();
        let start = std::time::Instant::now();
        let mut run = start_live(widget.graph(), &BuilderRegistry::standard(), &mut ctx)
            .unwrap_or_else(|errors| panic!("compile failed: {errors:?}"));
        run.wait();
        let elapsed = start.elapsed();
        let files = bin_files(output.path());
        let bytes: u64 = files
            .iter()
            .map(|name| std::fs::metadata(output.path().join(name)).unwrap().len())
            .sum();
        eprintln!(
            "compiled graph: elapsed={:.3}s files={} bytes={bytes}",
            elapsed.as_secs_f64(),
            files.len()
        );
        assert!(!files.is_empty(), "compiled graph produced no output");
    }

    #[test]
    #[ignore = "runs the full wipneus5.dsl capture; use --release"]
    fn benchmark_reference_pipeline_runtime() {
        let capture = repo_path("_captures/wipneus5.dsl");
        assert!(capture.exists(), "capture not found: {}", capture.display());

        let output = tempfile::tempdir().unwrap();
        let start = std::time::Instant::now();
        run_reference(&capture, output.path());
        let elapsed = start.elapsed();
        let files = bin_files(output.path());
        let bytes: u64 = files
            .iter()
            .map(|name| std::fs::metadata(output.path().join(name)).unwrap().len())
            .sum();
        eprintln!(
            "reference pipeline: elapsed={:.3}s files={} bytes={bytes}",
            elapsed.as_secs_f64(),
            files.len()
        );
        assert!(!files.is_empty(), "reference pipeline produced no output");
    }

    #[test]
    #[ignore = "runs the current full pipeline topology; use --release"]
    fn benchmark_current_reference_pipeline_runtime() {
        let capture = repo_path("_captures/wipneus5.dsl");
        let output = tempfile::tempdir().unwrap();
        let start = std::time::Instant::now();
        run_current_reference(&capture, output.path());
        let elapsed = start.elapsed();
        let files = bin_files(output.path());
        let bytes: u64 = files
            .iter()
            .map(|name| std::fs::metadata(output.path().join(name)).unwrap().len())
            .sum();
        eprintln!(
            "current reference: elapsed={:.3}s files={} bytes={bytes}",
            elapsed.as_secs_f64(),
            files.len()
        );
        assert!(!files.is_empty());
    }

    #[test]
    #[ignore = "runs the full checked-in SPI-controlled graph; use --release"]
    fn benchmark_checked_in_spi_controlled_graph_runtime() {
        let capture = repo_path("_captures/wipneus5.dsl");
        let graph_path = repo_path("graphs/spi_controlled_decode.json");
        let graph: GraphState = serde_json::from_str(
            &std::fs::read_to_string(&graph_path)
                .unwrap_or_else(|error| panic!("cannot read {}: {error}", graph_path.display())),
        )
        .unwrap();
        let mut widget = NodeGraphWidget::new(nodes::build_registry());
        widget.set_graph(graph);
        let output = tempfile::tempdir().unwrap();
        for node in widget.graph_mut().nodes.values_mut() {
            match node.def_name() {
                "DSL File Source" => {
                    node.state = serde_json::to_value(nodes::DslFileSourceState {
                        file: node_graph::FileValue::new(capture.display().to_string()),
                        channels: node_graph::IntValue::new(11, 1, 32),
                    })
                    .unwrap();
                }
                "String Formatter" => {
                    node.state = serde_json::to_value(nodes::StringFormatterState {
                        template: node_graph::StringValue::new(format!(
                            "{}/capture_{{n:04}}.bin",
                            output.path().display()
                        )),
                    })
                    .unwrap();
                }
                _ => {}
            }
        }

        let mut ctx = CompileCtx::default();
        let start = std::time::Instant::now();
        let mut run = start_live(widget.graph(), &BuilderRegistry::standard(), &mut ctx)
            .unwrap_or_else(|errors| panic!("compile failed: {errors:?}"));
        run.wait();
        let elapsed = start.elapsed();
        let files = bin_files(output.path());
        let bytes: u64 = files
            .iter()
            .map(|name| std::fs::metadata(output.path().join(name)).unwrap().len())
            .sum();
        eprintln!(
            "checked-in graph: elapsed={:.3}s files={} bytes={bytes}",
            elapsed.as_secs_f64(),
            files.len()
        );
        assert!(!files.is_empty(), "checked-in graph produced no output");
    }

    #[test]
    #[ignore = "runs the full graph while simulating a 60 Hz 5120-pixel viewer; use --release"]
    fn benchmark_checked_in_spi_controlled_graph_with_live_viewer_queries() {
        let capture = repo_path("_captures/wipneus5.dsl");
        let graph_path = repo_path("graphs/spi_controlled_decode.json");
        let graph: GraphState = serde_json::from_str(
            &std::fs::read_to_string(&graph_path)
                .unwrap_or_else(|error| panic!("cannot read {}: {error}", graph_path.display())),
        )
        .unwrap();
        let mut widget = NodeGraphWidget::new(nodes::build_registry());
        widget.set_graph(graph);
        let output = tempfile::tempdir().unwrap();
        for node in widget.graph_mut().nodes.values_mut() {
            match node.def_name() {
                "DSL File Source" => {
                    node.state = serde_json::to_value(nodes::DslFileSourceState {
                        file: node_graph::FileValue::new(capture.display().to_string()),
                        channels: node_graph::IntValue::new(11, 1, 32),
                    })
                    .unwrap();
                }
                "String Formatter" => {
                    node.state = serde_json::to_value(nodes::StringFormatterState {
                        template: node_graph::StringValue::new(format!(
                            "{}/capture_{{n:04}}.bin",
                            output.path().display()
                        )),
                    })
                    .unwrap();
                }
                _ => {}
            }
        }

        const TARGET_POINTS: usize = 5_120;
        const END_NS: u64 = 250_000_000_000;
        let mut ctx = CompileCtx::default();
        let start = Instant::now();
        let mut run = start_live(widget.graph(), &BuilderRegistry::standard(), &mut ctx)
            .unwrap_or_else(|errors| panic!("compile failed: {errors:?}"));
        let mut generations = HashMap::new();
        let mut sampled_at = HashMap::new();
        let mut query_time = Duration::ZERO;
        let mut query_count = 0u64;
        while !run.is_finished() {
            let frame_start = Instant::now();
            let queries: Vec<_> = run
                .lanes
                .read()
                .iter()
                .filter_map(|lane| match &lane.data {
                    DerivedLaneData::IndexedAnnotations(indexed) => {
                        Some((lane.name.clone(), Arc::clone(&indexed.query)))
                    }
                    _ => None,
                })
                .collect();
            for (name, query) in queries {
                let metadata = query.metadata();
                if generations.get(&name) == Some(&metadata.generation) {
                    continue;
                }
                if metadata.is_live
                    && sampled_at.get(&name).is_some_and(|sampled: &Instant| {
                        sampled.elapsed() < Duration::from_millis(50)
                    })
                {
                    continue;
                }
                sampled_at.insert(name.clone(), Instant::now());
                generations.insert(name, metadata.generation);
                let query_start = Instant::now();
                let buckets = query
                    .coarse_presence_window(0, END_NS, TARGET_POINTS)
                    .unwrap();
                let estimated_words = buckets
                    .iter()
                    .map(|bucket| bucket.word_count)
                    .fold(0u64, u64::saturating_add);
                if estimated_words <= (TARGET_POINTS * 2) as u64 {
                    let _ = query.exact_window(0, END_NS, TARGET_POINTS * 2).unwrap();
                }
                query_time += query_start.elapsed();
                query_count += 1;
            }
            let remaining = Duration::from_millis(16).saturating_sub(frame_start.elapsed());
            std::thread::sleep(remaining);
        }
        run.wait();
        eprintln!(
            "live viewer graph: elapsed={:.3}s queries={query_count} query_time={:.3}s",
            start.elapsed().as_secs_f64(),
            query_time.as_secs_f64()
        );
        assert!(query_count > 0, "viewer lane produced no live queries");
        assert!(
            !bin_files(output.path()).is_empty(),
            "checked-in graph produced no output"
        );
    }

    /// The golden correctness gate: the compiled startup graph must
    /// produce byte-identical output to the hand-built Phase-1 pipeline.
    /// Slow (full 12.7B-sample capture) — run explicitly:
    /// `cargo test -p logic-analyzer-graph --release -- --ignored golden`
    #[test]
    #[ignore = "runs the full wipneus5.dsl capture; use --release"]
    fn golden_compiled_graph_matches_reference() {
        let capture = repo_path("_captures/wipneus5.dsl");
        assert!(capture.exists(), "capture not found: {}", capture.display());

        let tmp = tempfile::tempdir().unwrap();
        let graph_dir = tmp.path().join("graph");
        let ref_dir = tmp.path().join("reference");
        std::fs::create_dir_all(&graph_dir).unwrap();
        std::fs::create_dir_all(&ref_dir).unwrap();

        // The reference pipeline is a second, entirely independent full pass
        // over the same multi-billion-sample capture (own process, own
        // output dir) — nothing about it depends on the compiled-graph run
        // below, so it runs concurrently on its own thread instead of
        // afterward, roughly halving this test's wall-clock time on a
        // machine with room for both.
        let reference_handle = {
            let capture = capture.clone();
            let ref_dir = ref_dir.clone();
            std::thread::spawn(move || run_reference(&capture, &ref_dir))
        };

        // Compiled-graph run: startup graph with capture path + output
        // template pointed at the temp dirs.
        let widget = golden_widget(&capture, &graph_dir);

        // Through the live path: shared sender lists + supervisor-driven
        // shutdown must reproduce the offline byte-exact behavior.
        let mut ctx = CompileCtx::default();
        let lanes = ctx.derived_lanes.clone();
        let mut run = start_live(widget.graph(), &BuilderRegistry::standard(), &mut ctx)
            .unwrap_or_else(|errors| panic!("compile failed: {errors:?}"));
        run.wait();

        // The viewer lanes filled while the pipeline ran.
        {
            let lanes = lanes.read();
            assert_eq!(lanes.len(), 5, "expected 5 viewer lanes");
            let annotations = lanes
                .iter()
                .find_map(|lane| match &lane.data {
                    signal_processing::DerivedLaneData::Annotations(a) => Some(a.len()),
                    signal_processing::DerivedLaneData::IndexedAnnotations(indexed) => {
                        Some(indexed.metadata().total_word_count as usize)
                    }
                    _ => None,
                })
                .expect("a words lane");
            assert!(annotations > 0, "words lane stayed empty");
            let markers: usize = lanes
                .iter()
                .filter_map(|lane| match &lane.data {
                    signal_processing::DerivedLaneData::Markers(m) => Some(m.len()),
                    _ => None,
                })
                .sum();
            // 26 windows → at least 26 start + 26 stop triggers.
            assert!(markers >= 52, "expected ≥52 trigger markers, got {markers}");
        }

        reference_handle.join().expect("reference run panicked");

        let graph_files = bin_files(&graph_dir);
        let ref_files = bin_files(&ref_dir);
        assert!(!ref_files.is_empty(), "reference produced no files");
        assert_eq!(graph_files, ref_files, "different file sets");
        for name in &ref_files {
            let a = std::fs::read(graph_dir.join(name)).unwrap();
            let b = std::fs::read(ref_dir.join(name)).unwrap();
            assert_eq!(a, b, "{name} differs");
        }
        assert_eq!(
            normalized_csv(&graph_dir),
            normalized_csv(&ref_dir),
            "captures.csv differs"
        );
    }
}
