//! Browser runtime builder for `DSL File Source`.

use serde_json::Value;

use logic_analyzer_graph_api::node::RuntimeBuilder;
use logic_analyzer_graph_api::node_support::{
    CapturePresentation, NodeBuildContext, PortKind, ResolvedInputs, parse_state,
};
use logic_analyzer_processing::nodes::sources::synthetic_capture_source::SyntheticCaptureSource;
use node_graph::Socket;
use signal_processing::{DerivedDataRetention, ProcessNode, Sample, SampleBlock, TextSample};

#[derive(Default)]
pub(crate) struct FileSourceBuilder;

impl RuntimeBuilder for FileSourceBuilder {
    fn is_source(&self) -> bool {
        true
    }

    fn derived_data_retention(&self, _state: &Value) -> DerivedDataRetention {
        DerivedDataRetention::Unlimited
    }

    fn accepted_kinds(&self, socket: &Socket, _state: &Value) -> Vec<PortKind> {
        if socket.def_index == 0 {
            vec![PortKind::of::<TextSample>()]
        } else {
            Vec::new()
        }
    }

    fn offered_kinds(&self, _socket: &Socket, _state: &Value) -> Vec<PortKind> {
        vec![PortKind::of::<SampleBlock>(), PortKind::of::<Sample>()]
    }

    fn input_port(&self, socket: &Socket, _: usize, _: &Value, _: PortKind) -> Option<String> {
        (socket.def_index == 0).then(|| "filename".to_owned())
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

    fn capture_presentation(&self, state: &Value) -> Result<Option<CapturePresentation>, String> {
        let state: super::definition::DslFileSourceState = parse_state(state)?;
        let channels = state.channels.value.clamp(1, 32) as usize;
        Ok(Some(
            super::super::synthetic_presentation::capture_presentation(
                (0..channels).map(|channel| format!("Ch {channel}")),
            ),
        ))
    }

    fn input_required(&self, _socket: &Socket, _state: &Value) -> bool {
        false
    }

    fn build(
        &self,
        name: &str,
        state: &Value,
        _resolved: &ResolvedInputs,
        _ctx: &mut dyn NodeBuildContext,
    ) -> Result<Box<dyn ProcessNode>, String> {
        let state: super::definition::DslFileSourceState = parse_state(state)?;
        Ok(Box::new(
            SyntheticCaptureSource::new()
                .with_channel_count(state.channels.value.clamp(1, 32) as usize)
                .with_name(name),
        ))
    }
}
