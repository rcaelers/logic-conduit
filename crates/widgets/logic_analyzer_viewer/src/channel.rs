use std::borrow::Cow;
use std::collections::HashSet;

use egui::{CursorIcon, Pos2, Rect, Response, Ui, vec2};

use signal_processing::{
    CaptureMetadata, CaptureSampledWindow, CaptureWaveformSegment, CollectedLaneSnapshotRequest,
};

use crate::lanes::{DerivedLaneId, ViewerLaneGroup, ViewerLaneGroupId, ViewerLaneInteraction};
use crate::sampling::sample_to_us;
use crate::types::{
    AnalyzerLayout, RowDragState, RowKey, RowLabel, RowRenameState, Transition, ViewerRowId,
    WaveformSegment, WaveformSegmentKind,
};
use crate::viewer::LogicAnalyzerViewer;

#[derive(Debug, Clone)]
pub(crate) struct LogicChannel {
    pub(crate) index: usize,
    pub(crate) name: String,
    pub(crate) initial: bool,
    pub(crate) transitions: Vec<Transition>,
    pub(crate) waveform: Vec<WaveformSegment>,
}

impl LogicChannel {
    pub(crate) fn visible_transitions(&self, start_us: f64, end_us: f64) -> (&[Transition], bool) {
        let start_index = self
            .transitions
            .partition_point(|transition| transition.time_us < start_us);
        let end_index = start_index
            + self.transitions[start_index..]
                .partition_point(|transition| transition.time_us <= end_us);
        let value = start_index
            .checked_sub(1)
            .and_then(|index| self.transitions.get(index))
            .map_or(self.initial, |transition| transition.value);
        (&self.transitions[start_index..end_index], value)
    }
}

impl LogicAnalyzerViewer {
    pub(crate) fn row_top(&self, origin_y: f32, row: usize, default_height: f32) -> f32 {
        origin_y
            + self
                .row_order
                .iter()
                .take(row)
                .map(|key| self.display_row_height(key, default_height))
                .sum::<f32>()
    }

    pub(crate) fn row_at_vertical(
        &self,
        origin_y: f32,
        y: f32,
        default_height: f32,
    ) -> Option<usize> {
        let mut top = origin_y;
        for (row, key) in self.row_order.iter().enumerate() {
            let height = self.display_row_height(key, default_height);
            if y >= top && y < top + height {
                return Some(row);
            }
            top += height;
        }
        None
    }

    pub(crate) fn display_row_height(&self, key: &RowKey, default_height: f32) -> f32 {
        let group = match key {
            RowKey::Derived(id) => self
                .waveform_presentations
                .read()
                .iter()
                .find(|group| &group.id == id)
                .cloned(),
            RowKey::Channel(_) => None,
        };
        if let Some(group) = group {
            return group.renderer.row_height(&group, default_height);
        }
        default_height
    }

    /// What to show for one row's label, whatever it is — the only place
    /// that knows a channel's badge is its index (colored by
    /// `color_profile`) and a derived lane's badge is a kind glyph (colored
    /// by payload family, matching the socket colors in the node editor).
    /// Respects a user rename either way.
    pub(crate) fn row_label(&self, key: &RowKey) -> Option<RowLabel> {
        match key {
            RowKey::Channel(index) => {
                let channel = self
                    .channels
                    .iter()
                    .find(|channel| channel.index == *index)?;
                Some(RowLabel {
                    name: channel.name.clone(),
                    badge_text: index.to_string(),
                    badge_color: self.color_profile.channel_color(*index),
                })
            }
            RowKey::Derived(group_id) => {
                let groups = self.waveform_presentations.read();
                let group = groups.iter().find(|group| &group.id == group_id)?;
                let name = self
                    .derived_names
                    .get(group_id)
                    .cloned()
                    .unwrap_or_else(|| group.label.clone());
                Some(RowLabel {
                    name,
                    badge_text: group.badge.text.clone(),
                    badge_color: group.badge.color,
                })
            }
        }
    }

    pub(crate) fn handle_row_label_input(
        &mut self,
        ui: &Ui,
        response: &Response,
        layout: AnalyzerLayout,
    ) -> bool {
        let Some(rename_button) = self
            .input_bindings
            .pointer_button(&["logic_analyzer.channel", "logic_analyzer"], "rename")
        else {
            return false;
        };
        if !response.double_clicked_by(rename_button) {
            return false;
        }
        let Some(pointer) = response.interact_pointer_pos() else {
            return false;
        };
        if !layout.labels_rect.contains(pointer) {
            return false;
        }
        let Some(row) =
            self.row_at_vertical(layout.labels_rect.top(), pointer.y, layout.row_height)
        else {
            return false;
        };
        let Some(key) = self.row_order.get(row).cloned() else {
            return false;
        };
        let Some(label) = self.row_label(&key) else {
            return false;
        };
        let row_top = self.row_top(layout.labels_rect.top(), row, layout.row_height);
        self.row_rename = Some(RowRenameState {
            key,
            text: label.name,
            screen_pos: Pos2::new(layout.labels_rect.left() + 8.0, row_top + 4.0),
        });
        ui.ctx().set_cursor_icon(CursorIcon::Text);
        true
    }

    pub(crate) fn handle_row_reorder(
        &mut self,
        ui: &Ui,
        response: &Response,
        layout: AnalyzerLayout,
    ) -> bool {
        let pointer = response
            .interact_pointer_pos()
            .or_else(|| ui.input(|input| input.pointer.hover_pos()));

        if self.row_drag.is_none()
            && let Some(pointer) = pointer
            && let Some(row) = self.row_at_pointer(layout, pointer)
            && self.row_badge_rect(layout, row).contains(pointer)
        {
            ui.ctx().set_cursor_icon(CursorIcon::Grab);
        }

        let Some(reorder_button) = self
            .input_bindings
            .pointer_button(&["logic_analyzer.channel", "logic_analyzer"], "reorder")
        else {
            return false;
        };
        if response.drag_started_by(reorder_button)
            && let Some(grab_pos) = ui.input(|input| input.pointer.press_origin()).or(pointer)
            && let Some(row) = self.row_at_pointer(layout, grab_pos)
            && self.row_badge_rect(layout, row).contains(grab_pos)
        {
            self.row_drag = self
                .row_order
                .get(row)
                .cloned()
                .map(|key| RowDragState { key });
        }

        let Some(drag_key) = self.row_drag.as_ref().map(|drag| drag.key.clone()) else {
            return false;
        };

        if !response.dragged_by(reorder_button) {
            self.row_drag = None;
            return false;
        }

        ui.ctx().set_cursor_icon(CursorIcon::Grabbing);
        if let Some(pointer) = response.interact_pointer_pos()
            && let Some(target_row) = self.row_at_y(layout, pointer.y)
        {
            self.move_row(&drag_key, target_row);
        }
        true
    }

    pub(crate) fn row_at_pointer(&self, layout: AnalyzerLayout, pointer: Pos2) -> Option<usize> {
        if !layout.labels_rect.contains(pointer) {
            return None;
        }
        self.row_at_y(layout, pointer.y)
    }

    fn row_at_y(&self, layout: AnalyzerLayout, y: f32) -> Option<usize> {
        if y < layout.labels_rect.top() || y > layout.labels_rect.bottom() {
            return None;
        }
        self.row_at_vertical(layout.labels_rect.top(), y, layout.row_height)
    }

    fn row_badge_rect(&self, layout: AnalyzerLayout, row: usize) -> Rect {
        let row_top = self.row_top(layout.labels_rect.top(), row, layout.row_height);
        let height = self
            .row_order
            .get(row)
            .map(|key| self.display_row_height(key, layout.row_height))
            .unwrap_or(layout.row_height);
        Rect::from_min_size(
            Pos2::new(
                layout.labels_rect.left()
                    + 12.0
                    + layout.trigger_width
                    + layout.name_col_width
                    + 10.0,
                row_top + height * 0.5 - 8.0,
            ),
            vec2(layout.badge_width, 16.0),
        )
    }

    pub(crate) fn show_row_rename(&mut self, ctx: &egui::Context) {
        let Some(state) = &mut self.row_rename else {
            return;
        };

        let mut apply = false;
        let mut cancel = false;
        egui::Window::new("Rename")
            .id(egui::Id::new("logic_analyzer_rename_row"))
            .fixed_pos(state.screen_pos)
            .collapsible(false)
            .resizable(false)
            .show(ctx, |ui| {
                let response = ui.add(
                    egui::TextEdit::singleline(&mut state.text)
                        .desired_width(240.0)
                        .hint_text("Name"),
                );
                if response.lost_focus()
                    && self.input_bindings.consume_shortcut_ctx(
                        ui.ctx(),
                        &["logic_analyzer.channel_edit"],
                        "confirm",
                    )
                {
                    apply = true;
                } else {
                    response.request_focus();
                }
                ui.horizontal(|ui| {
                    if ui.button("OK").clicked() {
                        apply = true;
                    }
                    if ui.button("Cancel").clicked() {
                        cancel = true;
                    }
                });
            });

        if self
            .input_bindings
            .consume_shortcut_ctx(ctx, &["logic_analyzer.channel_edit"], "cancel")
        {
            cancel = true;
        }
        if apply {
            if let Some(state) = self.row_rename.take() {
                self.set_row_name(&state.key, state.text);
            }
        } else if cancel {
            self.row_rename = None;
        }
    }

    fn set_row_name(&mut self, key: &RowKey, name: String) {
        match key {
            RowKey::Channel(index) => self.set_channel_name(*index, name),
            RowKey::Derived(group_id) => {
                let name = name.trim().to_string();
                let default_name = self
                    .waveform_presentations
                    .read()
                    .iter()
                    .find(|group| &group.id == group_id)
                    .map(|group| group.label.clone());
                if name.is_empty() || default_name.as_deref() == Some(name.as_str()) {
                    self.derived_names.remove(group_id);
                } else {
                    self.derived_names.insert(group_id.clone(), name);
                }
            }
        }
    }

    fn set_channel_name(&mut self, channel_index: usize, name: String) {
        let name = name.trim().to_string();
        if name.is_empty() || name == channel_index.to_string() {
            self.channel_names.remove(&channel_index);
        } else {
            self.channel_names.insert(channel_index, name);
        }
        let display_name = self.channel_display_name(channel_index);
        if let Some(channel) = self
            .channels
            .iter_mut()
            .find(|channel| channel.index == channel_index)
        {
            channel.name = display_name;
        }
    }

    fn channel_display_name(&self, channel_index: usize) -> String {
        self.channel_names
            .get(&channel_index)
            .cloned()
            .unwrap_or_else(|| channel_index.to_string())
    }

    pub(crate) fn apply_channel_names(&self, channels: &mut [LogicChannel]) {
        for channel in channels {
            channel.name = self.channel_display_name(channel.index);
        }
    }

    /// The channel indices in `row_order`, in display order — what to
    /// request from the index sampler. Derived lanes aren't index-backed,
    /// so they're simply not part of this.
    pub(crate) fn requested_channel_order(&self) -> Vec<usize> {
        self.row_order
            .iter()
            .filter_map(|key| match key {
                RowKey::Channel(index) => Some(*index),
                RowKey::Derived(_) => None,
            })
            .collect()
    }

    /// Current interleaved raw/derived row order using stable identities.
    pub fn viewer_row_order(&self) -> Vec<ViewerRowId> {
        self.row_order.iter().map(ViewerRowId::from).collect()
    }

    /// Applies the host's preferred order to rows that currently exist.
    /// Missing rows are ignored and newly appearing rows retain their current
    /// relative order until a later call includes them.
    pub fn apply_viewer_row_order(&mut self, requested: &[ViewerRowId]) {
        self.ensure_row_order();
        let current = self.row_order.iter().cloned().collect::<HashSet<_>>();
        let mut seen = HashSet::new();
        let mut order = Vec::with_capacity(self.row_order.len());
        for requested in requested {
            let key = RowKey::from(requested);
            if current.contains(&key) && seen.insert(key.clone()) {
                order.push(key);
            }
        }
        for key in &self.row_order {
            if seen.insert(key.clone()) {
                order.push(key.clone());
            }
        }
        if self.row_order != order {
            self.row_order = order;
            self.hover_measurement = None;
            self.sampled_key = None;
        }
    }

    /// Reports and clears the user-drag order change flag. Programmatic
    /// restoration does not set it.
    pub fn take_viewer_row_order_changed(&mut self) -> bool {
        std::mem::take(&mut self.row_order_changed)
    }

    pub(crate) fn apply_channel_order(&self, channels: &mut [LogicChannel]) {
        let order = self.requested_channel_order();
        channels.sort_by_key(|channel| {
            order
                .iter()
                .position(|&index| index == channel.index)
                .unwrap_or(usize::MAX)
        });
    }

    /// Reconciles `row_order` against the current `channels` and `derived`
    /// lanes: drops rows that no longer exist (a re-sampled capture with
    /// fewer channels, a run whose lane set changed), keeps everything
    /// else exactly where the user put it, and appends anything new at the
    /// end (a channel from a bigger capture, a lane just registered by a
    /// running pipeline). The single mechanism behind both "channels keep
    /// their position across a resample" and "derived lanes keep their
    /// position across a run restart".
    pub(crate) fn ensure_row_order(&mut self) {
        self.ensure_default_viewer_groups();
        let mut seen_channels = HashSet::new();
        let mut seen_derived = HashSet::new();
        let mut order = Vec::with_capacity(self.row_order.len());
        for key in &self.row_order {
            let keep = match key {
                RowKey::Channel(index) => {
                    self.channels.iter().any(|channel| channel.index == *index)
                        && seen_channels.insert(*index)
                }
                RowKey::Derived(group_id) => {
                    self.viewer_group_is_active(group_id) && seen_derived.insert(group_id.clone())
                }
            };
            if keep {
                order.push(key.clone());
            }
        }
        for channel in &self.channels {
            if seen_channels.insert(channel.index) {
                order.push(RowKey::Channel(channel.index));
            }
        }
        for group in self.waveform_presentations.read().iter() {
            if self.viewer_group_is_active(&group.id) && seen_derived.insert(group.id.clone()) {
                order.push(RowKey::Derived(group.id.clone()));
            }
        }
        if self.row_order != order {
            self.row_order = order;
            self.sampled_key = None;
        }
    }

    fn viewer_group_is_active(&self, group_id: &ViewerLaneGroupId) -> bool {
        let Some(store) = &self.derived else {
            return false;
        };
        let groups = self.waveform_presentations.read();
        let Some(group) = groups.iter().find(|group| &group.id == group_id) else {
            return false;
        };
        store.opaque_lanes().iter().any(|lane| {
            group
                .tracks
                .iter()
                .any(|track| lane.name() == track.lane.as_str())
        })
    }

    fn ensure_default_viewer_groups(&self) {
        if !self.waveform_presentations.implicit_groups() {
            return;
        }
        let Some(store) = &self.derived else {
            return;
        };
        let claimed: HashSet<DerivedLaneId> = self
            .waveform_presentations
            .read()
            .iter()
            .flat_map(|group| group.tracks.iter().map(|track| track.lane.clone()))
            .collect();
        for lane in store.opaque_lanes() {
            let lane_id = DerivedLaneId::new(lane.name());
            if claimed.contains(&lane_id) {
                continue;
            }
            let Some((badge, renderer)) = self
                .waveform_presentations
                .default_payload(lane.payload().stable_id())
            else {
                continue;
            };
            self.waveform_presentations.register(ViewerLaneGroup {
                id: ViewerLaneGroupId::new(format!("default:{}", lane.name())),
                label: lane.name().to_owned(),
                badge,
                tracks: vec![crate::lanes::ViewerLaneTrack::new("primary", lane_id, 1.0)],
                renderer,
            });
        }
    }

    /// Moves any row — channel or derived lane — to `target_row`, freely
    /// interleaving the two.
    fn move_row(&mut self, key: &RowKey, target_row: usize) {
        let Some(from_row) = self.row_order.iter().position(|existing| existing == key) else {
            return;
        };
        let target_row = target_row.min(self.row_order.len().saturating_sub(1));
        if from_row == target_row {
            return;
        }

        let entry = self.row_order.remove(from_row);
        self.row_order.insert(target_row, entry);
        self.row_order_changed = true;
        self.hover_measurement = None;
        self.sampled_key = None;
    }

    /// Row-addressable channel projection spanning raw channels and derived
    /// rows whose renderer supplies explicit interaction semantics.
    pub(crate) fn channel_at_row(&self, row: usize) -> Option<Cow<'_, LogicChannel>> {
        match self.row_order.get(row)? {
            RowKey::Channel(index) => self
                .channels
                .iter()
                .find(|channel| channel.index == *index)
                .map(Cow::Borrowed),
            RowKey::Derived(group_id) => {
                let (name, interaction) = self.derived_interaction(group_id)?;
                Some(Cow::Owned(logic_channel_from_interaction(
                    row,
                    &name,
                    &interaction,
                )))
            }
        }
    }

    pub(crate) fn is_event_row(&self, row: usize) -> bool {
        let Some(RowKey::Derived(group_id)) = self.row_order.get(row) else {
            return false;
        };
        self.derived_interaction(group_id)
            .is_some_and(|(_, interaction)| interaction.event)
    }

    fn derived_interaction(
        &self,
        group_id: &ViewerLaneGroupId,
    ) -> Option<(String, ViewerLaneInteraction)> {
        let store = self.derived.as_ref()?;
        let group = self
            .waveform_presentations
            .read()
            .iter()
            .find(|group| &group.id == group_id)
            .cloned()?;
        let (start_time_ns, end_time_ns) = self.visible_window_ns();
        let opaque_lanes = store.opaque_lanes();
        group.tracks.iter().find_map(|track| {
            let lane = opaque_lanes
                .iter()
                .find(|lane| lane.name() == track.lane.as_str())?;
            let snapshot = lane.snapshot(CollectedLaneSnapshotRequest {
                start_time_ns,
                end_time_ns,
                max_items: DETAIL_BUDGET,
            });
            group
                .renderer
                .interaction(track, snapshot.as_ref())
                .map(|interaction| (lane.name().to_owned(), interaction))
        })
    }
}

/// Records requested per query from a lane's summary index — a generous
/// detail budget for a single hover/snap lookup (as opposed to a whole
/// row's render), but still a hard bound: without one, a window that itself
/// spans millions of entries at extreme zoom-out would re-materialize all
/// of them, same as the raw-Vec bug this replaced.
const DETAIL_BUDGET: usize = 2_048;

fn logic_channel_from_interaction(
    row: usize,
    name: &str,
    interaction: &ViewerLaneInteraction,
) -> LogicChannel {
    LogicChannel {
        index: row,
        name: name.to_owned(),
        initial: interaction.initial,
        transitions: interaction
            .transitions
            .iter()
            .map(|(timestamp_ns, value)| Transition {
                time_us: *timestamp_ns as f64 / 1_000.0,
                value: *value,
            })
            .collect(),
        waveform: Vec::new(),
    }
}

pub(crate) fn channels_from_window(
    window: &CaptureSampledWindow,
    samplerate_hz: f64,
) -> Vec<LogicChannel> {
    window
        .channels
        .iter()
        .map(|channel| LogicChannel {
            index: channel.channel,
            name: channel.name.clone(),
            initial: channel.initial,
            transitions: channel
                .transitions
                .iter()
                .map(|transition| Transition {
                    time_us: sample_to_us(transition.sample, samplerate_hz),
                    value: transition.value,
                })
                .collect(),
            waveform: channel
                .waveform
                .iter()
                .map(|segment| match *segment {
                    CaptureWaveformSegment::Level {
                        start_sample,
                        end_sample,
                        value,
                    } => WaveformSegment {
                        start_us: sample_to_us(start_sample, samplerate_hz),
                        end_us: sample_to_us(end_sample, samplerate_hz),
                        kind: WaveformSegmentKind::Level { value },
                    },
                    CaptureWaveformSegment::Edge {
                        sample,
                        before,
                        after,
                    } => {
                        let time_us = sample_to_us(sample, samplerate_hz);
                        WaveformSegment {
                            start_us: time_us,
                            end_us: time_us,
                            kind: WaveformSegmentKind::Edge { before, after },
                        }
                    }
                    CaptureWaveformSegment::Activity {
                        start_sample,
                        end_sample,
                        first,
                        last,
                    } => WaveformSegment {
                        start_us: sample_to_us(start_sample, samplerate_hz),
                        end_us: sample_to_us(end_sample, samplerate_hz),
                        kind: WaveformSegmentKind::Activity { first, last },
                    },
                })
                .collect(),
        })
        .collect()
}

// Used by the native capture worker; the web worker implements the same
// platform contract without constructing file-backed placeholder channels.
pub(crate) fn placeholder_channels(header: &CaptureMetadata) -> Vec<LogicChannel> {
    let channel_count = header.total_probes.min(16);
    (0..channel_count)
        .map(|channel| LogicChannel {
            index: channel,
            name: header
                .probe_names
                .get(channel)
                .cloned()
                .unwrap_or_else(|| channel.to_string()),
            initial: false,
            transitions: Vec::new(),
            waveform: Vec::new(),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use egui::Color32;

    use signal_processing::{
        CollectedLaneQuery, CollectedLaneSnapshotRequest, CollectedPayloadRegistry, DerivedLanes,
        OpaqueCollectedLaneSnapshot,
    };

    use super::*;
    use crate::lanes::{
        ViewerLaneBadge, ViewerLaneInteraction, ViewerLaneRenderer, ViewerLaneTrack,
        WaveformPresentationRegistry,
    };
    use crate::viewer::LogicAnalyzerViewer;

    const TEST_PAYLOAD_ID: &str = "org.logic-conduit.test.interaction/v1";

    #[derive(Clone)]
    struct TestPayload;

    struct TestQuery(Option<ViewerLaneInteraction>);

    impl CollectedLaneQuery for TestQuery {
        fn into_any(self: Arc<Self>) -> Arc<dyn std::any::Any + Send + Sync> {
            self
        }

        fn snapshot(
            &self,
            _request: CollectedLaneSnapshotRequest,
        ) -> Option<OpaqueCollectedLaneSnapshot> {
            Some(OpaqueCollectedLaneSnapshot::new(Arc::new(self.0.clone())))
        }
    }

    struct TestRenderer;

    impl ViewerLaneRenderer for TestRenderer {
        fn interaction(
            &self,
            _track: &ViewerLaneTrack,
            snapshot: Option<&OpaqueCollectedLaneSnapshot>,
        ) -> Option<ViewerLaneInteraction> {
            snapshot
                .and_then(|snapshot| snapshot.value::<Option<ViewerLaneInteraction>>())
                .as_deref()
                .cloned()
                .flatten()
        }
    }

    fn publish_test_lane(
        lanes: &DerivedLanes,
        name: &str,
        interaction: Option<ViewerLaneInteraction>,
    ) {
        let mut payloads = CollectedPayloadRegistry::new();
        payloads.register::<TestPayload>(TEST_PAYLOAD_ID).unwrap();
        lanes.publish_opaque_lane(
            name,
            payloads.descriptor::<TestPayload>().unwrap().clone(),
            Arc::new(TestQuery(interaction)),
        );
    }

    fn test_presentations() -> WaveformPresentationRegistry {
        let presentations = WaveformPresentationRegistry::new();
        presentations.register_default_payload(
            TEST_PAYLOAD_ID,
            ViewerLaneBadge::new("T", Color32::WHITE),
            Arc::new(TestRenderer),
        );
        presentations
    }

    /// A viewer with `count` bare channels (no transitions) — enough for the
    /// row-order/labeling tests below, which only care about channel
    /// identity and count, not waveform content.
    fn viewer_with_channels(count: usize) -> LogicAnalyzerViewer {
        let mut viewer = LogicAnalyzerViewer::new();
        viewer.channels = (0..count)
            .map(|index| LogicChannel {
                index,
                name: index.to_string(),
                initial: false,
                transitions: Vec::new(),
                waveform: Vec::new(),
            })
            .collect();
        viewer.ensure_row_order();
        viewer
    }

    fn viewer_with_derived() -> LogicAnalyzerViewer {
        let mut viewer = viewer_with_channels(10);
        let lanes = DerivedLanes::new();
        publish_test_lane(
            &lanes,
            "decoded.rx",
            Some(ViewerLaneInteraction {
                initial: false,
                transitions: vec![(1_000, true), (3_000, false)],
                event: false,
            }),
        );
        publish_test_lane(&lanes, "decoded.words", None);
        viewer.set_waveform_presentations(test_presentations());
        viewer.set_derived_lanes(lanes);
        // `show()` does this every frame in the real app; the tests below
        // don't drive a frame, so they need it explicitly to see the lanes
        // reflected in `row_order`.
        viewer.ensure_row_order();
        viewer
    }

    #[test]
    fn real_channel_row_borrows_in_place() {
        let viewer = viewer_with_channels(10);
        let channel = viewer.channel_at_row(0).expect("row 0 is a real channel");
        assert!(matches!(channel, Cow::Borrowed(_)));
    }

    #[test]
    fn set_derived_lanes_never_clears_existing_channels() {
        let mut viewer = viewer_with_channels(10);
        let before = viewer.channels.len();
        assert!(before > 0);

        // A run only adds derived lanes below whatever channels are already
        // on screen — repeatedly, including across restarts.
        viewer.set_derived_lanes(DerivedLanes::new());
        assert_eq!(viewer.channels.len(), before);
        viewer.set_derived_lanes(DerivedLanes::new());
        assert_eq!(viewer.channels.len(), before);
    }

    #[test]
    fn digital_derived_lane_converts_to_logic_channel() {
        let viewer = viewer_with_derived();
        let row = viewer.channels.len();
        let channel = viewer
            .channel_at_row(row)
            .expect("first derived lane is Digital");
        assert!(matches!(channel, Cow::Owned(_)));
        assert_eq!(channel.name, "decoded.rx");
        assert!(!channel.initial);
        assert_eq!(
            channel.transitions,
            vec![
                Transition {
                    time_us: 1.0,
                    value: true
                },
                Transition {
                    time_us: 3.0,
                    value: false
                },
            ]
        );
        // No summarized bands: hover/snap always use the exact transitions
        // above, same as the "no index" path for a raw channel.
        assert!(channel.waveform.is_empty());
    }

    #[test]
    fn non_digital_derived_lane_has_no_logic_channel_view() {
        let viewer = viewer_with_derived();
        let annotations_row = viewer.channels.len() + 1;
        assert!(viewer.channel_at_row(annotations_row).is_none());
    }

    #[test]
    fn row_past_every_lane_is_none() {
        let viewer = viewer_with_derived();
        let past_everything = viewer.channels.len() + 2;
        assert!(viewer.channel_at_row(past_everything).is_none());
    }

    #[test]
    fn markers_derived_lane_converts_to_toggling_logic_channel() {
        let mut viewer = viewer_with_derived();
        let lanes = DerivedLanes::new();
        publish_test_lane(
            &lanes,
            "match.start",
            Some(ViewerLaneInteraction {
                initial: false,
                transitions: vec![(1_000, true), (3_000, false), (7_000, true)],
                event: true,
            }),
        );
        viewer.set_derived_lanes(lanes);
        viewer.ensure_row_order();

        // Rows: 10 channels, then the one Markers lane.
        let channel = viewer
            .channel_at_row(10)
            .expect("markers lane has a LogicChannel view");
        assert_eq!(channel.name, "match.start");
        assert!(matches!(channel, Cow::Owned(_)));
        assert_eq!(
            channel.transitions,
            vec![
                Transition {
                    time_us: 1.0,
                    value: true
                },
                Transition {
                    time_us: 3.0,
                    value: false
                },
                Transition {
                    time_us: 7.0,
                    value: true
                },
            ]
        );
        // No index-backed sampler for a synthetic marker channel.
        assert!(channel.waveform.is_empty());
    }

    #[test]
    fn is_event_row_true_only_for_markers_lanes() {
        let mut viewer = viewer_with_derived();
        let lanes = DerivedLanes::new();
        publish_test_lane(
            &lanes,
            "match.start",
            Some(ViewerLaneInteraction {
                initial: false,
                transitions: vec![(1_000, true)],
                event: true,
            }),
        );
        publish_test_lane(
            &lanes,
            "decoded.rx",
            Some(ViewerLaneInteraction {
                initial: false,
                transitions: vec![(1_000, true)],
                event: false,
            }),
        );
        viewer.set_derived_lanes(lanes);
        viewer.ensure_row_order();

        // Rows: 10 real channels, then "decoded.rx" (kept at its old row by
        // `ensure_row_order`, since a lane of that name already existed),
        // then the brand-new "match.start" appended after it.
        assert!(!viewer.is_event_row(0), "a real channel is never an event");
        assert!(!viewer.is_event_row(10), "decoded.rx is a Digital lane");
        assert!(viewer.is_event_row(11), "match.start is a Markers lane");
        assert!(!viewer.is_event_row(99), "out of range");
    }

    #[test]
    fn channel_at_row_follows_manual_reordering() {
        let mut viewer = viewer_with_derived();
        let derived_key = RowKey::Derived(ViewerLaneGroupId::new("default:decoded.rx"));

        // Drag the derived lane up to row 0, ahead of every real channel.
        viewer.move_row(&derived_key, 0);

        let channel = viewer
            .channel_at_row(0)
            .expect("row 0 is now the derived lane");
        assert_eq!(channel.name, "decoded.rx");
        assert!(matches!(channel, Cow::Owned(_)));

        // What used to be row 0 (channel 0) has shifted down to row 1.
        let shifted = viewer.channel_at_row(1).expect("row 1 is channel 0");
        assert_eq!(shifted.index, 0);
        assert!(matches!(shifted, Cow::Borrowed(_)));
    }

    #[test]
    fn ensure_row_order_appends_channels_then_derived_lanes_by_default() {
        let viewer = viewer_with_derived();
        let expected: Vec<RowKey> = (0..10)
            .map(RowKey::Channel)
            .chain([
                RowKey::Derived(ViewerLaneGroupId::new("default:decoded.rx")),
                RowKey::Derived(ViewerLaneGroupId::new("default:decoded.words")),
            ])
            .collect();
        assert_eq!(viewer.row_order, expected);
    }

    #[test]
    fn move_row_interleaves_a_derived_lane_between_channels() {
        let mut viewer = viewer_with_derived();
        let derived_key = RowKey::Derived(ViewerLaneGroupId::new("default:decoded.rx"));

        // Drop the derived lane in between channel 2 and channel 3.
        viewer.move_row(&derived_key, 3);

        assert_eq!(viewer.row_order[3], derived_key);
        assert_eq!(viewer.row_order[2], RowKey::Channel(2));
        assert_eq!(viewer.row_order[4], RowKey::Channel(3));
        // Nothing lost or duplicated by the move.
        assert_eq!(viewer.row_order.len(), 12);
    }

    #[test]
    fn viewer_row_order_restores_interleaved_raw_and_derived_rows() {
        let mut viewer = viewer_with_derived();
        let requested = vec![
            ViewerRowId::Derived(ViewerLaneGroupId::new("default:decoded.words")),
            ViewerRowId::Channel(2),
            ViewerRowId::Derived(ViewerLaneGroupId::new("default:decoded.rx")),
            ViewerRowId::Channel(0),
        ];

        viewer.apply_viewer_row_order(&requested);

        assert_eq!(&viewer.viewer_row_order()[..requested.len()], &requested);
        assert!(!viewer.take_viewer_row_order_changed());
    }

    #[test]
    fn saved_derived_row_order_can_be_reapplied_after_lanes_appear() {
        let mut viewer = viewer_with_channels(3);
        let requested = vec![
            ViewerRowId::Derived(ViewerLaneGroupId::new("default:decoded.words")),
            ViewerRowId::Channel(1),
            ViewerRowId::Channel(0),
        ];

        viewer.apply_viewer_row_order(&requested);
        assert_eq!(
            viewer.viewer_row_order(),
            vec![
                ViewerRowId::Channel(1),
                ViewerRowId::Channel(0),
                ViewerRowId::Channel(2),
            ]
        );

        let lanes = DerivedLanes::new();
        publish_test_lane(&lanes, "decoded.words", None);
        viewer.set_waveform_presentations(test_presentations());
        viewer.set_derived_lanes(lanes);
        viewer.apply_viewer_row_order(&requested);

        assert_eq!(&viewer.viewer_row_order()[..3], &requested);
    }

    #[test]
    fn user_row_move_reports_one_persistence_change() {
        let mut viewer = viewer_with_derived();
        let derived_key = RowKey::Derived(ViewerLaneGroupId::new("default:decoded.rx"));

        viewer.move_row(&derived_key, 0);

        assert!(viewer.take_viewer_row_order_changed());
        assert!(!viewer.take_viewer_row_order_changed());
    }

    #[test]
    fn ensure_row_order_drops_stale_rows_and_keeps_manual_order() {
        let mut viewer = viewer_with_derived();
        // User interleaves the two lanes with the first three channels.
        viewer.move_row(
            &RowKey::Derived(ViewerLaneGroupId::new("default:decoded.rx")),
            1,
        );
        viewer.move_row(
            &RowKey::Derived(ViewerLaneGroupId::new("default:decoded.words")),
            3,
        );

        // Capture reopened with fewer channels; the run also restarted with
        // only one lane still registered.
        viewer.channels.retain(|channel| channel.index < 2);
        let remaining_lane = DerivedLanes::new();
        publish_test_lane(&remaining_lane, "decoded.words", None);
        viewer.derived = Some(remaining_lane);

        viewer.ensure_row_order();

        // Survivors keep their relative order from before the drop: C0 and
        // C1 were never adjacent to `decoded.words` in the interleaved
        // order (C0, rx, C1, words, C2..C9), so removing the stale rows
        // leaves C0, C1, words — not the pre-drop adjacency.
        assert_eq!(
            viewer.row_order,
            vec![
                RowKey::Channel(0),
                RowKey::Channel(1),
                RowKey::Derived(ViewerLaneGroupId::new("default:decoded.words")),
            ]
        );
    }

    #[test]
    fn set_row_name_renames_channel_or_derived_lane() {
        let mut viewer = viewer_with_derived();

        viewer.set_row_name(&RowKey::Channel(0), "clk".to_string());
        assert_eq!(viewer.row_label(&RowKey::Channel(0)).unwrap().name, "clk");

        let derived_key = RowKey::Derived(ViewerLaneGroupId::new("default:decoded.rx"));
        viewer.set_row_name(&derived_key, "decoded input".to_string());
        assert_eq!(
            viewer.row_label(&derived_key).unwrap().name,
            "decoded input"
        );

        // Clearing the override falls back to the original names.
        viewer.set_row_name(&RowKey::Channel(0), String::new());
        assert_eq!(viewer.row_label(&RowKey::Channel(0)).unwrap().name, "0");
        viewer.set_row_name(&derived_key, String::new());
        assert_eq!(viewer.row_label(&derived_key).unwrap().name, "decoded.rx");
    }
}
