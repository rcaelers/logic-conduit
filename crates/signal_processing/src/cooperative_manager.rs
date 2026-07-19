//! Single-threaded sibling of [`PipelineManager`](crate::manager::PipelineManager).
//!
//! `PipelineManager` runs each node on its own `std::thread`, which does not
//! exist on `wasm32-unknown-unknown`. `CooperativeManager` drives the same
//! [`NodeSpec`](super::manager::NodeSpec)s, wired through the same
//! [`TYPE_REGISTRY`] subscriber-list machinery (so live add/remove/restart/
//! reconfigure and sticky level priming behave identically), but never
//! blocks: a node is only ever called when [`pump`](Self::pump) determines
//! every one of its inputs is ready *and* every one of its outputs could be
//! sent without blocking, and `pump` itself must be driven externally (the
//! UI frame loop on wasm) rather than running to completion on its own.
//!
//! Input readiness ("would `work()` block reading?") is tracked per input
//! via a small closed-set dispatch on the port's `TypeId` ([`make_probe`]),
//! because the type-erased channel handed back by
//! [`ErasedSharedSenders::subscribe`] can only be downcast against a
//! concrete `T`. A `closed` flag (shared with the producer's output list)
//! keeps a drained-and-finished input permanently "ready" — without it, a
//! multi-input node that keeps running after one producer finishes (e.g.
//! anything built on `ReceiverSelector`) would never be polled again once
//! that one input's queue emptied.
//!
//! Output readiness ("would `work()` block writing?") needs no such
//! dispatch — a node's outputs are already held as `Arc<dyn
//! ErasedSharedSenders>`, and [`ErasedSharedSenders::would_block`] answers
//! the question directly. A node with any output that would currently block
//! is skipped for this pump cycle, exactly like a node with an unready
//! input, and retried automatically once [`pump`](Self::pump)'s
//! `made_progress` loop comes back around. This is what makes it safe for
//! [`crate::nodes::sinks::ViewerSink`] (or any node) to actually let a
//! `Block`-policy channel fill and genuinely stall its producer's `send()`
//! (`docs/PIPELINE_DESIGN.md`, flow control) instead of that call permanently
//! wedging the one cooperative thread. **This check is a per-cycle
//! snapshot, not a hold on the channel** — `self.nodes` is a `HashMap`
//! with unspecified iteration order, so a producer can still be visited
//! before its consumer in one pass and fill a channel *during* its own
//! `work()` call if that call performs more than one send on the same
//! output. The safety net therefore depends on a systemic invariant: **no
//! `ProcessNode::work()` implementation may send more than once per output
//! per call while running under the cooperative backend.** Every node in
//! this codebase already satisfies that (one item in, one item out, per
//! call) except `ViewerSink`'s batched drain, which sends nothing itself
//! (it has no outputs) and so is exempt by construction. A future node
//! that fans out many sends from one `work()` call would reopen the
//! deadlock this check exists to close.

use std::any::{Any, TypeId};
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use crossbeam_channel::Receiver as CrossbeamReceiver;

use super::errors::WorkError;
use super::events::{NumberSample, TextSample, Trigger, Word};
use super::manager::{DisconnectEvent, InputSub, NodeSpec};
use super::node::{ConfigOutcome, ConfigurationBoundary, InputScheduling, NodeConfig, ProcessNode};
use super::ports::{InputPort, OutputPort, StreamReadiness};
use super::sample::{Sample, SampleBlock};
use super::sender::ChannelMessage;
use super::type_registry::{ErasedSharedSenders, TYPE_REGISTRY};
use super::watchdog::Watchdog;

/// Level streams get sticky lists; kept in sync with
/// [`manager::is_level_type`](super::manager) by hand — both are tiny and
/// change together only if a new level type is ever registered.
///
/// This manager deliberately does **not** get `SampleKind` negotiation
/// (the `Sample`/`SampleBlock` port-unification work `PipelineManager`
/// has): polymorphic raw-channel producers are excluded from the cooperative
/// platform registry and are unreachable from this manager by construction.
/// A connection here still
/// requires an exact `TypeId` match, which is correct as long as that
/// stays true. If a cooperative-platform node ever needs to negotiate
/// `SampleKind`, mirror `manager.rs`'s `build_output_lists`/
/// `negotiate_sample_kind_list`/`output_port_from_lists` here too.
fn is_level_type(type_id: TypeId) -> bool {
    type_id == TypeId::of::<Sample>()
        || type_id == TypeId::of::<NumberSample>()
        || type_id == TypeId::of::<TextSample>()
}

/// Per-input readiness check, dispatched on the port's registered payload
/// type. Covers every built-in runtime payload type.
enum Probe {
    Disconnected,
    Sample(CrossbeamReceiver<ChannelMessage<Sample>>, Arc<AtomicBool>),
    SampleBlock(
        CrossbeamReceiver<ChannelMessage<SampleBlock>>,
        Arc<AtomicBool>,
    ),
    Word(CrossbeamReceiver<ChannelMessage<Word>>, Arc<AtomicBool>),
    Trigger(CrossbeamReceiver<ChannelMessage<Trigger>>, Arc<AtomicBool>),
    Number(
        CrossbeamReceiver<ChannelMessage<NumberSample>>,
        Arc<AtomicBool>,
    ),
    Text(
        CrossbeamReceiver<ChannelMessage<TextSample>>,
        Arc<AtomicBool>,
    ),
}

impl Probe {
    fn producer_closed(&self) -> bool {
        match self {
            Self::Disconnected => true,
            Self::Sample(_, closed)
            | Self::SampleBlock(_, closed)
            | Self::Word(_, closed)
            | Self::Trigger(_, closed)
            | Self::Number(_, closed)
            | Self::Text(_, closed) => closed.load(Ordering::Acquire),
        }
    }

    /// True when calling `work()` will not block: a message (possibly the
    /// end-of-stream sentinel) is already queued, or the producer has
    /// finished (so any further wait would be forever).
    fn is_ready(&self, readiness: StreamReadiness) -> bool {
        let ready = |rx_empty: bool, closed: &AtomicBool| match readiness {
            StreamReadiness::Item => !rx_empty || closed.load(Ordering::Acquire),
            StreamReadiness::Complete => closed.load(Ordering::Acquire),
        };
        match self {
            Self::Disconnected => true,
            Self::Sample(rx, closed) => ready(rx.is_empty(), closed),
            Self::SampleBlock(rx, closed) => ready(rx.is_empty(), closed),
            Self::Word(rx, closed) => ready(rx.is_empty(), closed),
            Self::Trigger(rx, closed) => ready(rx.is_empty(), closed),
            Self::Number(rx, closed) => ready(rx.is_empty(), closed),
            Self::Text(rx, closed) => ready(rx.is_empty(), closed),
        }
    }
}

/// Builds a probe for `boxed` (the receiver `ErasedSharedSenders::subscribe`
/// just handed back) without consuming it — `downcast_ref` + `clone` leaves
/// the box intact for the caller to also pass to `InputPort::from_type_erased`.
fn make_probe(
    type_id: TypeId,
    boxed: &(dyn Any + Send),
    closed: Arc<AtomicBool>,
) -> Result<Probe, String> {
    macro_rules! try_type {
        ($ty:ty, $variant:ident) => {
            if type_id == TypeId::of::<$ty>() {
                return boxed
                    .downcast_ref::<CrossbeamReceiver<ChannelMessage<$ty>>>()
                    .map(|rx| Probe::$variant(rx.clone(), closed))
                    .ok_or_else(|| "receiver type mismatch".to_string());
            }
        };
    }
    try_type!(Sample, Sample);
    try_type!(SampleBlock, SampleBlock);
    try_type!(Word, Word);
    try_type!(Trigger, Trigger);
    try_type!(NumberSample, Number);
    try_type!(TextSample, Text);
    Err("port type not supported by the cooperative runner".to_string())
}

struct OutputList {
    list: Arc<dyn ErasedSharedSenders>,
    type_id: TypeId,
    /// Flipped once this node stops producing (finished, removed, or
    /// stopped). Shared with every current and future subscriber's probe so
    /// a drained-and-finished input stays permanently ready — see the
    /// module doc.
    closed: Arc<AtomicBool>,
}

struct CooperativeNode {
    node: Box<dyn ProcessNode>,
    inputs: Vec<InputPort>,
    outputs: Vec<OutputPort>,
    probes: Vec<Probe>,
    input_readiness: Vec<StreamReadiness>,
    input_scheduling: InputScheduling,
    output_lists: HashMap<String, OutputList>,
    input_subs: Vec<(String, String, u64)>,
    items: u64,
    done: bool,
}

/// Cooperative sibling of [`PipelineManager`](super::manager::PipelineManager);
/// see the module docs for how it differs.
pub struct CooperativeManager {
    nodes: HashMap<String, CooperativeNode>,
    watchdog: Watchdog,
}

impl CooperativeManager {
    pub fn new() -> Self {
        Self {
            nodes: HashMap::new(),
            watchdog: Watchdog::new(),
        }
    }

    pub fn is_finished(&self) -> bool {
        self.nodes.values().all(|node| node.done)
    }

    /// Same call as [`add_node_deferred`](Self::add_node_deferred) — there is
    /// no thread to start, so nothing is actually deferred; kept as a
    /// separate name only for API parity with `PipelineManager`.
    pub fn add_node(&mut self, spec: NodeSpec) -> Result<(), String> {
        self.add_node_deferred(spec)
    }

    pub fn add_node_deferred(&mut self, spec: NodeSpec) -> Result<(), String> {
        if self.nodes.contains_key(&spec.name) {
            return Err(format!("node '{}' already exists", spec.name));
        }
        let NodeSpec { name, node, inputs } = spec;

        let input_scheduling = node.input_scheduling();
        let input_schemas = node.input_schema();
        let input_readiness = input_schemas
            .iter()
            .map(|schema| schema.stream_readiness)
            .collect();
        let output_schemas = node.output_schema();
        if inputs.len() != input_schemas.len() {
            return Err(format!(
                "node '{}': {} input specs for {} ports",
                name,
                inputs.len(),
                input_schemas.len()
            ));
        }

        let mut output_lists: HashMap<String, OutputList> = HashMap::new();
        {
            let registry = TYPE_REGISTRY.lock().unwrap();
            for schema in &output_schemas {
                let sticky = is_level_type(schema.type_id);
                let list = registry
                    .create_shared(schema.type_id, sticky)
                    .ok_or_else(|| format!("type of port '{}' not registered", schema.name))?;
                output_lists.insert(
                    schema.name.clone(),
                    OutputList {
                        list,
                        type_id: schema.type_id,
                        closed: Arc::new(AtomicBool::new(false)),
                    },
                );
            }
        }

        let mut input_ports: Vec<InputPort> = Vec::with_capacity(inputs.len());
        let mut probes: Vec<Probe> = Vec::with_capacity(inputs.len());
        let mut input_subs: Vec<(String, String, u64)> = Vec::new();
        for (index, sub) in inputs.iter().enumerate() {
            let port_name = input_schemas
                .get(index)
                .map(|schema| schema.name.clone())
                .unwrap_or_else(|| format!("in{index}"));
            let port = match sub {
                None => {
                    probes.push(Probe::Disconnected);
                    InputPort::disconnected()
                }
                Some(sub) => {
                    let producer = self
                        .nodes
                        .get(&sub.from_node)
                        .ok_or_else(|| format!("producer '{}' not running", sub.from_node))?;
                    let output = producer.output_lists.get(&sub.from_port).ok_or_else(|| {
                        format!(
                            "producer '{}' has no port '{}'",
                            sub.from_node, sub.from_port
                        )
                    })?;
                    if output.type_id != input_schemas[index].type_id {
                        return Err(format!(
                            "type mismatch: {}.{} -> {}.{}",
                            sub.from_node, sub.from_port, name, input_schemas[index].name
                        ));
                    }
                    let closed = Arc::clone(&output.closed);
                    let label = Some(format!("{}.{}", name, input_schemas[index].name));
                    let (id, rx_box) = output
                        .list
                        .subscribe_with_label(sub.buffer, sub.policy, label);
                    let probe = make_probe(output.type_id, rx_box.as_ref(), closed)?;
                    input_subs.push((sub.from_node.clone(), sub.from_port.clone(), id));
                    probes.push(probe);
                    InputPort::from_type_erased(rx_box)
                }
            };
            input_ports.push(port.with_watchdog(self.watchdog.clone(), name.clone(), port_name));
        }

        let output_ports: Vec<OutputPort> = output_schemas
            .iter()
            .map(|schema| {
                let sender = output_lists[&schema.name].list.sender_box();
                OutputPort::from_type_erased(output_lists[&schema.name].type_id, sender)
                    .with_watchdog(self.watchdog.clone(), name.clone(), schema.name.clone())
            })
            .collect();

        self.nodes.insert(
            name,
            CooperativeNode {
                node,
                inputs: input_ports,
                outputs: output_ports,
                probes,
                input_readiness,
                input_scheduling,
                output_lists,
                input_subs,
                items: 0,
                done: false,
            },
        );
        Ok(())
    }

    pub fn start_all_deferred(&mut self) -> Result<(), String> {
        Ok(())
    }

    pub fn remove_node(&mut self, name: &str) -> Result<(), String> {
        let node = self
            .nodes
            .remove(name)
            .ok_or_else(|| format!("node '{name}' not running"))?;
        self.detach(&node);
        close_outputs(&node);
        Ok(())
    }

    fn detach(&self, node: &CooperativeNode) {
        for (from_node, from_port, sub_id) in &node.input_subs {
            if let Some(producer) = self.nodes.get(from_node)
                && let Some(output) = producer.output_lists.get(from_port)
            {
                output.list.unsubscribe(*sub_id);
            }
        }
    }

    pub fn reconfigure(&mut self, name: &str, config: NodeConfig) -> Result<(), String> {
        let node = self
            .nodes
            .get_mut(name)
            .ok_or_else(|| format!("node '{name}' not running"))?;
        // Applied directly (no thread to hand it to); a `NeedsRestart`
        // outcome here means the caller mis-judged hot-appliability, same
        // as on the threaded manager — log it, don't fail the edit.
        if node.node.apply_config(&config) == ConfigOutcome::NeedsRestart {
            tracing::error!("[{name}] config not hot-appliable");
        }
        Ok(())
    }

    pub fn reconfigure_at(
        &mut self,
        name: &str,
        config: NodeConfig,
        boundary: ConfigurationBoundary,
    ) -> Result<(), String> {
        let node = self
            .nodes
            .get_mut(name)
            .ok_or_else(|| format!("node '{name}' not running"))?;
        let scheduler = node.node.configuration_scheduler().ok_or_else(|| {
            format!("node '{name}' does not expose a scheduled configuration handle")
        })?;
        if scheduler.schedule_config(&config, boundary) == ConfigOutcome::NeedsRestart {
            return Err(format!(
                "node '{name}' rejected scheduled hot configuration"
            ));
        }
        Ok(())
    }

    /// Replaces a running node with a fresh instance wired to the *same*
    /// output lists — downstream connections and produced-item count survive
    /// untouched, mirroring `PipelineManager::restart_node`.
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
        self.detach(&old);

        let input_scheduling = node.input_scheduling();
        let input_schemas = node.input_schema();
        let input_readiness = input_schemas
            .iter()
            .map(|schema| schema.stream_readiness)
            .collect();
        if inputs.len() != input_schemas.len() {
            return Err(format!(
                "node '{name}': {} input specs for {} ports",
                inputs.len(),
                input_schemas.len()
            ));
        }
        let mut input_ports: Vec<InputPort> = Vec::with_capacity(inputs.len());
        let mut probes: Vec<Probe> = Vec::with_capacity(inputs.len());
        let mut input_subs: Vec<(String, String, u64)> = Vec::new();
        for (index, sub) in inputs.iter().enumerate() {
            let port_name = input_schemas
                .get(index)
                .map(|schema| schema.name.clone())
                .unwrap_or_else(|| format!("in{index}"));
            let port = match sub {
                None => {
                    probes.push(Probe::Disconnected);
                    InputPort::disconnected()
                }
                Some(sub) => {
                    let producer = self
                        .nodes
                        .get(&sub.from_node)
                        .ok_or_else(|| format!("producer '{}' not running", sub.from_node))?;
                    let output = producer.output_lists.get(&sub.from_port).ok_or_else(|| {
                        format!(
                            "producer '{}' has no port '{}'",
                            sub.from_node, sub.from_port
                        )
                    })?;
                    if output.type_id != input_schemas[index].type_id {
                        return Err(format!(
                            "type mismatch: {}.{} -> {}.{}",
                            sub.from_node, sub.from_port, name, input_schemas[index].name
                        ));
                    }
                    let closed = Arc::clone(&output.closed);
                    let label = Some(format!("{}.{}", name, input_schemas[index].name));
                    let (id, rx_box) = output
                        .list
                        .subscribe_with_label(sub.buffer, sub.policy, label);
                    let probe = make_probe(output.type_id, rx_box.as_ref(), closed)?;
                    input_subs.push((sub.from_node.clone(), sub.from_port.clone(), id));
                    probes.push(probe);
                    InputPort::from_type_erased(rx_box)
                }
            };
            input_ports.push(port.with_watchdog(self.watchdog.clone(), name.to_owned(), port_name));
        }

        let output_schemas = node.output_schema();
        let output_ports: Vec<OutputPort> = output_schemas
            .iter()
            .map(|schema| {
                let sender = old.output_lists[&schema.name].list.sender_box();
                OutputPort::from_type_erased(old.output_lists[&schema.name].type_id, sender)
                    .with_watchdog(self.watchdog.clone(), name.to_owned(), schema.name.clone())
            })
            .collect();

        self.nodes.insert(
            name.to_owned(),
            CooperativeNode {
                node,
                inputs: input_ports,
                outputs: output_ports,
                probes,
                input_readiness,
                input_scheduling,
                output_lists: old.output_lists,
                input_subs,
                items: old.items,
                done: false,
            },
        );
        Ok(())
    }

    pub fn progress(&self) -> Vec<(String, u64)> {
        self.nodes
            .iter()
            .map(|(name, node)| (name.clone(), node.items))
            .collect()
    }

    pub fn take_disconnected(&self) -> Vec<DisconnectEvent> {
        let mut consumers: HashMap<u64, &str> = HashMap::new();
        for (name, node) in &self.nodes {
            for (_, _, sub_id) in &node.input_subs {
                consumers.insert(*sub_id, name);
            }
        }
        let mut events = Vec::new();
        for (name, node) in &self.nodes {
            for (port, output) in &node.output_lists {
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

    /// Non-blocking stop, mirroring `PipelineManager::request_stop`. There
    /// are no threads to wind down on this single-threaded manager, so the
    /// stop is already complete when this returns.
    pub fn request_stop(&mut self) {
        self.stop_all();
    }

    pub fn stop_all(&mut self) {
        for node in self.nodes.values() {
            close_outputs(node);
        }
        self.nodes.clear();
    }

    /// Pumps until no node has progressed since the last round — as close to
    /// "block until natural completion" as a cooperative run can get without
    /// an external driver. Only used by tests today (native code always runs
    /// under the threaded manager); guards against spinning forever on a
    /// graph that is genuinely stuck waiting on something external.
    pub fn wait(&mut self) {
        loop {
            if self.is_finished() {
                break;
            }
            let before: u64 = self.nodes.values().map(|node| node.items).sum();
            self.pump(4096);
            let after: u64 = self.nodes.values().map(|node| node.items).sum();
            if before == after {
                break;
            }
        }
        self.nodes.clear();
    }

    /// Steps every node whose inputs are ready, up to `budget` `work()`
    /// calls total, stopping early once a full pass makes no progress. A
    /// no-op once [`is_finished`](Self::is_finished) — the caller (the UI
    /// frame loop on wasm) is expected to call this every frame regardless
    /// of run state.
    pub fn pump(&mut self, budget: usize) {
        let mut calls = 0usize;
        while calls < budget {
            let mut made_progress = false;
            for node in self.nodes.values_mut() {
                if node.done {
                    continue;
                }
                let ready = node
                    .probes
                    .iter()
                    .zip(&node.input_readiness)
                    .map(|(probe, readiness)| probe.is_ready(*readiness));
                let inputs_ready = match node.input_scheduling {
                    InputScheduling::All => ready.clone().all(|ready| ready),
                    InputScheduling::Any => {
                        // A closed input alone is not enough to call a
                        // multiplexing node while another producer remains
                        // live: the node can consume that input's EOS and
                        // then block selecting the still-empty live input.
                        // Once every producer is closed, one final call is
                        // safe and lets the node drain/observe shutdown.
                        let all_closed = node.probes.iter().all(Probe::producer_closed);
                        all_closed
                            || ready
                                .zip(&node.probes)
                                .any(|(ready, probe)| ready && !probe.producer_closed())
                    }
                };
                if !inputs_ready {
                    continue;
                }
                // Symmetric with the input-side check above: a node whose
                // next send would block (a downstream channel is full) is
                // skipped this cycle instead of being called and blocking
                // the one cooperative thread inside a real `send()` — see
                // the module doc's "would `work()` block?" framing, now
                // covering both directions.
                if node
                    .output_lists
                    .values()
                    .any(|output| output.list.would_block())
                {
                    continue;
                }
                calls += 1;
                match node.node.work(&node.inputs, &node.outputs) {
                    Ok(items) => {
                        if items > 0 {
                            node.items += items as u64;
                            made_progress = true;
                        }
                        if node.node.should_stop() {
                            node.done = true;
                            for output in node.output_lists.values() {
                                output.list.close();
                                output.closed.store(true, Ordering::Release);
                            }
                        }
                    }
                    Err(WorkError::Shutdown) => {
                        node.done = true;
                        for output in node.output_lists.values() {
                            output.list.close();
                            output.closed.store(true, Ordering::Release);
                        }
                    }
                    Err(error) => {
                        tracing::error!("[{}] work error: {error}", node.node.name());
                        node.done = true;
                        for output in node.output_lists.values() {
                            output.list.close();
                            output.closed.store(true, Ordering::Release);
                        }
                    }
                }
                if calls >= budget {
                    break;
                }
            }
            if !made_progress {
                break;
            }
        }
    }
}

impl Default for CooperativeManager {
    fn default() -> Self {
        Self::new()
    }
}

fn close_outputs(node: &CooperativeNode) {
    for output in node.output_lists.values() {
        output.list.close();
        output.closed.store(true, Ordering::Release);
    }
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::sync::Mutex;

    use super::*;
    use crate::errors::WorkResult;
    use crate::events::NumberSample;
    use crate::node::ConfigValue;
    use crate::ports::{PortDirection, PortSchema};

    /// Emits `NumberSample { value: i, start_time_ns: i }` for i in 0..max, one
    /// per `work()` call — no pacing needed since the cooperative pump loop
    /// is driven synchronously by the test, not by wall-clock time.
    struct CountingSource {
        next: i64,
        max: i64,
    }

    impl ProcessNode for CountingSource {
        fn name(&self) -> &str {
            "counting_source"
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
                start_time_ns: self.next as u64,
            })?;
            self.next += 1;
            Ok(1)
        }
    }

    /// Adds a configurable offset; hot-appliable.
    struct AddOffset {
        offset: i64,
        buffer: VecDeque<NumberSample>,
        scheduled: Arc<Mutex<VecDeque<(ConfigurationBoundary, i64)>>>,
    }

    struct OffsetConfigurationScheduler {
        scheduled: Arc<Mutex<VecDeque<(ConfigurationBoundary, i64)>>>,
    }

    impl crate::node::ConfigurationScheduler for OffsetConfigurationScheduler {
        fn schedule_config(
            &self,
            config: &NodeConfig,
            boundary: ConfigurationBoundary,
        ) -> ConfigOutcome {
            let Some(ConfigValue::I64(offset)) = config.get("offset") else {
                return ConfigOutcome::NeedsRestart;
            };
            self.scheduled
                .lock()
                .unwrap()
                .push_back((boundary, *offset));
            ConfigOutcome::Applied
        }
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
        fn configuration_scheduler(&self) -> Option<Arc<dyn crate::node::ConfigurationScheduler>> {
            Some(Arc::new(OffsetConfigurationScheduler {
                scheduled: Arc::clone(&self.scheduled),
            }))
        }
        fn work(&mut self, inputs: &[InputPort], outputs: &[OutputPort]) -> WorkResult<usize> {
            let mut input = inputs[0]
                .get::<NumberSample>(&mut self.buffer)
                .ok_or_else(|| WorkError::NodeError("missing input".into()))?;
            let sample = input.recv()?;
            let mut scheduled = self.scheduled.lock().unwrap();
            while scheduled
                .front()
                .is_some_and(|(boundary, _)| boundary.timestamp_ns <= sample.start_time_ns)
            {
                self.offset = scheduled.pop_front().unwrap().1;
            }
            drop(scheduled);
            let output = outputs[0]
                .get::<NumberSample>()
                .ok_or_else(|| WorkError::NodeError("missing output".into()))?;
            output.send(NumberSample {
                value: sample.value + self.offset,
                start_time_ns: sample.start_time_ns,
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

    /// Like `Collect`, but drains nothing while `open` is false — simulates
    /// a consumer that's momentarily not keeping up, under direct test
    /// control instead of a timing-dependent stall.
    struct GatedSink {
        store: Arc<Mutex<Vec<i64>>>,
        buffer: VecDeque<NumberSample>,
        open: Arc<AtomicBool>,
    }

    impl ProcessNode for GatedSink {
        fn name(&self) -> &str {
            "gated_sink"
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
            if !self.open.load(Ordering::Acquire) {
                return Ok(0);
            }
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
            policy: super::super::sender::OverflowPolicy::Block,
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

    #[test]
    fn add_tap_mid_run_gets_sticky_prime_and_live_data() {
        let mut manager = CooperativeManager::new();
        let base = Arc::new(Mutex::new(Vec::new()));
        let tap = Arc::new(Mutex::new(Vec::new()));

        manager
            .add_node(NodeSpec {
                name: "source".into(),
                node: Box::new(CountingSource { next: 0, max: 100 }),
                inputs: vec![],
            })
            .unwrap();
        manager
            .add_node(collect_spec("base", "source", "out", &base))
            .unwrap();

        // Let some values flow, then attach the tap mid-run.
        manager.pump(30);
        manager
            .add_node(collect_spec("tap", "source", "out", &tap))
            .unwrap();
        manager.pump(1000);

        assert!(manager.is_finished());
        let base = base.lock().unwrap();
        let tap = tap.lock().unwrap();
        assert_eq!(base.as_slice(), (0..100).collect::<Vec<i64>>().as_slice());
        assert!(!tap.is_empty(), "tap received nothing");
        let first = tap[0];
        assert!(first > 0, "tap joined mid-stream, got {first}");
        assert_eq!(
            tap.as_slice(),
            (first..100).collect::<Vec<i64>>().as_slice(),
            "tap stream has gaps"
        );
    }

    #[test]
    fn remove_branch_leaves_the_rest_running() {
        let mut manager = CooperativeManager::new();
        let base = Arc::new(Mutex::new(Vec::new()));
        let doomed = Arc::new(Mutex::new(Vec::new()));

        manager
            .add_node(NodeSpec {
                name: "source".into(),
                node: Box::new(CountingSource { next: 0, max: 100 }),
                inputs: vec![],
            })
            .unwrap();
        manager
            .add_node(collect_spec("base", "source", "out", &base))
            .unwrap();
        manager
            .add_node(collect_spec("doomed", "source", "out", &doomed))
            .unwrap();

        manager.pump(20);
        let doomed_count = doomed.lock().unwrap().len();
        assert!(doomed_count > 0, "branch never received data");
        manager.remove_node("doomed").unwrap();
        manager.pump(1000);

        assert!(manager.is_finished());
        assert_eq!(base.lock().unwrap().len(), 100, "survivor lost data");
        assert!(
            doomed.lock().unwrap().len() < 100,
            "removed branch kept receiving"
        );
    }

    #[test]
    fn reconfigure_applies_between_work_calls() {
        let mut manager = CooperativeManager::new();
        let out = Arc::new(Mutex::new(Vec::new()));

        manager
            .add_node(NodeSpec {
                name: "source".into(),
                node: Box::new(CountingSource { next: 0, max: 60 }),
                inputs: vec![],
            })
            .unwrap();
        manager
            .add_node(NodeSpec {
                name: "offset".into(),
                node: Box::new(AddOffset {
                    offset: 0,
                    buffer: VecDeque::new(),
                    scheduled: Arc::new(Mutex::new(VecDeque::new())),
                }),
                inputs: vec![sub("source", "out")],
            })
            .unwrap();
        manager
            .add_node(collect_spec("sink", "offset", "out", &out))
            .unwrap();

        manager.pump(15);
        let mut config = NodeConfig::new();
        config.insert("offset".into(), ConfigValue::I64(1000));
        manager.reconfigure("offset", config).unwrap();
        manager.pump(1000);

        assert!(manager.is_finished());
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
        let flips = values.windows(2).filter(|w| w[1] < w[0]).count();
        assert_eq!(flips, 0, "stream went backwards: {values:?}");
    }

    #[test]
    fn scheduled_reconfigure_switches_at_event_boundary_with_queued_input() {
        let mut manager = CooperativeManager::new();
        let out = Arc::new(Mutex::new(Vec::new()));
        manager
            .add_node(NodeSpec {
                name: "source".into(),
                node: Box::new(CountingSource { next: 0, max: 60 }),
                inputs: vec![],
            })
            .unwrap();
        manager
            .add_node(NodeSpec {
                name: "offset".into(),
                node: Box::new(AddOffset {
                    offset: 0,
                    buffer: VecDeque::new(),
                    scheduled: Arc::new(Mutex::new(VecDeque::new())),
                }),
                inputs: vec![sub("source", "out")],
            })
            .unwrap();
        manager
            .add_node(collect_spec("sink", "offset", "out", &out))
            .unwrap();
        manager
            .reconfigure_at(
                "offset",
                NodeConfig::from([("offset".into(), ConfigValue::I64(1000))]),
                ConfigurationBoundary::new(40, 40),
            )
            .unwrap();
        manager.pump(1000);

        let values = out.lock().unwrap();
        assert_eq!(&values[..40], (0..40).collect::<Vec<_>>().as_slice());
        assert_eq!(&values[40..], (1040..1060).collect::<Vec<_>>().as_slice());
    }

    #[test]
    fn restart_in_place_keeps_downstream_attached() {
        let mut manager = CooperativeManager::new();
        let out = Arc::new(Mutex::new(Vec::new()));

        manager
            .add_node(NodeSpec {
                name: "source".into(),
                node: Box::new(CountingSource { next: 0, max: 100 }),
                inputs: vec![],
            })
            .unwrap();
        manager
            .add_node(NodeSpec {
                name: "offset".into(),
                node: Box::new(AddOffset {
                    offset: 0,
                    buffer: VecDeque::new(),
                    scheduled: Arc::new(Mutex::new(VecDeque::new())),
                }),
                inputs: vec![sub("source", "out")],
            })
            .unwrap();
        manager
            .add_node(collect_spec("sink", "offset", "out", &out))
            .unwrap();

        manager.pump(20);
        manager
            .restart_node(
                "offset",
                Box::new(AddOffset {
                    offset: 5000,
                    buffer: VecDeque::new(),
                    scheduled: Arc::new(Mutex::new(VecDeque::new())),
                }),
                vec![sub("source", "out")],
            )
            .unwrap();
        manager.pump(1000);

        assert!(manager.is_finished());
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
        assert!(values.last().copied().unwrap() >= 5000);
    }

    #[test]
    fn stop_all_clears_every_node_without_finishing_the_run() {
        let mut manager = CooperativeManager::new();
        let out = Arc::new(Mutex::new(Vec::new()));
        manager
            .add_node(NodeSpec {
                name: "source".into(),
                node: Box::new(CountingSource {
                    next: 0,
                    max: i64::MAX,
                }),
                inputs: vec![],
            })
            .unwrap();
        manager
            .add_node(collect_spec("sink", "source", "out", &out))
            .unwrap();

        manager.pump(20);
        assert!(!out.lock().unwrap().is_empty());
        manager.stop_all();

        assert!(manager.is_finished(), "no nodes left to be unfinished");
        assert_eq!(manager.progress(), Vec::new());
    }

    #[test]
    fn producer_is_skipped_not_blocked_while_downstream_channel_is_full() {
        let mut manager = CooperativeManager::new();
        let out = Arc::new(Mutex::new(Vec::new()));
        let gate = Arc::new(AtomicBool::new(false)); // closed: sink drains nothing yet

        manager
            .add_node(NodeSpec {
                name: "source".into(),
                node: Box::new(CountingSource { next: 0, max: 20 }),
                inputs: vec![],
            })
            .unwrap();
        manager
            .add_node(NodeSpec {
                name: "sink".into(),
                node: Box::new(GatedSink {
                    store: Arc::clone(&out),
                    buffer: VecDeque::new(),
                    open: Arc::clone(&gate),
                }),
                inputs: vec![Some(InputSub {
                    from_node: "source".into(),
                    from_port: "out".into(),
                    buffer: 3,
                    policy: super::super::sender::OverflowPolicy::Block,
                })],
            })
            .unwrap();

        // Gate closed: the source can only fill the 3-slot channel, then
        // must be *skipped* — not called and left to block a real `send()`,
        // which on this single-threaded scheduler would hang forever, since
        // nothing else could ever come along to drain it.
        manager.pump(50);
        let source_items = manager
            .progress()
            .into_iter()
            .find(|(name, _)| name == "source")
            .map(|(_, items)| items)
            .unwrap();
        assert_eq!(
            source_items, 3,
            "source should be capped at the channel's capacity, not run away"
        );
        assert!(
            out.lock().unwrap().is_empty(),
            "gate is closed, nothing should have drained yet"
        );
        assert!(!manager.is_finished());

        // Open the gate: the channel drains, the source's own next `send()`
        // (still gated by the same output-readiness check) resumes, and the
        // run completes with every item delivered — no loss, no hang.
        gate.store(true, Ordering::Release);
        manager.pump(1000);

        assert!(manager.is_finished());
        assert_eq!(
            out.lock().unwrap().as_slice(),
            (0..20).collect::<Vec<i64>>().as_slice()
        );
    }
}
