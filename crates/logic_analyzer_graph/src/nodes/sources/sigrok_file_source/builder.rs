//! Runtime builder for `Sigrok File Source`.

use serde_json::Value;

use logic_analyzer_processing::SigrokFileSource;
use node_graph::Socket;
use signal_processing::{ProcessNode, Sample};

use crate::{CompileCtx, PortKind, ResolvedInputs, RuntimeBuilder, parse_state};

pub(crate) struct SigrokFileSourceBuilder;

impl RuntimeBuilder for SigrokFileSourceBuilder {
    fn is_source(&self) -> bool {
        true
    }
    fn accepted_kinds(&self, _socket: &Socket, _state: &Value) -> Vec<PortKind> {
        vec![]
    }
    fn offered_kinds(&self, _socket: &Socket, _state: &Value) -> Vec<PortKind> {
        vec![PortKind::of::<Sample>()]
    }
    fn input_port(&self, _socket: &Socket, _: usize, _: &Value, _: PortKind) -> Option<String> {
        None
    }
    fn output_port(&self, socket: &Socket, _state: &Value, kind: PortKind) -> Option<String> {
        (kind == PortKind::of::<Sample>()).then(|| format!("ch{}", socket.def_index))
    }
    fn viewer_channel_origin(&self, socket: &Socket, _state: &Value) -> Option<usize> {
        Some(socket.def_index)
    }
    fn input_required(&self, socket: &Socket, state: &Value) -> bool {
        socket.def_index == 0
            && parse_state::<super::definition::SigrokFileSourceState>(state)
                .map(|state| state.file.value.trim().is_empty())
                .unwrap_or(true)
    }
    fn build(
        &self,
        name: &str,
        state: &Value,
        _resolved: &ResolvedInputs,
        _ctx: &mut CompileCtx,
    ) -> Result<Box<dyn ProcessNode>, String> {
        let state: super::definition::SigrokFileSourceState = parse_state(state)?;
        SigrokFileSource::new(&state.file.value, state.channels.value.clamp(1, 32) as u8)
            .map(|source| Box::new(source.with_name(name)) as Box<dyn ProcessNode>)
            .map_err(|error| format!("cannot open '{}': {error}", state.file.value))
    }
}
