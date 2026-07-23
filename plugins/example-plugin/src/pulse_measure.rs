//! `Pulse Measure` — measures the duration of each high level on a `Signal`
//! input and emits it as a `Pulse` event. A minimal, deliberately small
//! example exercising every seam an out-of-tree plugin crate touches:
//! a new runtime payload type ([`PulseWidth`]), a new compiler `PortValue`,
//! a new graph `SocketDef`, a `NodeDef` reusing a host-crate socket type
//! (`logic_analyzer_graph::nodes::Signal`), and a matching `RuntimeBuilder`.

use std::collections::VecDeque;

use egui::Color32;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use logic_analyzer_graph::node::{GraphNodeRegistration, RuntimeBuilder};
use logic_analyzer_graph::node_support::{CompileCtx, PortKind, PortValue, ResolvedInputs};
use logic_analyzer_graph::nodes::Signal;
use node_graph::{InputDef, NodeDef, OutputDef, Socket, SocketDef, SocketShape};
use signal_processing::{
    InputPort, OutputPort, PortDirection, PortSchema, ProcessNode, Sample, WorkError, WorkResult,
};

// ── Runtime payload ──────────────────────────────────────────────────────────

/// One measured high pulse: how long the signal stayed high, and when it
/// started.
#[derive(Clone, Debug)]
pub struct PulseWidth {
    pub width_ns: u64,
    pub start_time_ns: u64,
}

/// Open compiler-layer identity for `PulseWidth` — the plugin-authored
/// equivalent of the built-in `impl PortValue for Sample` etc. in
/// `logic_analyzer_graph::port_kind`. No edits to that file were needed.
impl PortValue for PulseWidth {
    fn kind_name() -> &'static str {
        "Pulse"
    }
}

// ── Graph socket type ────────────────────────────────────────────────────────

/// Graph-side identity for [`PulseWidth`]. Same shape as the built-in
/// socket types in `logic_analyzer_graph::nodes` (`Signal`, `Words`, ...), just defined in
/// this crate instead — no orphan-rule issue, no registration beyond this
/// `impl`.
pub struct PulseSocket;
impl SocketDef for PulseSocket {
    type Value = u64;

    fn type_name() -> &'static str {
        "Pulse"
    }
    fn color() -> Color32 {
        Color32::from_rgb(230, 150, 60)
    }
    fn shape() -> SocketShape {
        SocketShape::Diamond
    }
}

// ── Graph node ────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PulseMeasureState;

pub struct PulseMeasure;
impl NodeDef for PulseMeasure {
    type State = PulseMeasureState;

    fn name() -> &'static str {
        "Pulse Measure"
    }
    fn category() -> &'static str {
        "Plugin"
    }
    fn color() -> Color32 {
        Color32::from_rgb(160, 100, 40)
    }
    fn inputs() -> Vec<InputDef<Self::State>> {
        // Reuses the host crate's `Signal` socket type, proving cross-crate
        // socket-type reuse works with zero special-casing.
        vec![InputDef::new::<Signal>("Signal")]
    }
    fn outputs() -> Vec<OutputDef<Self::State>> {
        vec![OutputDef::new::<PulseSocket>("Pulse")]
    }
    fn state() -> Self::State {
        PulseMeasureState
    }
}

// ── Compiler builder ─────────────────────────────────────────────────────────

#[derive(Default)]
struct PulseMeasureBuilder;
impl RuntimeBuilder for PulseMeasureBuilder {
    fn accepted_kinds(&self, _socket: &Socket, _state: &Value) -> Vec<PortKind> {
        vec![PortKind::of::<Sample>()]
    }
    fn offered_kinds(&self, _socket: &Socket, _state: &Value) -> Vec<PortKind> {
        vec![PortKind::of::<PulseWidth>()]
    }
    fn input_port(&self, _: &Socket, _: usize, _: &Value, _: PortKind) -> Option<String> {
        Some("signal".into())
    }
    fn output_port(&self, _: &Socket, _: &Value, _: PortKind) -> Option<String> {
        Some("pulse".into())
    }
    fn build(
        &self,
        name: &str,
        _state: &Value,
        _resolved: &ResolvedInputs,
        _ctx: &mut CompileCtx,
    ) -> Result<Box<dyn ProcessNode>, String> {
        Ok(Box::new(PulseMeasureNode::new(name)))
    }
}

fn register_pulse_width_channel() {
    signal_processing::register_type::<PulseWidth>();
}

inventory::submit! {
    GraphNodeRegistration::runnable::<PulseMeasure, PulseMeasureBuilder>(
        "org.logicconduit.example.graph-node.pulse-measure/v1",
    )
    .requiring_payloads(&["org.logicconduit.digital-sample/v1"])
    .with_runtime_setup(&[register_pulse_width_channel])
}

// ── Runtime node ──────────────────────────────────────────────────────────────

/// Emits a [`PulseWidth`] each time the input returns to low, covering the
/// preceding high level.
struct PulseMeasureNode {
    name: String,
    prev: Option<Sample>,
    input_buffer: VecDeque<Sample>,
}

impl PulseMeasureNode {
    fn new(name: &str) -> Self {
        Self {
            name: name.to_string(),
            prev: None,
            input_buffer: VecDeque::new(),
        }
    }
}

impl ProcessNode for PulseMeasureNode {
    fn name(&self) -> &str {
        &self.name
    }
    fn num_inputs(&self) -> usize {
        1
    }
    fn num_outputs(&self) -> usize {
        1
    }
    fn input_schema(&self) -> Vec<PortSchema> {
        vec![PortSchema::new::<Sample>("signal", 0, PortDirection::Input)]
    }
    fn output_schema(&self) -> Vec<PortSchema> {
        vec![PortSchema::new::<PulseWidth>(
            "pulse",
            0,
            PortDirection::Output,
        )]
    }
    fn work(&mut self, inputs: &[InputPort], outputs: &[OutputPort]) -> WorkResult<usize> {
        let mut input = inputs
            .first()
            .and_then(|port| port.get::<Sample>(&mut self.input_buffer))
            .ok_or_else(|| WorkError::NodeError("Missing signal input".to_string()))?;
        let output = outputs
            .first()
            .and_then(|port| port.get::<PulseWidth>())
            .ok_or_else(|| WorkError::NodeError("Missing pulse output".to_string()))?;

        let sample = input.recv()?;
        if let Some(prev) = self.prev
            && prev.value
        {
            output.send(PulseWidth {
                width_ns: sample.start_time_ns.saturating_sub(prev.start_time_ns),
                start_time_ns: prev.start_time_ns,
            })?;
        }
        self.prev = Some(sample);
        Ok(1)
    }
}

#[cfg(test)]
mod tests {
    use crossbeam_channel::bounded;
    use signal_processing::{ChannelMessage, Sender, Watchdog};

    use super::*;

    #[test]
    fn inventory_populates_both_registries() {
        let nodes = logic_analyzer_graph::nodes::build_registry();
        let _builders = logic_analyzer_graph::BuilderRegistry::standard();
        assert_eq!(nodes.category_of("Pulse Measure"), Some("Plugin"));
    }

    #[test]
    fn emits_pulse_width_covering_the_preceding_high_level() {
        let wd = Watchdog::new();
        let (tx, rx) = bounded::<ChannelMessage<Sample>>(64);
        for sample in [
            Sample::new(true, 100),
            Sample::new(false, 150),
            Sample::new(true, 300),
            Sample::new(false, 320),
        ] {
            tx.send(ChannelMessage::Sample(sample)).unwrap();
        }
        drop(tx);
        let inputs = [InputPort::new_with_watchdog(rx, &wd, "pulse", "signal")];
        let (out_tx, out_rx) = bounded::<ChannelMessage<PulseWidth>>(64);
        let outputs = [OutputPort::new_with_watchdog(
            Sender::new(vec![out_tx]),
            &wd,
            "pulse",
            "pulse",
        )];

        let mut node = PulseMeasureNode::new("pulse");
        loop {
            match node.work(&inputs, &outputs) {
                Ok(_) => {}
                Err(WorkError::Shutdown) => break,
                Err(e) => panic!("unexpected error: {e}"),
            }
        }
        let widths: Vec<(u64, u64)> = out_rx
            .try_iter()
            .filter_map(|m| match m {
                ChannelMessage::Sample(p) => Some((p.start_time_ns, p.width_ns)),
                _ => None,
            })
            .collect();
        assert_eq!(widths, vec![(100, 50), (300, 20)]);
    }
}
