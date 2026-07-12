//! `Viewer` builder — the sink that feeds the logic analyzer's derived lanes.

use super::{CompileCtx, PortKind, ResolvedInputs, RuntimeBuilder, parse_state};
use crate::nodes;
use dsl::runtime::ProcessNode;
#[cfg(not(target_arch = "wasm32"))]
use dsl::runtime::derived_word_store::LiveStoreConfig;
use dsl::{Sample, Trigger, ViewerLaneKind, ViewerSink, Word};
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
            PortKind::of::<Word>(),
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
        let mut sink = ViewerSink::new(ctx.derived_lanes.clone())
            .with_name(name)
            .with_retention(ctx.viewer_retention);
        for (member, input) in resolved.members(0) {
            #[cfg(target_arch = "wasm32")]
            let _ = member;
            let lane_name = if prefix.is_empty() {
                input.source.clone()
            } else {
                format!("{prefix}: {}", input.source)
            };
            sink = if input.kind == PortKind::of::<Sample>() {
                sink.with_lane(ViewerLaneKind::Signal, lane_name)
            } else if input.kind == PortKind::of::<Word>() {
                #[cfg(not(target_arch = "wasm32"))]
                if let Some(Some(persistent)) = ctx.viewer_word_caches.get(member) {
                    sink = sink.with_word_store_config(LiveStoreConfig {
                        directory: persistent.directory.clone(),
                        persistence: Some(persistent.clone()),
                        ..LiveStoreConfig::default()
                    });
                }
                sink.with_lane(ViewerLaneKind::Words, lane_name)
            } else if input.kind == PortKind::of::<Trigger>() {
                sink.with_lane(ViewerLaneKind::Trigger, lane_name)
            } else {
                return Err(format!("viewer cannot display {:?}", input.kind));
            };
        }
        Ok(Box::new(sink))
    }
}
