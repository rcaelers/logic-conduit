//! Runtime builder for `SR Flip-Flop`.

use serde_json::Value;

use logic_analyzer_processing::SrLatch;
use node_graph::Socket;
use signal_processing::{ProcessNode, Sample, Trigger};

use crate::compiler::{CompileCtx, PortKind, ResolvedInputs, RuntimeBuilder, parse_state};
use crate::nodes;

pub(crate) struct SrFlipFlopBuilder;

impl RuntimeBuilder for SrFlipFlopBuilder {
    fn accepted_kinds(&self, _socket: &Socket, _state: &Value) -> Vec<PortKind> {
        vec![PortKind::of::<Trigger>()]
    }
    fn offered_kinds(&self, _socket: &Socket, _state: &Value) -> Vec<PortKind> {
        vec![PortKind::of::<Sample>()]
    }
    fn input_port(&self, socket: &Socket, _: usize, _: &Value, _: PortKind) -> Option<String> {
        match socket.def_index {
            0 => Some("set".into()),
            1 => Some("reset".into()),
            _ => None,
        }
    }
    fn output_port(&self, _socket: &Socket, _state: &Value, _kind: PortKind) -> Option<String> {
        Some("q".into())
    }
    fn build(
        &self,
        name: &str,
        state: &Value,
        _resolved: &ResolvedInputs,
        _ctx: &mut CompileCtx,
    ) -> Result<Box<dyn ProcessNode>, String> {
        let state: nodes::SrFlipFlopState = parse_state(state)?;
        Ok(Box::new(SrLatch::new(state.initial.value).with_name(name)))
    }
}
