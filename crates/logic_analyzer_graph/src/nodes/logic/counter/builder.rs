//! Runtime builder for `Counter`.

use serde_json::Value;

use logic_analyzer_processing::nodes::logic::trigger_counter::TriggerCounter;
use node_graph::Socket;
use signal_processing::{NumberSample, ProcessNode, Trigger};

use crate::{CompileCtx, PortKind, ResolvedInputs, RuntimeBuilder, parse_state};

#[derive(Default)]
pub(crate) struct CounterBuilder;

impl RuntimeBuilder for CounterBuilder {
    fn accepted_kinds(&self, _socket: &Socket, _state: &Value) -> Vec<PortKind> {
        vec![PortKind::of::<Trigger>()]
    }
    fn offered_kinds(&self, _socket: &Socket, _state: &Value) -> Vec<PortKind> {
        vec![PortKind::of::<NumberSample>()]
    }
    fn input_port(&self, _: &Socket, _: usize, _: &Value, _: PortKind) -> Option<String> {
        Some("trigger".into())
    }
    fn output_port(&self, _socket: &Socket, _state: &Value, _kind: PortKind) -> Option<String> {
        Some("count".into())
    }
    fn build(
        &self,
        name: &str,
        state: &Value,
        _resolved: &ResolvedInputs,
        _ctx: &mut CompileCtx,
    ) -> Result<Box<dyn ProcessNode>, String> {
        let state: super::definition::CounterState = parse_state(state)?;
        Ok(Box::new(
            TriggerCounter::new(state.start.value as i64, state.step.value as i64).with_name(name),
        ))
    }
}
