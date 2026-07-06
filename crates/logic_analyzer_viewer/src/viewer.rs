use crate::channel::LogicChannel;
use crate::types::{
    AnalyzerLayout, CaptureInfo, ChannelDragState, ChannelRenameState, ColorProfile,
    IndexBuildProgress, PulseMeasurement, TimeCursor,
};
use dsl::DerivedLanes;
#[cfg(not(target_arch = "wasm32"))]
use dsl::{CaptureDataSource, DslCaptureReader, DslFileCaptureDataSource, IndexSampler};
use egui::{FontId, Pos2, Rect, Sense, Ui, vec2};
use std::collections::HashMap;
#[cfg(not(target_arch = "wasm32"))]
use std::path::{Path, PathBuf};
#[cfg(not(target_arch = "wasm32"))]
use std::sync::mpsc::{self, Receiver};

pub struct LogicAnalyzerViewer {
    pub(crate) channels: Vec<LogicChannel>,
    pub(crate) channel_order: Vec<usize>,
    pub(crate) channel_drag: Option<ChannelDragState>,
    pub(crate) channel_names: HashMap<usize, String>,
    pub(crate) channel_rename: Option<ChannelRenameState>,
    /// Synchronous sampler over the waveform index; present once the index
    /// build (which runs on a worker thread) has completed. Sampling the
    /// visible window happens on the UI thread every frame the view changes,
    /// so what is drawn is always the current view at the current zoom —
    /// there is no asynchronous refinement that could disagree with it.
    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) sampler: Option<IndexSampler<DslCaptureReader>>,
    /// (start_sample, end_sample, target_points) of the sampled `channels`.
    pub(crate) sampled_key: Option<(u64, u64, usize)>,
    /// Pulse measurement for the current hover position, refreshed each frame
    /// by `sample_hover_measurement`. Computed separately from `channels`
    /// because at low zoom the hovered channel may only have summarized
    /// `waveform` bands, which don't carry individual edge times — measuring
    /// then requires an extra exact query into the index around the pointer.
    pub(crate) hover_measurement: Option<PulseMeasurement>,
    pub(crate) visible_start_us: f64,
    pub(crate) visible_span_us: f64,
    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) capture_path: Option<PathBuf>,
    pub(crate) capture_info: Option<CaptureInfo>,
    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) worker_responses: Option<Receiver<crate::worker::WorkerResponse>>,
    pub(crate) status: String,
    pub(crate) index_progress: Option<IndexBuildProgress>,
    pub(crate) fit_to_capture: bool,
    /// DSView-style time cursors, in creation order. Unbounded.
    pub(crate) cursors: Vec<TimeCursor>,
    /// Index into `cursors` of the cursor currently being dragged.
    pub(crate) drag_cursor: Option<usize>,
    pub(crate) color_profile: ColorProfile,
    /// Lanes produced by Viewer nodes of the running pipeline, rendered as
    /// extra rows under the raw channels. Swapped wholesale on each run.
    pub(crate) derived: Option<DerivedLanes>,
}

impl LogicAnalyzerViewer {
    pub fn demo() -> Self {
        let mut channels = vec![LogicChannel::uart_demo(0, "serial.rx", b"HELLO\n")];
        for index in 1..10 {
            let period = match index {
                1 => 90.0,
                2 => 135.0,
                3 => 260.0,
                6 => 42.0,
                7 => 28.0,
                _ => 220.0 + index as f64 * 35.0,
            };
            let offset = index as f64 * 11.0;
            channels.push(LogicChannel::square_wave(
                index,
                index.to_string(),
                period,
                offset,
                index % 3 == 0,
            ));
        }
        Self {
            channels,
            channel_order: (0..10).collect(),
            channel_drag: None,
            channel_names: HashMap::new(),
            channel_rename: None,
            #[cfg(not(target_arch = "wasm32"))]
            sampler: None,
            sampled_key: None,
            hover_measurement: None,
            visible_start_us: 0.0,
            visible_span_us: 900.0,
            #[cfg(not(target_arch = "wasm32"))]
            capture_path: None,
            capture_info: None,
            #[cfg(not(target_arch = "wasm32"))]
            worker_responses: None,
            status: "Demo data".to_string(),
            index_progress: None,
            fit_to_capture: false,
            cursors: Vec::new(),
            drag_cursor: None,
            color_profile: ColorProfile::DsView,
            derived: None,
        }
    }

    /// Replaces the derived-lane store: the viewer renders whatever the
    /// running pipeline pushes into it, live. A fresh (empty) store clears
    /// the previous run's lanes.
    pub fn set_derived_lanes(&mut self, lanes: DerivedLanes) {
        self.derived = Some(lanes);
    }

    #[cfg(not(target_arch = "wasm32"))]
    pub fn set_capture_path(&mut self, path: impl AsRef<Path>) {
        let path = path.as_ref();
        if path.as_os_str().is_empty() {
            return;
        }

        if self.capture_path.as_deref() == Some(path) {
            return;
        }

        let path = path.to_path_buf();
        let data_source = match DslFileCaptureDataSource::open(&path) {
            Ok(data_source) => data_source,
            Err(err) => {
                self.capture_path = Some(path.clone());
                self.capture_info = None;
                self.channels.clear();
                self.channel_order.clear();
                self.channel_drag = None;
                self.channel_names.clear();
                self.channel_rename = None;
                self.sampler = None;
                self.sampled_key = None;
                self.index_progress = None;
                self.worker_responses = None;
                self.cursors.clear();
                self.drag_cursor = None;
                self.hover_measurement = None;
                self.status = format!("Could not inspect capture: {err}");
                return;
            }
        };

        self.capture_path = Some(path.clone());
        self.capture_info = None;
        self.channels.clear();
        self.channel_order.clear();
        self.channel_drag = None;
        self.channel_names.clear();
        self.channel_rename = None;
        self.sampler = None;
        self.sampled_key = None;
        self.index_progress = None;
        self.fit_to_capture = true;
        self.cursors.clear();
        self.drag_cursor = None;
        self.hover_measurement = None;
        self.status = format!("Opening {}", data_source.display_name());

        let (response_tx, response_rx) = mpsc::channel();
        crate::worker::spawn_capture_worker(path, data_source, response_tx);
        self.worker_responses = Some(response_rx);
    }

    pub fn show(&mut self, ui: &mut Ui) {
        let rect = ui.available_rect_before_wrap();
        let response = ui.allocate_rect(rect, Sense::click_and_drag());
        let painter = ui.painter_at(rect);

        #[cfg(not(target_arch = "wasm32"))]
        self.process_worker_responses();
        let mut layout = self.layout(ui, rect);
        let channel_rename_started = self.handle_channel_label_input(ui, &response, layout);
        let channel_dragging = self.handle_channel_reorder(ui, &response, layout);
        let cursor_input = self.handle_cursor_input(ui, &response, layout);
        if (response.double_clicked()
            && !cursor_input.ruler_double_click
            && !channel_rename_started)
            || (response.hovered() && ui.input(|input| input.key_pressed(egui::Key::F)))
        {
            self.fit_capture();
        }
        self.handle_input(
            ui,
            layout,
            response.hovered(),
            response.dragged_by(egui::PointerButton::Primary)
                && !cursor_input.blocks_pan
                && !channel_dragging,
        );
        self.sample_visible_window(layout);
        layout = self.layout(ui, rect);
        let hover_pointer = if cursor_input.blocks_pan {
            None
        } else {
            response.hover_pos()
        };
        self.sample_hover_measurement(layout, hover_pointer);
        self.draw(&painter, layout, hover_pointer, cursor_input.active);
        self.show_profile_selector(ui, rect);
        self.show_channel_rename(ui.ctx());
        #[cfg(not(target_arch = "wasm32"))]
        {
            if self.capture_path.is_some() && self.capture_info.is_none() {
                ui.ctx()
                    .request_repaint_after(std::time::Duration::from_millis(16));
            } else if self.index_progress.is_some()
                || (self.capture_info.is_some() && self.sampler.is_none())
            {
                ui.ctx()
                    .request_repaint_after(std::time::Duration::from_millis(100));
            }
        }
    }

    /// Color-profile combo box, overlaid on the right end of the header bar.
    fn show_profile_selector(&mut self, ui: &mut Ui, rect: Rect) {
        let combo_rect = Rect::from_min_size(
            Pos2::new(rect.right() - 112.0, rect.top() + 3.0),
            vec2(104.0, 20.0),
        );
        if combo_rect.left() <= rect.left() {
            return;
        }
        let mut combo_ui = ui.new_child(egui::UiBuilder::new().max_rect(combo_rect));
        egui::ComboBox::from_id_salt("logic_analyzer_color_profile")
            .selected_text(self.color_profile.name())
            .width(100.0)
            .show_ui(&mut combo_ui, |ui| {
                for profile in ColorProfile::ALL {
                    ui.selectable_value(&mut self.color_profile, profile, profile.name());
                }
            });
    }

    pub(crate) fn layout(&self, ui: &Ui, rect: Rect) -> AnalyzerLayout {
        let title_height = 26.0;
        let ruler_height = 34.0;
        let row_height = 30.0;
        let label_pad = 12.0;
        let name_badge_gap = 10.0;
        let label_right_pad = 10.0;
        let name_font = FontId::proportional(12.0);
        let badge_font = FontId::monospace(10.0);
        let derived_names: Vec<String> = self
            .derived
            .as_ref()
            .map(|store| store.read().iter().map(|lane| lane.name.clone()).collect())
            .unwrap_or_default();
        let (name_col_width, badge_width) = ui.ctx().fonts_mut(|fonts| {
            let name_col_width = self
                .channels
                .iter()
                .map(|channel| channel.name.clone())
                .chain(derived_names.iter().cloned())
                .map(|name| {
                    fonts
                        .layout_no_wrap(name, name_font.clone(), egui::Color32::WHITE)
                        .size()
                        .x
                })
                .fold(0.0, f32::max);
            let badge_width = self
                .channels
                .iter()
                .map(|channel| {
                    fonts
                        .layout_no_wrap(
                            channel.index.to_string(),
                            badge_font.clone(),
                            egui::Color32::WHITE,
                        )
                        .size()
                        .x
                        + 14.0
                })
                .fold(26.0, f32::max);
            (name_col_width, badge_width)
        });
        let desired_left_width =
            label_pad + name_col_width + name_badge_gap + badge_width + label_right_pad;
        let left_width = desired_left_width.max(72.0).min(rect.width().max(0.0));

        let header_rect = Rect::from_min_size(rect.min, vec2(rect.width(), title_height));
        let ruler_rect = Rect::from_min_max(
            Pos2::new(rect.left() + left_width, rect.top() + title_height),
            Pos2::new(rect.right(), rect.top() + title_height + ruler_height),
        );
        let labels_rect = Rect::from_min_max(
            Pos2::new(rect.left(), rect.top() + title_height + ruler_height),
            Pos2::new(rect.left() + left_width, rect.bottom()),
        );
        let wave_rect = Rect::from_min_max(
            Pos2::new(
                rect.left() + left_width,
                rect.top() + title_height + ruler_height,
            ),
            rect.max,
        );

        AnalyzerLayout {
            header_rect,
            ruler_rect,
            labels_rect,
            wave_rect,
            row_height,
            name_col_width,
            badge_width,
        }
    }

    pub(crate) fn fit_capture(&mut self) {
        if let Some(capture) = self.capture_info.as_ref() {
            self.visible_start_us = 0.0;
            self.visible_span_us = capture.duration_us.max(1.0);
            self.fit_to_capture = true;
        }
    }

    pub(crate) fn clamp_to_capture_duration(&mut self) {
        if let Some(capture) = self.capture_info.as_ref() {
            let duration_us = capture.duration_us;
            self.visible_span_us = self.visible_span_us.min(duration_us.max(1.0));
            self.visible_start_us = self
                .visible_start_us
                .clamp(0.0, (duration_us - self.visible_span_us).max(0.0));
        }
    }
}
