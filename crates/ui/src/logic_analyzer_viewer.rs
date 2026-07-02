use dsl::{
    CaptureDataSource, CaptureIndexProgress, CaptureMetadata, CaptureSampledWindow,
    CaptureWaveformSegment, DslCaptureReader, DslFileCaptureDataSource, IndexSampler,
};
use egui::{Align2, Color32, FontId, Painter, PointerButton, Pos2, Rect, Sense, Stroke, Ui, vec2};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, Sender};

const SCROLL_INPUT_EPSILON: f32 = 0.5;

pub struct LogicAnalyzerViewer {
    channels: Vec<LogicChannel>,
    /// Synchronous sampler over the waveform index; present once the index
    /// build (which runs on a worker thread) has completed. Sampling the
    /// visible window happens on the UI thread every frame the view changes,
    /// so what is drawn is always the current view at the current zoom —
    /// there is no asynchronous refinement that could disagree with it.
    sampler: Option<IndexSampler<DslCaptureReader>>,
    /// (start_sample, end_sample, target_points) of the sampled `channels`.
    sampled_key: Option<(u64, u64, usize)>,
    visible_start_us: f64,
    visible_span_us: f64,
    capture_path: Option<PathBuf>,
    capture_info: Option<CaptureInfo>,
    worker_responses: Option<Receiver<WorkerResponse>>,
    status: String,
    index_progress: Option<IndexBuildProgress>,
    fit_to_capture: bool,
}

#[derive(Debug, Clone)]
pub struct LogicChannel {
    index: usize,
    name: String,
    color: Color32,
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
    period_end_us: f64,
}

impl PulseMeasurement {
    fn width_us(self) -> f64 {
        self.end_us - self.start_us
    }

    fn period_us(self) -> f64 {
        self.period_end_us - self.start_us
    }

    fn duty_cycle(self) -> f64 {
        self.width_us() / self.period_us()
    }
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
        let palette = [
            Color32::from_rgb(210, 65, 65),
            Color32::from_rgb(210, 125, 45),
            Color32::from_rgb(215, 195, 45),
            Color32::from_rgb(80, 160, 85),
            Color32::from_rgb(70, 155, 190),
            Color32::from_rgb(95, 110, 205),
            Color32::from_rgb(155, 95, 185),
            Color32::from_rgb(180, 180, 180),
        ];

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
                format!("Ch {index}"),
                palette[index % palette.len()],
                period,
                offset,
                index % 3 == 0,
            ));
        }

        Self {
            channels,
            sampler: None,
            sampled_key: None,
            visible_start_us: 0.0,
            visible_span_us: 900.0,
            capture_path: None,
            capture_info: None,
            worker_responses: None,
            status: "Demo data".to_string(),
            index_progress: None,
            fit_to_capture: false,
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
                self.sampler = None;
                self.sampled_key = None;
                self.index_progress = None;
                self.worker_responses = None;
                self.status = format!("Could not inspect capture: {err}");
                return;
            }
        };

        self.capture_path = Some(path.clone());
        self.capture_info = None;
        self.channels.clear();
        self.sampler = None;
        self.sampled_key = None;
        self.index_progress = None;
        self.fit_to_capture = true;
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
        if response.double_clicked()
            || (response.hovered() && ui.input(|input| input.key_pressed(egui::Key::F)))
        {
            self.fit_capture();
        }
        self.handle_input(
            ui,
            rect,
            response.hovered(),
            response.dragged_by(PointerButton::Primary),
        );
        self.sample_visible_window(rect);
        self.draw(&painter, rect, response.hover_pos());
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

    /// Samples the visible window from the index synchronously, so the drawn
    /// waveform always matches the current view exactly. Skipped when neither
    /// the view nor the viewport size changed since the last sampling.
    fn sample_visible_window(&mut self, rect: Rect) {
        if rect.width() <= 1.0 {
            return;
        }
        let Some(capture) = self.capture_info.as_ref() else {
            return;
        };
        let samplerate_hz = capture.header.samplerate_hz;
        let channel_count = capture.header.total_probes.min(16);
        let (visible_start, visible_end) =
            visible_sample_range(capture, self.visible_start_us, self.visible_span_us);
        let left_width = 145.0_f32.min(rect.width() * 0.35);
        let target_points = (rect.width() - left_width).max(1.0).round() as usize;

        let key = (visible_start, visible_end, target_points);
        if self.sampled_key == Some(key) {
            return;
        }
        let Some(sampler) = self.sampler.as_mut() else {
            return;
        };

        let channels: Vec<usize> = (0..channel_count).collect();
        match sampler.sampled_window(&channels, visible_start, visible_end, target_points) {
            Ok(window) => {
                self.channels = channels_from_window(&window, samplerate_hz);
            }
            Err(err) => {
                self.status = format!("Could not read capture window: {err}");
            }
        }
        // Recorded even on failure so a persistent error does not retry every frame.
        self.sampled_key = Some(key);
    }

    fn handle_input(&mut self, ui: &Ui, rect: Rect, hovered: bool, dragging: bool) {
        if rect.width() <= 1.0 {
            return;
        }

        let wave_rect = waveform_rect(rect);
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
                self.visible_span_us = (self.visible_span_us * factor).clamp(20.0, max_span);
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
                    self.channels = placeholder_channels(&header);
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
                            // Sized so the widest L2 viewport (~17 blocks ×
                            // 16 channels) fits without evictions (~34 MB).
                            self.sampler = Some(sampler.with_max_cached_leaves(512));
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

    fn draw(&self, painter: &Painter, rect: Rect, pointer: Option<Pos2>) {
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

        let left_width = 145.0_f32.min(rect.width() * 0.35);
        let title_height = 26.0;
        let ruler_height = 34.0;
        let row_height = 30.0;
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

        painter.rect_filled(header_rect, 0.0, panel);
        painter.text(
            header_rect.left_center() + vec2(10.0, 0.0),
            Align2::LEFT_CENTER,
            "Logic Analyzer Viewer",
            FontId::proportional(13.0),
            text,
        );
        painter.text(
            header_rect.right_center() - vec2(10.0, 0.0),
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
                Pos2::new(rect.left() + left_width, rect.top()),
                Pos2::new(rect.left() + left_width, rect.bottom()),
            ],
            Stroke::new(1.0, Color32::from_rgb(45, 45, 45)),
        );

        self.draw_ruler(painter, ruler_rect, wave_rect, grid, grid_minor, muted);
        self.draw_channels(
            painter,
            labels_rect,
            wave_rect,
            row_height,
            text,
            muted,
            grid,
        );

        if let Some(pointer) = pointer
            && wave_rect.contains(pointer)
        {
            painter.line_segment(
                [
                    Pos2::new(pointer.x, wave_rect.top()),
                    Pos2::new(pointer.x, wave_rect.bottom()),
                ],
                Stroke::new(1.0, Color32::from_rgba_premultiplied(220, 220, 220, 70)),
            );

            if let Some(measurement) =
                self.hovered_pulse_measurement(wave_rect, row_height, pointer)
            {
                self.draw_pulse_measurement(painter, wave_rect, row_height, measurement);
            }
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
                    format_time(major),
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
        text: Color32,
        muted: Color32,
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

            let badge_rect = Rect::from_min_size(
                Pos2::new(labels_rect.left() + 8.0, row_rect.center().y - 8.0),
                vec2(26.0, 16.0),
            );
            painter.rect_filled(badge_rect, 2.0, channel.color);
            painter.text(
                badge_rect.center(),
                Align2::CENTER_CENTER,
                channel.index.to_string(),
                FontId::monospace(10.0),
                Color32::BLACK,
            );
            painter.text(
                Pos2::new(labels_rect.left() + 43.0, row_rect.center().y),
                Align2::LEFT_CENTER,
                &channel.name,
                FontId::proportional(12.0),
                text,
            );

            let center_y = row_rect.center().y;
            clip.line_segment(
                [
                    Pos2::new(wave_rect.left(), center_y),
                    Pos2::new(wave_rect.right(), center_y),
                ],
                Stroke::new(1.0, grid),
            );
            self.draw_channel_waveform(&clip, wave_rect, y_top, row_height, channel, muted);
        }
    }

    fn draw_channel_waveform(
        &self,
        painter: &Painter,
        wave_rect: Rect,
        y_top: f32,
        row_height: f32,
        channel: &LogicChannel,
        muted: Color32,
    ) {
        let high_y = y_top + row_height * 0.28;
        let low_y = y_top + row_height * 0.72;
        let start = self.visible_start_us;
        let end = start + self.visible_span_us;
        let stroke = Stroke::new(1.4, muted);

        if !channel.waveform.is_empty() {
            self.draw_segment_waveform(painter, wave_rect, high_y, low_y, channel, muted);
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
        muted: Color32,
    ) {
        let start = self.visible_start_us;
        let end = start + self.visible_span_us;
        let flat_stroke = Stroke::new(1.15, muted);
        let activity_stroke = Stroke::new(1.0, muted);

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

    fn hovered_pulse_measurement(
        &self,
        wave_rect: Rect,
        row_height: f32,
        pointer: Pos2,
    ) -> Option<PulseMeasurement> {
        if !wave_rect.contains(pointer) || row_height <= 0.0 {
            return None;
        }

        let channel_row = ((pointer.y - wave_rect.top()) / row_height).floor() as usize;
        let channel = self.channels.get(channel_row)?;
        if !channel.waveform.is_empty() {
            return None;
        }

        let time_us = self.x_to_time(wave_rect, pointer.x);
        channel
            .pulse_measurement_at(time_us)
            .map(|measurement| PulseMeasurement {
                channel_row,
                ..measurement
            })
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
        let row_bottom = row_top + row_height;
        let high_y = row_top + row_height * 0.28;
        let low_y = row_top + row_height * 0.72;
        let signal_y = if measurement.value { high_y } else { low_y };
        let marker_y = if measurement.value {
            (signal_y - 8.0).max(row_top + 4.0)
        } else {
            (signal_y + 8.0).min(row_bottom - 4.0)
        };

        let x0 = self.time_to_x(wave_rect, measurement.start_us);
        let x1 = self.time_to_x(wave_rect, measurement.end_us);
        let x2 = self.time_to_x(wave_rect, measurement.period_end_us);
        if (x2 - x0).abs() < 2.0 {
            return;
        }

        painter.line_segment([Pos2::new(x0, marker_y), Pos2::new(x2, marker_y)], stroke);
        self.draw_measurement_arrow_end(painter, Pos2::new(x0, marker_y), 1.0, yellow);
        self.draw_measurement_arrow_end(painter, Pos2::new(x2, marker_y), -1.0, yellow);

        painter.line_segment(
            [Pos2::new(x0, marker_y - 4.0), Pos2::new(x0, signal_y)],
            stroke,
        );
        painter.line_segment(
            [Pos2::new(x2, marker_y - 4.0), Pos2::new(x2, signal_y)],
            stroke,
        );

        if x1 > wave_rect.left() && x1 < wave_rect.right() {
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
        let width = 145.0_f32.min(wave_rect.width().max(1.0));
        let height = 80.0_f32.min(wave_rect.height().max(1.0));
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

        let lines = [
            format!("Width: {}", format_delta(measurement.width_us())),
            format!("Period: {}", format_delta(measurement.period_us())),
            format!("Frequency: {}", format_frequency(measurement.period_us())),
            format!("Duty Cycle: {:.2}%", measurement.duty_cycle() * 100.0),
        ];

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
                let enough_time = now.duration_since(last_progress_sent)
                    >= std::time::Duration::from_millis(100);
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
        color: Color32,
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
            color,
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

    fn pulse_measurement_at(&self, time_us: f64) -> Option<PulseMeasurement> {
        if self.transitions.len() < 3 {
            return None;
        }

        let end_index = self
            .transitions
            .iter()
            .position(|transition| transition.time_us > time_us)?;
        let start_index = end_index.checked_sub(1)?;
        let period_end_index = start_index + 2;
        let period_end = self.transitions.get(period_end_index)?;
        let start = self.transitions[start_index];
        let end = self.transitions[end_index];

        let width_us = end.time_us - start.time_us;
        let period_us = period_end.time_us - start.time_us;
        if width_us <= 0.0 || period_us <= width_us {
            return None;
        }

        Some(PulseMeasurement {
            channel_row: 0,
            value: start.value,
            start_us: start.time_us,
            end_us: end.time_us,
            period_end_us: period_end.time_us,
        })
    }
}

fn channels_from_window(window: &CaptureSampledWindow, samplerate_hz: f64) -> Vec<LogicChannel> {
    window
        .channels
        .iter()
        .map(|channel| LogicChannel {
            index: channel.channel,
            name: channel.name.clone(),
            color: channel_color(channel.channel),
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
            color: channel_color(channel),
            initial: false,
            transitions: Vec::new(),
            waveform: Vec::new(),
        })
        .collect()
}

fn channel_color(index: usize) -> Color32 {
    const PALETTE: [Color32; 8] = [
        Color32::from_rgb(210, 65, 65),
        Color32::from_rgb(210, 125, 45),
        Color32::from_rgb(215, 195, 45),
        Color32::from_rgb(80, 160, 85),
        Color32::from_rgb(70, 155, 190),
        Color32::from_rgb(95, 110, 205),
        Color32::from_rgb(155, 95, 185),
        Color32::from_rgb(180, 180, 180),
    ];
    PALETTE[index % PALETTE.len()]
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

fn waveform_rect(rect: Rect) -> Rect {
    let left_width = 145.0_f32.min(rect.width() * 0.35);
    let title_height = 26.0;
    let ruler_height = 34.0;
    Rect::from_min_max(
        Pos2::new(
            rect.left() + left_width,
            rect.top() + title_height + ruler_height,
        ),
        rect.max,
    )
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

fn format_time(us: f64) -> String {
    if us.abs() >= 1_000.0 {
        format!("+{:.2}ms", us / 1_000.0)
    } else {
        format!("+{:.0}µs", us)
    }
}

fn format_duration(us: f64) -> String {
    if us >= 1_000.0 {
        format!("{:.2} ms", us / 1_000.0)
    } else {
        format!("{:.0} µs", us)
    }
}

fn format_delta(us: f64) -> String {
    let ns = us * 1_000.0;
    if ns.abs() < 1_000.0 {
        format!("+{ns:.0}ns")
    } else if us.abs() < 1_000.0 {
        format!("+{us:.2}µs")
    } else if us.abs() < 1_000_000.0 {
        format!("+{:.2}ms", us / 1_000.0)
    } else {
        format!("+{:.2}s", us / 1_000_000.0)
    }
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
