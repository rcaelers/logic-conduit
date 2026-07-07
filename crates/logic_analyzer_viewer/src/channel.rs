use crate::sampling::sample_to_us;
use crate::types::{
    AnalyzerLayout, RowDragState, RowKey, RowLabel, RowRenameState, Transition, WaveformSegment,
    WaveformSegmentKind,
};
use crate::viewer::LogicAnalyzerViewer;
use dsl::{CaptureMetadata, CaptureSampledWindow, CaptureWaveformSegment, DerivedLaneData, Sample};
use egui::{Color32, CursorIcon, PointerButton, Pos2, Rect, Response, Ui, vec2};
use std::borrow::Cow;
use std::collections::HashSet;

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
    /// What to show for one row's label, whatever it is — the only place
    /// that knows a channel's badge is its index (colored by
    /// `color_profile`) and a derived lane's badge is a kind glyph (colored
    /// by payload family, matching the socket colors in the node editor).
    /// Respects a user rename either way.
    pub(crate) fn row_label(&self, key: &RowKey) -> Option<RowLabel> {
        match key {
            RowKey::Channel(index) => {
                let channel = self.channels.iter().find(|channel| channel.index == *index)?;
                Some(RowLabel {
                    name: channel.name.clone(),
                    badge_text: index.to_string(),
                    badge_color: self.color_profile.channel_color(*index),
                })
            }
            RowKey::Derived(lane_name) => {
                let lanes = self.derived.as_ref()?.read();
                let lane = lanes.iter().find(|lane| &lane.name == lane_name)?;
                let (badge_color, badge_glyph) = match &lane.data {
                    DerivedLaneData::Digital(_) => (Color32::from_rgb(95, 175, 95), "S"),
                    DerivedLaneData::Annotations(_) => (Color32::from_rgb(215, 140, 60), "W"),
                    DerivedLaneData::Markers(_) => (Color32::from_rgb(230, 190, 80), "T"),
                };
                let name = self.derived_names.get(lane_name).cloned().unwrap_or_else(|| {
                    if lane.dropped > 0 {
                        format!("{} ⚠", lane.name)
                    } else {
                        lane.name.clone()
                    }
                });
                Some(RowLabel {
                    name,
                    badge_text: badge_glyph.to_string(),
                    badge_color,
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
        if !response.double_clicked() {
            return false;
        }
        let Some(pointer) = response.interact_pointer_pos() else {
            return false;
        };
        if !layout.labels_rect.contains(pointer) {
            return false;
        }
        let row = ((pointer.y - layout.labels_rect.top()) / layout.row_height).floor() as usize;
        let Some(key) = self.row_order.get(row).cloned() else {
            return false;
        };
        let Some(label) = self.row_label(&key) else {
            return false;
        };
        let row_top = layout.labels_rect.top() + row as f32 * layout.row_height;
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

        if response.drag_started_by(PointerButton::Primary)
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

        if !response.dragged_by(PointerButton::Primary) {
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

    pub(crate) fn row_at_y(&self, layout: AnalyzerLayout, y: f32) -> Option<usize> {
        if y < layout.labels_rect.top() || y > layout.labels_rect.bottom() {
            return None;
        }
        let row = ((y - layout.labels_rect.top()) / layout.row_height).floor() as usize;
        (row < self.row_order.len()).then_some(row)
    }

    pub(crate) fn row_badge_rect(&self, layout: AnalyzerLayout, row: usize) -> Rect {
        let row_top = layout.labels_rect.top() + row as f32 * layout.row_height;
        Rect::from_min_size(
            Pos2::new(
                layout.labels_rect.left() + 12.0 + layout.name_col_width + 10.0,
                row_top + layout.row_height * 0.5 - 8.0,
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
                if response.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
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

        if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
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

    pub(crate) fn set_row_name(&mut self, key: &RowKey, name: String) {
        match key {
            RowKey::Channel(index) => self.set_channel_name(*index, name),
            RowKey::Derived(lane_name) => {
                let name = name.trim().to_string();
                if name.is_empty() || &name == lane_name {
                    self.derived_names.remove(lane_name);
                } else {
                    self.derived_names.insert(lane_name.clone(), name);
                }
            }
        }
    }

    pub(crate) fn set_channel_name(&mut self, channel_index: usize, name: String) {
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

    pub(crate) fn channel_display_name(&self, channel_index: usize) -> String {
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
        let mut seen_channels = HashSet::new();
        let mut seen_derived = HashSet::new();
        let mut order = Vec::with_capacity(self.row_order.len());
        for key in &self.row_order {
            let keep = match key {
                RowKey::Channel(index) => {
                    self.channels.iter().any(|channel| channel.index == *index)
                        && seen_channels.insert(*index)
                }
                RowKey::Derived(name) => {
                    self.derived
                        .as_ref()
                        .is_some_and(|store| store.read().iter().any(|lane| &lane.name == name))
                        && seen_derived.insert(name.clone())
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
        if let Some(store) = &self.derived {
            for lane in store.read().iter() {
                if seen_derived.insert(lane.name.clone()) {
                    order.push(RowKey::Derived(lane.name.clone()));
                }
            }
        }
        if self.row_order != order {
            self.row_order = order;
            self.sampled_key = None;
        }
    }

    /// Moves any row — channel or derived lane — to `target_row`, freely
    /// interleaving the two.
    pub(crate) fn move_row(&mut self, key: &RowKey, target_row: usize) {
        let Some(from_row) = self.row_order.iter().position(|existing| existing == key) else {
            return;
        };
        let target_row = target_row.min(self.row_order.len().saturating_sub(1));
        if from_row == target_row {
            return;
        }

        let entry = self.row_order.remove(from_row);
        self.row_order.insert(target_row, entry);
        self.hover_measurement = None;
        self.sampled_key = None;
    }

    /// Row-addressable channel view spanning both raw channels and any
    /// derived `Digital` lane (§4.9), resolved through `row_order` so the
    /// visual position (drag-reordered, interleaved) always matches what's
    /// actually on screen — the same `LogicChannel` shape either way, so
    /// hover measurement and cursor snap don't need a separate code path for
    /// derived lanes; they just don't know the difference.
    /// `Annotations`/`Markers` lanes have no `LogicChannel` equivalent (no
    /// boolean level to measure or toggle to snap to), so this returns
    /// `None` for them, same as an out-of-range row.
    pub(crate) fn channel_at_row(&self, row: usize) -> Option<Cow<'_, LogicChannel>> {
        match self.row_order.get(row)? {
            RowKey::Channel(index) => self
                .channels
                .iter()
                .find(|channel| channel.index == *index)
                .map(Cow::Borrowed),
            RowKey::Derived(name) => {
                let lanes = self.derived.as_ref()?.read();
                let lane = lanes.iter().find(|lane| &lane.name == name)?;
                let DerivedLaneData::Digital(samples) = &lane.data else {
                    return None;
                };
                Some(Cow::Owned(derived_digital_channel(row, &lane.name, samples)))
            }
        }
    }
}

/// Reinterprets a derived `Digital` lane's samples as a `LogicChannel`.
/// Before the first recorded sample there is nothing to show — same default
/// `draw_derived_digital` uses — so `initial` is always `false`.
fn derived_digital_channel(row: usize, name: &str, samples: &[Sample]) -> LogicChannel {
    LogicChannel {
        index: row,
        name: name.to_owned(),
        initial: false,
        transitions: samples
            .iter()
            .map(|sample| Transition {
                time_us: sample.start_time as f64 / 1_000.0,
                value: sample.value,
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
    use super::*;
    use crate::viewer::LogicAnalyzerViewer;
    use dsl::{DerivedLanes, Sample};

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
        lanes.register(
            "decoded.rx",
            DerivedLaneData::Digital(vec![Sample::new(true, 1_000), Sample::new(false, 3_000)]),
        );
        lanes.register(
            "decoded.words",
            DerivedLaneData::Annotations(Vec::new()),
        );
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
    fn channel_at_row_follows_manual_reordering() {
        let mut viewer = viewer_with_derived();
        let derived_key = RowKey::Derived("decoded.rx".to_string());

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
                RowKey::Derived("decoded.rx".to_string()),
                RowKey::Derived("decoded.words".to_string()),
            ])
            .collect();
        assert_eq!(viewer.row_order, expected);
    }

    #[test]
    fn move_row_interleaves_a_derived_lane_between_channels() {
        let mut viewer = viewer_with_derived();
        let derived_key = RowKey::Derived("decoded.rx".to_string());

        // Drop the derived lane in between channel 2 and channel 3.
        viewer.move_row(&derived_key, 3);

        assert_eq!(viewer.row_order[3], derived_key);
        assert_eq!(viewer.row_order[2], RowKey::Channel(2));
        assert_eq!(viewer.row_order[4], RowKey::Channel(3));
        // Nothing lost or duplicated by the move.
        assert_eq!(viewer.row_order.len(), 12);
    }

    #[test]
    fn ensure_row_order_drops_stale_rows_and_keeps_manual_order() {
        let mut viewer = viewer_with_derived();
        // User interleaves the two lanes with the first three channels.
        viewer.move_row(&RowKey::Derived("decoded.rx".to_string()), 1);
        viewer.move_row(&RowKey::Derived("decoded.words".to_string()), 3);

        // Capture reopened with fewer channels; the run also restarted with
        // only one lane still registered.
        viewer.channels.retain(|channel| channel.index < 2);
        let remaining_lane = DerivedLanes::new();
        remaining_lane.register("decoded.words", DerivedLaneData::Annotations(Vec::new()));
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
                RowKey::Derived("decoded.words".to_string()),
            ]
        );
    }

    #[test]
    fn set_row_name_renames_channel_or_derived_lane() {
        let mut viewer = viewer_with_derived();

        viewer.set_row_name(&RowKey::Channel(0), "clk".to_string());
        assert_eq!(viewer.row_label(&RowKey::Channel(0)).unwrap().name, "clk");

        let derived_key = RowKey::Derived("decoded.rx".to_string());
        viewer.set_row_name(&derived_key, "uart.rx".to_string());
        assert_eq!(viewer.row_label(&derived_key).unwrap().name, "uart.rx");

        // Clearing the override falls back to the original names.
        viewer.set_row_name(&RowKey::Channel(0), String::new());
        assert_eq!(viewer.row_label(&RowKey::Channel(0)).unwrap().name, "0");
        viewer.set_row_name(&derived_key, String::new());
        assert_eq!(
            viewer.row_label(&derived_key).unwrap().name,
            "decoded.rx"
        );
    }
}
