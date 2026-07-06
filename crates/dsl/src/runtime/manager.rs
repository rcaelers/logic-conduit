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

use super::node::{ConfigOutcome, InputPort, NodeConfig, OutputPort, ProcessNode};
use super::sender::OverflowPolicy;
use super::type_registry::{ErasedSharedSenders, TYPE_REGISTRY};
use super::watchdog::Watchdog;
use crate::runtime::errors::WorkError;
use std::any::{Any, TypeId};
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
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

/// A consumer dropped by [`OverflowPolicy::Disconnect`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DisconnectEvent {
    pub producer: String,
    pub port: String,
    pub consumer: Option<String>,
}

struct OutputList {
    list: Arc<dyn ErasedSharedSenders>,
    type_id: TypeId,
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

        // Output subscriber lists, supervisor-owned.
        let mut outputs: HashMap<String, OutputList> = HashMap::new();
        {
            let registry = TYPE_REGISTRY.lock().unwrap();
            for schema in &output_schemas {
                let sticky = is_level_type(schema.type_id);
                let list = registry
                    .create_shared(schema.type_id, sticky)
                    .ok_or_else(|| format!("type of port '{}' not registered", schema.name))?;
                outputs.insert(
                    schema.name.clone(),
                    OutputList {
                        list,
                        type_id: schema.type_id,
                    },
                );
            }
        }

        // Subscribe inputs into the producers' lists.
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
                        format!("producer '{}' has no port '{}'", sub.from_node, sub.from_port)
                    })?;
                    if output.type_id != input_schemas[index].type_id {
                        return Err(format!(
                            "type mismatch: {}.{} -> {}.{}",
                            sub.from_node, sub.from_port, name, input_schemas[index].name
                        ));
                    }
                    let (id, rx) = output.list.subscribe(sub.buffer, sub.policy);
                    input_subs.push((sub.from_node.clone(), sub.from_port.clone(), id));
                    InputPort::from_type_erased(rx)
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
                let sender = outputs[&schema.name].list.sender_box();
                OutputPort::from_type_erased(sender).with_watchdog(
                    self.watchdog.clone(),
                    name.clone(),
                    schema.name.clone(),
                )
            })
            .collect();

        self.register(name, node, input_ports, output_ports, outputs, input_subs, 0);
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
        let close_handles: Vec<Arc<dyn ErasedSharedSenders>> = entry
            .outputs
            .values()
            .map(|output| Arc::clone(&output.list))
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
                            Ok(_) => {}
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
            output.list.close();
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
                output.list.unsubscribe(*sub_id);
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
                        format!("producer '{}' has no port '{}'", sub.from_node, sub.from_port)
                    })?;
                    if output.type_id != input_schemas[index].type_id {
                        return Err(format!(
                            "type mismatch: {}.{} -> {}.{}",
                            sub.from_node, sub.from_port, name, input_schemas[index].name
                        ));
                    }
                    let (id, rx) = output.list.subscribe(sub.buffer, sub.policy);
                    input_subs.push((sub.from_node.clone(), sub.from_port.clone(), id));
                    InputPort::from_type_erased(rx)
                }
            };
            let port_name = input_schemas
                .get(index)
                .map(|schema| schema.name.clone())
                .unwrap_or_else(|| format!("in{index}"));
            input_ports.push(port.with_watchdog(
                self.watchdog.clone(),
                name.to_owned(),
                port_name,
            ));
        }

        let output_schemas = node.output_schema();
        let output_ports: Vec<OutputPort> = output_schemas
            .iter()
            .map(|schema| {
                let sender = old.outputs[&schema.name].list.sender_box();
                OutputPort::from_type_erased(sender).with_watchdog(
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
        self.start_node(name)
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
                for sub_id in output.list.take_disconnected() {
                    events.push(DisconnectEvent {
                        producer: name.clone(),
                        port: port.clone(),
                        consumer: consumers.get(&sub_id).map(|s| s.to_string()),
                    });
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
                output.list.close();
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
        assert!(values.first().copied().unwrap() < 1000, "config applied too early?");
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
}
