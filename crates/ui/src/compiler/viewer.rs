//! `Viewer` builder — the sink that feeds the logic analyzer's derived lanes.

use super::{CompileCtx, PortKind, ResolvedInputs, RuntimeBuilder, parse_state};
use crate::nodes;
use dsl::runtime::ProcessNode;
use dsl::{ParallelWord, Sample, SpiTransfer, Trigger, ViewerLaneKind, ViewerSink};
use node_graph::Socket;
use serde_json::Value;

pub(super) struct ViewerBuilder;

impl RuntimeBuilder for ViewerBuilder {
    fn is_sink(&self) -> bool {
        true
    }
    fn accepted_kinds(&self, _socket: &Socket, _state: &Value) -> Vec<PortKind> {
        vec![
            PortKind::of::<Sample>(),
            PortKind::of::<SpiTransfer>(),
            PortKind::of::<ParallelWord>(),
            PortKind::of::<Trigger>(),
        ]
    }
    fn offered_kinds(&self, _socket: &Socket, _state: &Value) -> Vec<PortKind> {
        vec![]
    }
    fn input_port(
        &self,
        _socket: &Socket,
        member_index: usize,
        _state: &Value,
        _kind: PortKind,
    ) -> Option<String> {
        Some(format!("in{member_index}"))
    }
    fn output_port(&self, _: &Socket, _: &Value, _: PortKind) -> Option<String> {
        None
    }
    fn input_required(&self, _: &Socket, _: &Value) -> bool {
        // A lane-less viewer is pointless but harmless.
        false
    }
    fn build(
        &self,
        name: &str,
        state: &Value,
        resolved: &ResolvedInputs,
        ctx: &mut CompileCtx,
    ) -> Result<Box<dyn ProcessNode>, String> {
        let state: nodes::ViewerState = parse_state(state)?;
        let prefix = state.label.value.trim().to_owned();
        let mut sink = ViewerSink::new(ctx.derived_lanes.clone()).with_name(name);
        for (_, input) in resolved.members(0) {
            let lane_name = if prefix.is_empty() {
                input.source.clone()
            } else {
                format!("{prefix}: {}", input.source)
            };
            // `ViewerSink` only ever sees `Signal`/`Words`/`Trigger` —
            // picking the concrete `T: WordSource` for a `Words` lane is
            // this builder's job, not something the viewer itself needs to
            // know (see `ViewerLaneKind`'s doc).
            sink = if input.kind == PortKind::of::<Sample>() {
                sink.with_lane(ViewerLaneKind::Signal, lane_name)
            } else if input.kind == PortKind::of::<SpiTransfer>() {
                sink.with_words_lane::<SpiTransfer>(lane_name)
            } else if input.kind == PortKind::of::<ParallelWord>() {
                sink.with_words_lane::<ParallelWord>(lane_name)
            } else if input.kind == PortKind::of::<Trigger>() {
                sink.with_lane(ViewerLaneKind::Trigger, lane_name)
            } else {
                return Err(format!("viewer cannot display {:?}", input.kind));
            };
        }
        Ok(Box::new(sink))
    }
}
