use egui::Color32;
use serde_json::Value;

use logic_analyzer_graph_api::node::RuntimeBuilder;
use logic_analyzer_graph_api::node_support::{NodeBuildContext, PortKind, ResolvedInputs};
use node_graph::{AnySocket, InputDef, NodeDef, OutputDef, Socket};
use signal_processing::{InputPort, OutputPort, ProcessNode, WorkResult};

use crate::BuilderRegistry;

pub(crate) const SOURCE_NAME: &str = "Isolated Test Source";
pub(crate) const SINK_NAME: &str = "Isolated Test Sink";

pub(crate) struct TestSource;

impl NodeDef for TestSource {
    type State = ();

    fn name() -> &'static str {
        SOURCE_NAME
    }

    fn category() -> &'static str {
        "Test"
    }

    fn color() -> Color32 {
        Color32::GRAY
    }

    fn inputs() -> Vec<InputDef<Self::State>> {
        Vec::new()
    }

    fn outputs() -> Vec<OutputDef<Self::State>> {
        vec![OutputDef::new::<AnySocket>("Out").view_selectable(false)]
    }

    fn state() -> Self::State {}
}

pub(crate) struct TestSink;

impl NodeDef for TestSink {
    type State = ();

    fn name() -> &'static str {
        SINK_NAME
    }

    fn category() -> &'static str {
        "Test"
    }

    fn color() -> Color32 {
        Color32::GRAY
    }

    fn inputs() -> Vec<InputDef<Self::State>> {
        vec![InputDef::new::<AnySocket>("In").variadic(64)]
    }

    fn outputs() -> Vec<OutputDef<Self::State>> {
        Vec::new()
    }

    fn state() -> Self::State {}
}

pub(crate) fn install_builders(registry: &mut BuilderRegistry, kinds: Vec<PortKind>) {
    registry.insert_test_builder(SOURCE_NAME, Box::new(TestSourceBuilder(kinds.clone())));
    registry.insert_test_builder(SINK_NAME, Box::new(TestSinkBuilder(kinds)));
}

struct TestSourceBuilder(Vec<PortKind>);

impl RuntimeBuilder for TestSourceBuilder {
    fn is_source(&self) -> bool {
        true
    }

    fn accepted_kinds(&self, _socket: &Socket, _state: &Value) -> Vec<PortKind> {
        Vec::new()
    }

    fn offered_kinds(&self, _socket: &Socket, _state: &Value) -> Vec<PortKind> {
        self.0.clone()
    }

    fn input_port(
        &self,
        _socket: &Socket,
        _member_index: usize,
        _state: &Value,
        _kind: PortKind,
    ) -> Option<String> {
        None
    }

    fn output_port(&self, _socket: &Socket, _state: &Value, kind: PortKind) -> Option<String> {
        Some(format!("out_{}", kind.name()))
    }

    fn input_required(&self, _socket: &Socket, _state: &Value) -> bool {
        false
    }

    fn build(
        &self,
        name: &str,
        _state: &Value,
        _resolved: &ResolvedInputs,
        _ctx: &mut dyn NodeBuildContext,
    ) -> Result<Box<dyn ProcessNode>, String> {
        Ok(Box::new(InertEndpoint::new(name)))
    }
}

struct TestSinkBuilder(Vec<PortKind>);

impl RuntimeBuilder for TestSinkBuilder {
    fn is_sink(&self) -> bool {
        true
    }

    fn accepted_kinds(&self, _socket: &Socket, _state: &Value) -> Vec<PortKind> {
        self.0.clone()
    }

    fn offered_kinds(&self, _socket: &Socket, _state: &Value) -> Vec<PortKind> {
        Vec::new()
    }

    fn input_port(
        &self,
        _socket: &Socket,
        member_index: usize,
        _state: &Value,
        kind: PortKind,
    ) -> Option<String> {
        Some(format!("in_{member_index}_{}", kind.name()))
    }

    fn output_port(&self, _socket: &Socket, _state: &Value, _kind: PortKind) -> Option<String> {
        None
    }

    fn input_required(&self, _socket: &Socket, _state: &Value) -> bool {
        false
    }

    fn build(
        &self,
        name: &str,
        _state: &Value,
        _resolved: &ResolvedInputs,
        _ctx: &mut dyn NodeBuildContext,
    ) -> Result<Box<dyn ProcessNode>, String> {
        Ok(Box::new(InertEndpoint::new(name)))
    }
}

struct InertEndpoint {
    name: String,
}

impl InertEndpoint {
    fn new(name: &str) -> Self {
        Self {
            name: name.to_owned(),
        }
    }
}

impl ProcessNode for InertEndpoint {
    fn name(&self) -> &str {
        &self.name
    }

    fn num_inputs(&self) -> usize {
        0
    }

    fn num_outputs(&self) -> usize {
        0
    }

    fn work(&mut self, _inputs: &[InputPort], _outputs: &[OutputPort]) -> WorkResult<usize> {
        Ok(0)
    }
}
