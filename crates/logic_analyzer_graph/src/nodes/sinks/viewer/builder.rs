//! Runtime builder for `Viewer`.

use std::collections::HashMap;
use std::sync::Arc;

use serde_json::Value;

use logic_analyzer_viewer::{
    DerivedLaneId, ViewerLaneBadge, ViewerLaneGroup, ViewerLaneGroupId, ViewerLaneRenderer,
    ViewerLaneTrack,
};
use node_graph::Socket;

use crate::{CompileCtx, PortKind, ResolvedInputs, RuntimeBuilder, parse_state};

pub(crate) struct ViewerSubscriptionBuilder;

struct PendingGroup {
    source_node: node_graph::NodeId,
    key: String,
    label: String,
    badge: ViewerLaneBadge,
    renderer: Arc<dyn ViewerLaneRenderer>,
    tracks: Vec<(usize, ViewerLaneTrack)>,
}

impl RuntimeBuilder for ViewerSubscriptionBuilder {
    fn is_data_subscription(&self) -> bool {
        true
    }
    fn collected_lane_names(
        &self,
        state: &Value,
        resolved: &ResolvedInputs,
    ) -> Vec<(usize, String)> {
        let Ok(state) = parse_state::<super::definition::ViewerState>(state) else {
            return Vec::new();
        };
        let prefix = state.label.value.trim();
        let mut counts: HashMap<String, usize> = HashMap::new();
        resolved
            .members(0)
            .into_iter()
            .map(|(member, input)| {
                let base = if prefix.is_empty() {
                    input.source.clone()
                } else {
                    format!("{prefix}: {}", input.source)
                };
                let count = counts.entry(base.clone()).or_default();
                *count += 1;
                let name = if *count == 1 {
                    base
                } else {
                    format!("{base} ({count})")
                };
                (member, name)
            })
            .collect()
    }
    fn accepted_kinds(&self, _socket: &Socket, _state: &Value) -> Vec<PortKind> {
        // `lower()` supplies the registry's subscribed payload kinds for a
        // data subscription. Keeping this empty prevents a second, fixed
        // built-in list from becoming the source of truth.
        Vec::new()
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
    fn register_presentations(
        &self,
        name: &str,
        state: &Value,
        resolved: &ResolvedInputs,
        lane_names: &[(usize, String)],
        ctx: &CompileCtx,
    ) -> Result<(), String> {
        let state: super::definition::ViewerState = parse_state(state)?;
        let prefix = state.label.value.trim().to_owned();
        let mut pending_groups: Vec<PendingGroup> = Vec::new();
        for (member, input) in resolved.members(0) {
            let lane_name = lane_names
                .iter()
                .find(|(lane_member, _)| *lane_member == member)
                .map(|(_, name)| name)
                .ok_or_else(|| format!("missing retained lane identity for input {member}"))?;
            let lane_id = DerivedLaneId::new(lane_name.clone());

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
                let presentation = input.default_viewer_presentation.as_ref().ok_or_else(|| {
                    format!(
                        "subscribed payload '{}' has no default presentation",
                        input.kind.name()
                    )
                })?;
                ctx.waveform_presentations().register(ViewerLaneGroup {
                    id: ViewerLaneGroupId::new(format!("{name}:lane:{member}")),
                    label: lane_name.clone(),
                    badge: presentation.badge().clone(),
                    tracks: vec![ViewerLaneTrack::new("primary", lane_id, 1.0)],
                    renderer: presentation.renderer(),
                });
            }
        }
        for mut pending in pending_groups {
            pending.tracks.sort_by_key(|(order, _)| *order);
            ctx.waveform_presentations().register(ViewerLaneGroup {
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
        Ok(())
    }
}
