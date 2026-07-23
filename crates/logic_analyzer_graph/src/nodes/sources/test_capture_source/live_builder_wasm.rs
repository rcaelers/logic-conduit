//! Browser stand-in for the deterministic live test source.

use serde_json::Value;

use logic_analyzer_graph_api::node::RuntimeBuilder;
use logic_analyzer_graph_api::node_support::{NodeBuildContext, PortKind, ResolvedInputs};
use logic_analyzer_processing::nodes::sources::synthetic_capture_source::SyntheticCaptureSource;
use node_graph::Socket;
use signal_processing::{ProcessNode, Sample, SampleBlock};

#[derive(Default)]
pub(crate) struct TestLiveCaptureSourceBuilder;

impl RuntimeBuilder for TestLiveCaptureSourceBuilder {
    fn is_source(&self) -> bool {
        true
    }

    fn accepted_kinds(&self, _socket: &Socket, _state: &Value) -> Vec<PortKind> {
        Vec::new()
    }

    fn offered_kinds(&self, _socket: &Socket, _state: &Value) -> Vec<PortKind> {
        vec![PortKind::of::<SampleBlock>(), PortKind::of::<Sample>()]
    }

    fn input_port(&self, _: &Socket, _: usize, _: &Value, _: PortKind) -> Option<String> {
        None
    }

    fn output_port(&self, socket: &Socket, _state: &Value, kind: PortKind) -> Option<String> {
        if kind == PortKind::of::<SampleBlock>() {
            Some(format!("block{}", socket.def_index))
        } else if kind == PortKind::of::<Sample>() {
            Some(format!("ch{}", socket.def_index))
        } else {
            None
        }
    }

    fn viewer_channel_origin(&self, socket: &Socket, _state: &Value) -> Option<usize> {
        Some(socket.def_index)
    }

    fn build(
        &self,
        name: &str,
        _state: &Value,
        _resolved: &ResolvedInputs,
        _ctx: &mut dyn NodeBuildContext,
    ) -> Result<Box<dyn ProcessNode>, String> {
        Ok(Box::new(SyntheticCaptureSource::new().with_name(name)))
    }
}
