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
use dsl::runtime::{Pipeline, ProcessNode, Scheduler, StopHandle};
use dsl::{
    BinaryFileWriter, CsPolarity, GateOp, LogicGate, ParallelWord, SpiDecoder, SpiMode,
    SpiTransfer, SrLatch, StrobeMode, TextFormatter, TriggerCounter, WordField, WordMatcher,
    WriteWidth,
};
use node_graph::{GraphState, Node, NodeId, NodeKind, Socket, SocketId};
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

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

/// Shared resources handed to builders (Phase 4 adds the `DerivedLanes`
/// store for viewer sinks).
#[derive(Default)]
pub struct CompileCtx {}

/// Per input socket, keyed `(def_index, member_index)`: the `PortKind` its
/// incoming edge settled on. Keys are def-relative so variadic growth does
/// not shift them.
#[derive(Debug, Clone, Default)]
pub struct ResolvedInputs(HashMap<(usize, usize), PortKind>);

impl ResolvedInputs {
    pub fn kind(&self, def_index: usize) -> Option<PortKind> {
        self.0.get(&(def_index, 0)).copied()
    }
    pub fn member_count(&self, def_index: usize) -> usize {
        self.0.keys().filter(|(def, _)| *def == def_index).count()
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

        resolved
            .entry(wire.to.node)
            .or_default()
            .0
            .insert((to_socket.def_index, member), kind);
        edges.push(CompiledEdge {
            from: (wire.from.node, out_port),
            to: (wire.to.node, in_port),
            buffer: buffer_size(kind, from_builder.is_source()),
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

// ── Stage 2: materialize ─────────────────────────────────────────────────────

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

/// `lower` + `materialize` in one shot (offline runs).
pub fn compile(
    graph: &GraphState,
    registry: &BuilderRegistry,
) -> Result<Scheduler, Vec<CompileError>> {
    let compiled = lower(graph, registry)?;
    materialize(&compiled, registry, &mut CompileCtx::default()).map_err(|e| vec![e])
}

// ── Run lifecycle (§5.5) ─────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RunStatus {
    Running,
    Finished,
}

pub struct RunHandle {
    status: Arc<Mutex<RunStatus>>,
    stop: StopHandle,
}

impl RunHandle {
    pub fn status(&self) -> RunStatus {
        self.status.lock().unwrap().clone()
    }
    pub fn is_running(&self) -> bool {
        self.status() == RunStatus::Running
    }
    pub fn stop(&self) {
        self.stop.stop();
    }
}

/// Moves `Scheduler::wait()` to a background thread and returns a handle the
/// UI can poll each frame.
pub fn start(scheduler: Scheduler) -> RunHandle {
    let stop = scheduler.stop_handle();
    let status = Arc::new(Mutex::new(RunStatus::Running));
    let thread_status = Arc::clone(&status);
    std::thread::Builder::new()
        .name("pipeline-wait".into())
        .spawn(move || {
            scheduler.wait();
            *thread_status.lock().unwrap() = RunStatus::Finished;
        })
        .expect("spawn pipeline-wait thread");
    RunHandle { status, stop }
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
                    .with_name(name),
            )),
            Some(PortKind::ParallelWords) => Ok(Box::new(
                WordMatcher::<ParallelWord>::new(pattern, mask)
                    .with_field(field)
                    .with_name(name),
            )),
            _ => Err("words input is not connected".into()),
        }
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
    fn input_port(&self, _: &Socket, _: usize, _: &Value, _: PortKind) -> Option<String> {
        Some("value".into())
    }
    fn output_port(&self, _socket: &Socket, _state: &Value, _kind: PortKind) -> Option<String> {
        Some("text".into())
    }
    fn build(
        &self,
        name: &str,
        state: &Value,
        _resolved: &ResolvedInputs,
        _ctx: &mut CompileCtx,
    ) -> Result<Box<dyn ProcessNode>, String> {
        let state: nodes::StringFormatterState = parse_state(state)?;
        Ok(Box::new(
            TextFormatter::new(state.template.value.clone()).with_name(name),
        ))
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

        // The viewer has no builder yet (Phase 4); everything else runs.
        assert_eq!(compiled.nodes.len(), 10);
        // 28 UI wires minus the 5 viewer lanes.
        assert_eq!(compiled.edges.len(), 23);

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
        let mut widget = startup_widget();
        let (source_id, formatter_id) = {
            let graph = widget.graph();
            let find = |name: &str| {
                graph
                    .nodes
                    .values()
                    .find(|n| n.def_name() == name)
                    .unwrap()
                    .id
            };
            (find("DSL File Source"), find("String Formatter"))
        };
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
                    graph_dir.display()
                )),
            })
            .unwrap(),
        );

        let scheduler = compile(widget.graph(), &BuilderRegistry::standard())
            .unwrap_or_else(|errors| panic!("compile failed: {errors:?}"));
        scheduler.wait();

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
