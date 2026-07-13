//! `Viewer` builder — the sink that feeds the logic analyzer's derived lanes.

use std::collections::HashMap;

use serde_json::Value;

use node_graph::Socket;
use signal_processing::{
    LiveStoreConfig, ProcessNode, Sample, Trigger, ViewerLaneKind, ViewerSink, Word,
};

use super::{CompileCtx, PortKind, ResolvedInputs, RuntimeBuilder, parse_state};
use crate::nodes;

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
        // `DerivedLanes` uses a lane name as its stable identity. Nodes of
        // the same type share the default title (e.g. two UART Decoders),
        // so make only colliding labels distinct instead of silently merging
        // their output into one row.
        let mut lane_name_counts: HashMap<String, usize> = HashMap::new();
        for (member, input) in resolved.members(0) {
            let lane_name = if prefix.is_empty() {
                input.source.clone()
            } else {
                format!("{prefix}: {}", input.source)
            };
            let count = lane_name_counts.entry(lane_name.clone()).or_default();
            *count += 1;
            let lane_name = if *count == 1 {
                lane_name
            } else {
                format!("{lane_name} ({count})")
            };
            sink = if input.kind == PortKind::of::<Sample>() {
                sink.with_lane(ViewerLaneKind::Signal, lane_name)
            } else if input.kind == PortKind::of::<Word>() {
                // UART's Bits/Data pair is drawn as two tracks in one
                // protocol row. Keep those compact annotations in memory so
                // they can be rendered together immediately.
                let uart_track = input.source.ends_with(".Bits") || input.source.ends_with(".Data");
                if uart_track {
                    sink = sink.with_indexed_words(false);
                }
                if let Some(Some(persistent)) = ctx.viewer_word_caches.get(member) {
                    sink = sink.with_word_store_config(LiveStoreConfig {
                        directory: persistent.directory.clone(),
                        persistence: Some(persistent.clone()),
                        ..LiveStoreConfig::default()
                    });
                }
                sink = sink.with_lane_format(
                    ViewerLaneKind::Words,
                    lane_name,
                    input.word_display_format.clone(),
                );
                if uart_track {
                    sink = sink.with_indexed_words(true);
                }
                sink
            } else if input.kind == PortKind::of::<Trigger>() {
                sink.with_lane(ViewerLaneKind::Trigger, lane_name)
            } else {
                return Err(format!("viewer cannot display {:?}", input.kind));
            };
        }
        Ok(Box::new(sink))
    }
}
