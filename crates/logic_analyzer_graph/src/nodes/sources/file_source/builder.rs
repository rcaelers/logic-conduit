//! Runtime builder for `DSL File Source`.
//! Native-only: no filesystem in the browser.

use serde_json::Value;

use logic_analyzer_processing::nodes::sources::dsl_file::DslFileSource;
use node_graph::Socket;
use signal_processing::{
    DEFAULT_DERIVED_DATA_MAX_ENTRIES, DerivedDataRetention, ProcessNode, Sample, SampleBlock,
    TextSample,
};

use crate::{
    CaptureCacheIdentity, CapturePresentation, CompileCtx, PortKind, ResolvedInputs,
    RuntimeBuilder, parse_state,
};

#[derive(Default)]
pub(crate) struct FileSourceBuilder;

impl RuntimeBuilder for FileSourceBuilder {
    fn is_source(&self) -> bool {
        true
    }
    fn derived_data_retention(&self, _state: &Value) -> DerivedDataRetention {
        DerivedDataRetention::MaxEntries(DEFAULT_DERIVED_DATA_MAX_ENTRIES)
    }
    fn accepted_kinds(&self, socket: &Socket, _state: &Value) -> Vec<PortKind> {
        match socket.def_index {
            // A wired File socket delivers the filename at run start (the
            // source below); the trade-off is documented on
            // `logic_analyzer_processing::nodes::sources::dsl_file::DslFileSource`.
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
    fn capture_presentation(&self, state: &Value) -> Result<Option<CapturePresentation>, String> {
        let state: super::definition::DslFileSourceState = parse_state(state)?;
        let path = std::path::PathBuf::from(state.file.value);
        if path.as_os_str().is_empty() {
            return Ok(None);
        }
        let indexed = DslFileSource::indexed_capture_presentation(&path);
        Ok(Some(CapturePresentation::Indexed {
            identity: indexed.identity,
            factory: indexed.factory,
        }))
    }
    fn capture_cache_identity(
        &self,
        state: &Value,
        resolved: &ResolvedInputs,
    ) -> CaptureCacheIdentity {
        let Ok(state) = parse_state::<super::definition::DslFileSourceState>(state) else {
            return CaptureCacheIdentity::Dynamic;
        };
        if resolved.kind(0).is_some() || state.file.value.trim().is_empty() {
            return CaptureCacheIdentity::Dynamic;
        }
        DslFileSource::capture_cache_identity(&state.file.value)
            .map(CaptureCacheIdentity::Stable)
            .unwrap_or(CaptureCacheIdentity::Dynamic)
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
        let state: super::definition::DslFileSourceState = parse_state(state)?;
        let channels = state.channels.value.clamp(1, 32) as u8;
        if resolved.kind(0).is_some() {
            // File socket wired: the path arrives over the wire at run
            // start; consumers stream (no index to query yet at build).
            return Ok(DslFileSource::from_filename_input(name, channels));
        }
        let source = DslFileSource::new(&state.file.value, channels)
            .map_err(|e| format!("cannot open '{}': {e}", state.file.value))?
            .with_name(name);
        Ok(Box::new(source))
    }
}
