//! Runtime builder for the in-memory mixed-protocol demo capture.

use serde_json::Value;

use logic_analyzer_processing::DemoCaptureSource;
use node_graph::Socket;
use signal_processing::{ProcessNode, Sample, SampleBlock};

use crate::{
    CompileCtx, LiveCaptureEdit, LiveCaptureFeature, PortKind, ResolvedInputs, RuntimeBuilder,
    TriggerConfigurationFeature,
};

pub(crate) struct DemoCaptureSourceBuilder;

impl RuntimeBuilder for DemoCaptureSourceBuilder {
    fn is_source(&self) -> bool {
        true
    }

    fn accepted_kinds(&self, _socket: &Socket, _state: &Value) -> Vec<PortKind> {
        vec![]
    }

    fn offered_kinds(&self, _socket: &Socket, _state: &Value) -> Vec<PortKind> {
        vec![PortKind::of::<SampleBlock>(), PortKind::of::<Sample>()]
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

    fn live_capture_feature(
        &self,
        state: &Value,
    ) -> Result<Option<Box<dyn LiveCaptureFeature>>, String> {
        super::live_capture::feature(state)
    }

    fn trigger_configuration(
        &self,
        state: &Value,
    ) -> Result<Option<TriggerConfigurationFeature>, String> {
        let state =
            serde_json::from_value::<super::definition::DemoCaptureSourceState>(state.clone())
                .map_err(|error| format!("invalid demo capture state: {error}"))?;
        super::trigger::configuration(&state).map(Some)
    }

    fn apply_live_capture_edit(
        &self,
        state: &Value,
        edit: &LiveCaptureEdit,
    ) -> Result<Option<Value>, String> {
        super::implementation::apply_live_capture_edit(state, edit).map(Some)
    }

    fn input_required(&self, _socket: &Socket, _state: &Value) -> bool {
        false
    }

    fn build(
        &self,
        name: &str,
        _state: &Value,
        _resolved: &ResolvedInputs,
        _ctx: &mut CompileCtx,
    ) -> Result<Box<dyn ProcessNode>, String> {
        Ok(Box::new(DemoCaptureSource::new().with_name(name)))
    }
}
