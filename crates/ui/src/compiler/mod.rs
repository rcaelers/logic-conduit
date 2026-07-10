//! Graph → Pipeline compiler (`ANALYSIS_PIPELINE_DESIGN.md` §5).
//!
//! Two stages: `lower()` turns the UI graph into a pure, diffable
//! `CompiledGraph` IR (prune to sink-reachable nodes, follow reroutes,
//! validate, negotiate per-edge stream kinds); `start_live()` materializes
//! it into a running [`LiveRun`], the supervisor-driven live path (§6) used
//! by both the app and its own tests — nothing builds an offline `Pipeline`
//! from this IR anymore; that's what `examples/*.rs` do directly against
//! `dsl::Pipeline` for headless/scripted captures.
//!
//! Kind negotiation (§5.4): each edge picks `offered ∩ accepted`, producer
//! preference order winning. That is what maps one UI `Signal` socket onto
//! the source's dual `d{i}`/`b{i}` ports; every `Words` socket carries the
//! same `Word` runtime type regardless of which decoder produced it.

use dsl::DerivedLanes;
use dsl::SampleBlock;
use dsl::runtime::{
    AppManager, DisconnectEvent, InputSub, NodeConfig, OverflowPolicy, ProcessNode,
};
use node_graph::{GraphState, Node, NodeId, NodeKind, Socket, SocketId};
use serde_json::Value;
use std::collections::{BTreeSet, HashMap, HashSet};

mod binary_decoder;
mod buffer;
mod counter;
#[cfg(not(target_arch = "wasm32"))]
mod file_source;
#[cfg(not(target_arch = "wasm32"))]
mod file_writer;
mod formatter;
mod logic_gate;
mod plugin;
mod port_kind;
mod spi_decoder;
mod sr_flip_flop;
#[cfg(not(target_arch = "wasm32"))]
mod text_file_writer;
mod tgck_recorder;
mod uart_decoder;
mod uart_demo_source;
mod viewer;
mod word_matcher;

// ── Stream kinds ─────────────────────────────────────────────────────────────

pub use plugin::PluginContext;
pub use port_kind::{PortKind, PortValue};

// ── Errors, context ──────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct CompileError {
    /// Offending node, for editor badges; `None` for graph-level errors.
    pub node: Option<NodeId>,
    pub message: String,
}

impl CompileError {
    fn on(node: NodeId, message: impl Into<String>) -> Self {
        Self {
            node: Some(node),
            message: message.into(),
        }
    }
    fn global(message: impl Into<String>) -> Self {
        Self {
            node: None,
            message: message.into(),
        }
    }
}

/// Shared resources handed to builders. A fresh `DerivedLanes` store per
/// run makes stale viewer lanes vanish atomically on re-run (§5.5).
#[derive(Default)]
pub struct CompileCtx {
    pub derived_lanes: DerivedLanes,
}

/// What one input edge settled on: the negotiated stream kind plus a
/// human-readable producer label (`"{node title}.{socket}"`, used for
/// viewer lane names).
#[derive(Debug, Clone)]
pub struct ResolvedInput {
    pub kind: PortKind,
    pub source: String,
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
    /// Whether an unconnected input is a compile error (given the state:
    /// e.g. CS is only required while its polarity isn't Disabled).
    fn input_required(&self, _socket: &Socket, _state: &Value) -> bool {
        true
    }
    /// Overrides the policy-table buffer size (§5.3) for this input's
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
    /// apply the whole state without restarting (§6.2 "Prop change" row).
    /// `None` (default) means a state change restarts the node in place.
    fn hot_config(&self, _state: &Value) -> Option<NodeConfig> {
        None
    }
}

pub struct BuilderRegistry(HashMap<String, Box<dyn RuntimeBuilder>>);

impl BuilderRegistry {
    pub fn standard() -> Self {
        let mut builders: HashMap<String, Box<dyn RuntimeBuilder>> = HashMap::new();
        #[cfg(not(target_arch = "wasm32"))]
        builders.insert(
            "DSL File Source".into(),
            Box::new(file_source::FileSourceBuilder),
        );
        builders.insert(
            "UART Demo Source".into(),
            Box::new(uart_demo_source::UartDemoSourceBuilder),
        );
        builders.insert(
            "SPI Decoder".into(),
            Box::new(spi_decoder::SpiDecoderBuilder),
        );
        builders.insert(
            "UART Decoder".into(),
            Box::new(uart_decoder::UartDecoderBuilder),
        );
        builders.insert(
            "Binary Decoder".into(),
            Box::new(binary_decoder::BinaryDecoderBuilder),
        );
        builders.insert(
            "Word Matcher".into(),
            Box::new(word_matcher::WordMatcherBuilder),
        );
        builders.insert(
            "SR Flip-Flop".into(),
            Box::new(sr_flip_flop::SrFlipFlopBuilder),
        );
        builders.insert(
            "Logic Gate".into(),
            Box::new(logic_gate::LogicGateBuilder),
        );
        builders.insert("Buffer".into(), Box::new(buffer::BufferBuilder));
        builders.insert("Counter".into(), Box::new(counter::CounterBuilder));
        builders.insert(
            "String Formatter".into(),
            Box::new(formatter::FormatterBuilder),
        );
        #[cfg(not(target_arch = "wasm32"))]
        builders.insert(
            "File Writer".into(),
            Box::new(file_writer::FileWriterBuilder),
        );
        #[cfg(not(target_arch = "wasm32"))]
        builders.insert(
            "Text File Writer".into(),
            Box::new(text_file_writer::TextFileWriterBuilder),
        );
        builders.insert(
            "TGCK Recorder".into(),
            Box::new(tgck_recorder::TgckRecorderBuilder),
        );
        builders.insert("Viewer".into(), Box::new(viewer::ViewerBuilder));
        Self(builders)
    }

    /// Adds (or overwrites) one builder, keyed the same way `standard()`
    /// keys its own entries — the string must match the corresponding
    /// `NodeDef::name()`. Lets a plugin crate extend the registry `standard()`
    /// builds, without touching `standard()` itself.
    pub fn insert(&mut self, name: impl Into<String>, builder: Box<dyn RuntimeBuilder>) -> &mut Self {
        self.0.insert(name.into(), builder);
        self
    }

    fn get(&self, def_name: &str) -> Option<&dyn RuntimeBuilder> {
        self.0.get(def_name).map(|b| b.as_ref())
    }
}

pub(super) fn parse_state<T: serde::de::DeserializeOwned>(state: &Value) -> Result<T, String> {
    serde_json::from_value(state.clone()).map_err(|e| format!("invalid node state: {e}"))
}

pub(super) fn parse_hex(text: &str) -> Result<u64, String> {
    let trimmed = text.trim();
    let digits = trimmed
        .strip_prefix("0x")
        .or_else(|| trimmed.strip_prefix("0X"))
        .unwrap_or(trimmed);
    u64::from_str_radix(digits, 16).map_err(|_| format!("'{text}' is not a hex value"))
}

// ── IR ───────────────────────────────────────────────────────────────────────

/// Pure description — no threads, no channels. Cheap to rebuild on every
/// edit and cheap to diff (live reconfiguration, §6).
#[derive(Debug, Clone, Default)]
pub struct CompiledGraph {
    pub nodes: Vec<CompiledNode>,
    pub edges: Vec<CompiledEdge>,
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

/// A UI wire with reroutes collapsed away: both endpoints on regular nodes.
struct Wire {
    from: SocketId,
    to: SocketId,
}

fn resolve_reroute_edges(graph: &GraphState) -> Vec<Wire> {
    graph
        .connections
        .iter()
        .filter_map(|connection| {
            let to_node = graph.nodes.get(&connection.to.node)?;
            if to_node.kind == NodeKind::Reroute {
                // Handled when the wire *leaving* the reroute is resolved.
                return None;
            }
            let mut from = connection.from;
            let mut hops = 0;
            while graph.nodes.get(&from.node)?.kind == NodeKind::Reroute {
                let upstream = graph.connections.iter().find(|c| c.to.node == from.node)?;
                from = upstream.from;
                hops += 1;
                if hops > graph.connections.len() {
                    return None; // reroute cycle
                }
            }
            Some(Wire {
                from,
                to: connection.to,
            })
        })
        .collect()
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

pub fn lower(
    graph: &GraphState,
    registry: &BuilderRegistry,
) -> Result<CompiledGraph, Vec<CompileError>> {
    let mut errors: Vec<CompileError> = Vec::new();
    let wires = resolve_reroute_edges(graph);

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
    for &id in &kept {
        let node = &graph.nodes[&id];
        match registry.get(node.def_name()) {
            None => errors.push(CompileError::on(
                id,
                format!("'{}' has no runtime implementation", node.def_name()),
            )),
            Some(builder) if builder.is_source() => source_count += 1,
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
            if !socket.visible || socket.has_control {
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
            }
        })
        .collect();
    Ok(CompiledGraph { nodes, edges })
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

// ── Live pipeline (§6) ───────────────────────────────────────────────────────

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

fn compiled_node<'a>(compiled: &'a CompiledGraph, id: NodeId) -> &'a CompiledNode {
    compiled
        .nodes
        .iter()
        .find(|node| node.id == id)
        .expect("node in compiled graph")
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

#[derive(Debug)]
#[allow(dead_code)] // payloads carried for logs/tests
pub enum ApplyError {
    /// The edited graph does not lower; the running pipeline is untouched.
    Compile(Vec<CompileError>),
    /// The edit class cannot be applied live (§6.2 bottom row); the running
    /// pipeline is untouched — stop and rerun to pick it up.
    NeedsFullRestart(String),
    /// A live edit failed midway (e.g. a node failed to build).
    Apply(String),
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
/// (§6.2). Returns the edit list, or the reason a full restart is needed.
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
                return Err(format!(
                    "new node consumes block channels; block subscriptions cannot join mid-stream"
                ));
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
    /// Set by [`Self::stop`]: the wind-down has been signalled but node
    /// threads may still be finishing their current `work()` call.
    stop_requested: bool,
}

/// Lowers and materializes `graph` under an [`AppManager`] — real OS threads
/// natively, a cooperative single-thread runner on wasm.
pub fn start_live(
    graph: &GraphState,
    registry: &BuilderRegistry,
    ctx: &mut CompileCtx,
) -> Result<LiveRun, Vec<CompileError>> {
    let compiled = lower(graph, registry)?;
    let mut manager = AppManager::new();
    let mut names: HashMap<NodeId, String> = HashMap::new();

    for id in topo_order(&compiled) {
        let node = compiled_node(&compiled, id);
        let builder = registry.get(&node.builder).ok_or_else(|| {
            vec![CompileError::on(
                id,
                format!("unknown builder '{}'", node.builder),
            )]
        })?;
        let process = builder
            .build(&node.runtime_name, &node.state, &node.resolved, ctx)
            .map_err(|message| vec![CompileError::on(id, message)])?;
        let inputs = input_subs(&compiled, id, process.as_ref(), &names)
            .map_err(|message| vec![CompileError::on(id, message)])?;
        manager
            .add_node_deferred(dsl::runtime::NodeSpec {
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
        stop_requested: false,
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
        let new = lower(graph, registry).map_err(ApplyError::Compile)?;
        let edits = diff(&self.compiled, &new, registry).map_err(ApplyError::NeedsFullRestart)?;
        if edits.is_empty() {
            self.compiled = new;
            return Ok(ApplySummary::default());
        }

        let mut ctx = CompileCtx {
            derived_lanes: self.lanes.clone(),
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
                    let process = builder
                        .build(&node.runtime_name, &node.state, &node.resolved, &mut ctx)
                        .map_err(ApplyError::Apply)?;
                    let inputs = input_subs(&new, id, process.as_ref(), &self.names)
                        .map_err(ApplyError::Apply)?;
                    self.manager
                        .add_node(dsl::runtime::NodeSpec {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::nodes;
    use dsl::runtime::{ConfigValue, Pipeline};
    #[cfg(not(target_arch = "wasm32"))]
    use dsl::BinaryFileWriter;
    use dsl::{Sample, Trigger, Word};
    use node_graph::NodeGraphWidget;
    use std::path::{Path, PathBuf};

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

    #[test]
    fn startup_graph_lowers() {
        let widget = startup_widget();
        let compiled = lower(widget.graph(), &BuilderRegistry::standard())
            .unwrap_or_else(|errors| panic!("lower failed: {errors:?}"));

        // Every startup node has a runtime, including the viewer sink and
        // the explicit buffer decoupling the viewer from the file writer's
        // shared decoder output.
        assert_eq!(compiled.nodes.len(), 12);
        assert_eq!(compiled.edges.len(), 29);

        // Viewer lanes resolve with per-lane kinds and producer labels.
        let viewer = compiled
            .nodes
            .iter()
            .find(|n| n.builder == "Viewer")
            .unwrap();
        let lanes = viewer.resolved.members(0);
        assert_eq!(lanes.len(), 5);
        assert_eq!(lanes[0].1.kind, PortKind::of::<Sample>());
        assert!(
            lanes
                .iter()
                .any(|(_, input)| input.kind == PortKind::of::<Word>()
                    && input.source == "Viewer Buffer.Out")
        );
        assert!(lanes.iter().any(
            |(_, input)| input.kind == PortKind::of::<Trigger>() && input.source == "Match Start.Match"
        ));

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
        assert_eq!(decoder.resolved.kind(0), Some(PortKind::of::<SampleBlock>())); // strobe
        assert_eq!(edge_to(decoder.id, "strobe").buffer, 4);
        assert_eq!(edge_to(spi.id, "clk").buffer, 10_000_000);
        assert_eq!(edge_to(decoder.id, "d7").from.1, "ch7");
        assert!(
            compiled
                .edges
                .iter()
                .any(|e| e.to.1 == "enable_signal" && e.buffer == 1_000)
        );
    }

    #[test]
    fn uart_demo_graph_lowers() {
        let widget = uart_demo_widget();
        let compiled = lower(widget.graph(), &BuilderRegistry::standard())
            .unwrap_or_else(|errors| panic!("lower failed: {errors:?}"));

        assert_eq!(compiled.nodes.len(), 3);
        assert_eq!(compiled.edges.len(), 3);
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

    // ── diff classification (§6.2) ───────────────────────────────────────────

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

    /// Wires a fresh Word Matcher (start pattern) into the SPI words stream
    /// and its trigger into the existing viewer; returns the matcher id.
    fn attach_matcher_tap(widget: &mut NodeGraphWidget) -> NodeId {
        let matcher = widget
            .add_node_at("Word Matcher", egui::Pos2::new(620.0, 600.0))
            .unwrap();
        let mut state: nodes::WordMatcherState =
            serde_json::from_value(widget.graph().nodes[&matcher].state.clone()).unwrap();
        state.pattern = node_graph::StringValue::new("0x600081");
        widget.set_node_state(matcher, serde_json::to_value(state).unwrap());

        let spi = node_by_def(widget, "SPI Decoder");
        let viewer = node_by_def(widget, "Viewer");
        let out_idx = |graph: &node_graph::GraphState, id: NodeId, name: &str| {
            graph.nodes[&id]
                .outputs
                .iter()
                .position(|s| s.name == name)
                .unwrap()
        };
        let in_idx = |graph: &node_graph::GraphState, id: NodeId, name: &str| {
            graph.nodes[&id]
                .inputs
                .iter()
                .position(|s| s.name == name && s.visible)
                .unwrap()
        };
        let graph = widget.graph_mut();
        let spi_words = out_idx(graph, spi, "MOSI Words");
        let matcher_in = in_idx(graph, matcher, "Words");
        graph.add_connection(
            SocketId {
                node: spi,
                index: spi_words,
                direction: node_graph::SocketDirection::Output,
            },
            SocketId {
                node: matcher,
                index: matcher_in,
                direction: node_graph::SocketDirection::Input,
            },
        );
        let matcher_out = out_idx(graph, matcher, "Match");
        let viewer_in = in_idx(graph, viewer, "In");
        graph.add_connection(
            SocketId {
                node: matcher,
                index: matcher_out,
                direction: node_graph::SocketDirection::Output,
            },
            SocketId {
                node: viewer,
                index: viewer_in,
                direction: node_graph::SocketDirection::Input,
            },
        );
        matcher
    }

    #[test]
    fn diff_classifies_tap_attach_as_add_plus_viewer_restart() {
        let registry = BuilderRegistry::standard();
        let mut widget = startup_widget();
        let old = lower(widget.graph(), &registry).unwrap();

        let matcher = attach_matcher_tap(&mut widget);
        let viewer = node_by_def(&widget, "Viewer");
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
                .any(|edit| matches!(edit, LiveEdit::Restart(id) if *id == viewer)),
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
        let viewer = node_by_def(&widget, "Viewer");
        let graph = widget.graph_mut();
        let viewer_in = graph.nodes[&viewer]
            .inputs
            .iter()
            .position(|s| s.is_variadic_placeholder())
            .unwrap();
        graph.add_connection(
            SocketId {
                node: source,
                index: 9, // Ch 9 (TGCK), unused elsewhere
                direction: node_graph::SocketDirection::Output,
            },
            SocketId {
                node: viewer,
                index: viewer_in,
                direction: node_graph::SocketDirection::Input,
            },
        );

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
    /// `spi_controlled_decode.rs`).
    fn run_reference(capture: &Path, out_dir: &Path) {
        use dsl::nodes::decoders::{CsPolarity, ParallelDecoder, SpiDecoder, SpiMode, StrobeMode};
        use dsl::{SrLatch, TextFormatter, TriggerCounter, WordMatcher};

        let mut pipeline = Pipeline::new().with_default_buffer_size(10_000_000);
        pipeline
            .add_process("source", dsl::DslFileSource::new(capture, 11).unwrap())
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

    /// The §7 Phase-5 gate: attach a matcher tap mid-run and detach it
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
                    && matches!(&lane.data, dsl::DerivedLaneData::Markers(markers) if !markers.is_empty())
            });
            if observed {
                break;
            }
            assert!(!run.is_finished(), "run finished before the tap observed anything");
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
            dsl::DerivedLaneData::Markers(markers) => {
                assert!(!markers.is_empty(), "tap never fired while attached");
            }
            other => panic!("expected marker lane, got {other:?}"),
        }
    }

    /// The Phase-3 correctness gate (§7): the compiled startup graph must
    /// produce byte-identical output to the hand-built Phase-1 pipeline.
    /// Slow (full 12.7B-sample capture) — run explicitly:
    /// `cargo test -p dsl-ui --release -- --ignored golden`
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
        // shutdown must reproduce the offline byte-exact behavior (§7.5.1).
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
                    dsl::DerivedLaneData::Annotations(a) => Some(a.len()),
                    _ => None,
                })
                .expect("a words lane");
            assert!(annotations > 0, "words lane stayed empty");
            let markers: usize = lanes
                .iter()
                .filter_map(|lane| match &lane.data {
                    dsl::DerivedLaneData::Markers(m) => Some(m.len()),
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
