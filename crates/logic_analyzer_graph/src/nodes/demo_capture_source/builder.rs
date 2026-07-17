//! Runtime builder for the in-memory mixed-protocol demo capture.

use serde_json::Value;

use logic_analyzer_processing::DemoCaptureSource;
use node_graph::Socket;
use signal_processing::{ProcessNode, Sample, SampleBlock};

use crate::compiler::{CompileCtx, PortKind, ResolvedInputs, RuntimeBuilder};

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
