//! Graph → Pipeline compiler (`ANALYSIS_PIPELINE_DESIGN.md` §5).
//!
//! Two stages: `lower()` turns the UI graph into a pure, diffable
//! `CompiledGraph` IR (prune to sink-reachable nodes, follow reroutes,
//! validate, negotiate per-edge stream kinds); `materialize()` builds the
//! runtime `Pipeline` from the IR and returns a ready `Scheduler`.
//!
//! Kind negotiation (§5.4): each edge picks `offered ∩ accepted`, producer
//! preference order winning. That is what maps one UI `Signal` socket onto
//! the source's dual `d{i}`/`b{i}` ports and one `Words` socket onto
//! `SpiTransfer` vs `ParallelWord` consumers.

use dsl::nodes::decoders::{BitOrder, Endianness, UartParity, UartStopBits};
use dsl::runtime::{
    ConfigValue, DisconnectEvent, InputSub, NodeConfig, OverflowPolicy, Pipeline, PipelineManager,
    ProcessNode, Scheduler, StopHandle,
};
use dsl::{
    BinaryFileWriter, CsPolarity, DerivedLanes, GateOp, LogicGate, MatchOp, ParallelWord,
    SpiDecoder, SpiMode, SpiTransfer, SrLatch, StrobeMode, TextFormatter, TriggerCounter,
    ViewerLaneKind, ViewerSink, WordField, WordMatcher, WriteWidth,
};
use node_graph::{GraphState, Node, NodeId, NodeKind, Socket, SocketId};
use serde_json::Value;
use std::collections::{BTreeSet, HashMap, HashSet};

use crate::nodes;

// ── Stream kinds ─────────────────────────────────────────────────────────────

/// How a UI socket maps onto runtime channel payloads.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PortKind {
    /// `Sample` edge stream (raw channels, gates, enables).
    SampleEdge,
    /// `SampleBlock` stream (binary decoder inputs).
    Block,
    /// `SpiTransfer` events.
    SpiWords,
    /// `ParallelWord` events.
    ParallelWords,
    /// `Trigger` events.
    Trigger,
    /// `NumberSample` level.
    Number,
    /// `TextSample` level.
    Text,
}

/// Buffer policy (§5.3), keyed on the edge's kind; for `SampleEdge` the
/// producer decides raw-channel vs control sizing.
fn buffer_size(kind: PortKind, producer_is_source: bool) -> usize {
    match kind {
        PortKind::Block => 4,
        PortKind::SampleEdge => {
            if producer_is_source {
                10_000_000
            } else {
                1_000
            }
        }
        PortKind::SpiWords => 1_000,
        PortKind::ParallelWords => 100_000,
        PortKind::Trigger | PortKind::Number | PortKind::Text => 100,
    }
}

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
        builders.insert("DSL File Source".into(), Box::new(FileSourceBuilder));
        builders.insert("SPI Decoder".into(), Box::new(SpiDecoderBuilder));
        builders.insert("UART Decoder".into(), Box::new(UartDecoderBuilder));
        builders.insert("Binary Decoder".into(), Box::new(BinaryDecoderBuilder));
        builders.insert("Word Matcher".into(), Box::new(WordMatcherBuilder));
        builders.insert("SR Flip-Flop".into(), Box::new(SrFlipFlopBuilder));
        builders.insert("Logic Gate".into(), Box::new(LogicGateBuilder));
        builders.insert("Counter".into(), Box::new(CounterBuilder));
        builders.insert("String Formatter".into(), Box::new(FormatterBuilder));
        builders.insert("File Writer".into(), Box::new(FileWriterBuilder));
        builders.insert("TGCK Recorder".into(), Box::new(TgckRecorderBuilder));
        builders.insert("Viewer".into(), Box::new(ViewerBuilder));
        Self(builders)
    }

    fn get(&self, def_name: &str) -> Option<&dyn RuntimeBuilder> {
        self.0.get(def_name).map(|b| b.as_ref())
    }
}

fn parse_state<T: serde::de::DeserializeOwned>(state: &Value) -> Result<T, String> {
    serde_json::from_value(state.clone()).map_err(|e| format!("invalid node state: {e}"))
}

fn parse_hex(text: &str) -> Result<u64, String> {
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
                let upstream = graph
                    .connections
                    .iter()
                    .find(|c| c.to.node == from.node)?;
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

pub fn lower(graph: &GraphState, registry: &BuilderRegistry) -> Result<CompiledGraph, Vec<CompileError>> {
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
            buffer: buffer_size(kind, from_builder.is_source()),
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

// ── Stage 2: materialize (offline `Pipeline` path) ───────────────────────────

/// Builds the classic thread-per-node `Pipeline` from the IR. The UI uses
/// the live path ([`start_live`]); this stays as the §5.2 offline
/// materializer for headless/scripted use.
#[allow(dead_code)]
pub fn materialize(
    compiled: &CompiledGraph,
    registry: &BuilderRegistry,
    ctx: &mut CompileCtx,
) -> Result<Scheduler, CompileError> {
    let mut pipeline = Pipeline::new();
    let mut names: HashMap<NodeId, &str> = HashMap::new();

    for node in &compiled.nodes {
        let builder = registry
            .get(&node.builder)
            .ok_or_else(|| CompileError::on(node.id, format!("unknown builder '{}'", node.builder)))?;
        let process = builder
            .build(&node.runtime_name, &node.state, &node.resolved, ctx)
            .map_err(|message| CompileError::on(node.id, message))?;
        pipeline
            .add_process(node.runtime_name.clone(), process)
            .map_err(|message| CompileError::on(node.id, message))?;
        names.insert(node.id, &node.runtime_name);
    }

    for edge in &compiled.edges {
        pipeline
            .connect_with_buffer(
                names[&edge.from.0],
                &edge.from.1,
                names[&edge.to.0],
                &edge.to.1,
                edge.buffer,
            )
            .map_err(|e| CompileError::on(edge.to.0, e.to_string()))?;
    }

    pipeline.build().map_err(CompileError::global)
}

// Silence the unused-import lint for the offline-only types above.
#[allow(dead_code)]
fn _offline_types(_: &Scheduler, _: &StopHandle) {}

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
            if edge.kind == PortKind::Block {
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
            if edge.kind == PortKind::Block {
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
    manager: PipelineManager,
    compiled: CompiledGraph,
    /// Supervisor key per UI node — assigned at add time and stable across
    /// title renames and in-place restarts.
    names: HashMap<NodeId, String>,
    lanes: DerivedLanes,
}

/// Lowers and materializes `graph` under a `PipelineManager`.
pub fn start_live(
    graph: &GraphState,
    registry: &BuilderRegistry,
    ctx: &mut CompileCtx,
) -> Result<LiveRun, Vec<CompileError>> {
    let compiled = lower(graph, registry)?;
    let mut manager = PipelineManager::new();
    let mut names: HashMap<NodeId, String> = HashMap::new();

    for id in topo_order(&compiled) {
        let node = compiled_node(&compiled, id);
        let builder = registry
            .get(&node.builder)
            .ok_or_else(|| vec![CompileError::on(id, format!("unknown builder '{}'", node.builder))])?;
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
                        self.manager
                            .remove_node(&name)
                            .map_err(ApplyError::Apply)?;
                    }
                    summary.removed += 1;
                }
                LiveEdit::Add(id) => {
                    let node = compiled_node(&new, id);
                    let builder = registry
                        .get(&node.builder)
                        .ok_or_else(|| ApplyError::Apply(format!("no builder '{}'", node.builder)))?;
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
                    let builder = registry
                        .get(&node.builder)
                        .ok_or_else(|| ApplyError::Apply(format!("no builder '{}'", node.builder)))?;
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

    pub fn stop(&mut self) {
        self.manager.stop_all();
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

// ── Builders ─────────────────────────────────────────────────────────────────

struct FileSourceBuilder;

impl RuntimeBuilder for FileSourceBuilder {
    fn is_source(&self) -> bool {
        true
    }
    fn accepted_kinds(&self, _socket: &Socket, _state: &Value) -> Vec<PortKind> {
        vec![]
    }
    fn offered_kinds(&self, _socket: &Socket, _state: &Value) -> Vec<PortKind> {
        vec![PortKind::SampleEdge, PortKind::Block]
    }
    fn input_port(&self, _: &Socket, _: usize, _: &Value, _: PortKind) -> Option<String> {
        None
    }
    fn output_port(&self, socket: &Socket, _state: &Value, kind: PortKind) -> Option<String> {
        let channel = socket.def_index;
        match kind {
            PortKind::SampleEdge => Some(format!("d{channel}")),
            PortKind::Block => Some(format!("b{channel}")),
            _ => None,
        }
    }
    fn input_required(&self, _: &Socket, _: &Value) -> bool {
        false
    }
    fn build(
        &self,
        name: &str,
        state: &Value,
        _resolved: &ResolvedInputs,
        _ctx: &mut CompileCtx,
    ) -> Result<Box<dyn ProcessNode>, String> {
        let state: nodes::DslFileSourceState = parse_state(state)?;
        let channels = state.channels.value.clamp(1, 32) as u8;
        let source = dsl::DslFileSource::new(&state.file.value, channels)
            .map_err(|e| format!("cannot open '{}': {e}", state.file.value))?
            .with_name(name);
        Ok(Box::new(source))
    }
}

struct SpiDecoderBuilder;

impl SpiDecoderBuilder {
    fn parsed(state: &Value) -> Result<nodes::SpiDecoderState, String> {
        parse_state(state)
    }
    fn cs_polarity(state: &nodes::SpiDecoderState) -> CsPolarity {
        match state.cs_polarity.selected() {
            "Active high" => CsPolarity::ActiveHigh,
            "Disabled" => CsPolarity::Disabled,
            _ => CsPolarity::ActiveLow,
        }
    }
}

impl RuntimeBuilder for SpiDecoderBuilder {
    fn accepted_kinds(&self, _socket: &Socket, _state: &Value) -> Vec<PortKind> {
        vec![PortKind::SampleEdge]
    }
    fn offered_kinds(&self, _socket: &Socket, _state: &Value) -> Vec<PortKind> {
        vec![PortKind::SpiWords]
    }
    fn input_port(&self, socket: &Socket, _: usize, _: &Value, _: PortKind) -> Option<String> {
        match socket.def_index {
            0 => Some("clk".into()),
            1 => Some("mosi".into()),
            2 => Some("miso".into()),
            3 => Some("cs".into()),
            _ => None,
        }
    }
    fn output_port(&self, _socket: &Socket, _state: &Value, kind: PortKind) -> Option<String> {
        // Both UI word outputs map to the single transfer stream; the
        // MOSI/MISO split is the consumer's field selection (§4.2).
        (kind == PortKind::SpiWords).then(|| "spi_transfers".into())
    }
    fn input_required(&self, socket: &Socket, state: &Value) -> bool {
        let Ok(state) = Self::parsed(state) else {
            return true;
        };
        match socket.def_index {
            2 => state.has_miso.value,
            3 => Self::cs_polarity(&state) != CsPolarity::Disabled,
            _ => true,
        }
    }
    fn build(
        &self,
        name: &str,
        state: &Value,
        _resolved: &ResolvedInputs,
        _ctx: &mut CompileCtx,
    ) -> Result<Box<dyn ProcessNode>, String> {
        let state = Self::parsed(state)?;
        let mode = match (state.cpol.selected(), state.cpha.selected()) {
            ("0", "0") => SpiMode::Mode0,
            ("0", "1") => SpiMode::Mode1,
            ("1", "0") => SpiMode::Mode2,
            ("1", "1") => SpiMode::Mode3,
            _ => return Err("invalid CPOL/CPHA".into()),
        };
        let bit_order = if state.bit_order.selected() == "LSB first" {
            BitOrder::LsbFirst
        } else {
            BitOrder::MsbFirst
        };
        let decoder = SpiDecoder::with_cs_polarity(
            mode,
            state.word_size.value.clamp(1, 32) as usize,
            true,
            state.has_miso.value,
            Self::cs_polarity(&state),
        )
        .with_bit_order(bit_order)
        .with_name(name);
        Ok(Box::new(decoder))
    }
}

struct UartDecoderBuilder;

impl RuntimeBuilder for UartDecoderBuilder {
    fn accepted_kinds(&self, _socket: &Socket, _state: &Value) -> Vec<PortKind> {
        vec![PortKind::SampleEdge]
    }
    fn offered_kinds(&self, socket: &Socket, _state: &Value) -> Vec<PortKind> {
        match socket.def_index {
            0 => vec![PortKind::ParallelWords],
            1 => vec![PortKind::Trigger],
            _ => vec![],
        }
    }
    fn input_port(&self, socket: &Socket, _: usize, _: &Value, _: PortKind) -> Option<String> {
        (socket.def_index == 0).then(|| "rx".into())
    }
    fn output_port(&self, socket: &Socket, _state: &Value, _kind: PortKind) -> Option<String> {
        match socket.def_index {
            0 => Some("words".into()),
            1 => Some("error".into()),
            _ => None,
        }
    }
    fn input_required(&self, socket: &Socket, _state: &Value) -> bool {
        socket.def_index == 0
    }
    fn build(
        &self,
        name: &str,
        state: &Value,
        _resolved: &ResolvedInputs,
        _ctx: &mut CompileCtx,
    ) -> Result<Box<dyn ProcessNode>, String> {
        let state: nodes::UartDecoderState = parse_state(state)?;
        let parity = match state.parity.selected() {
            "Odd" => UartParity::Odd,
            "Even" => UartParity::Even,
            "Mark" => UartParity::Mark,
            "Space" => UartParity::Space,
            _ => UartParity::None,
        };
        let stop_bits = match state.stop_bits.selected() {
            "0" => UartStopBits::S0,
            "0.5" => UartStopBits::S0_5,
            "1.5" => UartStopBits::S1_5,
            "2" => UartStopBits::S2,
            _ => UartStopBits::S1,
        };
        let bit_order = if state.bit_order.selected() == "MSB first" {
            BitOrder::MsbFirst
        } else {
            BitOrder::LsbFirst
        };
        let decoder = dsl::nodes::decoders::UartDecoder::new(
            state.baud_rate.value.max(1) as u64,
            state.data_bits.value.clamp(5, 9) as usize,
        )
        .with_parity(parity, state.check_parity.value)
        .with_stop_bits(stop_bits)
        .with_bit_order(bit_order)
        .with_invert(state.invert.value)
        .with_name(name);
        Ok(Box::new(decoder))
    }
}

struct BinaryDecoderBuilder;

impl BinaryDecoderBuilder {
    fn parsed(state: &Value) -> Result<nodes::BinaryDecoderState, String> {
        parse_state(state)
    }
    fn cs_polarity(state: &nodes::BinaryDecoderState) -> CsPolarity {
        match state.cs_polarity.selected() {
            "Active low" => CsPolarity::ActiveLow,
            "Active high" => CsPolarity::ActiveHigh,
            _ => CsPolarity::Disabled,
        }
    }
}

impl RuntimeBuilder for BinaryDecoderBuilder {
    fn accepted_kinds(&self, socket: &Socket, _state: &Value) -> Vec<PortKind> {
        match socket.def_index {
            3 => vec![PortKind::SampleEdge], // Enable is a level stream
            _ => vec![PortKind::Block],      // Clock, D group, CS read blocks
        }
    }
    fn offered_kinds(&self, _socket: &Socket, _state: &Value) -> Vec<PortKind> {
        vec![PortKind::ParallelWords]
    }
    fn input_port(
        &self,
        socket: &Socket,
        member_index: usize,
        _state: &Value,
        _kind: PortKind,
    ) -> Option<String> {
        match socket.def_index {
            0 => Some("strobe".into()),
            1 => Some(format!("d{member_index}")),
            2 => Some("cs".into()),
            3 => Some("enable_signal".into()),
            _ => None,
        }
    }
    fn output_port(&self, _socket: &Socket, _state: &Value, kind: PortKind) -> Option<String> {
        (kind == PortKind::ParallelWords).then(|| "words".into())
    }
    fn input_required(&self, socket: &Socket, state: &Value) -> bool {
        match socket.def_index {
            2 => Self::parsed(state)
                .map(|s| Self::cs_polarity(&s) != CsPolarity::Disabled)
                .unwrap_or(false),
            3 => false, // unconnected Enable = always enabled
            _ => true,
        }
    }
    fn build(
        &self,
        name: &str,
        state: &Value,
        resolved: &ResolvedInputs,
        _ctx: &mut CompileCtx,
    ) -> Result<Box<dyn ProcessNode>, String> {
        let state = Self::parsed(state)?;
        let data_bits = resolved.member_count(1);
        if data_bits == 0 {
            return Err("no data channels connected".into());
        }
        let strobe_mode = match state.sample_on.selected() {
            "Falling (SDR)" => StrobeMode::FallingEdge,
            "Both (DDR)" => StrobeMode::AnyEdge,
            "High level" => StrobeMode::HighLevel,
            "Low level" => StrobeMode::LowLevel,
            _ => StrobeMode::RisingEdge,
        };
        let mut decoder =
            dsl::ParallelDecoder::new(data_bits, strobe_mode, Self::cs_polarity(&state))
                .with_name(name);
        let cycles = state.word_size.value.clamp(1, 8) as usize;
        if cycles > 1 {
            let endianness = if state.endianness.selected() == "Big" {
                Endianness::Big
            } else {
                Endianness::Little
            };
            decoder = decoder.with_word_assembly(cycles, endianness);
        }
        Ok(Box::new(decoder))
    }
}

struct WordMatcherBuilder;

impl WordMatcherBuilder {
    /// UI op glyph → runtime `MatchOp` and its config wire name.
    fn match_op(selected: &str) -> (MatchOp, &'static str) {
        match selected {
            "≠" => (MatchOp::Ne, "ne"),
            "<" => (MatchOp::Lt, "lt"),
            "≤" => (MatchOp::Le, "le"),
            ">" => (MatchOp::Gt, "gt"),
            "≥" => (MatchOp::Ge, "ge"),
            _ => (MatchOp::Eq, "eq"),
        }
    }
}

impl RuntimeBuilder for WordMatcherBuilder {
    fn accepted_kinds(&self, _socket: &Socket, _state: &Value) -> Vec<PortKind> {
        vec![PortKind::SpiWords, PortKind::ParallelWords]
    }
    fn offered_kinds(&self, socket: &Socket, _state: &Value) -> Vec<PortKind> {
        match socket.def_index {
            0 => vec![PortKind::Trigger],
            1 => vec![PortKind::SampleEdge],
            _ => vec![],
        }
    }
    fn input_port(&self, socket: &Socket, _: usize, _: &Value, _: PortKind) -> Option<String> {
        (socket.def_index == 0).then(|| "words".into())
    }
    fn output_port(&self, socket: &Socket, _state: &Value, _kind: PortKind) -> Option<String> {
        match socket.def_index {
            0 => Some("trigger".into()),
            1 => Some("matched".into()),
            _ => None,
        }
    }
    fn build(
        &self,
        name: &str,
        state: &Value,
        resolved: &ResolvedInputs,
        _ctx: &mut CompileCtx,
    ) -> Result<Box<dyn ProcessNode>, String> {
        let state: nodes::WordMatcherState = parse_state(state)?;
        let pattern = parse_hex(&state.pattern.value)?;
        let mask = parse_hex(&state.mask.value)?;
        let (op, _) = Self::match_op(state.op.selected());
        let field = if state.field.selected() == "MISO" {
            WordField::Miso
        } else {
            WordField::Mosi
        };
        // The words input kind picks the concrete consumer type (§5.4).
        match resolved.kind(0) {
            Some(PortKind::SpiWords) => Ok(Box::new(
                WordMatcher::<SpiTransfer>::new(pattern, mask)
                    .with_field(field)
                    .with_op(op)
                    .with_name(name),
            )),
            Some(PortKind::ParallelWords) => Ok(Box::new(
                WordMatcher::<ParallelWord>::new(pattern, mask)
                    .with_field(field)
                    .with_op(op)
                    .with_name(name),
            )),
            _ => Err("words input is not connected".into()),
        }
    }

    fn hot_config(&self, state: &Value) -> Option<NodeConfig> {
        let state: nodes::WordMatcherState = parse_state(state).ok()?;
        let mut config = NodeConfig::new();
        config.insert(
            "pattern".into(),
            ConfigValue::U64(parse_hex(&state.pattern.value).ok()?),
        );
        config.insert(
            "mask".into(),
            ConfigValue::U64(parse_hex(&state.mask.value).ok()?),
        );
        let (_, op_name) = Self::match_op(state.op.selected());
        config.insert("op".into(), ConfigValue::Text(op_name.into()));
        config.insert(
            "field".into(),
            ConfigValue::Text(if state.field.selected() == "MISO" {
                "miso".into()
            } else {
                "mosi".into()
            }),
        );
        // The pulse-output toggle only affects UI socket visibility.
        Some(config)
    }
}

struct SrFlipFlopBuilder;

impl RuntimeBuilder for SrFlipFlopBuilder {
    fn accepted_kinds(&self, _socket: &Socket, _state: &Value) -> Vec<PortKind> {
        vec![PortKind::Trigger]
    }
    fn offered_kinds(&self, _socket: &Socket, _state: &Value) -> Vec<PortKind> {
        vec![PortKind::SampleEdge]
    }
    fn input_port(&self, socket: &Socket, _: usize, _: &Value, _: PortKind) -> Option<String> {
        match socket.def_index {
            0 => Some("set".into()),
            1 => Some("reset".into()),
            _ => None,
        }
    }
    fn output_port(&self, _socket: &Socket, _state: &Value, _kind: PortKind) -> Option<String> {
        Some("q".into())
    }
    fn build(
        &self,
        name: &str,
        state: &Value,
        _resolved: &ResolvedInputs,
        _ctx: &mut CompileCtx,
    ) -> Result<Box<dyn ProcessNode>, String> {
        let state: nodes::SrFlipFlopState = parse_state(state)?;
        Ok(Box::new(SrLatch::new(state.initial.value).with_name(name)))
    }
}

struct LogicGateBuilder;

impl RuntimeBuilder for LogicGateBuilder {
    fn accepted_kinds(&self, _socket: &Socket, _state: &Value) -> Vec<PortKind> {
        vec![PortKind::SampleEdge]
    }
    fn offered_kinds(&self, _socket: &Socket, _state: &Value) -> Vec<PortKind> {
        vec![PortKind::SampleEdge]
    }
    fn input_port(
        &self,
        _socket: &Socket,
        member_index: usize,
        _state: &Value,
        _kind: PortKind,
    ) -> Option<String> {
        Some(format!("in{member_index}"))
    }
    fn output_port(&self, _socket: &Socket, _state: &Value, _kind: PortKind) -> Option<String> {
        Some("out".into())
    }
    fn build(
        &self,
        name: &str,
        state: &Value,
        resolved: &ResolvedInputs,
        _ctx: &mut CompileCtx,
    ) -> Result<Box<dyn ProcessNode>, String> {
        let state: nodes::LogicGateState = parse_state(state)?;
        let inputs = resolved.member_count(0);
        if inputs == 0 {
            return Err("no inputs connected".into());
        }
        let op = match state.op.selected() {
            "NOT" => GateOp::Not,
            "NAND" => GateOp::Nand,
            "OR" => GateOp::Or,
            "NOR" => GateOp::Nor,
            "XOR" => GateOp::Xor,
            "XNOR" => GateOp::Xnor,
            _ => GateOp::And,
        };
        if op == GateOp::Not && inputs != 1 {
            return Err("NOT takes exactly one input".into());
        }
        Ok(Box::new(LogicGate::new(op, inputs).with_name(name)))
    }
}

struct CounterBuilder;

impl RuntimeBuilder for CounterBuilder {
    fn accepted_kinds(&self, _socket: &Socket, _state: &Value) -> Vec<PortKind> {
        vec![PortKind::Trigger]
    }
    fn offered_kinds(&self, _socket: &Socket, _state: &Value) -> Vec<PortKind> {
        vec![PortKind::Number]
    }
    fn input_port(&self, _: &Socket, _: usize, _: &Value, _: PortKind) -> Option<String> {
        Some("trigger".into())
    }
    fn output_port(&self, _socket: &Socket, _state: &Value, _kind: PortKind) -> Option<String> {
        Some("count".into())
    }
    fn build(
        &self,
        name: &str,
        state: &Value,
        _resolved: &ResolvedInputs,
        _ctx: &mut CompileCtx,
    ) -> Result<Box<dyn ProcessNode>, String> {
        let state: nodes::CounterState = parse_state(state)?;
        Ok(Box::new(
            TriggerCounter::new(state.start.value as i64, state.step.value as i64)
                .with_name(name),
        ))
    }
}

struct FormatterBuilder;

impl RuntimeBuilder for FormatterBuilder {
    fn accepted_kinds(&self, _socket: &Socket, _state: &Value) -> Vec<PortKind> {
        vec![PortKind::Number]
    }
    fn offered_kinds(&self, _socket: &Socket, _state: &Value) -> Vec<PortKind> {
        vec![PortKind::Text]
    }
    fn input_port(
        &self,
        _socket: &Socket,
        member_index: usize,
        _: &Value,
        _: PortKind,
    ) -> Option<String> {
        // First value keeps the historic port name.
        Some(if member_index == 0 {
            "value".into()
        } else {
            format!("value{member_index}")
        })
    }
    fn output_port(&self, _socket: &Socket, _state: &Value, _kind: PortKind) -> Option<String> {
        Some("text".into())
    }
    fn build(
        &self,
        name: &str,
        state: &Value,
        resolved: &ResolvedInputs,
        _ctx: &mut CompileCtx,
    ) -> Result<Box<dyn ProcessNode>, String> {
        let state: nodes::StringFormatterState = parse_state(state)?;
        let values = resolved.member_count(0).max(1);
        Ok(Box::new(
            TextFormatter::with_num_values(state.template.value.clone(), values).with_name(name),
        ))
    }

    fn hot_config(&self, state: &Value) -> Option<NodeConfig> {
        let state: nodes::StringFormatterState = parse_state(state).ok()?;
        let mut config = NodeConfig::new();
        config.insert(
            "template".into(),
            ConfigValue::Text(state.template.value.clone()),
        );
        Some(config)
    }
}

struct FileWriterBuilder;

impl RuntimeBuilder for FileWriterBuilder {
    fn is_sink(&self) -> bool {
        true
    }
    fn accepted_kinds(&self, socket: &Socket, _state: &Value) -> Vec<PortKind> {
        match socket.def_index {
            0 => vec![PortKind::ParallelWords],
            1 => vec![PortKind::Text],
            _ => vec![],
        }
    }
    fn offered_kinds(&self, _socket: &Socket, _state: &Value) -> Vec<PortKind> {
        vec![]
    }
    fn input_port(&self, socket: &Socket, _: usize, _: &Value, _: PortKind) -> Option<String> {
        match socket.def_index {
            0 => Some("data".into()),
            1 => Some("filename".into()),
            _ => None,
        }
    }
    fn output_port(&self, _: &Socket, _: &Value, _: PortKind) -> Option<String> {
        None
    }
    fn build(
        &self,
        name: &str,
        state: &Value,
        _resolved: &ResolvedInputs,
        _ctx: &mut CompileCtx,
    ) -> Result<Box<dyn ProcessNode>, String> {
        let state: nodes::FileWriterState = parse_state(state)?;
        let width = match state.write_width.selected() {
            "U16 LE" => WriteWidth::U16Le,
            "U32 LE" => WriteWidth::U32Le,
            _ => WriteWidth::U8,
        };
        Ok(Box::new(
            BinaryFileWriter::new()
                .with_width(width)
                .with_index_csv(state.index_csv.value)
                .with_name(name),
        ))
    }
}

struct TgckRecorderBuilder;

impl RuntimeBuilder for TgckRecorderBuilder {
    fn is_sink(&self) -> bool {
        true
    }
    fn accepted_kinds(&self, socket: &Socket, _state: &Value) -> Vec<PortKind> {
        match socket.def_index {
            0 => vec![PortKind::ParallelWords],
            1 => vec![PortKind::SampleEdge],
            2 => vec![PortKind::Text],
            _ => vec![],
        }
    }
    fn offered_kinds(&self, _socket: &Socket, _state: &Value) -> Vec<PortKind> {
        vec![]
    }
    fn input_port(&self, socket: &Socket, _: usize, _: &Value, _: PortKind) -> Option<String> {
        match socket.def_index {
            0 => Some("words".into()),
            1 => Some("tgck".into()),
            2 => Some("filename".into()),
            _ => None,
        }
    }
    fn output_port(&self, _: &Socket, _: &Value, _: PortKind) -> Option<String> {
        None
    }
    fn build(
        &self,
        name: &str,
        _state: &Value,
        _resolved: &ResolvedInputs,
        _ctx: &mut CompileCtx,
    ) -> Result<Box<dyn ProcessNode>, String> {
        Ok(Box::new(dsl::TgckRecorder::new().with_name(name)))
    }
}

struct ViewerBuilder;

impl RuntimeBuilder for ViewerBuilder {
    fn is_sink(&self) -> bool {
        true
    }
    fn accepted_kinds(&self, _socket: &Socket, _state: &Value) -> Vec<PortKind> {
        vec![
            PortKind::SampleEdge,
            PortKind::SpiWords,
            PortKind::ParallelWords,
            PortKind::Trigger,
        ]
    }
    fn offered_kinds(&self, _socket: &Socket, _state: &Value) -> Vec<PortKind> {
        vec![]
    }
    fn input_port(
        &self,
        _socket: &Socket,
        member_index: usize,
        _state: &Value,
        _kind: PortKind,
    ) -> Option<String> {
        Some(format!("in{member_index}"))
    }
    fn output_port(&self, _: &Socket, _: &Value, _: PortKind) -> Option<String> {
        None
    }
    fn input_required(&self, _: &Socket, _: &Value) -> bool {
        // A lane-less viewer is pointless but harmless.
        false
    }
    fn build(
        &self,
        name: &str,
        state: &Value,
        resolved: &ResolvedInputs,
        ctx: &mut CompileCtx,
    ) -> Result<Box<dyn ProcessNode>, String> {
        let state: nodes::ViewerState = parse_state(state)?;
        let prefix = state.label.value.trim().to_owned();
        let mut sink = ViewerSink::new(ctx.derived_lanes.clone()).with_name(name);
        for (_, input) in resolved.members(0) {
            let kind = match input.kind {
                PortKind::SampleEdge => ViewerLaneKind::Signal,
                PortKind::SpiWords => ViewerLaneKind::SpiWords,
                PortKind::ParallelWords => ViewerLaneKind::ParallelWords,
                PortKind::Trigger => ViewerLaneKind::Trigger,
                other => return Err(format!("viewer cannot display {other:?}")),
            };
            let lane_name = if prefix.is_empty() {
                input.source.clone()
            } else {
                format!("{prefix}: {}", input.source)
            };
            sink = sink.with_lane(kind, lane_name);
        }
        Ok(Box::new(sink))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::nodes;
    use node_graph::NodeGraphWidget;
    use std::path::{Path, PathBuf};

    fn startup_widget() -> NodeGraphWidget {
        let mut widget = NodeGraphWidget::new(nodes::build_registry());
        nodes::populate_startup(&mut widget);
        widget
    }

    #[test]
    fn startup_graph_lowers() {
        let widget = startup_widget();
        let compiled = lower(widget.graph(), &BuilderRegistry::standard())
            .unwrap_or_else(|errors| panic!("lower failed: {errors:?}"));

        // Every startup node has a runtime, including the viewer sink.
        assert_eq!(compiled.nodes.len(), 11);
        assert_eq!(compiled.edges.len(), 28);

        // Viewer lanes resolve with per-lane kinds and producer labels.
        let viewer = compiled
            .nodes
            .iter()
            .find(|n| n.builder == "Viewer")
            .unwrap();
        let lanes = viewer.resolved.members(0);
        assert_eq!(lanes.len(), 5);
        assert_eq!(lanes[0].1.kind, PortKind::SampleEdge);
        assert!(
            lanes.iter().any(|(_, input)| input.kind == PortKind::ParallelWords
                && input.source == "Binary Decoder.Words")
        );
        assert!(
            lanes
                .iter()
                .any(|(_, input)| input.kind == PortKind::Trigger
                    && input.source == "Match Start.Match")
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
        assert!(edge_to(spi.id, "clk").from.1.starts_with('d'));
        assert!(edge_to(decoder.id, "strobe").from.1.starts_with('b'));
        assert_eq!(edge_to(decoder.id, "strobe").buffer, 4);
        assert_eq!(edge_to(spi.id, "clk").buffer, 10_000_000);
        assert_eq!(edge_to(decoder.id, "d7").from.1, "b7");
        assert!(
            compiled
                .edges
                .iter()
                .any(|e| e.to.1 == "enable_signal" && e.buffer == 1_000)
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
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../..").join(relative)
    }

    /// Reference pipeline: the byte-exact Phase-1 wiring of
    /// `examples/spi_graph_decode.rs` (itself verified against the original
    /// `spi_controlled_decode.rs`).
    fn run_reference(capture: &Path, out_dir: &Path) {
        use dsl::nodes::decoders::{CsPolarity, ParallelDecoder, SpiDecoder, SpiMode, StrobeMode};
        use dsl::{SpiTransfer, SrLatch, TextFormatter, TriggerCounter, WordMatcher};

        let mut pipeline = Pipeline::new().with_default_buffer_size(10_000_000);
        pipeline
            .add_process("source", dsl::DslFileSource::new(capture, 11).unwrap())
            .unwrap();
        pipeline
            .add_process("spi", SpiDecoder::new(SpiMode::Mode0, 24, true, false))
            .unwrap();
        pipeline
            .add_process(
                "start",
                WordMatcher::<SpiTransfer>::new(0x600081, u64::MAX),
            )
            .unwrap();
        pipeline
            .add_process("stop", WordMatcher::<SpiTransfer>::new(0x600000, u64::MAX))
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

        pipeline.connect("source", "d7", "spi", "clk").unwrap();
        pipeline.connect("source", "d8", "spi", "cs").unwrap();
        pipeline.connect("source", "d6", "spi", "mosi").unwrap();
        pipeline
            .connect_with_buffer("spi", "spi_transfers", "start", "words", 1_000)
            .unwrap();
        pipeline
            .connect_with_buffer("spi", "spi_transfers", "stop", "words", 1_000)
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
            .connect_with_buffer("source", "b10", "decoder", "strobe", 4)
            .unwrap();
        for bit in 0..8 {
            pipeline
                .connect_with_buffer("source", &format!("b{bit}"), "decoder", &format!("d{bit}"), 4)
                .unwrap();
        }
        pipeline
            .connect_with_buffer("source", "b8", "decoder", "cs", 4)
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

        // Let the tap observe some windows, then detach it.
        std::thread::sleep(std::time::Duration::from_secs(20));
        widget.graph_mut().remove_node(matcher);
        let summary = run.apply(widget.graph(), &registry).expect("detach tap");
        assert_eq!(summary.removed, 1, "{summary:?}");
        assert_eq!(summary.restarted, 1, "{summary:?}");

        run.wait();
        run_reference(&capture, &ref_dir);

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

        run_reference(&capture, &ref_dir);

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
