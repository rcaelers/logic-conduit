//! `Buffer` builder — see `crates/logic_analyzer_graph/src/nodes/buffer.rs` and
//! the buffer policy in `docs/APP_DESIGN.md`.

use serde_json::Value;

use node_graph::Socket;
use signal_processing::{
    BufferNode, NumberSample, ProcessNode, Sample, SampleBlock, TextSample, Trigger, Word,
};

use super::graph::{CompileCtx, ResolvedInputs, RuntimeBuilder, parse_state};
use super::port_kind::PortKind;
use crate::nodes;

/// Maps a `BufferState.kind` selection (see `nodes::buffer::KIND_LABELS`) to
/// the concrete `PortKind` it names. Falls back to `Signal` for state that
/// fails to parse — matches `nodes::BufferState::state()`'s default index.
fn selected_kind(state: &Value) -> PortKind {
    let selected = parse_state::<nodes::BufferState>(state)
        .map(|s| s.kind.selected().to_string())
        .unwrap_or_default();
    match selected.as_str() {
        "Block" => PortKind::of::<SampleBlock>(),
        "Word" => PortKind::of::<Word>(),
        "Number" => PortKind::of::<NumberSample>(),
        "Text" => PortKind::of::<TextSample>(),
        "Trigger" => PortKind::of::<Trigger>(),
        _ => PortKind::of::<Sample>(),
    }
}

pub(super) struct BufferBuilder;

impl RuntimeBuilder for BufferBuilder {
    fn accepted_kinds(&self, _socket: &Socket, state: &Value) -> Vec<PortKind> {
        vec![selected_kind(state)]
    }
    fn offered_kinds(&self, _socket: &Socket, state: &Value) -> Vec<PortKind> {
        vec![selected_kind(state)]
    }
    fn input_port(&self, _: &Socket, _: usize, _: &Value, _: PortKind) -> Option<String> {
        Some("in".into())
    }
    fn output_port(&self, _: &Socket, _: &Value, _: PortKind) -> Option<String> {
        Some("out".into())
    }
    fn input_buffer_override(&self, _socket: &Socket, state: &Value) -> Option<usize> {
        parse_state::<nodes::BufferState>(state)
            .ok()
            .map(|s| s.capacity.value.max(1) as usize)
    }
    fn build(
        &self,
        name: &str,
        state: &Value,
        _resolved: &ResolvedInputs,
        _ctx: &mut CompileCtx,
    ) -> Result<Box<dyn ProcessNode>, String> {
        let state: nodes::BufferState = parse_state(state)?;
        let node: Box<dyn ProcessNode> = match state.kind.selected() {
            "Block" => Box::new(BufferNode::<SampleBlock>::new(name)),
            "Word" => Box::new(BufferNode::<Word>::new(name)),
            "Number" => Box::new(BufferNode::<NumberSample>::new(name)),
            "Text" => Box::new(BufferNode::<TextSample>::new(name)),
            "Trigger" => Box::new(BufferNode::<Trigger>::new(name)),
            _ => Box::new(BufferNode::<Sample>::new(name)),
        };
        Ok(node)
    }
}
