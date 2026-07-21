//! Runtime builder for `Viewer`.

use std::collections::HashMap;
use std::sync::Arc;

use egui::Color32;
use serde_json::Value;

use logic_analyzer_viewer::{
    DerivedLaneId, ViewerLaneBadge, ViewerLaneGroup, ViewerLaneGroupId, ViewerLaneRenderer,
    ViewerLaneTrack, ViewerLaneTrackId,
};
use node_graph::Socket;
use signal_processing::{
    LiveStoreConfig, NumberSample, ProcessNode, Sample, TextSample, Trigger, ViewerLaneKind,
    ViewerSink, Word,
};

use crate::decoder_table::{DecoderTableColumn, DecoderTableSource};
use crate::{CompileCtx, PortKind, ResolvedInputs, RuntimeBuilder, parse_state};

pub(crate) struct ViewerBuilder;

struct PendingGroup {
    source_node: node_graph::NodeId,
    key: String,
    label: String,
    badge: ViewerLaneBadge,
    renderer: Arc<dyn ViewerLaneRenderer>,
    tracks: Vec<(usize, ViewerLaneTrack)>,
}

struct PendingTable {
    source_node: node_graph::NodeId,
    key: String,
    label: String,
    columns: Vec<(usize, DecoderTableColumn)>,
}

impl RuntimeBuilder for ViewerBuilder {
    fn is_sink(&self) -> bool {
        true
    }
    fn accepted_kinds(&self, _socket: &Socket, _state: &Value) -> Vec<PortKind> {
        vec![
            PortKind::of::<Sample>(),
            PortKind::of::<Word>(),
            PortKind::of::<Trigger>(),
            PortKind::of::<NumberSample>(),
            PortKind::of::<TextSample>(),
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
        let state: super::definition::ViewerState = parse_state(state)?;
        let prefix = state.label.value.trim().to_owned();
        let mut sink = ViewerSink::new(ctx.derived_lanes().clone())
            .with_name(name)
            .with_retention(ctx.viewer_retention());
        // `DerivedLanes` uses a lane name as its stable identity. Nodes of
        // the same type can share a default title,
        // so make only colliding labels distinct instead of silently merging
        // their output into one row.
        let mut lane_name_counts: HashMap<String, usize> = HashMap::new();
        let mut pending_groups: Vec<PendingGroup> = Vec::new();
        let mut pending_tables: Vec<PendingTable> = Vec::new();
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
            let lane_id = DerivedLaneId::new(lane_name.clone());
            sink = if input.kind == PortKind::of::<Sample>() {
                sink.with_lane(ViewerLaneKind::Signal, lane_name.clone())
            } else if input.kind == PortKind::of::<Word>() {
                if let Some(persistent) = ctx.viewer_word_cache(member) {
                    sink = sink.with_word_store_config(LiveStoreConfig {
                        directory: persistent.directory.clone(),
                        persistence: Some(persistent.clone()),
                        ..LiveStoreConfig::default()
                    });
                }
                sink = sink.with_lane_format(
                    ViewerLaneKind::Words,
                    lane_name.clone(),
                    input.word_display_format.clone(),
                );
                sink
            } else if input.kind == PortKind::of::<Trigger>() {
                sink.with_lane(ViewerLaneKind::Trigger, lane_name.clone())
            } else if input.kind == PortKind::of::<NumberSample>() {
                sink.with_lane(ViewerLaneKind::Number, lane_name.clone())
            } else if input.kind == PortKind::of::<TextSample>() {
                sink.with_lane(ViewerLaneKind::Text, lane_name.clone())
            } else {
                return Err(format!("viewer cannot display {:?}", input.kind));
            };

            if let Some(presentation) = &input.viewer_presentation {
                let label = if prefix.is_empty() {
                    input.source_node_title.clone()
                } else {
                    format!("{prefix}: {}", input.source_node_title)
                };
                let track = ViewerLaneTrack::new(
                    presentation.track_key.clone(),
                    lane_id,
                    presentation.relative_height,
                );
                if let Some(group) = pending_groups.iter_mut().find(|group| {
                    group.source_node == input.source_node && group.key == presentation.group_key
                }) {
                    group.tracks.push((presentation.track_order, track));
                } else {
                    pending_groups.push(PendingGroup {
                        source_node: input.source_node,
                        key: presentation.group_key.clone(),
                        label,
                        badge: presentation.badge.clone(),
                        renderer: Arc::clone(&presentation.renderer),
                        tracks: vec![(presentation.track_order, track)],
                    });
                }
            } else {
                let badge = if input.kind == PortKind::of::<Sample>() {
                    ViewerLaneBadge::new("S", Color32::from_rgb(95, 175, 95))
                } else if input.kind == PortKind::of::<Word>() {
                    ViewerLaneBadge::new("W", Color32::from_rgb(215, 140, 60))
                } else if input.kind == PortKind::of::<NumberSample>() {
                    ViewerLaneBadge::new("N", Color32::from_rgb(95, 145, 210))
                } else if input.kind == PortKind::of::<TextSample>() {
                    ViewerLaneBadge::new("TXT", Color32::from_rgb(215, 150, 170))
                } else {
                    ViewerLaneBadge::new("T", Color32::from_rgb(230, 190, 80))
                };
                ctx.viewer_lanes().register(ViewerLaneGroup::singleton(
                    ViewerLaneGroupId::new(format!("{name}:lane:{member}")),
                    lane_name.clone(),
                    badge,
                    lane_id,
                ));
            }
            if let Some(table) = &input.decoder_table_column {
                let column = DecoderTableColumn {
                    key: table.column_key.clone(),
                    label: table.label.clone(),
                    lane: DerivedLaneId::new(lane_name.clone()),
                    track: ViewerLaneTrackId::new(table.track_key.clone()),
                    row_anchor: table.row_anchor,
                    cell_mode: table.cell_mode.clone(),
                    renderer: Arc::clone(&table.renderer),
                };
                if let Some(source) = pending_tables.iter_mut().find(|source| {
                    source.source_node == input.source_node && source.key == table.source_key
                }) {
                    source.columns.push((table.order, column));
                } else {
                    pending_tables.push(PendingTable {
                        source_node: input.source_node,
                        key: table.source_key.clone(),
                        label: input.source_node_title.clone(),
                        columns: vec![(table.order, column)],
                    });
                }
            }
        }
        for mut pending in pending_groups {
            pending.tracks.sort_by_key(|(order, _)| *order);
            ctx.viewer_lanes().register(ViewerLaneGroup {
                id: ViewerLaneGroupId::new(format!(
                    "{name}:node:{}:{}",
                    pending.source_node.0, pending.key
                )),
                label: pending.label,
                badge: pending.badge,
                tracks: pending.tracks.into_iter().map(|(_, track)| track).collect(),
                renderer: pending.renderer,
            });
        }
        for mut pending in pending_tables {
            pending.columns.sort_by_key(|(order, _)| *order);
            ctx.decoder_tables().register(DecoderTableSource {
                id: format!("{name}:node:{}:{}", pending.source_node.0, pending.key),
                label: pending.label,
                columns: pending
                    .columns
                    .into_iter()
                    .map(|(_, column)| column)
                    .collect(),
            });
        }
        Ok(Box::new(sink))
    }
}
