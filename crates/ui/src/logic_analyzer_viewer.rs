use dsl::{
    CaptureDataSource, CaptureIndexProgress, CaptureMetadata, CaptureSampledWindow,
    CaptureWaveformSegment, DslCaptureReader, DslFileCaptureDataSource, IndexSampler,
};
use egui::{
    Align2, Color32, CursorIcon, FontId, Painter, PointerButton, Pos2, Rect, Response, Sense,
    Shape, Stroke, StrokeKind, Ui, vec2,
};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, Sender};

const SCROLL_INPUT_EPSILON: f32 = 0.5;

/// Color profile for the viewer. DSView (Tango-based channel colors, bright
/// traces) is the default; Classic is the viewer's original muted look.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColorProfile {
    DsView,
    Classic,
}

impl ColorProfile {
    const ALL: [Self; 2] = [Self::DsView, Self::Classic];

    fn name(self) -> &'static str {
        match self {
            Self::DsView => "DSView",
            Self::Classic => "Classic",
        }
    }

    fn channel_color(self, index: usize) -> Color32 {
        const DSVIEW: [Color32; 8] = [
            Color32::from_rgb(80, 80, 80),   // grey
            Color32::from_rgb(143, 82, 2),   // brown
            Color32::from_rgb(204, 0, 0),    // red
            Color32::from_rgb(245, 121, 0),  // orange
            Color32::from_rgb(237, 212, 0),  // yellow
            Color32::from_rgb(115, 210, 22), // green
            Color32::from_rgb(52, 101, 164), // blue
            Color32::from_rgb(117, 80, 123), // violet
        ];
        const CLASSIC: [Color32; 8] = [
            Color32::from_rgb(210, 65, 65),
            Color32::from_rgb(210, 125, 45),
            Color32::from_rgb(215, 195, 45),
            Color32::from_rgb(80, 160, 85),
            Color32::from_rgb(70, 155, 190),
            Color32::from_rgb(95, 110, 205),
            Color32::from_rgb(155, 95, 185),
            Color32::from_rgb(180, 180, 180),
        ];
        match self {
            Self::DsView => DSVIEW[index % DSVIEW.len()],
            Self::Classic => CLASSIC[index % CLASSIC.len()],
        }
    }

    /// Waveform trace color: DSView draws bright, near-white traces.
    fn trace(self) -> Color32 {
        match self {
            Self::DsView => Color32::from_rgb(205, 205, 205),
            Self::Classic => Color32::from_rgb(135, 135, 135),
        }
    }
}

pub struct LogicAnalyzerViewer {
    channels: Vec<LogicChannel>,
    channel_order: Vec<usize>,
    channel_drag: Option<ChannelDragState>,
    channel_names: HashMap<usize, String>,
    channel_rename: Option<ChannelRenameState>,
    /// Synchronous sampler over the waveform index; present once the index
    /// build (which runs on a worker thread) has completed. Sampling the
    /// visible window happens on the UI thread every frame the view changes,
    /// so what is drawn is always the current view at the current zoom —
    /// there is no asynchronous refinement that could disagree with it.
    sampler: Option<IndexSampler<DslCaptureReader>>,
    /// (start_sample, end_sample, target_points) of the sampled `channels`.
    sampled_key: Option<(u64, u64, usize)>,
    /// Pulse measurement for the current hover position, refreshed each frame
    /// by `sample_hover_measurement`. Computed separately from `channels`
    /// because at low zoom the hovered channel may only have summarized
    /// `waveform` bands, which don't carry individual edge times — measuring
    /// then requires an extra exact query into the index around the pointer.
    hover_measurement: Option<PulseMeasurement>,
    visible_start_us: f64,
    visible_span_us: f64,
    capture_path: Option<PathBuf>,
    capture_info: Option<CaptureInfo>,
    worker_responses: Option<Receiver<WorkerResponse>>,
    status: String,
    index_progress: Option<IndexBuildProgress>,
    fit_to_capture: bool,
    /// DSView-style time cursors, in creation order. Unbounded.
    cursors: Vec<TimeCursor>,
    /// Index into `cursors` of the cursor currently being dragged.
    drag_cursor: Option<usize>,
    color_profile: ColorProfile,
}

struct ChannelRenameState {
    channel_index: usize,
    text: String,
    screen_pos: Pos2,
}

struct ChannelDragState {
    channel_index: usize,
}

#[derive(Clone, Copy)]
struct AnalyzerLayout {
    header_rect: Rect,
    ruler_rect: Rect,
    labels_rect: Rect,
    wave_rect: Rect,
    row_height: f32,
    name_col_width: f32,
    badge_width: f32,
}

/// A vertical time marker (DSView-style "cursor"), added by double-clicking
/// the ruler and moved by dragging its flag or line.
#[derive(Debug, Clone, Copy)]
struct TimeCursor {
    /// Display number (1-based). Freed numbers are reused, so a cursor's
    /// number — and the flag color derived from it — stays stable while
    /// other cursors come and go.
    number: usize,
    time_us: f64,
}

/// Per-frame outcome of cursor interaction, used to keep cursor drags from
/// also panning the view and ruler double-clicks from also fitting it.
#[derive(Default, Clone, Copy)]
struct CursorInput {
    /// Cursor being dragged or hovered, for highlighting.
    active: Option<usize>,
    blocks_pan: bool,
    ruler_double_click: bool,
}

#[derive(Debug, Clone)]
pub struct LogicChannel {
    index: usize,
    name: String,
    initial: bool,
    transitions: Vec<Transition>,
    waveform: Vec<WaveformSegment>,
}

#[derive(Debug, Clone, Copy)]
struct Transition {
    time_us: f64,
    value: bool,
}

#[derive(Debug, Clone, Copy)]
struct WaveformSegment {
    start_us: f64,
    end_us: f64,
    kind: WaveformSegmentKind,
}

#[derive(Debug, Clone, Copy)]
enum WaveformSegmentKind {
    Level { value: bool },
    Edge { before: bool, after: bool },
    Activity { first: bool, last: bool },
}

#[derive(Debug, Clone, Copy)]
struct PulseMeasurement {
    channel_row: usize,
    value: bool,
    start_us: f64,
    end_us: f64,
    /// The bounding toggle on this side lies outside the examined window, so
    /// `start_us`/`end_us` is the window edge and Width is a lower bound.
    start_open: bool,
    end_open: bool,
    // `None` when the trace doesn't have a following transition to close a
    // full period (e.g. a single isolated pulse) — Width is still valid.
    period_end_us: Option<f64>,
}

impl PulseMeasurement {
    fn width_us(self) -> f64 {
        self.end_us - self.start_us
    }

    fn period_us(self) -> Option<f64> {
        self.period_end_us
            .map(|period_end_us| period_end_us - self.start_us)
    }

    fn duty_cycle(self) -> Option<f64> {
        self.period_us()
            .map(|period_us| self.width_us() / period_us)
    }
}

/// Exact transitions pulled from the index for a window around a point of
/// interest, with the window bounds and the level at its start.
#[derive(Debug, Clone)]
struct ExactWindow {
    initial: bool,
    start_us: f64,
    end_us: f64,
    transitions: Vec<Transition>,
}

#[derive(Debug, Clone)]
struct CaptureInfo {
    path: PathBuf,
    header: CaptureMetadata,
    duration_us: f64,
}

#[derive(Debug, Clone, Copy)]
struct IndexBuildProgress {
    completed_roots: usize,
    total_roots: usize,
}

impl IndexBuildProgress {
    fn fraction(self) -> f32 {
        if self.total_roots == 0 {
            1.0
        } else {
            self.completed_roots as f32 / self.total_roots as f32
        }
    }
}

enum WorkerResponse {
    Opened {
        path: PathBuf,
        header: CaptureMetadata,
        duration_us: f64,
    },
    Status {
        path: PathBuf,
        message: String,
    },
    IndexProgress {
        path: PathBuf,
        progress: CaptureIndexProgress,
    },
    IndexReady {
        path: PathBuf,
    },
    Error {
        path: PathBuf,
        message: String,
    },
}

impl LogicAnalyzerViewer {
    pub fn demo() -> Self {
        let mut channels = Vec::new();
        for index in 0..10 {
            let period = match index {
                0 => 180.0,
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
            sampler: None,
            sampled_key: None,
            hover_measurement: None,
            visible_start_us: 0.0,
            visible_span_us: 900.0,
            capture_path: None,
            capture_info: None,
            worker_responses: None,
            status: "Demo data".to_string(),
            index_progress: None,
            fit_to_capture: false,
            cursors: Vec::new(),
            drag_cursor: None,
            color_profile: ColorProfile::DsView,
        }
    }

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
        spawn_capture_worker(path, data_source, response_tx);
        self.worker_responses = Some(response_rx);
    }

    pub fn show(&mut self, ui: &mut Ui) {
        let rect = ui.available_rect_before_wrap();
        let response = ui.allocate_rect(rect, Sense::click_and_drag());
        let painter = ui.painter_at(rect);

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
            response.dragged_by(PointerButton::Primary)
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

    fn layout(&self, ui: &Ui, rect: Rect) -> AnalyzerLayout {
        let title_height = 26.0;
        let ruler_height = 34.0;
        let row_height = 30.0;
        let label_pad = 12.0;
        let name_badge_gap = 10.0;
        let label_right_pad = 10.0;
        let name_font = FontId::proportional(12.0);
        let badge_font = FontId::monospace(10.0);
        let (name_col_width, badge_width) = ui.ctx().fonts_mut(|fonts| {
            let name_col_width = self
                .channels
                .iter()
                .map(|channel| {
                    fonts
                        .layout_no_wrap(channel.name.clone(), name_font.clone(), Color32::WHITE)
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
                            Color32::WHITE,
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

    fn handle_channel_label_input(
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

    fn handle_channel_reorder(
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

    fn channel_row_at_pointer(&self, layout: AnalyzerLayout, pointer: Pos2) -> Option<usize> {
        if !layout.labels_rect.contains(pointer) {
            return None;
        }
        self.channel_row_at_y(layout, pointer.y)
    }

    fn channel_row_at_y(&self, layout: AnalyzerLayout, y: f32) -> Option<usize> {
        if y < layout.labels_rect.top() || y > layout.labels_rect.bottom() {
            return None;
        }
        let row = ((y - layout.labels_rect.top()) / layout.row_height).floor() as usize;
        self.channels.get(row).map(|_| row)
    }

    fn channel_badge_rect(&self, layout: AnalyzerLayout, row: usize) -> Rect {
        let row_top = layout.labels_rect.top() + row as f32 * layout.row_height;
        Rect::from_min_size(
            Pos2::new(
                layout.labels_rect.left() + 12.0 + layout.name_col_width + 10.0,
                row_top + layout.row_height * 0.5 - 8.0,
            ),
            vec2(layout.badge_width, 16.0),
        )
    }

    fn show_channel_rename(&mut self, ctx: &egui::Context) {
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

    fn apply_channel_names(&self, channels: &mut [LogicChannel]) {
        for channel in channels {
            channel.name = self.channel_display_name(channel.index);
        }
    }

    fn ensure_channel_order(&mut self, channel_count: usize) {
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

    fn apply_channel_order(&self, channels: &mut Vec<LogicChannel>) {
        channels.sort_by_key(|channel| {
            self.channel_order
                .iter()
                .position(|&channel_index| channel_index == channel.index)
                .unwrap_or(usize::MAX)
        });
    }

    fn move_channel_to_row(&mut self, channel_index: usize, target_row: usize) {
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

    /// Samples the visible window from the index synchronously, so the drawn
    /// waveform always matches the current view exactly. Skipped when neither
    /// the view nor the viewport size changed since the last sampling.
    fn sample_visible_window(&mut self, layout: AnalyzerLayout) {
        if layout.wave_rect.width() <= 1.0 {
            return;
        }
        let Some(capture) = self.capture_info.as_ref() else {
            return;
        };
        let samplerate_hz = capture.header.samplerate_hz;
        let channel_count = capture.header.total_probes.min(16);
        let (visible_start, visible_end) =
            visible_sample_range(capture, self.visible_start_us, self.visible_span_us);
        let target_points = layout.wave_rect.width().max(1.0).round() as usize;
        self.ensure_channel_order(channel_count);

        let key = (visible_start, visible_end, target_points);
        if self.sampled_key == Some(key) {
            return;
        }
        let Some(sampler) = self.sampler.as_mut() else {
            return;
        };

        let requested_channels = self.channel_order.clone();
        match sampler.sampled_window(
            &requested_channels,
            visible_start,
            visible_end,
            target_points,
        ) {
            Ok(window) => {
                let mut channels = channels_from_window(&window, samplerate_hz);
                self.apply_channel_names(&mut channels);
                self.apply_channel_order(&mut channels);
                self.channels = channels;
            }
            Err(err) => {
                self.status = format!("Could not read capture window: {err}");
            }
        }
        // Recorded even on failure so a persistent error does not retry every frame.
        self.sampled_key = Some(key);
    }

    /// Refreshes the pulse measurement for the current hover position.
    ///
    /// At low zoom the hovered channel is drawn from summarized `waveform`
    /// bands (see `draw_channel_waveform`), which record only first/last
    /// levels per band and don't carry individual edge times. To keep
    /// measurement accurate at any zoom, this pulls a small exact window
    /// straight from the index around the pointer instead of reusing the
    /// (possibly summarized) data backing the main view.
    fn sample_hover_measurement(&mut self, layout: AnalyzerLayout, pointer: Option<Pos2>) {
        let previous = self.hover_measurement.take();
        let Some(pointer) = pointer else {
            return;
        };
        let wave_rect = layout.wave_rect;
        if !wave_rect.contains(pointer) || wave_rect.width() <= 1.0 {
            return;
        }

        let row_height = layout.row_height;
        let channel_row = ((pointer.y - wave_rect.top()) / row_height).floor() as usize;
        let Some(channel) = self.channels.get(channel_row) else {
            return;
        };
        let time_us = self.x_to_time(wave_rect, pointer.x);

        // A measurement is a property of the run under the pointer, not of
        // the zoom level; while the pointer stays inside a fully resolved
        // run on the same row, the previous result is still exact.
        if let Some(previous) = previous
            && previous.channel_row == channel_row
            && !previous.start_open
            && !previous.end_open
            && time_us >= previous.start_us
            && time_us < previous.end_us
        {
            self.hover_measurement = Some(previous);
            return;
        }

        let visible_end_us = self.visible_start_us + self.visible_span_us;
        // Only demo data (no index) measures from the in-memory transitions;
        // with a capture loaded the index path always runs, since even at
        // zoom levels where the visible window is exact, the run or its
        // period may close beyond the viewport.
        let measurement = if self.sampler.is_none() {
            pulse_measurement_from_window(
                &channel.transitions,
                channel.initial,
                self.visible_start_us,
                visible_end_us,
                time_us,
            )
        } else {
            let channel_index = channel.index;
            let Some(capture) = self.capture_info.as_ref() else {
                return;
            };
            let samplerate_hz = capture.header.samplerate_hz;
            let duration_us = capture.duration_us;

            let window = self.exact_transitions_around(wave_rect, channel_index, time_us, 24.0);
            let mut measurement = window.as_ref().and_then(|window| {
                pulse_measurement_from_window(
                    &window.transitions,
                    window.initial,
                    window.start_us,
                    window.end_us,
                    time_us,
                )
            });

            if let Some(measurement) = measurement.as_mut() {
                let pointer_sample = us_to_sample(time_us, samplerate_hz);
                let mut end_is_toggle = !measurement.end_open;
                // Resolve open sides exactly: search the index for the true
                // bounding toggles, however far away. The measured width
                // must never depend on the zoom level or query window size.
                if measurement.start_open {
                    measurement.start_open = false;
                    if let Some((sample, value)) =
                        self.prev_transition_at_or_before(channel_index, pointer_sample)
                    {
                        measurement.start_us = sample_to_us(sample, samplerate_hz);
                        measurement.value = value;
                    } else {
                        // The run reaches back to the start of the capture.
                        measurement.start_us = 0.0;
                    }
                }
                if measurement.end_open {
                    measurement.end_open = false;
                    if let Some((sample, _)) =
                        self.next_transition_after(channel_index, pointer_sample)
                    {
                        measurement.end_us = sample_to_us(sample, samplerate_hz);
                        end_is_toggle = true;
                    } else {
                        // The run reaches to the end of the capture.
                        measurement.end_us = duration_us;
                    }
                }
                // With the end edge exact, the period may still close beyond
                // the narrow window; one more search finds it.
                if measurement.period_end_us.is_none() && end_is_toggle {
                    let end_sample = us_to_sample(measurement.end_us, samplerate_hz);
                    if let Some((sample, _)) = self.next_transition_after(channel_index, end_sample)
                    {
                        let period_end_us = sample_to_us(sample, samplerate_hz);
                        if period_end_us - measurement.start_us > measurement.width_us() {
                            measurement.period_end_us = Some(period_end_us);
                        }
                    }
                }
            }
            measurement
        };

        self.hover_measurement = measurement.map(|measurement| PulseMeasurement {
            channel_row,
            ..measurement
        });
    }

    /// First toggle strictly after `sample`, searched across the whole
    /// capture.
    fn next_transition_after(&mut self, channel_index: usize, sample: u64) -> Option<(u64, bool)> {
        let total_samples = self.capture_info.as_ref()?.header.total_samples;
        self.find_transition(channel_index, sample, sample, total_samples, false)
    }

    /// Last toggle at or before `sample`, searched across the whole capture.
    fn prev_transition_at_or_before(
        &mut self,
        channel_index: usize,
        sample: u64,
    ) -> Option<(u64, bool)> {
        self.find_transition(channel_index, sample, 0, sample.saturating_add(1), true)
    }

    /// Locates the toggle nearest to `from_sample` within `[lo, hi)` —
    /// forward (first strictly after) or backward (last at or before) — by
    /// descending through the index's summary levels. Idle stretches are
    /// skipped wholesale, so even a bounding toggle many seconds away costs
    /// only a handful of coarse queries. Returns the toggle's sample and the
    /// level after it.
    fn find_transition(
        &mut self,
        channel_index: usize,
        from_sample: u64,
        lo: u64,
        hi: u64,
        backward: bool,
    ) -> Option<(u64, bool)> {
        if hi <= lo {
            return None;
        }
        const POINTS: usize = 1_024;
        let window = self
            .sampler
            .as_mut()?
            .sampled_window(&[channel_index], lo, hi, POINTS)
            .ok()?;
        let channel = window.channels.first()?;
        if window.sample_step == 1 {
            return if backward {
                channel
                    .transitions
                    .iter()
                    .rev()
                    .find(|transition| transition.sample <= from_sample)
            } else {
                channel
                    .transitions
                    .iter()
                    .find(|transition| transition.sample > from_sample)
            }
            .map(|transition| (transition.sample, transition.value));
        }

        let segments: Box<dyn Iterator<Item = &CaptureWaveformSegment>> = if backward {
            Box::new(channel.waveform.iter().rev())
        } else {
            Box::new(channel.waveform.iter())
        };
        for segment in segments {
            match *segment {
                CaptureWaveformSegment::Level { .. } => {}
                CaptureWaveformSegment::Edge { sample, after, .. } => {
                    if (backward && sample <= from_sample) || (!backward && sample > from_sample) {
                        return Some((sample, after));
                    }
                }
                CaptureWaveformSegment::Activity {
                    start_sample,
                    end_sample,
                    ..
                } => {
                    let relevant = if backward {
                        start_sample <= from_sample
                    } else {
                        end_sample > from_sample
                    };
                    if !relevant {
                        continue;
                    }
                    let sub_lo = start_sample.max(lo);
                    let sub_hi = end_sample.min(hi);
                    let found = if (sub_lo, sub_hi) == (lo, hi) {
                        // The summary could not split this range; bisect,
                        // trying the half nearest `from_sample` first.
                        let mid = lo + (hi - lo) / 2;
                        if backward {
                            self.find_transition(channel_index, from_sample, mid, hi, true)
                                .or_else(|| {
                                    self.find_transition(channel_index, from_sample, lo, mid, true)
                                })
                        } else {
                            self.find_transition(channel_index, from_sample, lo, mid, false)
                                .or_else(|| {
                                    self.find_transition(channel_index, from_sample, mid, hi, false)
                                })
                        }
                    } else {
                        self.find_transition(channel_index, from_sample, sub_lo, sub_hi, backward)
                    };
                    if found.is_some() {
                        return found;
                    }
                }
            }
        }
        None
    }

    /// Exact transitions for `channel_index` in an index-backed window
    /// around `time_us`, spanning `neighborhood_px` on-screen pixels to
    /// either side.
    ///
    /// The query is sized to the current zoom, so a signal dense enough to
    /// need band rendering still has its real edges captured. Bounded below
    /// (very zoomed in) and above (very zoomed out) to keep the raw scan
    /// cheap.
    fn exact_transitions_around(
        &mut self,
        wave_rect: Rect,
        channel_index: usize,
        time_us: f64,
        neighborhood_px: f64,
    ) -> Option<ExactWindow> {
        let capture = self.capture_info.as_ref()?;
        let samplerate_hz = capture.header.samplerate_hz;
        let total_samples = capture.header.total_samples;
        let sampler = self.sampler.as_mut()?;

        let samples_per_pixel =
            (self.visible_span_us * samplerate_hz / 1_000_000.0 / wave_rect.width() as f64)
                .max(1.0);
        let half_window_samples =
            ((samples_per_pixel * neighborhood_px) as u64).clamp(4_096, 2_000_000);
        let center_sample = us_to_sample(time_us, samplerate_hz);
        let start_sample = center_sample.saturating_sub(half_window_samples);
        let end_sample = (center_sample + half_window_samples).min(total_samples);
        if end_sample <= start_sample {
            return None;
        }
        let window_samples = (end_sample - start_sample) as usize;

        let window = sampler
            .sampled_window(&[channel_index], start_sample, end_sample, window_samples)
            .ok()?;
        let sampled = window.channels.first()?;
        Some(ExactWindow {
            initial: sampled.initial,
            start_us: sample_to_us(start_sample, samplerate_hz),
            end_us: sample_to_us(end_sample, samplerate_hz),
            transitions: sampled
                .transitions
                .iter()
                .map(|transition| Transition {
                    time_us: sample_to_us(transition.sample, samplerate_hz),
                    value: transition.value,
                })
                .collect(),
        })
    }

    /// Drives cursor add / hover / drag / delete for one frame.
    ///
    /// Runs before pan/zoom handling so an active cursor drag can suppress
    /// panning, and before the fit-on-double-click check so a ruler
    /// double-click means "add cursor" instead.
    fn handle_cursor_input(
        &mut self,
        ui: &Ui,
        response: &Response,
        layout: AnalyzerLayout,
    ) -> CursorInput {
        let mut state = CursorInput::default();
        let wave_rect = layout.wave_rect;
        let ruler_rect = layout.ruler_rect;
        if wave_rect.width() <= 1.0 {
            self.drag_cursor = None;
            return state;
        }

        let pointer = response
            .interact_pointer_pos()
            .or_else(|| ui.input(|input| input.pointer.hover_pos()));
        let flags = self.cursor_flag_layout(ui, wave_rect, ruler_rect);

        // Delete via the flag's close box.
        if response.clicked()
            && let Some(pointer) = pointer
            && let Some(index) = flags.iter().position(|(_, close)| close.contains(pointer))
        {
            self.cursors.remove(index);
            self.drag_cursor = None;
            return state;
        }

        // Double-click in the ruler adds a cursor; double-click elsewhere
        // keeps its fit-to-capture meaning.
        if response.double_clicked()
            && let Some(pointer) = pointer
            && ruler_rect.contains(pointer)
        {
            state.ruler_double_click = true;
            let time_us = self.x_to_time(wave_rect, pointer.x);
            let number = next_cursor_number(&self.cursors);
            self.cursors.push(TimeCursor { number, time_us });
            return state;
        }

        let over_close_box =
            pointer.is_some_and(|pointer| flags.iter().any(|(_, close)| close.contains(pointer)));
        let hovered_cursor = pointer
            .and_then(|pointer| self.cursor_at_pointer(wave_rect, ruler_rect, &flags, pointer));

        if response.drag_started_by(PointerButton::Primary) {
            // Hit-test where the button went down, not where the pointer is
            // now: egui reports drag_started only after the pointer moved
            // past the click-vs-drag threshold, by which time it may already
            // have left the narrow line hit zone.
            let grab_pos = ui.input(|input| input.pointer.press_origin()).or(pointer);
            self.drag_cursor =
                grab_pos.and_then(|pos| self.cursor_at_pointer(wave_rect, ruler_rect, &flags, pos));
        }
        if self.drag_cursor.is_some() {
            if response.dragged_by(PointerButton::Primary) {
                if let (Some(index), Some(pointer)) =
                    (self.drag_cursor, response.interact_pointer_pos())
                {
                    let raw_time_us = self.x_to_time(wave_rect, pointer.x);
                    let time_us = self.snap_cursor_time(wave_rect, pointer, raw_time_us);
                    if let Some(cursor) = self.cursors.get_mut(index) {
                        cursor.time_us = time_us;
                    }
                }
                state.blocks_pan = true;
            } else {
                self.drag_cursor = None;
            }
        }

        state.active = self.drag_cursor.or(hovered_cursor);
        if over_close_box && self.drag_cursor.is_none() {
            ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
        } else if state.active.is_some() {
            ui.ctx().set_cursor_icon(CursorIcon::ResizeHorizontal);
        }
        state
    }

    /// Flag and close-box rects for every cursor, in `cursors` order.
    fn cursor_flag_layout(&self, ui: &Ui, wave_rect: Rect, ruler_rect: Rect) -> Vec<(Rect, Rect)> {
        self.cursors
            .iter()
            .map(|cursor| {
                let x = self.time_to_x_unclamped(wave_rect, cursor.time_us);
                let label = cursor_flag_label(cursor);
                let label_width = ui.ctx().fonts_mut(|fonts| {
                    fonts
                        .layout_no_wrap(label, FontId::proportional(10.0), Color32::BLACK)
                        .size()
                        .x
                });
                cursor_flag_geometry(x, ruler_rect, label_width)
            })
            .collect()
    }

    /// The cursor whose flag or vertical line is under the pointer, if any.
    fn cursor_at_pointer(
        &self,
        wave_rect: Rect,
        ruler_rect: Rect,
        flags: &[(Rect, Rect)],
        pointer: Pos2,
    ) -> Option<usize> {
        const LINE_HIT_PX: f32 = 6.0;

        // The close box deletes on click; it is not a drag handle.
        if flags.iter().any(|(_, close)| close.contains(pointer)) {
            return None;
        }
        if let Some(index) = flags.iter().position(|(flag, _)| flag.contains(pointer)) {
            return Some(index);
        }
        if pointer.y < ruler_rect.top()
            || pointer.y > wave_rect.bottom()
            || pointer.x < wave_rect.left() - LINE_HIT_PX
            || pointer.x > wave_rect.right() + LINE_HIT_PX
        {
            return None;
        }
        self.cursors
            .iter()
            .enumerate()
            .map(|(index, cursor)| {
                let x = self.time_to_x_unclamped(wave_rect, cursor.time_us);
                (index, (pointer.x - x).abs())
            })
            .filter(|&(_, distance)| distance <= LINE_HIT_PX)
            .min_by(|a, b| a.1.total_cmp(&b.1))
            .map(|(index, _)| index)
    }

    /// Snaps `time_us` to the nearest toggle of the channel row under the
    /// pointer when that toggle is within a few pixels, so dragging a cursor
    /// over a signal locks onto its edges. Over the ruler or an empty row
    /// the time stays free.
    fn snap_cursor_time(&mut self, wave_rect: Rect, pointer: Pos2, time_us: f64) -> f64 {
        const SNAP_DISTANCE_PX: f32 = 8.0;
        let row_height = 30.0;
        if pointer.y < wave_rect.top() || pointer.y > wave_rect.bottom() {
            return time_us;
        }
        let channel_row = ((pointer.y - wave_rect.top()) / row_height).floor() as usize;
        let (channel_index, needs_exact_query, nearest_visible) = {
            let Some(channel) = self.channels.get(channel_row) else {
                return time_us;
            };
            (
                channel.index,
                !channel.waveform.is_empty(),
                nearest_transition_time(&channel.transitions, time_us),
            )
        };
        // Band-rendered channels don't carry exact edge times on screen;
        // query the index around the pointer, as hover measurement does.
        let nearest = if needs_exact_query {
            self.exact_transitions_around(wave_rect, channel_index, time_us, 24.0)
                .and_then(|window| nearest_transition_time(&window.transitions, time_us))
        } else {
            nearest_visible
        };
        let Some(nearest) = nearest else {
            return time_us;
        };
        let distance_px = (self.time_to_x_unclamped(wave_rect, nearest)
            - self.time_to_x_unclamped(wave_rect, time_us))
        .abs();
        if distance_px <= SNAP_DISTANCE_PX {
            nearest
        } else {
            time_us
        }
    }

    fn handle_input(&mut self, ui: &Ui, layout: AnalyzerLayout, hovered: bool, dragging: bool) {
        let wave_rect = layout.wave_rect;
        if wave_rect.width() <= 1.0 {
            return;
        }

        if dragging {
            if self.capture_info.is_some() {
                self.fit_to_capture = false;
            }
            let delta = ui.input(|input| input.pointer.delta());
            self.visible_start_us -=
                delta.x as f64 / wave_rect.width() as f64 * self.visible_span_us;
            self.visible_start_us = self.visible_start_us.max(0.0);
            self.clamp_to_capture_duration();
        }

        if hovered {
            let scroll_delta = ui.input(|input| input.smooth_scroll_delta);
            if scroll_delta.x.abs() > SCROLL_INPUT_EPSILON {
                if self.capture_info.is_some() {
                    self.fit_to_capture = false;
                }
                self.visible_start_us -=
                    scroll_delta.x as f64 / wave_rect.width() as f64 * self.visible_span_us;
                self.visible_start_us = self.visible_start_us.max(0.0);
                self.clamp_to_capture_duration();
            }
            if scroll_delta.y.abs() > SCROLL_INPUT_EPSILON {
                if self.capture_info.is_some() {
                    self.fit_to_capture = false;
                }
                let pointer_x = ui
                    .input(|input| input.pointer.hover_pos())
                    .map_or(0.5, |pos| {
                        ((pos.x - wave_rect.left()) / wave_rect.width()).clamp(0.0, 1.0)
                    }) as f64;

                let old_span = self.visible_span_us;
                let pivot_time = self.visible_start_us + old_span * pointer_x;
                let factor = (1.0_f64 - scroll_delta.y as f64 * 0.0015).clamp(0.35, 2.5);
                let max_span = self
                    .capture_info
                    .as_ref()
                    .map_or(f64::MAX, |capture| capture.duration_us.max(1.0));
                self.visible_span_us = (self.visible_span_us * factor).clamp(0.001, max_span);
                self.visible_start_us = pivot_time - self.visible_span_us * pointer_x;
                self.visible_start_us = self.visible_start_us.max(0.0);
                self.clamp_to_capture_duration();
            }
        }
    }

    fn fit_capture(&mut self) {
        if let Some(capture) = self.capture_info.as_ref() {
            self.visible_start_us = 0.0;
            self.visible_span_us = capture.duration_us.max(1.0);
            self.fit_to_capture = true;
        }
    }

    fn clamp_to_capture_duration(&mut self) {
        if let Some(capture) = self.capture_info.as_ref() {
            let duration_us = capture.duration_us;
            self.visible_span_us = self.visible_span_us.min(duration_us.max(1.0));
            self.visible_start_us = self
                .visible_start_us
                .clamp(0.0, (duration_us - self.visible_span_us).max(0.0));
        }
    }

    fn process_worker_responses(&mut self) {
        let mut responses = Vec::new();
        if let Some(receiver) = &self.worker_responses {
            responses.extend(receiver.try_iter());
        }

        for response in responses {
            match response {
                WorkerResponse::Opened {
                    path,
                    header,
                    duration_us,
                } => {
                    if self.capture_path.as_deref() != Some(path.as_path()) {
                        continue;
                    }
                    self.capture_info = Some(CaptureInfo {
                        path: path.clone(),
                        header: header.clone(),
                        duration_us,
                    });
                    self.visible_start_us = 0.0;
                    self.visible_span_us = duration_us.max(1.0);
                    self.fit_to_capture = true;
                    if let Some(capture) = self.capture_info.as_ref() {
                        self.status = capture_status(capture);
                    }
                    self.ensure_channel_order(header.total_probes.min(16));
                    let mut channels = placeholder_channels(&header);
                    self.apply_channel_names(&mut channels);
                    self.apply_channel_order(&mut channels);
                    self.channels = channels;
                    self.sampler = None;
                    self.sampled_key = None;
                    self.index_progress = None;
                }
                WorkerResponse::Status { path, message } => {
                    if self.capture_path.as_deref() == Some(path.as_path()) {
                        self.status = message;
                    }
                }
                WorkerResponse::IndexProgress { path, progress } => {
                    if self.capture_path.as_deref() == Some(path.as_path()) {
                        self.index_progress = Some(IndexBuildProgress {
                            completed_roots: progress.completed_roots,
                            total_roots: progress.total_roots,
                        });
                        self.status = format!(
                            "Building waveform index… {}/{}",
                            progress.completed_roots, progress.total_roots
                        );
                    }
                }
                WorkerResponse::IndexReady { path } => {
                    if self.capture_path.as_deref() != Some(path.as_path()) {
                        continue;
                    }
                    self.index_progress = None;
                    // The worker validated/built the index; opening it here is
                    // cheap (header + directory read) and gives the UI thread
                    // its own sampler for synchronous per-frame sampling.
                    match DslFileCaptureDataSource::open(&path)
                        .and_then(IndexSampler::open_data_source)
                    {
                        Ok(sampler) => {
                            self.sampler = Some(sampler);
                            self.sampled_key = None;
                            if self.fit_to_capture {
                                self.fit_capture();
                            }
                            self.status = self
                                .capture_info
                                .as_ref()
                                .map(capture_status)
                                .unwrap_or_else(|| "Capture ready".to_string());
                        }
                        Err(err) => {
                            self.status = format!("Could not open capture: {err}");
                        }
                    }
                }
                WorkerResponse::Error { path, message } => {
                    if self.capture_path.as_deref() == Some(path.as_path()) {
                        self.status = message;
                    }
                }
            }
        }
    }

    fn draw(
        &self,
        painter: &Painter,
        layout: AnalyzerLayout,
        pointer: Option<Pos2>,
        active_cursor: Option<usize>,
    ) {
        let rect = Rect::from_min_max(layout.header_rect.min, layout.wave_rect.right_bottom());
        if rect.width() <= 1.0 || rect.height() <= 1.0 {
            return;
        }

        let background = Color32::from_rgb(22, 22, 22);
        let panel = Color32::from_rgb(30, 30, 30);
        let grid = Color32::from_rgb(52, 52, 52);
        let grid_minor = Color32::from_rgb(38, 38, 38);
        let text = Color32::from_rgb(205, 205, 205);
        let muted = Color32::from_rgb(135, 135, 135);

        painter.rect_filled(rect, 0.0, background);

        let header_rect = layout.header_rect;
        let ruler_rect = layout.ruler_rect;
        let labels_rect = layout.labels_rect;
        let wave_rect = layout.wave_rect;
        let row_height = layout.row_height;

        painter.rect_filled(header_rect, 0.0, panel);
        painter.text(
            header_rect.left_center() + vec2(10.0, 0.0),
            Align2::LEFT_CENTER,
            "Logic Analyzer Viewer",
            FontId::proportional(13.0),
            text,
        );
        painter.text(
            // Leave room for the color-profile selector at the far right.
            header_rect.right_center() - vec2(120.0, 0.0),
            Align2::RIGHT_CENTER,
            format!(
                "{} channels · {} span · {}",
                self.channels.len(),
                format_duration(self.visible_span_us),
                self.status
            ),
            FontId::proportional(11.0),
            muted,
        );
        if let Some(progress) = self.index_progress {
            let progress_rect = Rect::from_min_max(
                Pos2::new(header_rect.left(), header_rect.bottom() - 3.0),
                header_rect.right_bottom(),
            );
            painter.rect_filled(progress_rect, 0.0, Color32::from_rgb(45, 45, 45));
            let fill_rect = Rect::from_min_max(
                progress_rect.left_top(),
                Pos2::new(
                    progress_rect.left() + progress_rect.width() * progress.fraction(),
                    progress_rect.bottom(),
                ),
            );
            painter.rect_filled(fill_rect, 0.0, Color32::from_rgb(75, 145, 210));
        }

        painter.rect_filled(labels_rect, 0.0, Color32::from_rgb(25, 25, 25));
        painter.line_segment(
            [
                Pos2::new(wave_rect.left(), rect.top()),
                Pos2::new(wave_rect.left(), rect.bottom()),
            ],
            Stroke::new(1.0, Color32::from_rgb(45, 45, 45)),
        );

        self.draw_ruler(painter, ruler_rect, wave_rect, grid, grid_minor, muted);
        let trace = self.color_profile.trace();
        self.draw_channels(
            painter,
            labels_rect,
            wave_rect,
            row_height,
            layout.name_col_width,
            layout.badge_width,
            text,
            trace,
            grid,
        );

        // Pointer position marker: a small triangle hanging from the ruler
        // bottom instead of a full-height crosshair line.
        if let Some(pointer) = pointer
            && pointer.x >= wave_rect.left()
            && pointer.x <= wave_rect.right()
            && pointer.y >= ruler_rect.top()
            && pointer.y <= wave_rect.bottom()
        {
            painter.add(Shape::convex_polygon(
                vec![
                    Pos2::new(pointer.x - 4.0, ruler_rect.bottom() - 6.0),
                    Pos2::new(pointer.x + 4.0, ruler_rect.bottom() - 6.0),
                    Pos2::new(pointer.x, ruler_rect.bottom()),
                ],
                Color32::from_rgba_premultiplied(220, 220, 220, 200),
                Stroke::NONE,
            ));

            if wave_rect.contains(pointer)
                && let Some(measurement) = self.hover_measurement
            {
                self.draw_pulse_measurement(painter, wave_rect, row_height, measurement);
            }
        }

        self.draw_cursors(painter, ruler_rect, wave_rect, active_cursor);
    }

    fn draw_cursors(
        &self,
        painter: &Painter,
        ruler_rect: Rect,
        wave_rect: Rect,
        active: Option<usize>,
    ) {
        for (index, cursor) in self.cursors.iter().enumerate() {
            let x = self.time_to_x_unclamped(wave_rect, cursor.time_us);
            if x < wave_rect.left() - 1.0 || x > wave_rect.right() + 1.0 {
                continue;
            }
            let color = cursor_color(cursor.number.wrapping_sub(1));
            let is_active = active == Some(index);

            let label = cursor_flag_label(cursor);
            let galley = painter.layout_no_wrap(label, FontId::proportional(10.0), Color32::BLACK);
            let (flag, close) = cursor_flag_geometry(x, ruler_rect, galley.size().x);

            painter.extend(Shape::dashed_line(
                &[
                    Pos2::new(x, flag.bottom()),
                    Pos2::new(x, wave_rect.bottom()),
                ],
                Stroke::new(if is_active { 1.8 } else { 1.0 }, color),
                5.0,
                4.0,
            ));

            painter.rect_filled(flag, 3.0, color);
            if is_active {
                painter.rect_stroke(
                    flag,
                    3.0,
                    Stroke::new(1.0, Color32::WHITE),
                    StrokeKind::Outside,
                );
            }
            painter.add(Shape::convex_polygon(
                vec![
                    Pos2::new(x - 5.0, flag.bottom()),
                    Pos2::new(x + 5.0, flag.bottom()),
                    Pos2::new(x, (flag.bottom() + 7.0).min(ruler_rect.bottom())),
                ],
                color,
                Stroke::NONE,
            ));
            painter.galley(
                Pos2::new(flag.left() + 6.0, flag.center().y - galley.size().y * 0.5),
                galley,
                Color32::BLACK,
            );

            let close_stroke = Stroke::new(1.3, Color32::from_rgb(25, 25, 25));
            let pad = 4.5;
            painter.line_segment(
                [
                    close.left_top() + vec2(pad, pad),
                    close.right_bottom() - vec2(pad, pad),
                ],
                close_stroke,
            );
            painter.line_segment(
                [
                    Pos2::new(close.right() - pad, close.top() + pad),
                    Pos2::new(close.left() + pad, close.bottom() - pad),
                ],
                close_stroke,
            );
        }
    }

    fn draw_ruler(
        &self,
        painter: &Painter,
        ruler_rect: Rect,
        wave_rect: Rect,
        grid: Color32,
        grid_minor: Color32,
        muted: Color32,
    ) {
        painter.rect_filled(ruler_rect, 0.0, Color32::from_rgb(26, 26, 26));

        let start = self.visible_start_us;
        let end = self.visible_start_us + self.visible_span_us;
        let major_step =
            nice_step(self.visible_span_us / (wave_rect.width() as f64 / 120.0).max(2.0));
        let minor_step = major_step / 10.0;

        let mut minor = (start / minor_step).floor() * minor_step;
        while minor <= end {
            let x = self.time_to_x(wave_rect, minor);
            if x >= wave_rect.left() && x <= wave_rect.right() {
                let h = if ((minor / major_step).round() - minor / major_step).abs() < 0.001 {
                    18.0
                } else {
                    9.0
                };
                painter.line_segment(
                    [
                        Pos2::new(x, ruler_rect.bottom() - h),
                        Pos2::new(x, ruler_rect.bottom()),
                    ],
                    Stroke::new(1.0, grid_minor),
                );
            }
            minor += minor_step;
        }

        let mut major = (start / major_step).floor() * major_step;
        while major <= end {
            let x = self.time_to_x(wave_rect, major);
            if x >= wave_rect.left() && x <= wave_rect.right() {
                painter.line_segment(
                    [
                        Pos2::new(x, ruler_rect.top() + 7.0),
                        Pos2::new(x, wave_rect.bottom()),
                    ],
                    Stroke::new(1.0, grid),
                );
                painter.text(
                    Pos2::new(x + 4.0, ruler_rect.top() + 5.0),
                    Align2::LEFT_TOP,
                    format_time(major, major_step),
                    FontId::proportional(10.0),
                    muted,
                );
            }
            major += major_step;
        }

        painter.line_segment(
            [ruler_rect.left_bottom(), ruler_rect.right_bottom()],
            Stroke::new(1.0, grid),
        );
    }

    fn draw_channels(
        &self,
        painter: &Painter,
        labels_rect: Rect,
        wave_rect: Rect,
        row_height: f32,
        name_col_width: f32,
        badge_width: f32,
        text: Color32,
        trace: Color32,
        grid: Color32,
    ) {
        let clip = painter.with_clip_rect(wave_rect);

        for (index, channel) in self.channels.iter().enumerate() {
            let y_top = labels_rect.top() + index as f32 * row_height;
            let row_rect = Rect::from_min_max(
                Pos2::new(labels_rect.left(), y_top),
                Pos2::new(wave_rect.right(), y_top + row_height),
            );
            if row_rect.top() > labels_rect.bottom() {
                break;
            }

            painter.line_segment(
                [
                    Pos2::new(labels_rect.left(), row_rect.bottom()),
                    Pos2::new(wave_rect.right(), row_rect.bottom()),
                ],
                Stroke::new(1.0, Color32::from_rgb(42, 42, 42)),
            );

            let name_pos = Pos2::new(labels_rect.left() + 12.0, row_rect.center().y);
            painter.text(
                name_pos,
                Align2::LEFT_CENTER,
                &channel.name,
                FontId::proportional(12.0),
                text,
            );

            let badge_rect = Rect::from_min_size(
                Pos2::new(
                    labels_rect.left() + 12.0 + name_col_width + 10.0,
                    row_rect.center().y - 8.0,
                ),
                vec2(badge_width, 16.0),
            );
            let badge_color = self.color_profile.channel_color(channel.index);
            painter.rect_filled(badge_rect, 2.0, badge_color);
            painter.text(
                badge_rect.center(),
                Align2::CENTER_CENTER,
                channel.index.to_string(),
                FontId::monospace(10.0),
                badge_text_color(badge_color),
            );

            let center_y = row_rect.center().y;
            clip.line_segment(
                [
                    Pos2::new(wave_rect.left(), center_y),
                    Pos2::new(wave_rect.right(), center_y),
                ],
                Stroke::new(1.0, grid),
            );
            self.draw_channel_waveform(&clip, wave_rect, y_top, row_height, channel, trace);
        }
    }

    fn draw_channel_waveform(
        &self,
        painter: &Painter,
        wave_rect: Rect,
        y_top: f32,
        row_height: f32,
        channel: &LogicChannel,
        trace: Color32,
    ) {
        let high_y = y_top + row_height * 0.28;
        let low_y = y_top + row_height * 0.72;
        let start = self.visible_start_us;
        let end = start + self.visible_span_us;
        let stroke = Stroke::new(1.4, trace);

        if !channel.waveform.is_empty() {
            self.draw_segment_waveform(painter, wave_rect, high_y, low_y, channel, trace);
            return;
        }

        let (visible_transitions, mut value) = channel.visible_transitions(start, end);
        let mut prev_x = wave_rect.left();
        let mut y = if value { high_y } else { low_y };

        for transition in visible_transitions {
            let x = self.time_to_x(wave_rect, transition.time_us);
            painter.line_segment([Pos2::new(prev_x, y), Pos2::new(x, y)], stroke);

            value = transition.value;
            let next_y = if value { high_y } else { low_y };
            painter.line_segment([Pos2::new(x, y), Pos2::new(x, next_y)], stroke);

            prev_x = x;
            y = next_y;
        }

        painter.line_segment(
            [Pos2::new(prev_x, y), Pos2::new(wave_rect.right(), y)],
            stroke,
        );
    }

    fn draw_segment_waveform(
        &self,
        painter: &Painter,
        wave_rect: Rect,
        high_y: f32,
        low_y: f32,
        channel: &LogicChannel,
        trace: Color32,
    ) {
        let start = self.visible_start_us;
        let end = start + self.visible_span_us;
        let flat_stroke = Stroke::new(1.15, trace);
        let activity_stroke = Stroke::new(1.0, trace);

        for segment in channel
            .waveform
            .iter()
            .filter(|segment| segment.end_us >= start && segment.start_us <= end)
        {
            let x0 = self.time_to_x(wave_rect, segment.start_us);
            let x1 = self.time_to_x(wave_rect, segment.end_us);

            match segment.kind {
                WaveformSegmentKind::Level { value } => {
                    let y = if value { high_y } else { low_y };
                    Self::draw_clipped_horizontal(painter, wave_rect, x0, x1, y, flat_stroke);
                }
                WaveformSegmentKind::Edge { before, after } => {
                    let y0 = if before { high_y } else { low_y };
                    let y1 = if after { high_y } else { low_y };
                    painter.line_segment([Pos2::new(x0, y0), Pos2::new(x0, y1)], activity_stroke);
                }
                WaveformSegmentKind::Activity { first, last } => {
                    Self::draw_activity_summary(
                        painter,
                        wave_rect,
                        x0,
                        x1,
                        high_y,
                        low_y,
                        first,
                        last,
                        flat_stroke,
                        activity_stroke,
                    );
                }
            }
        }
    }

    fn draw_activity_summary(
        painter: &Painter,
        clip: Rect,
        x0: f32,
        x1: f32,
        high_y: f32,
        low_y: f32,
        first: bool,
        last: bool,
        flat_stroke: Stroke,
        activity_stroke: Stroke,
    ) {
        let left = x0.min(x1).max(clip.left());
        let right = x0.max(x1).min(clip.right());
        if right <= left {
            return;
        }

        // An activity segment wider than a couple of pixels (a coarse window
        // stretched by zooming in) only promises "at least one toggle in this
        // range" — draw it as a solid band rather than inventing edge
        // positions that a refresh would then contradict.
        if right - left > 3.0 {
            painter.rect_filled(
                Rect::from_min_max(Pos2::new(left, high_y), Pos2::new(right, low_y)),
                0.0,
                flat_stroke.color,
            );
            return;
        }

        let y_first = if first { high_y } else { low_y };
        let y_last = if last { high_y } else { low_y };
        let marker_x = ((left + right) * 0.5).clamp(clip.left(), clip.right());

        if first == last {
            Self::draw_clipped_horizontal(painter, clip, left, right, y_last, flat_stroke);
        } else if right - left >= 4.0 {
            Self::draw_clipped_horizontal(painter, clip, left, marker_x, y_first, flat_stroke);
            Self::draw_clipped_horizontal(painter, clip, marker_x, right, y_last, flat_stroke);
        } else {
            Self::draw_clipped_horizontal(painter, clip, left, right, y_last, flat_stroke);
        }

        painter.line_segment(
            [Pos2::new(marker_x, high_y), Pos2::new(marker_x, low_y)],
            activity_stroke,
        );
    }

    fn draw_clipped_horizontal(
        painter: &Painter,
        clip: Rect,
        x0: f32,
        x1: f32,
        y: f32,
        stroke: Stroke,
    ) {
        let left = x0.min(x1).max(clip.left());
        let right = x0.max(x1).min(clip.right());
        if right > left {
            painter.line_segment([Pos2::new(left, y), Pos2::new(right, y)], stroke);
        }
    }

    fn draw_pulse_measurement(
        &self,
        painter: &Painter,
        wave_rect: Rect,
        row_height: f32,
        measurement: PulseMeasurement,
    ) {
        let yellow = Color32::from_rgb(255, 190, 0);
        let stroke = Stroke::new(1.2, yellow);
        let row_top = wave_rect.top() + measurement.channel_row as f32 * row_height;
        let high_y = row_top + row_height * 0.28;
        let low_y = row_top + row_height * 0.72;
        let signal_y = if measurement.value { high_y } else { low_y };
        let marker_y = row_top + row_height * 0.5;

        let x0_raw = self.time_to_x_unclamped(wave_rect, measurement.start_us);
        let x1 = self.time_to_x_unclamped(wave_rect, measurement.end_us);
        // Without a following transition to close a full period, fall back to
        // a Width-only bracket spanning just the measured pulse.
        let has_period = measurement.period_end_us.is_some();
        let x2_raw = measurement.period_end_us.map_or(x1, |period_end_us| {
            self.time_to_x_unclamped(wave_rect, period_end_us)
        });
        if (x2_raw - x0_raw).abs() < 2.0 {
            return;
        }

        // Edges can fall outside the visible window (or, for an open run,
        // outside the examined window entirely); clamp the line to what's on
        // screen. The arrowhead and the vertical connector down to the
        // signal only draw for a real toggle that is actually in view — on
        // any other side the plain line simply runs off the viewport edge.
        let x0 = x0_raw.clamp(wave_rect.left(), wave_rect.right());
        let x2 = x2_raw.clamp(wave_rect.left(), wave_rect.right());
        let start_edge_in_view =
            !measurement.start_open && x0_raw >= wave_rect.left() && x0_raw <= wave_rect.right();
        let end_edge_in_view = !(measurement.end_open && !has_period)
            && x2_raw >= wave_rect.left()
            && x2_raw <= wave_rect.right();

        painter.line_segment([Pos2::new(x0, marker_y), Pos2::new(x2, marker_y)], stroke);
        if start_edge_in_view {
            self.draw_measurement_arrow_end(painter, Pos2::new(x0, marker_y), 1.0, yellow);
            painter.line_segment(
                [Pos2::new(x0, marker_y - 4.0), Pos2::new(x0, signal_y)],
                stroke,
            );
        }
        if end_edge_in_view {
            self.draw_measurement_arrow_end(painter, Pos2::new(x2, marker_y), -1.0, yellow);
            painter.line_segment(
                [Pos2::new(x2, marker_y - 4.0), Pos2::new(x2, signal_y)],
                stroke,
            );
        }

        if has_period && x1 > wave_rect.left() && x1 < wave_rect.right() {
            painter.line_segment(
                [
                    Pos2::new(x1 - 3.5, marker_y - 3.5),
                    Pos2::new(x1 + 3.5, marker_y + 3.5),
                ],
                stroke,
            );
            painter.line_segment(
                [
                    Pos2::new(x1 - 3.5, marker_y + 3.5),
                    Pos2::new(x1 + 3.5, marker_y - 3.5),
                ],
                stroke,
            );
            painter.line_segment([Pos2::new(x1, marker_y), Pos2::new(x1, signal_y)], stroke);
        }

        self.draw_measurement_tooltip(painter, wave_rect, marker_y, measurement);
    }

    fn draw_measurement_arrow_end(
        &self,
        painter: &Painter,
        tip: Pos2,
        direction: f32,
        color: Color32,
    ) {
        let stroke = Stroke::new(1.2, color);
        let dx = 5.0 * direction;
        painter.line_segment([tip, Pos2::new(tip.x + dx, tip.y - 4.0)], stroke);
        painter.line_segment([tip, Pos2::new(tip.x + dx, tip.y + 4.0)], stroke);
    }

    fn draw_measurement_tooltip(
        &self,
        painter: &Painter,
        wave_rect: Rect,
        marker_y: f32,
        measurement: PulseMeasurement,
    ) {
        let width_line = if measurement.start_open || measurement.end_open {
            // One or both toggles lie beyond the examined window; the width
            // only says how long the run provably is.
            format!(
                "Width: > {}",
                format_delta(measurement.width_us()).trim_start_matches('+')
            )
        } else {
            format!("Width: {}", format_delta(measurement.width_us()))
        };
        let mut lines = vec![width_line];
        if let Some(period_us) = measurement.period_us() {
            lines.push(format!("Period: {}", format_delta(period_us)));
            lines.push(format!("Frequency: {}", format_frequency(period_us)));
        }
        if let Some(duty_cycle) = measurement.duty_cycle() {
            lines.push(format!("Duty Cycle: {:.2}%", duty_cycle * 100.0));
        }

        let width = 175.0_f32.min(wave_rect.width().max(1.0));
        let height = (20.0 * lines.len() as f32 + 16.0).min(wave_rect.height().max(1.0));
        let x0 = self.time_to_x(wave_rect, measurement.start_us);
        let x1 = self.time_to_x(wave_rect, measurement.end_us);
        let center_x = ((x0 + x1) * 0.5).clamp(wave_rect.left(), wave_rect.right());
        let left = (center_x - width * 0.5)
            .max(wave_rect.left())
            .min(wave_rect.right() - width);
        let top = (marker_y + 8.0)
            .max(wave_rect.top())
            .min(wave_rect.bottom() - height);
        let rect = Rect::from_min_size(Pos2::new(left, top), vec2(width, height));
        let background = Color32::from_rgba_premultiplied(0, 120, 180, 225);
        let yellow = Color32::from_rgb(255, 190, 0);

        painter.rect_filled(rect, 0.0, background);

        for (index, line) in lines.iter().enumerate() {
            painter.text(
                Pos2::new(rect.right() - 8.0, rect.top() + 10.0 + index as f32 * 20.0),
                Align2::RIGHT_TOP,
                line,
                FontId::proportional(11.0),
                yellow,
            );
        }
    }

    fn time_to_x(&self, rect: Rect, time_us: f64) -> f32 {
        let t = ((time_us - self.visible_start_us) / self.visible_span_us).clamp(0.0, 1.0);
        rect.left() + rect.width() * t as f32
    }

    /// Like [`Self::time_to_x`] but without pinning off-screen times to the
    /// viewport edge, so callers can cull (cursors) instead of drawing a
    /// misleading edge line.
    fn time_to_x_unclamped(&self, rect: Rect, time_us: f64) -> f32 {
        let t = (time_us - self.visible_start_us) / self.visible_span_us;
        rect.left() + rect.width() * t as f32
    }

    fn x_to_time(&self, rect: Rect, x: f32) -> f64 {
        let t = ((x - rect.left()) / rect.width()).clamp(0.0, 1.0) as f64;
        self.visible_start_us + self.visible_span_us * t
    }
}

/// Opens the capture and builds (or validates) the waveform index on a
/// background thread, reporting progress. Window sampling itself happens
/// synchronously on the UI thread once the index is ready.
fn spawn_capture_worker(
    identity: PathBuf,
    data_source: impl CaptureDataSource,
    responses: Sender<WorkerResponse>,
) {
    std::thread::Builder::new()
        .name("dsl_capture_indexer".to_string())
        .spawn(move || {
            let header = data_source.metadata().clone();
            let duration_us = header.duration_us();
            if responses
                .send(WorkerResponse::Opened {
                    path: identity.clone(),
                    header,
                    duration_us,
                })
                .is_err()
            {
                return;
            }

            if responses
                .send(WorkerResponse::Status {
                    path: identity.clone(),
                    message: "Building waveform index…".to_string(),
                })
                .is_err()
            {
                return;
            }

            let progress_path = identity.clone();
            let progress_responses = responses.clone();
            let mut last_progress_sent = std::time::Instant::now()
                .checked_sub(std::time::Duration::from_millis(100))
                .unwrap_or_else(std::time::Instant::now);
            let mut last_progress_completed = 0_usize;
            let result = IndexSampler::open_data_source_with_progress(data_source, |progress| {
                let now = std::time::Instant::now();
                let is_first = progress.completed_roots == 0;
                let is_done = progress.completed_roots >= progress.total_roots;
                let enough_time =
                    now.duration_since(last_progress_sent) >= std::time::Duration::from_millis(100);
                let enough_work = progress
                    .completed_roots
                    .saturating_sub(last_progress_completed)
                    >= 64;
                if is_first || is_done || enough_time || enough_work {
                    last_progress_sent = now;
                    last_progress_completed = progress.completed_roots;
                    let _ = progress_responses.send(WorkerResponse::IndexProgress {
                        path: progress_path.clone(),
                        progress,
                    });
                }
            });

            let response = match result {
                Ok(_) => WorkerResponse::IndexReady { path: identity },
                Err(err) => WorkerResponse::Error {
                    path: identity,
                    message: format!("Could not open capture: {err}"),
                },
            };
            let _ = responses.send(response);
        })
        .expect("capture indexer thread should start");
}

fn capture_status(capture: &CaptureInfo) -> String {
    format!(
        "{} · {} · {:.1} MHz · {} samples",
        capture
            .path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("capture"),
        capture.header.samplerate,
        capture.header.samplerate_hz / 1_000_000.0,
        capture.header.total_samples
    )
}

impl LogicChannel {
    fn square_wave(
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

    fn visible_transitions(&self, start_us: f64, end_us: f64) -> (&[Transition], bool) {
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

/// Measures the run (high or low) under `time_us` from transitions covering
/// `[window_start_us, window_end_us]`. When a bounding toggle lies outside
/// the window, that side falls back to the window edge and is marked open,
/// so hovering the tail after the last visible toggle still measures.
fn pulse_measurement_from_window(
    transitions: &[Transition],
    initial: bool,
    window_start_us: f64,
    window_end_us: f64,
    time_us: f64,
) -> Option<PulseMeasurement> {
    let end_index = transitions.partition_point(|transition| transition.time_us <= time_us);
    let start = end_index
        .checked_sub(1)
        .and_then(|index| transitions.get(index));
    let end = transitions.get(end_index);

    let (start_us, start_open, value) = match start {
        Some(transition) => (transition.time_us, false, transition.value),
        None => (window_start_us, true, initial),
    };
    let (end_us, end_open) = match end {
        Some(transition) => (transition.time_us, false),
        None => (window_end_us, true),
    };

    let width_us = end_us - start_us;
    if width_us <= 0.0 {
        return None;
    }

    let period_end_us = if start_open || end_open {
        None
    } else {
        transitions
            .get(end_index + 1)
            .map(|period_end| period_end.time_us)
            .filter(|&period_end_us| period_end_us - start_us > width_us)
    };

    Some(PulseMeasurement {
        channel_row: 0,
        value,
        start_us,
        end_us,
        start_open,
        end_open,
        period_end_us,
    })
}

fn channels_from_window(window: &CaptureSampledWindow, samplerate_hz: f64) -> Vec<LogicChannel> {
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

fn placeholder_channels(header: &CaptureMetadata) -> Vec<LogicChannel> {
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

/// Black on light badges, white on dark ones (grey, brown, blue, violet).
fn badge_text_color(background: Color32) -> Color32 {
    let luminance = 0.299 * background.r() as f32
        + 0.587 * background.g() as f32
        + 0.114 * background.b() as f32;
    if luminance < 128.0 {
        Color32::WHITE
    } else {
        Color32::BLACK
    }
}

fn us_to_sample(time_us: f64, samplerate_hz: f64) -> u64 {
    (time_us.max(0.0) * samplerate_hz / 1_000_000.0).round() as u64
}

fn sample_to_us(sample: u64, samplerate_hz: f64) -> f64 {
    sample as f64 * 1_000_000.0 / samplerate_hz
}

fn visible_sample_range(capture: &CaptureInfo, start_us: f64, span_us: f64) -> (u64, u64) {
    let samplerate_hz = capture.header.samplerate_hz;
    let total_samples = capture.header.total_samples;
    let visible_start = us_to_sample(start_us, samplerate_hz).min(total_samples.saturating_sub(1));
    let visible_end =
        us_to_sample(start_us + span_us, samplerate_hz).clamp(visible_start + 1, total_samples);
    (visible_start, visible_end)
}

/// Flag box and its embedded close-box for a cursor whose line is at `x`,
/// clamped to stay inside the ruler. Shared by hit-testing and drawing so
/// they can never disagree.
fn cursor_flag_geometry(x: f32, ruler_rect: Rect, label_width: f32) -> (Rect, Rect) {
    const CLOSE_WIDTH: f32 = 15.0;
    const HEIGHT: f32 = 16.0;
    let width = label_width + 12.0 + CLOSE_WIDTH;
    let left = (x - width * 0.5).clamp(
        ruler_rect.left(),
        (ruler_rect.right() - width).max(ruler_rect.left()),
    );
    let top = ruler_rect.top() + 1.0;
    let flag = Rect::from_min_size(Pos2::new(left, top), vec2(width, HEIGHT));
    let close = Rect::from_min_size(
        Pos2::new(flag.right() - CLOSE_WIDTH, top),
        vec2(CLOSE_WIDTH, HEIGHT),
    );
    (flag, close)
}

fn cursor_flag_label(cursor: &TimeCursor) -> String {
    format!("{}  {}", cursor.number, format_cursor_time(cursor.time_us))
}

/// Smallest positive number not used by an existing cursor, so numbers (and
/// their colors) are stable while cursors come and go.
fn next_cursor_number(cursors: &[TimeCursor]) -> usize {
    let mut used: Vec<usize> = cursors.iter().map(|cursor| cursor.number).collect();
    used.sort_unstable();
    let mut number = 1;
    for existing in used {
        if existing == number {
            number += 1;
        } else if existing > number {
            break;
        }
    }
    number
}

fn nearest_transition_time(transitions: &[Transition], time_us: f64) -> Option<f64> {
    let index = transitions.partition_point(|transition| transition.time_us < time_us);
    let after = transitions.get(index).map(|transition| transition.time_us);
    let before = index
        .checked_sub(1)
        .and_then(|index| transitions.get(index))
        .map(|transition| transition.time_us);
    match (before, after) {
        (Some(before), Some(after)) => Some(if time_us - before <= after - time_us {
            before
        } else {
            after
        }),
        (before, after) => before.or(after),
    }
}

fn cursor_color(index: usize) -> Color32 {
    const PALETTE: [Color32; 8] = [
        Color32::from_rgb(60, 180, 75),
        Color32::from_rgb(70, 140, 220),
        Color32::from_rgb(230, 90, 70),
        Color32::from_rgb(220, 185, 60),
        Color32::from_rgb(180, 100, 210),
        Color32::from_rgb(70, 195, 200),
        Color32::from_rgb(235, 130, 180),
        Color32::from_rgb(160, 200, 90),
    ];
    PALETTE[index % PALETTE.len()]
}

/// Cursor flags show more precision than the ruler ticks, since a snapped
/// cursor marks an exact edge.
fn format_cursor_time(us: f64) -> String {
    let abs = us.abs();
    if abs >= 1_000_000.0 {
        format!("+{:.6}s", us / 1_000_000.0)
    } else if abs >= 1_000.0 {
        format!("+{:.4}ms", us / 1_000.0)
    } else if abs >= 1.0 {
        format!("+{:.3}µs", us)
    } else {
        format!("+{:.1}ns", us * 1_000.0)
    }
}

fn nice_step(raw: f64) -> f64 {
    if raw <= 0.0 {
        return 1.0;
    }

    let base = 10.0_f64.powf(raw.log10().floor());
    let fraction = raw / base;
    let nice = if fraction <= 1.0 {
        1.0
    } else if fraction <= 2.0 {
        2.0
    } else if fraction <= 5.0 {
        5.0
    } else {
        10.0
    };
    nice * base
}

/// Formats a ruler tick label, choosing the unit from the tick's magnitude
/// and the decimal count from the tick spacing, so adjacent labels stay
/// distinguishable at any zoom (down to nanoseconds, even at large offsets).
fn format_time(us: f64, step_us: f64) -> String {
    let (scale, unit) = if us.abs() >= 1_000_000.0 {
        (1e-6, "s")
    } else if us.abs() >= 1_000.0 {
        (1e-3, "ms")
    } else if us.abs() >= 1.0 {
        (1.0, "µs")
    } else {
        (1e3, "ns")
    };
    let value = us * scale;
    let step = (step_us * scale).abs();
    let decimals = if step > 0.0 {
        (-step.log10().floor()).clamp(0.0, 9.0) as usize
    } else {
        0
    };
    format!("+{value:.decimals$}{unit}")
}

fn format_duration(us: f64) -> String {
    if us >= 1_000_000.0 {
        format!("{:.2} s", us / 1_000_000.0)
    } else if us >= 1_000.0 {
        format!("{:.2} ms", us / 1_000.0)
    } else if us >= 1.0 {
        format!("{:.2} µs", us)
    } else {
        format!("{:.0} ns", us * 1_000.0)
    }
}

/// Formats a time delta with at least 8 significant digits (DSView-style),
/// scaled to the natural unit.
fn format_delta(us: f64) -> String {
    let ns = us * 1_000.0;
    let (value, unit) = if ns.abs() < 1_000.0 {
        (ns, "ns")
    } else if us.abs() < 1_000.0 {
        (us, "µs")
    } else if us.abs() < 1_000_000.0 {
        (us / 1_000.0, "ms")
    } else {
        (us / 1_000_000.0, "s")
    };
    let integer_digits = if value.abs() < 1.0 {
        1
    } else {
        value.abs().log10().floor() as usize + 1
    };
    let decimals = 8_usize.saturating_sub(integer_digits);
    format!("+{value:.decimals$}{unit}")
}

fn format_frequency(period_us: f64) -> String {
    if period_us <= 0.0 {
        return "—".to_string();
    }

    let hz = 1_000_000.0 / period_us;
    if hz >= 1_000_000.0 {
        format!("{:.2}MHz", hz / 1_000_000.0)
    } else if hz >= 1_000.0 {
        format!("{:.2}kHz", hz / 1_000.0)
    } else {
        format!("{hz:.2}Hz")
    }
}

#[cfg(test)]
mod cursor_tests {
    use super::*;

    fn transition(time_us: f64) -> Transition {
        Transition {
            time_us,
            value: false,
        }
    }

    #[test]
    fn nearest_transition_picks_closest_side() {
        let transitions = [transition(10.0), transition(20.0), transition(30.0)];
        assert_eq!(nearest_transition_time(&transitions, 14.0), Some(10.0));
        assert_eq!(nearest_transition_time(&transitions, 16.0), Some(20.0));
        assert_eq!(nearest_transition_time(&transitions, 5.0), Some(10.0));
        assert_eq!(nearest_transition_time(&transitions, 35.0), Some(30.0));
        assert_eq!(nearest_transition_time(&[], 5.0), None);
    }

    fn edge(time_us: f64, value: bool) -> Transition {
        Transition { time_us, value }
    }

    #[test]
    fn measurement_between_two_toggles_is_closed() {
        let transitions = [edge(10.0, true), edge(20.0, false), edge(40.0, true)];
        let measurement =
            pulse_measurement_from_window(&transitions, false, 0.0, 100.0, 15.0).unwrap();
        assert_eq!(measurement.start_us, 10.0);
        assert_eq!(measurement.end_us, 20.0);
        assert!(!measurement.start_open && !measurement.end_open);
        assert!(measurement.value);
        assert_eq!(measurement.period_end_us, Some(40.0));
    }

    #[test]
    fn measurement_after_last_toggle_is_open_ended() {
        let transitions = [edge(10.0, true), edge(20.0, false)];
        let measurement =
            pulse_measurement_from_window(&transitions, false, 0.0, 100.0, 60.0).unwrap();
        assert_eq!(measurement.start_us, 20.0);
        assert_eq!(measurement.end_us, 100.0);
        assert!(!measurement.start_open);
        assert!(measurement.end_open);
        assert!(!measurement.value);
        assert_eq!(measurement.period_end_us, None);
    }

    #[test]
    fn measurement_before_first_toggle_uses_initial_level() {
        let transitions = [edge(50.0, true)];
        let measurement =
            pulse_measurement_from_window(&transitions, false, 0.0, 100.0, 25.0).unwrap();
        assert_eq!(measurement.start_us, 0.0);
        assert_eq!(measurement.end_us, 50.0);
        assert!(measurement.start_open);
        assert!(!measurement.end_open);
        assert!(!measurement.value);
    }

    #[test]
    fn measurement_with_no_toggles_spans_whole_window() {
        let measurement = pulse_measurement_from_window(&[], true, 0.0, 100.0, 50.0).unwrap();
        assert!(measurement.start_open && measurement.end_open);
        assert!(measurement.value);
        assert_eq!(measurement.width_us(), 100.0);
    }

    #[test]
    fn cursor_numbers_reuse_freed_slots() {
        assert_eq!(next_cursor_number(&[]), 1);
        let with_gap = [
            TimeCursor {
                number: 1,
                time_us: 0.0,
            },
            TimeCursor {
                number: 3,
                time_us: 0.0,
            },
        ];
        assert_eq!(next_cursor_number(&with_gap), 2);
        let contiguous = [
            TimeCursor {
                number: 1,
                time_us: 0.0,
            },
            TimeCursor {
                number: 2,
                time_us: 0.0,
            },
        ];
        assert_eq!(next_cursor_number(&contiguous), 3);
    }
}
