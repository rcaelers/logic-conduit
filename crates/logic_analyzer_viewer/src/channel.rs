#[cfg(not(target_arch = "wasm32"))]
use crate::sampling::sample_to_us;
use crate::types::{AnalyzerLayout, ChannelDragState, ChannelRenameState, Transition, WaveformSegment};
#[cfg(not(target_arch = "wasm32"))]
use crate::types::WaveformSegmentKind;
use crate::viewer::LogicAnalyzerViewer;
use dsl::CaptureMetadata;
#[cfg(not(target_arch = "wasm32"))]
use dsl::{CaptureSampledWindow, CaptureWaveformSegment};
use egui::{CursorIcon, PointerButton, Pos2, Rect, Response, Ui, vec2};

#[derive(Debug, Clone)]
pub(crate) struct LogicChannel {
    pub(crate) index: usize,
    pub(crate) name: String,
    pub(crate) initial: bool,
    pub(crate) transitions: Vec<Transition>,
    pub(crate) waveform: Vec<WaveformSegment>,
}

impl LogicChannel {
    pub(crate) fn uart_demo(index: usize, name: &str, bytes: &[u8]) -> Self {
        const BAUD: f64 = 115_200.0;
        const FIRST_START_NS: u64 = 60_000;
        let bit_ns = (1_000_000_000.0 / BAUD).round() as u64;
        let mut transitions = Vec::new();
        let mut raw_level = true;
        let mut time_ns = FIRST_START_NS;

        for &byte in bytes {
            let frame_start = time_ns;
            let mut bits = Vec::with_capacity(10);
            bits.push(false);
            for bit in 0..8 {
                bits.push(((byte >> bit) & 1) == 1);
            }
            bits.push(true);

            for (bit_index, bit_value) in bits.into_iter().enumerate() {
                let bit_time_ns = frame_start + bit_index as u64 * bit_ns;
                if raw_level != bit_value {
                    raw_level = bit_value;
                    transitions.push(Transition {
                        time_us: bit_time_ns as f64 / 1_000.0,
                        value: raw_level,
                    });
                }
            }
            time_ns = frame_start + 10 * bit_ns;
        }

        Self {
            index,
            name: name.to_owned(),
            initial: true,
            transitions,
            waveform: Vec::new(),
        }
    }

    pub(crate) fn square_wave(
        index: usize,
        name: String,
        period_us: f64,
        offset_us: f64,
        initial: bool,
    ) -> Self {
        let mut transitions = Vec::new();
        let mut value = initial;
        let mut time = offset_us.max(0.0);
        while time <= 60_000.0 {
            value = !value;
            transitions.push(Transition {
                time_us: time,
                value,
            });
            time += period_us * 0.5;
        }

        Self {
            index,
            name,
            initial,
            transitions,
            waveform: Vec::new(),
        }
    }

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
    pub(crate) fn handle_channel_label_input(
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
        let Some(channel) = self.channels.get(row) else {
            return false;
        };
        let row_top = layout.labels_rect.top() + row as f32 * layout.row_height;
        self.channel_rename = Some(ChannelRenameState {
            channel_index: channel.index,
            text: channel.name.clone(),
            screen_pos: Pos2::new(layout.labels_rect.left() + 8.0, row_top + 4.0),
        });
        ui.ctx().set_cursor_icon(CursorIcon::Text);
        true
    }

    pub(crate) fn handle_channel_reorder(
        &mut self,
        ui: &Ui,
        response: &Response,
        layout: AnalyzerLayout,
    ) -> bool {
        let pointer = response
            .interact_pointer_pos()
            .or_else(|| ui.input(|input| input.pointer.hover_pos()));

        if self.channel_drag.is_none()
            && let Some(pointer) = pointer
            && let Some(row) = self.channel_row_at_pointer(layout, pointer)
            && self.channel_badge_rect(layout, row).contains(pointer)
        {
            ui.ctx().set_cursor_icon(CursorIcon::Grab);
        }

        if response.drag_started_by(PointerButton::Primary)
            && let Some(grab_pos) = ui.input(|input| input.pointer.press_origin()).or(pointer)
            && let Some(row) = self.channel_row_at_pointer(layout, grab_pos)
            && self.channel_badge_rect(layout, row).contains(grab_pos)
        {
            self.channel_drag = self.channels.get(row).map(|channel| ChannelDragState {
                channel_index: channel.index,
            });
        }

        let Some(drag_channel_index) = self.channel_drag.as_ref().map(|drag| drag.channel_index)
        else {
            return false;
        };

        if !response.dragged_by(PointerButton::Primary) {
            self.channel_drag = None;
            return false;
        }

        ui.ctx().set_cursor_icon(CursorIcon::Grabbing);
        if let Some(pointer) = response.interact_pointer_pos()
            && let Some(target_row) = self.channel_row_at_y(layout, pointer.y)
        {
            self.move_channel_to_row(drag_channel_index, target_row);
        }
        true
    }

    pub(crate) fn channel_row_at_pointer(
        &self,
        layout: AnalyzerLayout,
        pointer: Pos2,
    ) -> Option<usize> {
        if !layout.labels_rect.contains(pointer) {
            return None;
        }
        self.channel_row_at_y(layout, pointer.y)
    }

    pub(crate) fn channel_row_at_y(&self, layout: AnalyzerLayout, y: f32) -> Option<usize> {
        if y < layout.labels_rect.top() || y > layout.labels_rect.bottom() {
            return None;
        }
        let row = ((y - layout.labels_rect.top()) / layout.row_height).floor() as usize;
        self.channels.get(row).map(|_| row)
    }

    pub(crate) fn channel_badge_rect(&self, layout: AnalyzerLayout, row: usize) -> Rect {
        let row_top = layout.labels_rect.top() + row as f32 * layout.row_height;
        Rect::from_min_size(
            Pos2::new(
                layout.labels_rect.left() + 12.0 + layout.name_col_width + 10.0,
                row_top + layout.row_height * 0.5 - 8.0,
            ),
            vec2(layout.badge_width, 16.0),
        )
    }

    pub(crate) fn show_channel_rename(&mut self, ctx: &egui::Context) {
        let Some(state) = &mut self.channel_rename else {
            return;
        };

        let mut apply = false;
        let mut cancel = false;
        egui::Window::new("Rename Channel")
            .id(egui::Id::new("logic_analyzer_rename_channel"))
            .fixed_pos(state.screen_pos)
            .collapsible(false)
            .resizable(false)
            .show(ctx, |ui| {
                let response = ui.add(
                    egui::TextEdit::singleline(&mut state.text)
                        .desired_width(240.0)
                        .hint_text("Channel name"),
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
            if let Some(state) = self.channel_rename.take() {
                self.set_channel_name(state.channel_index, state.text);
            }
        } else if cancel {
            self.channel_rename = None;
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

    pub(crate) fn ensure_channel_order(&mut self, channel_count: usize) {
        let mut seen = vec![false; channel_count];
        let mut order = Vec::with_capacity(channel_count);
        for channel_index in self.channel_order.iter().copied() {
            if channel_index < channel_count && !seen[channel_index] {
                seen[channel_index] = true;
                order.push(channel_index);
            }
        }
        for (channel_index, seen) in seen.iter().enumerate() {
            if !*seen {
                order.push(channel_index);
            }
        }
        if self.channel_order != order {
            self.channel_order = order;
            self.sampled_key = None;
        }
    }

    pub(crate) fn apply_channel_order(&self, channels: &mut [LogicChannel]) {
        channels.sort_by_key(|channel| {
            self.channel_order
                .iter()
                .position(|&channel_index| channel_index == channel.index)
                .unwrap_or(usize::MAX)
        });
    }

    pub(crate) fn move_channel_to_row(&mut self, channel_index: usize, target_row: usize) {
        let Some(from_row) = self
            .channels
            .iter()
            .position(|channel| channel.index == channel_index)
        else {
            return;
        };
        let target_row = target_row.min(self.channels.len().saturating_sub(1));
        if from_row == target_row {
            return;
        }

        let channel = self.channels.remove(from_row);
        self.channels.insert(target_row, channel);
        self.channel_order = self.channels.iter().map(|channel| channel.index).collect();
        self.hover_measurement = None;
        self.sampled_key = None;
    }
}

#[cfg(not(target_arch = "wasm32"))]
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
