//! `DSL File Source` builder — reads channels from a `.dsl` capture file.
//! Native-only: no filesystem in the browser.

use super::{CompileCtx, PortKind, ResolvedInputs, RuntimeBuilder, parse_state};
use crate::nodes;
use dsl::runtime::ProcessNode;
use dsl::{Sample, SampleBlock, TextSample};
use node_graph::Socket;
use serde_json::Value;

pub(super) struct FileSourceBuilder;

impl RuntimeBuilder for FileSourceBuilder {
    fn is_source(&self) -> bool {
        true
    }
    fn accepted_kinds(&self, socket: &Socket, _state: &Value) -> Vec<PortKind> {
        match socket.def_index {
            // A wired File socket delivers the filename at run start (the
            // deferred source below); the trade-off is documented on
            // `dsl::DeferredDslFileSource`.
            0 => vec![PortKind::of::<TextSample>()],
            _ => vec![],
        }
    }
    fn offered_kinds(&self, _socket: &Socket, _state: &Value) -> Vec<PortKind> {
        vec![PortKind::of::<Sample>(), PortKind::of::<SampleBlock>()]
    }
    fn input_port(&self, socket: &Socket, _: usize, _: &Value, _: PortKind) -> Option<String> {
        (socket.def_index == 0).then(|| "filename".into())
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
    fn input_required(&self, socket: &Socket, state: &Value) -> bool {
        match socket.def_index {
            // Unconnected File with an empty picker is a configuration
            // error — catch it at compile time, not as a failed open.
            0 => parse_state::<nodes::DslFileSourceState>(state)
                .map(|state| state.file.value.trim().is_empty())
                .unwrap_or(true),
            _ => false,
        }
    }
    fn build(
        &self,
        name: &str,
        state: &Value,
        resolved: &ResolvedInputs,
        _ctx: &mut CompileCtx,
    ) -> Result<Box<dyn ProcessNode>, String> {
        let state: nodes::DslFileSourceState = parse_state(state)?;
        let channels = state.channels.value.clamp(1, 32) as u8;
        if resolved.kind(0).is_some() {
            // File socket wired: the path arrives over the wire at run
            // start; consumers stream (no index to query yet at build).
            return Ok(Box::new(
                dsl::DeferredDslFileSource::new(channels).with_name(name),
            ));
        }
        let source = dsl::DslFileSource::new(&state.file.value, channels)
            .map_err(|e| format!("cannot open '{}': {e}", state.file.value))?
            .with_name(name);
        Ok(Box::new(source))
    }
}
