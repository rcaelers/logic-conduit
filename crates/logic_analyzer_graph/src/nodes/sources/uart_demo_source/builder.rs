//! Runtime builder for `UART Demo Source` — generates a fixed UART byte sequence
//! in-memory. Available on every target (no file/USB access needed).

use serde_json::Value;

use logic_analyzer_processing::UartDemoSource;
use node_graph::Socket;
use signal_processing::{ProcessNode, Sample};

use crate::{CompileCtx, PortKind, ResolvedInputs, RuntimeBuilder, parse_state};

pub(crate) struct UartDemoSourceBuilder;

impl RuntimeBuilder for UartDemoSourceBuilder {
    fn is_source(&self) -> bool {
        true
    }
    fn accepted_kinds(&self, _socket: &Socket, _state: &Value) -> Vec<PortKind> {
        vec![]
    }
    fn offered_kinds(&self, _socket: &Socket, _state: &Value) -> Vec<PortKind> {
        vec![PortKind::of::<Sample>()]
    }
    fn input_port(&self, _: &Socket, _: usize, _: &Value, _: PortKind) -> Option<String> {
        None
    }
    fn output_port(&self, _socket: &Socket, _state: &Value, kind: PortKind) -> Option<String> {
        (kind == PortKind::of::<Sample>()).then(|| "rx".into())
    }
    fn viewer_channel_origin(&self, _socket: &Socket, _state: &Value) -> Option<usize> {
        Some(0)
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
        let state: super::definition::UartDemoSourceState = parse_state(state)?;
        let source = UartDemoSource::new(
            state.message.value.into_bytes(),
            state.baud_rate.value.max(1) as u64,
        )
        .with_name(name);
        Ok(Box::new(source))
    }
}
