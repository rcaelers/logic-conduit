//! `UART Demo Source` builder — generates a fixed UART byte sequence
//! in-memory. Available on every target (no file/USB access needed).

use super::{CompileCtx, PortKind, ResolvedInputs, RuntimeBuilder, parse_state};
use crate::nodes;
use dsl::runtime::ProcessNode;
use node_graph::Socket;
use serde_json::Value;

pub(super) struct UartDemoSourceBuilder;

impl RuntimeBuilder for UartDemoSourceBuilder {
    fn is_source(&self) -> bool {
        true
    }
    fn accepted_kinds(&self, _socket: &Socket, _state: &Value) -> Vec<PortKind> {
        vec![]
    }
    fn offered_kinds(&self, _socket: &Socket, _state: &Value) -> Vec<PortKind> {
        vec![PortKind::SampleEdge]
    }
    fn input_port(&self, _: &Socket, _: usize, _: &Value, _: PortKind) -> Option<String> {
        None
    }
    fn output_port(&self, _socket: &Socket, _state: &Value, kind: PortKind) -> Option<String> {
        (kind == PortKind::SampleEdge).then(|| "rx".into())
    }
    fn input_required(&self, _: &Socket, _: &Value) -> bool {
        false
    }
    fn build(
        &self,
        name: &str,
        state: &Value,
        _resolved: &ResolvedInputs,
        _ctx: &mut CompileCtx,
    ) -> Result<Box<dyn ProcessNode>, String> {
        let state: nodes::UartDemoSourceState = parse_state(state)?;
        let source = dsl::UartDemoSource::new(
            state.message.value.into_bytes(),
            state.baud_rate.value.max(1) as u64,
        )
        .with_name(name);
        Ok(Box::new(source))
    }
}
