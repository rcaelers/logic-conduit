//! Runtime builder for `DSL File Source`.
//! Native-only: no filesystem in the browser.

use serde_json::Value;

use logic_analyzer_processing::{DeferredDslFileSource, DslFileSource};
use node_graph::Socket;
use signal_processing::{
    DEFAULT_VIEWER_MAX_ENTRIES, ProcessNode, Sample, SampleBlock, TextSample, ViewerRetention,
};

use crate::{CompileCtx, PortKind, ResolvedInputs, RuntimeBuilder, nodes, parse_state};

pub(crate) struct FileSourceBuilder;

impl RuntimeBuilder for FileSourceBuilder {
    fn is_source(&self) -> bool {
        true
    }
    fn viewer_retention(&self, _state: &Value) -> ViewerRetention {
        ViewerRetention::MaxEntries(DEFAULT_VIEWER_MAX_ENTRIES)
    }
    fn accepted_kinds(&self, socket: &Socket, _state: &Value) -> Vec<PortKind> {
        match socket.def_index {
            // A wired File socket delivers the filename at run start (the
            // deferred source below); the trade-off is documented on
            // `logic_analyzer_processing::DeferredDslFileSource`.
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
    fn viewer_channel_origin(&self, socket: &Socket, _state: &Value) -> Option<usize> {
        Some(socket.def_index)
    }
    fn input_required(&self, _socket: &Socket, _state: &Value) -> bool {
        // Empty paths are valid in saved/example graphs.  Runtime start will
        // report a missing path when the user actually runs the source.
        false
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
                DeferredDslFileSource::new(channels).with_name(name),
            ));
        }
        let source = DslFileSource::new(&state.file.value, channels)
            .map_err(|e| format!("cannot open '{}': {e}", state.file.value))?
            .with_name(name);
        Ok(Box::new(source))
    }
}
