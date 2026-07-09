//! Live pipeline supervisor (`ANALYSIS_PIPELINE_DESIGN.md` §6.1)
//!
//! Unlike [`Pipeline::build`](super::pipeline::Pipeline::build), which moves
//! every channel endpoint into node threads and forgets them, the
//! `PipelineManager` *owns* each node's output subscriber lists
//! ([`SharedSenders`](super::sender::SharedSenders) behind
//! [`ErasedSharedSenders`]). That inversion is what makes partial change
//! possible while data flows:
//!
//! - **Add a tap**: subscribe new receivers into existing lists; sticky
//!   level lists prime the joiner with the current value.
//! - **Remove a branch**: unsubscribe its roots and close its own lists —
//!   the ordinary shutdown cascade, confined to the branch.
//! - **Reconfigure**: a control message applied between `work()` calls.
//! - **Restart in place**: kill via input unsubscription (the node sees a
//!   normal end-of-stream), then spawn a fresh instance wired to the *same*
//!   output lists — downstream consumers just see a quiet channel.
//!
//! A node exiting *naturally* (source finished, upstream EOS) closes its own
//! output lists on the way out, so end-of-run propagates exactly like the
//! offline drop-cascade — supervisor-driven, same semantics.

use super::edge_query::EdgeQuery;
use super::node::{ConfigOutcome, InputPort, NodeConfig, OutputPort, ProcessNode};
use super::ports::PortSchema;
use super::protocol::ProtocolKind;
use super::sample_kind::{self, SampleKind};
use super::sender::OverflowPolicy;
use super::type_registry::{ErasedSharedSenders, TYPE_REGISTRY};
use super::watchdog::Watchdog;
use crate::runtime::errors::WorkError;
use std::any::{Any, TypeId};
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::thread::JoinHandle;
use tracing::{debug, error, info};

/// One input wire of a node being added: which producer list to join.
#[derive(Debug, Clone)]
pub struct InputSub {
    pub from_node: String,
    pub from_port: String,
    pub buffer: usize,
    pub policy: OverflowPolicy,
}

/// A node to run under the supervisor.
pub struct NodeSpec {
    pub name: String,
    pub node: Box<dyn ProcessNode>,
    /// One entry per input-schema index; `None` = unconnected (dummy port).
    pub inputs: Vec<Option<InputSub>>,
}

/// Level streams (§3.1) get sticky lists: the last value primes late
/// joiners. Events (words, triggers, blocks) must not — replaying a stale
/// event would fabricate history.
fn is_level_type(type_id: TypeId) -> bool {
    type_id == TypeId::of::<crate::runtime::sample::Sample>()
        || type_id == TypeId::of::<crate::runtime::events::NumberSample>()
        || type_id == TypeId::of::<crate::runtime::events::TextSample>()
}

/// Builds this node's output subscriber lists plus each port's negotiable
/// protocol capability. `node` must still be locally owned — this is the
/// only point at which the live manager can ever call `edge_query` on it
/// (see [`OutputList::edge_query`]'s doc: once `start_node` moves it into
/// its thread, nothing outside that thread can call methods on it again,
/// unlike the offline `Pipeline::build`, which negotiates every connection
/// before any node is spawned). `protocols`/`sample_kinds` themselves come
/// straight off `output_schemas`, which was already obtained from the node
/// without needing a live reference.
fn build_output_lists(
    node: &dyn ProcessNode,
    output_schemas: &[PortSchema],
) -> Result<HashMap<String, OutputList>, String> {
    let mut outputs: HashMap<String, OutputList> = HashMap::new();
    let registry = TYPE_REGISTRY.lock().unwrap();
    for schema in output_schemas {
        let sample_kinds = schema.sample_kinds.clone();
        let type_ids: Vec<TypeId> = if sample_kinds.is_empty() {
            vec![schema.type_id]
        } else {
            sample_kinds.iter().map(|kind| kind.payload_type()).collect()
        };
        // Sticky-ness is a property of the concrete payload (a `Sample`
        // level wants late-joiner priming; a `SampleBlock` burst must not
        // replay a stale block), so it's computed per kind here, not once
        // per port.
        let mut lists = Vec::with_capacity(type_ids.len());
        for type_id in type_ids {
            let sticky = is_level_type(type_id);
            let list = registry
                .create_shared(type_id, sticky)
                .ok_or_else(|| format!("type of port '{}' not registered", schema.name))?;
            lists.push((type_id, list));
        }

        let mut protocols = schema.protocols.clone();
        let edge_query = if protocols.contains(&ProtocolKind::EdgeQuery) {
            node.edge_query(schema.index, &[])
        } else {
            None
        };
        // A node can claim EdgeQuery support in general while a specific
        // instance can't deliver it right now (e.g. the waveform index
        // failed to build) — drop the claim rather than let a later
        // consumer negotiate onto a handle that doesn't exist.
        if edge_query.is_none() {
            protocols.retain(|protocol| *protocol != ProtocolKind::EdgeQuery);
        }

        outputs.insert(
            schema.name.clone(),
            OutputList {
                type_id: schema.type_id,
                sample_kinds,
                lists,
                protocols,
                edge_query,
            },
        );
    }
    Ok(outputs)
}

/// Negotiates one connection's protocol the same way `Pipeline::build`
/// does (producer preference order wins) and returns the EdgeQuery handle
/// if that's what it settled on, `None` if it settled on `Stream` (the
/// caller subscribes normally in that case).
fn negotiate_edge_query(
    output: &OutputList,
    consumer_accepts: &[ProtocolKind],
) -> Option<Arc<dyn EdgeQuery>> {
    let negotiated = output
        .protocols
        .iter()
        .find(|protocol| consumer_accepts.contains(protocol))?;
    if *negotiated == ProtocolKind::EdgeQuery {
        output.edge_query.clone()
    } else {
        None
    }
}

/// Negotiates one connection's payload type (`output`'s declared
/// alternatives against the consumer's `accepted` list — see
/// [`sample_kind::negotiate`]) and returns the matching subscriber list.
/// `None` means no common `SampleKind`/type — a real type mismatch.
fn negotiate_sample_kind_list<'a>(
    output: &'a OutputList,
    accepted: &[SampleKind],
    to_type: TypeId,
) -> Option<&'a Arc<dyn ErasedSharedSenders>> {
    let negotiated_type =
        sample_kind::negotiate(&output.sample_kinds, output.type_id, accepted, to_type)?;
    Some(
        &output
            .lists
            .iter()
            .find(|(type_id, _)| *type_id == negotiated_type)
            .expect("negotiated type must be one of this output's own lists")
            .1,
    )
}

/// Builds one `OutputPort` from every sender this output actually has —
/// one per negotiated `SampleKind` for a polymorphic port (see
/// [`OutputList::lists`]), folded into a single port the node's `work()`
/// queries by type (`OutputPort::split_senders::<Sample>()` and
/// `::<SampleBlock>()` independently, each seeing only its own senders).
fn output_port_from_lists(output: &OutputList) -> OutputPort {
    let mut port: Option<OutputPort> = None;
    for (type_id, list) in &output.lists {
        let sender = list.sender_box();
        port = Some(match port {
            None => OutputPort::from_type_erased(*type_id, sender),
            Some(p) => p.extend_type_erased(*type_id, sender),
        });
    }
    port.expect("build_output_lists always creates at least one list per port")
}

/// A consumer dropped by [`OverflowPolicy::Disconnect`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DisconnectEvent {
    pub producer: String,
    pub port: String,
    pub consumer: Option<String>,
}

struct OutputList {
    /// This port's own declared type (before any `SampleKind`
    /// negotiation) — `sample_kind::negotiate`'s fallback when
    /// `sample_kinds` is empty (every ordinary, non-polymorphic port).
    type_id: TypeId,
    /// This port's declared payload alternatives
    /// (`schema.sample_kinds`), empty for ordinary single-kind ports.
    sample_kinds: Vec<SampleKind>,
    /// One shared subscriber list per concrete `TypeId` this port
    /// actually exposes — one entry for an ordinary port, one per
    /// negotiated kind for a polymorphic Sample/SampleBlock port (e.g. a
    /// raw file channel feeding one `Sample`-only consumer and one
    /// `SampleBlock`-only consumer at once).
    lists: Vec<(TypeId, Arc<dyn ErasedSharedSenders>)>,
    /// Protocols this port can actually deliver — `schema.protocols` with
    /// `EdgeQuery` dropped again if `edge_query` below turned out to
    /// be `None` (index unavailable), so a mismatch between what a node
    /// *claims* and what it *delivers* never reaches negotiation.
    protocols: Vec<ProtocolKind>,
    /// Cached `node.edge_query(port, &[])`, computed once when this output
    /// is registered (the node is only reachable here — once `start_node`
    /// moves it into its thread, nothing outside that thread can call
    /// methods on it again, unlike the offline `Pipeline::build`, which
    /// negotiates before any node is spawned). `Some` iff `protocols`
    /// contains `EdgeQuery`.
    edge_query: Option<Arc<dyn EdgeQuery>>,
}

/// Everything a node needs to run, held between `add_node_deferred` and
/// `start_node`. Deferring the spawn matters at initial materialization:
/// a self-threading source snapshots its subscriber lists on its first
/// `work()`, so every initial consumer must subscribe *before* any thread
/// starts.
struct PendingStart {
    node: Box<dyn ProcessNode>,
    inputs: Vec<InputPort>,
    outputs: Vec<OutputPort>,
    control_rx: crossbeam_channel::Receiver<NodeConfig>,
}

struct RunningNode {
    generation: u64,
    thread: Option<JoinHandle<()>>,
    pending: Option<PendingStart>,
    control_tx: crossbeam_channel::Sender<NodeConfig>,
    stop_flag: Arc<AtomicBool>,
    /// Set before a restart-kill so the exiting thread does not close the
    /// output lists the replacement will reuse.
    keep_outputs_open: Arc<AtomicBool>,
    /// Items produced across `work()` calls (survives restarts in place).
    items: Arc<AtomicU64>,
    outputs: HashMap<String, OutputList>,
    /// `(producer node, producer port, subscription id)` per connected input.
    input_subs: Vec<(String, String, u64)>,
}

pub struct PipelineManager {
    nodes: HashMap<String, RunningNode>,
    watchdog: Watchdog,
    watchdog_handle: Option<JoinHandle<()>>,
}

impl PipelineManager {
    pub fn new() -> Self {
        let watchdog = Watchdog::new();
        let watchdog_handle = watchdog.start_monitoring_thread();
        Self {
            nodes: HashMap::new(),
            watchdog,
            watchdog_handle: Some(watchdog_handle),
        }
    }

    pub fn node_names(&self) -> Vec<String> {
        self.nodes.keys().cloned().collect()
    }

    /// No-op: threads drive themselves. Exists so callers that hold
    /// [`super::AppManager`] can call `pump` unconditionally — it only does
    /// real work on [`CooperativeManager`](super::CooperativeManager).
    pub fn pump(&mut self, _budget: usize) {}

    pub fn contains(&self, name: &str) -> bool {
        self.nodes.contains_key(name)
    }

    /// All node threads have exited (run complete or fully stopped).
    pub fn is_finished(&self) -> bool {
        self.nodes.values().all(|node| {
            node.pending.is_none()
                && node
                    .thread
                    .as_ref()
                    .is_none_or(|thread| thread.is_finished())
        })
    }

    /// Adds and starts a node immediately — the live-edit path: producers
    /// are already running, and regular nodes read their subscriber lists
    /// on every send, so a joiner is seen at once.
    pub fn add_node(&mut self, spec: NodeSpec) -> Result<(), String> {
        let name = spec.name.clone();
        self.add_node_deferred(spec)?;
        self.start_node(&name)
    }

    /// Registers a node — lists created, inputs subscribed — without
    /// starting its thread. Initial materialization adds every node
    /// deferred, then calls [`Self::start_all_deferred`], so no producer
    /// can run ahead of (or snapshot past) its initial consumers.
    pub fn add_node_deferred(&mut self, spec: NodeSpec) -> Result<(), String> {
        if self.nodes.contains_key(&spec.name) {
            return Err(format!("node '{}' already exists", spec.name));
        }
        let NodeSpec { name, node, inputs } = spec;

        let input_schemas = node.input_schema();
        let output_schemas = node.output_schema();
        if inputs.len() != input_schemas.len() {
            return Err(format!(
                "node '{}': {} input specs for {} ports",
                name,
                inputs.len(),
                input_schemas.len()
            ));
        }

        // Output subscriber lists, supervisor-owned. `node` is still
        // locally owned here (not yet moved into a thread), so this is the
        // only point where its `edge_query` can ever be queried — see
        // `OutputList::edge_query`'s doc.
        let outputs = build_output_lists(node.as_ref(), &output_schemas)?;

        // Wire inputs: subscribe into the producers' lists, unless this
        // connection negotiates EdgeQuery (producer has a cached handle for
        // that port *and* this node accepts it), in which case no stream
        // subscription happens at all.
        let mut input_ports: Vec<InputPort> = Vec::with_capacity(inputs.len());
        let mut input_subs: Vec<(String, String, u64)> = Vec::new();
        for (index, sub) in inputs.iter().enumerate() {
            let port = match sub {
                None => InputPort::from_type_erased(Box::new(()) as Box<dyn Any + Send>),
                Some(sub) => {
                    let producer = self
                        .nodes
                        .get(&sub.from_node)
                        .ok_or_else(|| format!("producer '{}' not running", sub.from_node))?;
                    let output = producer.outputs.get(&sub.from_port).ok_or_else(|| {
                        format!(
                            "producer '{}' has no port '{}'",
                            sub.from_node, sub.from_port
                        )
                    })?;
                    let list = negotiate_sample_kind_list(
                        output,
                        &input_schemas[index].sample_kinds,
                        input_schemas[index].type_id,
                    )
                    .ok_or_else(|| {
                        format!(
                            "type mismatch: {}.{} -> {}.{}",
                            sub.from_node, sub.from_port, name, input_schemas[index].name
                        )
                    })?;
                    if let Some(handle) =
                        negotiate_edge_query(output, &input_schemas[index].protocols)
                    {
                        InputPort::from_type_erased(Box::new(()) as Box<dyn Any + Send>)
                            .with_edge_query(Some(handle))
                    } else {
                        let label = Some(format!("{}.{}", name, input_schemas[index].name));
                        let (id, rx) = list.subscribe_with_label(sub.buffer, sub.policy, label);
                        input_subs.push((sub.from_node.clone(), sub.from_port.clone(), id));
                        InputPort::from_type_erased(rx)
                    }
                }
            };
            let port_name = input_schemas
                .get(index)
                .map(|schema| schema.name.clone())
                .unwrap_or_else(|| format!("in{index}"));
            input_ports.push(port.with_watchdog(self.watchdog.clone(), name.clone(), port_name));
        }

        let output_ports: Vec<OutputPort> = output_schemas
            .iter()
            .map(|schema| {
                output_port_from_lists(&outputs[&schema.name]).with_watchdog(
                    self.watchdog.clone(),
                    name.clone(),
                    schema.name.clone(),
                )
            })
            .collect();

        self.register(
            name,
            node,
            input_ports,
            output_ports,
            outputs,
            input_subs,
            0,
        );
        Ok(())
    }

    /// Stores a fully wired node awaiting `start_node`.
    #[allow(clippy::too_many_arguments)]
    fn register(
        &mut self,
        name: String,
        node: Box<dyn ProcessNode>,
        inputs: Vec<InputPort>,
        outputs: Vec<OutputPort>,
        output_lists: HashMap<String, OutputList>,
        input_subs: Vec<(String, String, u64)>,
        generation: u64,
    ) {
        let (control_tx, control_rx) = crossbeam_channel::unbounded::<NodeConfig>();
        let items = Arc::new(AtomicU64::new(0));
        self.nodes.insert(
            name,
            RunningNode {
                generation,
                thread: None,
                pending: Some(PendingStart {
                    node,
                    inputs,
                    outputs,
                    control_rx,
                }),
                control_tx,
                stop_flag: Arc::new(AtomicBool::new(false)),
                keep_outputs_open: Arc::new(AtomicBool::new(false)),
                items,
                outputs: output_lists,
                input_subs,
            },
        );
    }

    /// Starts every node still awaiting its thread (initial bring-up).
    pub fn start_all_deferred(&mut self) -> Result<(), String> {
        let deferred: Vec<String> = self
            .nodes
            .iter()
            .filter(|(_, node)| node.pending.is_some())
            .map(|(name, _)| name.clone())
            .collect();
        for name in deferred {
            self.start_node(&name)?;
        }
        Ok(())
    }

    fn start_node(&mut self, name: &str) -> Result<(), String> {
        let entry = self
            .nodes
            .get_mut(name)
            .ok_or_else(|| format!("node '{name}' not registered"))?;
        let Some(PendingStart {
            mut node,
            inputs,
            outputs,
            control_rx,
        }) = entry.pending.take()
        else {
            return Err(format!("node '{name}' already started"));
        };
        let generation = entry.generation;

        let thread_name = format!("{name}@{generation}");
        let thread_stop = Arc::clone(&entry.stop_flag);
        let thread_keep_open = Arc::clone(&entry.keep_outputs_open);
        let thread_items = Arc::clone(&entry.items);
        let close_handles: Vec<Arc<dyn ErasedSharedSenders>> = entry
            .outputs
            .values()
            .flat_map(|output| output.lists.iter().map(|(_, list)| Arc::clone(list)))
            .collect();

        let thread = std::thread::Builder::new()
            .name(thread_name.clone())
            .spawn(move || {
                if node.is_self_threading() {
                    // Start internal threads once, then supervise.
                    if let Err(e) = node.work(&inputs, &outputs) {
                        error!("[{thread_name}] failed to start: {e}");
                    } else {
                        loop {
                            if thread_stop.load(Ordering::Relaxed) || node.should_stop() {
                                break;
                            }
                            while let Ok(config) = control_rx.try_recv() {
                                if node.apply_config(&config) == ConfigOutcome::NeedsRestart {
                                    error!("[{thread_name}] config not hot-appliable");
                                }
                            }
                            std::thread::sleep(std::time::Duration::from_millis(50));
                        }
                    }
                } else {
                    loop {
                        while let Ok(config) = control_rx.try_recv() {
                            if node.apply_config(&config) == ConfigOutcome::NeedsRestart {
                                error!("[{thread_name}] config not hot-appliable");
                            }
                        }
                        if thread_stop.load(Ordering::Relaxed) || node.should_stop() {
                            break;
                        }
                        match node.work(&inputs, &outputs) {
                            Ok(items) => {
                                if items > 0 {
                                    thread_items.fetch_add(items as u64, Ordering::Relaxed);
                                }
                            }
                            Err(WorkError::Shutdown) => {
                                debug!("[{thread_name}] shutdown");
                                break;
                            }
                            Err(e) => {
                                error!("[{thread_name}] work error: {e}");
                                break;
                            }
                        }
                    }
                }

                // Flush/close node resources (writer Drop, source threads).
                drop(node);
                drop(inputs);
                drop(outputs);

                // Natural completion propagates EOS downstream. A restart
                // kill keeps the lists open for the replacement instance.
                if !thread_keep_open.load(Ordering::Relaxed) {
                    for list in &close_handles {
                        list.close();
                    }
                }
                info!("[{thread_name}] exited");
            })
            .map_err(|e| format!("spawn '{name}': {e}"))?;

        entry.thread = Some(thread);
        Ok(())
    }

    /// Unsubscribes `name` from its producers (its next reads see
    /// end-of-stream), closes its own output lists (the cascade continues
    /// through the branch), and joins the thread.
    pub fn remove_node(&mut self, name: &str) -> Result<(), String> {
        let node = self
            .nodes
            .remove(name)
            .ok_or_else(|| format!("node '{name}' not running"))?;
        self.detach(&node);
        node.stop_flag.store(true, Ordering::Relaxed);
        for output in node.outputs.values() {
            for (_, list) in &output.lists {
                list.close();
            }
        }
        if let Some(thread) = node.thread
            && thread.join().is_err()
        {
            error!("[{name}] thread panicked during removal");
        }
        Ok(())
    }

    fn detach(&self, node: &RunningNode) {
        for (from_node, from_port, sub_id) in &node.input_subs {
            if let Some(producer) = self.nodes.get(from_node)
                && let Some(output) = producer.outputs.get(from_port)
            {
                // Subscription ids are globally unique (one counter shared
                // across every SharedSenders list, see sender.rs), so
                // unsubscribing from a list that doesn't hold this id is a
                // harmless no-op — no need to track which negotiated kind
                // this particular subscription resolved to.
                for (_, list) in &output.lists {
                    list.unsubscribe(*sub_id);
                }
            }
        }
    }

    /// Sends a hot configuration to a running node; applied between
    /// `work()` calls. Whether the change is hot-appliable is decided
    /// statically by the caller (builder capability), so a `NeedsRestart`
    /// outcome inside the node is logged as a bug rather than handled.
    pub fn reconfigure(&self, name: &str, config: NodeConfig) -> Result<(), String> {
        let node = self
            .nodes
            .get(name)
            .ok_or_else(|| format!("node '{name}' not running"))?;
        node.control_tx
            .send(config)
            .map_err(|_| format!("node '{name}' no longer accepts config"))
    }

    /// Replaces a running node with a fresh instance wired to the *same*
    /// output lists (downstream connections survive untouched), generation
    /// +1. `inputs` re-declares its input wiring.
    pub fn restart_node(
        &mut self,
        name: &str,
        node: Box<dyn ProcessNode>,
        inputs: Vec<Option<InputSub>>,
    ) -> Result<(), String> {
        let old = self
            .nodes
            .remove(name)
            .ok_or_else(|| format!("node '{name}' not running"))?;
        old.keep_outputs_open.store(true, Ordering::Relaxed);
        self.detach(&old);
        old.stop_flag.store(true, Ordering::Relaxed);
        if let Some(thread) = old.thread
            && thread.join().is_err()
        {
            error!("[{name}] thread panicked during restart");
        }

        let input_schemas = node.input_schema();
        if inputs.len() != input_schemas.len() {
            return Err(format!(
                "node '{name}': {} input specs for {} ports",
                inputs.len(),
                input_schemas.len()
            ));
        }
        let mut input_ports: Vec<InputPort> = Vec::with_capacity(inputs.len());
        let mut input_subs: Vec<(String, String, u64)> = Vec::new();
        for (index, sub) in inputs.iter().enumerate() {
            let port = match sub {
                None => InputPort::from_type_erased(Box::new(()) as Box<dyn Any + Send>),
                Some(sub) => {
                    let producer = self
                        .nodes
                        .get(&sub.from_node)
                        .ok_or_else(|| format!("producer '{}' not running", sub.from_node))?;
                    let output = producer.outputs.get(&sub.from_port).ok_or_else(|| {
                        format!(
                            "producer '{}' has no port '{}'",
                            sub.from_node, sub.from_port
                        )
                    })?;
                    let list = negotiate_sample_kind_list(
                        output,
                        &input_schemas[index].sample_kinds,
                        input_schemas[index].type_id,
                    )
                    .ok_or_else(|| {
                        format!(
                            "type mismatch: {}.{} -> {}.{}",
                            sub.from_node, sub.from_port, name, input_schemas[index].name
                        )
                    })?;
                    if let Some(handle) =
                        negotiate_edge_query(output, &input_schemas[index].protocols)
                    {
                        InputPort::from_type_erased(Box::new(()) as Box<dyn Any + Send>)
                            .with_edge_query(Some(handle))
                    } else {
                        let label = Some(format!("{}.{}", name, input_schemas[index].name));
                        let (id, rx) = list.subscribe_with_label(sub.buffer, sub.policy, label);
                        input_subs.push((sub.from_node.clone(), sub.from_port.clone(), id));
                        InputPort::from_type_erased(rx)
                    }
                }
            };
            let port_name = input_schemas
                .get(index)
                .map(|schema| schema.name.clone())
                .unwrap_or_else(|| format!("in{index}"));
            input_ports.push(port.with_watchdog(self.watchdog.clone(), name.to_owned(), port_name));
        }

        let output_schemas = node.output_schema();
        let output_ports: Vec<OutputPort> = output_schemas
            .iter()
            .map(|schema| {
                output_port_from_lists(&old.outputs[&schema.name]).with_watchdog(
                    self.watchdog.clone(),
                    name.to_owned(),
                    schema.name.clone(),
                )
            })
            .collect();

        let generation = old.generation + 1;
        self.register(
            name.to_owned(),
            node,
            input_ports,
            output_ports,
            old.outputs,
            input_subs,
            generation,
        );
        // Same logical node: the progress count carries across the restart.
        if let Some(entry) = self.nodes.get_mut(name) {
            entry.items = Arc::clone(&old.items);
        }
        self.start_node(name)
    }

    /// Items produced per node (sum of `work()` return values), for
    /// progress display. Self-threading sources report 0.
    pub fn progress(&self) -> Vec<(String, u64)> {
        self.nodes
            .iter()
            .map(|(name, node)| (name.clone(), node.items.load(Ordering::Relaxed)))
            .collect()
    }

    /// Consumers dropped by `OverflowPolicy::Disconnect` since the last call.
    pub fn take_disconnected(&self) -> Vec<DisconnectEvent> {
        // Reverse map: subscription id → consumer node.
        let mut consumers: HashMap<u64, &str> = HashMap::new();
        for (name, node) in &self.nodes {
            for (_, _, sub_id) in &node.input_subs {
                consumers.insert(*sub_id, name);
            }
        }
        let mut events = Vec::new();
        for (name, node) in &self.nodes {
            for (port, output) in &node.outputs {
                for (_, list) in &output.lists {
                    for sub_id in list.take_disconnected() {
                        events.push(DisconnectEvent {
                            producer: name.clone(),
                            port: port.clone(),
                            consumer: consumers.get(&sub_id).map(|s| s.to_string()),
                        });
                    }
                }
            }
        }
        events
    }

    /// Stops everything: closes every output list (unblocking every waiting
    /// consumer with end-of-stream), sets all stop flags, joins all threads.
    /// Writer flushes run in the node `Drop`s, as offline.
    pub fn stop_all(&mut self) {
        for node in self.nodes.values() {
            node.stop_flag.store(true, Ordering::Relaxed);
            for output in node.outputs.values() {
                for (_, list) in &output.lists {
                    list.close();
                }
            }
        }
        for (name, node) in self.nodes.drain() {
            if let Some(thread) = node.thread
                && thread.join().is_err()
            {
                error!("[{name}] thread panicked during stop");
            }
        }
        self.watchdog.stop();
        if let Some(handle) = self.watchdog_handle.take() {
            let _ = handle.join();
        }
    }

    /// Blocks until every node thread has exited (natural end of a file
    /// run), then reaps them. Live edits must come from another thread's
    /// point of view *before* calling this.
    pub fn wait(&mut self) {
        for (name, node) in self.nodes.drain() {
            if let Some(thread) = node.thread
                && thread.join().is_err()
            {
                error!("[{name}] thread panicked");
            }
        }
        self.watchdog.stop();
        if let Some(handle) = self.watchdog_handle.take() {
            let _ = handle.join();
        }
    }
}

impl Default for PipelineManager {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for PipelineManager {
    fn drop(&mut self) {
        if !self.nodes.is_empty() {
            self.stop_all();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::events::NumberSample;
    use crate::runtime::node::{ConfigValue, WorkResult};
    use crate::runtime::ports::{PortDirection, PortSchema};
    use std::collections::VecDeque;
    use std::sync::Mutex;
    use std::time::Duration;

    /// Emits `NumberSample { value: i, start_time: i }` for i in 0..max,
    /// paced so tests can attach taps mid-stream.
    struct PacedSource {
        next: i64,
        max: i64,
        pace: Duration,
    }

    impl ProcessNode for PacedSource {
        fn name(&self) -> &str {
            "paced_source"
        }
        fn num_inputs(&self) -> usize {
            0
        }
        fn num_outputs(&self) -> usize {
            1
        }
        fn output_schema(&self) -> Vec<PortSchema> {
            vec![PortSchema::new::<NumberSample>(
                "out",
                0,
                PortDirection::Output,
            )]
        }
        fn work(&mut self, _inputs: &[InputPort], outputs: &[OutputPort]) -> WorkResult<usize> {
            if self.next >= self.max {
                return Err(WorkError::Shutdown);
            }
            let output = outputs[0]
                .get::<NumberSample>()
                .ok_or_else(|| WorkError::NodeError("missing output".into()))?;
            output.send(NumberSample {
                value: self.next,
                start_time: self.next as u64,
            })?;
            self.next += 1;
            std::thread::sleep(self.pace);
            Ok(1)
        }
    }

    /// Adds a configurable offset; hot-appliable.
    struct AddOffset {
        offset: i64,
        buffer: VecDeque<NumberSample>,
    }

    impl ProcessNode for AddOffset {
        fn name(&self) -> &str {
            "add_offset"
        }
        fn num_inputs(&self) -> usize {
            1
        }
        fn num_outputs(&self) -> usize {
            1
        }
        fn input_schema(&self) -> Vec<PortSchema> {
            vec![PortSchema::new::<NumberSample>(
                "in",
                0,
                PortDirection::Input,
            )]
        }
        fn output_schema(&self) -> Vec<PortSchema> {
            vec![PortSchema::new::<NumberSample>(
                "out",
                0,
                PortDirection::Output,
            )]
        }
        fn apply_config(&mut self, config: &NodeConfig) -> ConfigOutcome {
            if let Some(ConfigValue::I64(offset)) = config.get("offset") {
                self.offset = *offset;
                ConfigOutcome::Applied
            } else {
                ConfigOutcome::NeedsRestart
            }
        }
        fn work(&mut self, inputs: &[InputPort], outputs: &[OutputPort]) -> WorkResult<usize> {
            let mut input = inputs[0]
                .get::<NumberSample>(&mut self.buffer)
                .ok_or_else(|| WorkError::NodeError("missing input".into()))?;
            let sample = input.recv()?;
            let output = outputs[0]
                .get::<NumberSample>()
                .ok_or_else(|| WorkError::NodeError("missing output".into()))?;
            output.send(NumberSample {
                value: sample.value + self.offset,
                start_time: sample.start_time,
            })?;
            Ok(1)
        }
    }

    struct Collect {
        store: Arc<Mutex<Vec<i64>>>,
        buffer: VecDeque<NumberSample>,
    }

    impl ProcessNode for Collect {
        fn name(&self) -> &str {
            "collect"
        }
        fn num_inputs(&self) -> usize {
            1
        }
        fn num_outputs(&self) -> usize {
            0
        }
        fn input_schema(&self) -> Vec<PortSchema> {
            vec![PortSchema::new::<NumberSample>(
                "in",
                0,
                PortDirection::Input,
            )]
        }
        fn work(&mut self, inputs: &[InputPort], _outputs: &[OutputPort]) -> WorkResult<usize> {
            let mut input = inputs[0]
                .get::<NumberSample>(&mut self.buffer)
                .ok_or_else(|| WorkError::NodeError("missing input".into()))?;
            let sample = input.recv()?;
            self.store.lock().unwrap().push(sample.value);
            Ok(1)
        }
    }

    fn sub(from: &str, port: &str) -> Option<InputSub> {
        Some(InputSub {
            from_node: from.to_owned(),
            from_port: port.to_owned(),
            buffer: 64,
            policy: OverflowPolicy::Block,
        })
    }

    fn collect_spec(name: &str, from: &str, port: &str, store: &Arc<Mutex<Vec<i64>>>) -> NodeSpec {
        NodeSpec {
            name: name.to_owned(),
            node: Box::new(Collect {
                store: Arc::clone(store),
                buffer: VecDeque::new(),
            }),
            inputs: vec![sub(from, port)],
        }
    }

    fn wait_finished(manager: &PipelineManager, timeout: Duration) {
        let start = std::time::Instant::now();
        while !manager.is_finished() {
            assert!(start.elapsed() < timeout, "pipeline did not finish in time");
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    #[test]
    fn add_tap_mid_run_gets_sticky_prime_and_live_data() {
        let mut manager = PipelineManager::new();
        let base = Arc::new(Mutex::new(Vec::new()));
        let tap = Arc::new(Mutex::new(Vec::new()));

        manager
            .add_node(NodeSpec {
                name: "source".into(),
                node: Box::new(PacedSource {
                    next: 0,
                    max: 100,
                    pace: Duration::from_millis(2),
                }),
                inputs: vec![],
            })
            .unwrap();
        manager
            .add_node(collect_spec("base", "source", "out", &base))
            .unwrap();

        // Let some values flow, then attach the tap.
        std::thread::sleep(Duration::from_millis(60));
        manager
            .add_node(collect_spec("tap", "source", "out", &tap))
            .unwrap();

        wait_finished(&manager, Duration::from_secs(5));
        manager.wait();

        let base = base.lock().unwrap();
        let tap = tap.lock().unwrap();
        assert_eq!(base.as_slice(), (0..100).collect::<Vec<i64>>().as_slice());
        assert!(!tap.is_empty(), "tap received nothing");
        let first = tap[0];
        assert!(first > 0, "tap joined mid-stream, got {first}");
        // Sticky priming: the first value is the level current at join time,
        // then the stream continues gapless.
        assert_eq!(
            tap.as_slice(),
            (first..100).collect::<Vec<i64>>().as_slice(),
            "tap stream has gaps"
        );
    }

    #[test]
    fn remove_branch_leaves_the_rest_running() {
        let mut manager = PipelineManager::new();
        let base = Arc::new(Mutex::new(Vec::new()));
        let doomed = Arc::new(Mutex::new(Vec::new()));

        manager
            .add_node(NodeSpec {
                name: "source".into(),
                node: Box::new(PacedSource {
                    next: 0,
                    max: 100,
                    pace: Duration::from_millis(2),
                }),
                inputs: vec![],
            })
            .unwrap();
        manager
            .add_node(collect_spec("base", "source", "out", &base))
            .unwrap();
        manager
            .add_node(collect_spec("doomed", "source", "out", &doomed))
            .unwrap();

        std::thread::sleep(Duration::from_millis(50));
        manager.remove_node("doomed").unwrap();
        let doomed_count = doomed.lock().unwrap().len();
        assert!(doomed_count > 0, "branch never received data");

        wait_finished(&manager, Duration::from_secs(5));
        manager.wait();
        assert_eq!(base.lock().unwrap().len(), 100, "survivor lost data");
        assert!(
            doomed.lock().unwrap().len() < 100,
            "removed branch kept receiving"
        );
    }

    #[test]
    fn reconfigure_applies_between_work_calls() {
        let mut manager = PipelineManager::new();
        let out = Arc::new(Mutex::new(Vec::new()));

        manager
            .add_node(NodeSpec {
                name: "source".into(),
                node: Box::new(PacedSource {
                    next: 0,
                    max: 60,
                    pace: Duration::from_millis(2),
                }),
                inputs: vec![],
            })
            .unwrap();
        manager
            .add_node(NodeSpec {
                name: "offset".into(),
                node: Box::new(AddOffset {
                    offset: 0,
                    buffer: VecDeque::new(),
                }),
                inputs: vec![sub("source", "out")],
            })
            .unwrap();
        manager
            .add_node(collect_spec("sink", "offset", "out", &out))
            .unwrap();

        std::thread::sleep(Duration::from_millis(40));
        let mut config = NodeConfig::new();
        config.insert("offset".into(), ConfigValue::I64(1000));
        manager.reconfigure("offset", config).unwrap();

        wait_finished(&manager, Duration::from_secs(5));
        manager.wait();

        let values = out.lock().unwrap();
        assert_eq!(values.len(), 60);
        assert!(
            values.first().copied().unwrap() < 1000,
            "config applied too early?"
        );
        assert!(
            values.last().copied().unwrap() >= 1000,
            "config never applied"
        );
        // Offset flips exactly once: values are (i) then (i + 1000), both
        // strictly increasing.
        let flips = values.windows(2).filter(|w| w[1] < w[0]).count();
        assert_eq!(flips, 0, "stream went backwards: {values:?}");
    }

    #[test]
    fn restart_in_place_keeps_downstream_attached() {
        let mut manager = PipelineManager::new();
        let out = Arc::new(Mutex::new(Vec::new()));

        manager
            .add_node(NodeSpec {
                name: "source".into(),
                node: Box::new(PacedSource {
                    next: 0,
                    max: 100,
                    pace: Duration::from_millis(2),
                }),
                inputs: vec![],
            })
            .unwrap();
        manager
            .add_node(NodeSpec {
                name: "offset".into(),
                node: Box::new(AddOffset {
                    offset: 0,
                    buffer: VecDeque::new(),
                }),
                inputs: vec![sub("source", "out")],
            })
            .unwrap();
        manager
            .add_node(collect_spec("sink", "offset", "out", &out))
            .unwrap();

        std::thread::sleep(Duration::from_millis(50));
        manager
            .restart_node(
                "offset",
                Box::new(AddOffset {
                    offset: 5000,
                    buffer: VecDeque::new(),
                }),
                vec![sub("source", "out")],
            )
            .unwrap();

        wait_finished(&manager, Duration::from_secs(5));
        manager.wait();

        let values = out.lock().unwrap();
        assert!(!values.is_empty());
        assert!(
            values.iter().any(|v| *v >= 5000),
            "restarted node never produced: {values:?}"
        );
        assert!(
            values.iter().any(|v| *v < 5000),
            "old node never produced before restart"
        );
        // Downstream sink survived the restart: it kept collecting after.
        assert!(values.last().copied().unwrap() >= 5000);
    }

    #[test]
    fn stop_all_unblocks_and_joins_everything() {
        let mut manager = PipelineManager::new();
        let out = Arc::new(Mutex::new(Vec::new()));
        manager
            .add_node(NodeSpec {
                name: "source".into(),
                node: Box::new(PacedSource {
                    next: 0,
                    max: i64::MAX, // endless
                    pace: Duration::from_millis(1),
                }),
                inputs: vec![],
            })
            .unwrap();
        manager
            .add_node(collect_spec("sink", "source", "out", &out))
            .unwrap();

        std::thread::sleep(Duration::from_millis(30));
        let start = std::time::Instant::now();
        manager.stop_all();
        assert!(
            start.elapsed() < Duration::from_secs(2),
            "stop_all took too long"
        );
        assert!(!out.lock().unwrap().is_empty());
    }

    // ── EdgeQuery negotiation ─────────────────────────────────────────

    use crate::runtime::capture::CaptureTransition;
    use crate::runtime::edge_query::EdgeQuery;

    struct ConstQuery;

    impl EdgeQuery for ConstQuery {
        fn sample_period(&self) -> f64 {
            1.0
        }
        fn samplerate_hz(&self) -> f64 {
            1.0
        }
        fn total_samples(&self) -> u64 {
            100
        }
        fn value_at(&self, _position: u64) -> crate::Result<bool> {
            Ok(true)
        }
        fn next_edge(&self, _position: u64, _limit: u64) -> crate::Result<Option<CaptureTransition>> {
            Ok(None)
        }
    }

    /// Self-threading source that never streams anything — a well-behaved
    /// consumer of this port has no choice but to use EdgeQuery.
    struct QueryableSource;

    impl ProcessNode for QueryableSource {
        fn name(&self) -> &str {
            "queryable_source"
        }
        fn is_self_threading(&self) -> bool {
            true
        }
        fn should_stop(&self) -> bool {
            true
        }
        fn num_inputs(&self) -> usize {
            0
        }
        fn num_outputs(&self) -> usize {
            1
        }
        fn output_schema(&self) -> Vec<PortSchema> {
            vec![
                PortSchema::new::<NumberSample>("out", 0, PortDirection::Output)
                    .with_protocols(vec![ProtocolKind::EdgeQuery, ProtocolKind::Stream]),
            ]
        }
        fn work(&mut self, _inputs: &[InputPort], _outputs: &[OutputPort]) -> WorkResult<usize> {
            Ok(0)
        }
        fn edge_query(
            &self,
            _port: usize,
            _input_queries: &[Option<Arc<dyn EdgeQuery>>],
        ) -> Option<Arc<dyn EdgeQuery>> {
            Some(Arc::new(ConstQuery))
        }
    }

    /// Records whether its input negotiated an EdgeQuery handle, then exits.
    struct QueryProbe {
        got_edge_query: Arc<AtomicBool>,
    }

    impl ProcessNode for QueryProbe {
        fn name(&self) -> &str {
            "query_probe"
        }
        fn num_inputs(&self) -> usize {
            1
        }
        fn num_outputs(&self) -> usize {
            0
        }
        fn input_schema(&self) -> Vec<PortSchema> {
            vec![
                PortSchema::new::<NumberSample>("in", 0, PortDirection::Input)
                    .with_protocols(vec![ProtocolKind::EdgeQuery, ProtocolKind::Stream]),
            ]
        }
        fn work(&mut self, inputs: &[InputPort], _outputs: &[OutputPort]) -> WorkResult<usize> {
            if inputs[0].edge_query().is_some() {
                self.got_edge_query.store(true, Ordering::Relaxed);
            }
            Err(WorkError::Shutdown)
        }
    }

    #[test]
    fn edge_query_negotiated_even_after_producer_already_running() {
        let mut manager = PipelineManager::new();
        manager
            .add_node(NodeSpec {
                name: "source".into(),
                node: Box::new(QueryableSource),
                inputs: vec![],
            })
            .unwrap();

        // Give the (self-threading, should_stop-immediately) source a
        // chance to fully exit — its `Box<dyn ProcessNode>` is gone by
        // then, so this proves the EdgeQuery handle came from the cache in
        // `OutputList`, not a live call into the still-running node.
        std::thread::sleep(Duration::from_millis(50));

        let got = Arc::new(AtomicBool::new(false));
        manager
            .add_node(NodeSpec {
                name: "probe".into(),
                node: Box::new(QueryProbe {
                    got_edge_query: Arc::clone(&got),
                }),
                inputs: vec![sub("source", "out")],
            })
            .unwrap();

        wait_finished(&manager, Duration::from_secs(5));
        manager.wait();

        assert!(
            got.load(Ordering::Relaxed),
            "consumer never received an EdgeQuery handle even though both \
             sides declared support for it"
        );
    }

    // ── SampleKind negotiation (live path) ──────────────────────────────

    use crate::runtime::sample::{Sample, SampleBlock};

    /// One output port that can serve `Sample` and `SampleBlock`
    /// destinations simultaneously — sends exactly one of each, then
    /// signals completion. Mirrors `pipeline.rs`'s offline
    /// `MultiKindSource` test, but exercised through the live
    /// `PipelineManager` path this time (the class of gap that let the
    /// `ProtocolKind` work silently miss the live app earlier).
    struct MultiKindSource {
        sent: bool,
    }
    impl ProcessNode for MultiKindSource {
        fn name(&self) -> &str {
            "multi_kind_source"
        }
        fn num_inputs(&self) -> usize {
            0
        }
        fn num_outputs(&self) -> usize {
            1
        }
        fn output_schema(&self) -> Vec<PortSchema> {
            vec![
                PortSchema::new::<Sample>("out", 0, PortDirection::Output)
                    .with_sample_kinds(vec![SampleKind::Block, SampleKind::Edge]),
            ]
        }
        fn work(&mut self, _inputs: &[InputPort], outputs: &[OutputPort]) -> WorkResult<usize> {
            if self.sent {
                return Err(WorkError::Shutdown);
            }
            self.sent = true;
            if let Some(sender) = outputs[0].get::<Sample>() {
                let _ = sender.send(Sample::new(true, 0));
            }
            if let Some(sender) = outputs[0].get::<SampleBlock>() {
                let _ = sender.send(SampleBlock::new(Arc::from([0u8].as_slice()), 0, 1, 1));
            }
            Ok(1)
        }
    }

    struct SampleSink {
        got: Arc<Mutex<Vec<Sample>>>,
    }
    impl ProcessNode for SampleSink {
        fn name(&self) -> &str {
            "sample_sink"
        }
        fn num_inputs(&self) -> usize {
            1
        }
        fn num_outputs(&self) -> usize {
            0
        }
        fn input_schema(&self) -> Vec<PortSchema> {
            vec![PortSchema::new::<Sample>("in", 0, PortDirection::Input)]
        }
        fn work(&mut self, inputs: &[InputPort], _outputs: &[OutputPort]) -> WorkResult<usize> {
            let mut buf = VecDeque::new();
            let mut recv = inputs[0].get::<Sample>(&mut buf).unwrap();
            let item = recv.recv()?;
            self.got.lock().unwrap().push(item);
            Ok(1)
        }
    }

    struct BlockSink {
        got: Arc<Mutex<Vec<SampleBlock>>>,
    }
    impl ProcessNode for BlockSink {
        fn name(&self) -> &str {
            "block_sink"
        }
        fn num_inputs(&self) -> usize {
            1
        }
        fn num_outputs(&self) -> usize {
            0
        }
        fn input_schema(&self) -> Vec<PortSchema> {
            vec![PortSchema::new::<SampleBlock>(
                "in",
                0,
                PortDirection::Input,
            )]
        }
        fn work(&mut self, inputs: &[InputPort], _outputs: &[OutputPort]) -> WorkResult<usize> {
            let mut buf = VecDeque::new();
            let mut recv = inputs[0].get::<SampleBlock>(&mut buf).unwrap();
            let item = recv.recv()?;
            self.got.lock().unwrap().push(item);
            Ok(1)
        }
    }

    #[test]
    fn mixed_kind_fan_out_from_one_port_reaches_both_destinations_live() {
        // `SampleBlock` isn't a sticky/level type (unlike `Sample`), so a
        // subscriber added after the source already sent and closed would
        // genuinely miss it — no amount of pacing makes that safe. Add
        // every node deferred and start them together instead, so both
        // sinks are subscribed before the source's thread can run at all
        // (the same guarantee `start_all_deferred`'s own doc describes
        // for initial materialization).
        let mut manager = PipelineManager::new();
        manager
            .add_node_deferred(NodeSpec {
                name: "source".into(),
                node: Box::new(MultiKindSource { sent: false }),
                inputs: vec![],
            })
            .unwrap();

        let sample_got = Arc::new(Mutex::new(Vec::new()));
        let block_got = Arc::new(Mutex::new(Vec::new()));
        manager
            .add_node_deferred(NodeSpec {
                name: "sample_sink".into(),
                node: Box::new(SampleSink {
                    got: sample_got.clone(),
                }),
                inputs: vec![sub("source", "out")],
            })
            .unwrap();
        manager
            .add_node_deferred(NodeSpec {
                name: "block_sink".into(),
                node: Box::new(BlockSink {
                    got: block_got.clone(),
                }),
                inputs: vec![sub("source", "out")],
            })
            .unwrap();
        manager.start_all_deferred().unwrap();

        wait_finished(&manager, Duration::from_secs(5));
        manager.wait();

        assert_eq!(sample_got.lock().unwrap().len(), 1);
        assert_eq!(block_got.lock().unwrap().len(), 1);
    }
}
