//! `DSL File Source` builder — reads channels from a `.dsl` capture file.
//! Native-only: no filesystem in the browser.

use super::{CompileCtx, PortKind, ResolvedInputs, RuntimeBuilder, parse_state};
use crate::nodes;
use dsl::runtime::ProcessNode;
use dsl::{Sample, SampleBlock};
use node_graph::Socket;
use serde_json::Value;

pub(super) struct FileSourceBuilder;

impl RuntimeBuilder for FileSourceBuilder {
    fn is_source(&self) -> bool {
        true
    }
    fn accepted_kinds(&self, _socket: &Socket, _state: &Value) -> Vec<PortKind> {
        vec![]
    }
    fn offered_kinds(&self, _socket: &Socket, _state: &Value) -> Vec<PortKind> {
        vec![PortKind::of::<Sample>(), PortKind::of::<SampleBlock>()]
    }
    fn input_port(&self, _: &Socket, _: usize, _: &Value, _: PortKind) -> Option<String> {
        None
    }
    fn output_port(&self, socket: &Socket, _state: &Value, kind: PortKind) -> Option<String> {
        let channel = socket.def_index;
        // The runtime negotiates Sample vs SampleBlock per connection on a
        // single `ch{channel}` port now — both kinds resolve to the same
        // port name here.
        if kind == PortKind::of::<Sample>() || kind == PortKind::of::<SampleBlock>() {
            Some(format!("ch{channel}"))
        } else {
            None
        }
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
        let state: nodes::DslFileSourceState = parse_state(state)?;
        let channels = state.channels.value.clamp(1, 32) as u8;
        let source = dsl::DslFileSource::new(&state.file.value, channels)
            .map_err(|e| format!("cannot open '{}': {e}", state.file.value))?
            .with_name(name);
        Ok(Box::new(source))
    }
}
