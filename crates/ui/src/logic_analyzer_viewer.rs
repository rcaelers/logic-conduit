use dsl::{
    CaptureDataSource, CaptureIndexProgress, CaptureMetadata, CaptureSampledWindow, CaptureSource,
    CaptureWaveformSegment, DslCaptureReader, DslFileCaptureDataSource, IndexSampler,
    exact_window_sample_limit,
};
use egui::{Align2, Color32, FontId, Painter, PointerButton, Pos2, Rect, Sense, Stroke, Ui, vec2};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, Sender};

const SCROLL_INPUT_EPSILON: f32 = 0.5;
const MAX_EXACT_TRANSITIONS_PER_PIXEL: f32 = 6.0;

pub struct LogicAnalyzerViewer {
    channels: Vec<LogicChannel>,
    visible_start_us: f64,
    visible_span_us: f64,
    capture_path: Option<PathBuf>,
    capture_info: Option<CaptureInfo>,
    worker_requests: Option<Sender<WorkerRequest>>,
    worker_responses: Option<Receiver<WorkerResponse>>,
    dsl_data_source: Option<DslFileCaptureDataSource>,
    interactive_sampler: Option<IndexSampler<DslCaptureReader>>,
    status: String,
    last_window: Option<WindowKey>,
    pending_window: Option<WindowKey>,
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
    Activity,
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

#[derive(Debug, Clone, PartialEq, Eq)]
struct WindowKey {
    path: PathBuf,
    start_sample: u64,
    end_sample: u64,
    target_points: usize,
    channel_count: usize,
    exact: bool,
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

enum WorkerRequest {
    LoadWindow(WindowKey),
}

enum WorkerResponse {
    Opened {
        path: PathBuf,
        header: CaptureMetadata,
        duration_us: f64,
    },
    Window {
        key: WindowKey,
        samplerate_hz: f64,
        window: CaptureSampledWindow,
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
            visible_start_us: 0.0,
            visible_span_us: 900.0,
            capture_path: None,
            capture_info: None,
            worker_requests: None,
            worker_responses: None,
            dsl_data_source: None,
            interactive_sampler: None,
            status: "Demo data".to_string(),
            last_window: None,
            pending_window: None,
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
                self.last_window = None;
                self.pending_window = None;
                self.index_progress = None;
                self.dsl_data_source = None;
                self.interactive_sampler = None;
                self.status = format!("Could not inspect capture: {err}");
                return;
            }
        };

        self.capture_path = Some(path.clone());
        self.capture_info = None;
        self.channels.clear();
        self.last_window = None;
        self.pending_window = None;
        self.index_progress = None;
        self.fit_to_capture = true;
        self.dsl_data_source = Some(data_source.clone());
        self.interactive_sampler = None;
        self.status = format!("Opening {}", data_source.display_name());

        let (request_tx, request_rx) = mpsc::channel();
        let (response_tx, response_rx) = mpsc::channel();
        spawn_capture_worker(path, data_source, request_rx, response_tx);
        self.worker_requests = Some(request_tx);
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
        self.request_visible_window(rect);
        self.draw(&painter, rect, response.hover_pos());
        if self.pending_window.is_some()
            || (self.capture_path.is_some() && self.capture_info.is_none())
        {
            ui.ctx()
                .request_repaint_after(std::time::Duration::from_millis(16));
        } else if self.index_progress.is_some() {
            ui.ctx()
                .request_repaint_after(std::time::Duration::from_millis(100));
        }
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
                    self.last_window = None;
                    self.pending_window = None;
                    self.index_progress = None;
                }
                WorkerResponse::Window {
                    key,
                    samplerate_hz,
                    window,
                } => {
                    if self.interactive_sampler.is_some() {
                        if self.pending_window.as_ref() == Some(&key) {
                            self.pending_window = None;
                        }
                        continue;
                    }
                    if self.capture_path.as_deref() != Some(key.path.as_path()) {
                        continue;
                    }
                    let Some(capture) = self.capture_info.as_ref() else {
                        continue;
                    };
                    let (visible_start, visible_end) =
                        visible_sample_range(capture, self.visible_start_us, self.visible_span_us);
                    let visible_exact = visible_end.saturating_sub(visible_start)
                        <= exact_window_sample_limit(key.target_points);
                    if key.exact != visible_exact {
                        if self.pending_window.as_ref() == Some(&key) {
                            self.pending_window = None;
                        }
                        continue;
                    }
                    if key.start_sample > visible_start || key.end_sample < visible_end {
                        if self.pending_window.as_ref() == Some(&key) {
                            self.pending_window = None;
                        }
                        continue;
                    }
                    if key.exact && window.sample_step != 1 {
                        if self.pending_window.as_ref() == Some(&key) {
                            self.pending_window = None;
                        }
                        continue;
                    }
                    self.channels = channels_from_window(&window, samplerate_hz);
                    self.last_window = Some(key.clone());
                    if self.pending_window.as_ref() == Some(&key) {
                        self.pending_window = None;
                    }
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
                    if self.capture_path.as_deref() == Some(path.as_path()) {
                        self.index_progress = None;
                        if self.interactive_sampler.is_none()
                            && let Some(data_source) = self.dsl_data_source.clone()
                        {
                            match IndexSampler::open_data_source(data_source)
                                .map(|sampler| sampler.with_max_cached_leaves(64))
                            {
                                Ok(sampler) => {
                                    self.interactive_sampler = Some(sampler);
                                    self.worker_requests = None;
                                    self.last_window = None;
                                    self.pending_window = None;
                                }
                                Err(err) => {
                                    self.status =
                                        format!("Could not open interactive index reader: {err}");
                                    continue;
                                }
                            }
                        }
                        if self.fit_to_capture {
                            self.fit_capture();
                        }
                        self.status = self
                            .capture_info
                            .as_ref()
                            .map(capture_status)
                            .unwrap_or_else(|| "Capture ready".to_string());
                    }
                }
                WorkerResponse::Error { path, message } => {
                    if self.capture_path.as_deref() == Some(path.as_path()) {
                        self.status = message;
                        self.pending_window = None;
                    }
                }
            }
        }
    }

    fn request_visible_window(&mut self, rect: Rect) {
        let Some(capture) = self.capture_info.as_ref() else {
            return;
        };
        if rect.width() <= 1.0 {
            return;
        }

        let left_width = 145.0_f32.min(rect.width() * 0.35);
        let target_points = (rect.width() - left_width).max(1.0).round() as usize;
        let channel_count = capture.header.total_probes.min(16);
        let (visible_start, visible_end) =
            visible_sample_range(capture, self.visible_start_us, self.visible_span_us);
        let visible_samples = visible_end - visible_start;
        let exact = visible_samples <= exact_window_sample_limit(target_points);
        let path = capture.path.clone();

        if self.loaded_window_covers(
            &path,
            visible_start,
            visible_end,
            target_points,
            channel_count,
            exact,
        ) {
            self.pending_window = None;
            return;
        }

        let key = WindowKey {
            path: path.clone(),
            start_sample: visible_start,
            end_sample: visible_end,
            target_points,
            channel_count,
            exact,
        };

        if let Some(sampler) = self.interactive_sampler.as_mut() {
            let channels: Vec<usize> = (0..channel_count).collect();
            match sampler.sampled_window(&channels, visible_start, visible_end, target_points) {
                Ok(window) => {
                    let samplerate_hz = sampler.header().samplerate_hz;
                    self.channels = channels_from_window(&window, samplerate_hz);
                    self.last_window = Some(key);
                    self.pending_window = None;
                    self.status = self
                        .capture_info
                        .as_ref()
                        .map(capture_status)
                        .unwrap_or_else(|| "Capture ready".to_string());
                }
                Err(err) => {
                    self.status = format!("Could not read capture window: {err}");
                    self.pending_window = None;
                }
            }
            return;
        }

        if self.pending_window.as_ref() == Some(&key) {
            return;
        }

        if let Some(sender) = &self.worker_requests
            && sender.send(WorkerRequest::LoadWindow(key.clone())).is_ok()
        {
            self.pending_window = Some(key);
        }
    }

    fn loaded_window_covers(
        &self,
        path: &Path,
        visible_start: u64,
        visible_end: u64,
        target_points: usize,
        channel_count: usize,
        exact: bool,
    ) -> bool {
        self.last_window.as_ref().is_some_and(|window| {
            window.path.as_path() == path
                && window.start_sample <= visible_start
                && window.end_sample >= visible_end
                && (exact || window.target_points >= target_points)
                && window.channel_count == channel_count
                && window.exact == exact
        })
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
        let max_exact_transitions =
            (wave_rect.width().max(1.0) * MAX_EXACT_TRANSITIONS_PER_PIXEL) as usize;
        if visible_transitions.len() > max_exact_transitions {
            self.draw_dense_transition_waveform(
                painter,
                wave_rect,
                high_y,
                low_y,
                visible_transitions,
                value,
                muted,
            );
            return;
        }

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

    fn draw_dense_transition_waveform(
        &self,
        painter: &Painter,
        wave_rect: Rect,
        high_y: f32,
        low_y: f32,
        transitions: &[Transition],
        initial_value: bool,
        muted: Color32,
    ) {
        let start = self.visible_start_us;
        let end = start + self.visible_span_us;
        let pixel_count = wave_rect.width().ceil().max(1.0) as usize;
        let flat_stroke = Stroke::new(1.15, muted);

        let mut transition_index = 0;
        let mut value = initial_value;
        for pixel in 0..pixel_count {
            let x0 = wave_rect.left() + pixel as f32;
            let x1 = (x0 + 1.0).min(wave_rect.right());
            let t0 = start + self.visible_span_us * pixel as f64 / pixel_count as f64;
            let t1 = if pixel + 1 == pixel_count {
                end
            } else {
                start + self.visible_span_us * (pixel + 1) as f64 / pixel_count as f64
            };

            while transition_index < transitions.len() && transitions[transition_index].time_us < t0
            {
                value = transitions[transition_index].value;
                transition_index += 1;
            }

            let mut active = false;
            while transition_index < transitions.len()
                && transitions[transition_index].time_us <= t1
            {
                active = true;
                value = transitions[transition_index].value;
                transition_index += 1;
            }

            if active {
                Self::draw_activity_band(painter, wave_rect, x0, x1, high_y, low_y, muted);
            } else {
                let y = if value { high_y } else { low_y };
                Self::draw_clipped_horizontal(painter, wave_rect, x0, x1, y, flat_stroke);
            }
        }
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
                WaveformSegmentKind::Activity => {
                    Self::draw_activity_band(painter, wave_rect, x0, x1, high_y, low_y, muted);
                }
            }
        }
    }

    fn draw_activity_band(
        painter: &Painter,
        clip: Rect,
        x0: f32,
        x1: f32,
        high_y: f32,
        low_y: f32,
        color: Color32,
    ) {
        let center = ((x0 + x1) * 0.5).clamp(clip.left(), clip.right());
        let half_width = ((x1 - x0).abs().max(1.0)) * 0.5;
        let left = (center - half_width).max(clip.left());
        let right = (center + half_width).min(clip.right());
        if right <= left {
            return;
        }

        let fill = Color32::from_rgba_premultiplied(color.r(), color.g(), color.b(), 118);
        let edge = Color32::from_rgba_premultiplied(color.r(), color.g(), color.b(), 170);
        let rect = Rect::from_min_max(Pos2::new(left, high_y), Pos2::new(right, low_y));
        painter.rect_filled(rect, 0.0, fill);
        painter.line_segment(
            [Pos2::new(left, high_y), Pos2::new(right, high_y)],
            Stroke::new(0.8, edge),
        );
        painter.line_segment(
            [Pos2::new(left, low_y), Pos2::new(right, low_y)],
            Stroke::new(0.8, edge),
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

fn spawn_capture_worker(
    identity: PathBuf,
    data_source: impl CaptureDataSource,
    requests: Receiver<WorkerRequest>,
    responses: Sender<WorkerResponse>,
) {
    std::thread::Builder::new()
        .name("dsl_capture_viewer".to_string())
        .spawn(move || {
            let header = data_source.metadata().clone();
            let samplerate_hz = header.samplerate_hz;
            let duration_us = header.duration_us();
            if responses
                .send(WorkerResponse::Opened {
                    path: identity.clone(),
                    header: header.clone(),
                    duration_us,
                })
                .is_err()
            {
                return;
            }

            match data_source.open_reader() {
                Ok(mut source) => {
                    let channel_count = header.total_probes.min(16);
                    let channels: Vec<usize> = (0..channel_count).collect();
                    let preview_end = header.total_samples.min(100_000).max(1);
                    if let Ok(window) = source.sampled_window(&channels, 0, preview_end, 1_000) {
                        let key = WindowKey {
                            path: identity.clone(),
                            start_sample: 0,
                            end_sample: preview_end,
                            target_points: 1_000,
                            channel_count,
                            exact: false,
                        };
                        if responses
                            .send(WorkerResponse::Window {
                                key,
                                samplerate_hz,
                                window,
                            })
                            .is_err()
                        {
                            return;
                        }
                    }
                }
                Err(err) => {
                    let _ = responses.send(WorkerResponse::Error {
                        path: identity,
                        message: format!("Could not open capture: {err}"),
                    });
                    return;
                }
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
            let mut reader =
                match IndexSampler::open_data_source_with_progress(data_source, |progress| {
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
                })
                .map(|reader| reader.with_max_cached_leaves(8))
                {
                    Ok(reader) => reader,
                    Err(err) => {
                        let _ = responses.send(WorkerResponse::Error {
                            path: identity,
                            message: format!("Could not open capture: {err}"),
                        });
                        return;
                    }
                };

            if responses
                .send(WorkerResponse::IndexReady {
                    path: identity.clone(),
                })
                .is_err()
            {
                return;
            }

            while let Ok(mut request) = requests.recv() {
                while let Ok(newer_request) = requests.try_recv() {
                    request = newer_request;
                }

                match request {
                    WorkerRequest::LoadWindow(key) => {
                        let channels: Vec<usize> = (0..key.channel_count).collect();
                        let samplerate_hz = reader.header().samplerate_hz;
                        match reader.sampled_window(
                            &channels,
                            key.start_sample,
                            key.end_sample,
                            key.target_points,
                        ) {
                            Ok(window) => {
                                if responses
                                    .send(WorkerResponse::Window {
                                        key,
                                        samplerate_hz,
                                        window,
                                    })
                                    .is_err()
                                {
                                    return;
                                }
                            }
                            Err(err) => {
                                if responses
                                    .send(WorkerResponse::Error {
                                        path: identity.clone(),
                                        message: format!("Could not read capture window: {err}"),
                                    })
                                    .is_err()
                                {
                                    return;
                                }
                            }
                        }
                    }
                }
            }
        })
        .expect("capture viewer worker thread should start");
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
                        ..
                    } => WaveformSegment {
                        start_us: sample_to_us(start_sample, samplerate_hz),
                        end_us: sample_to_us(end_sample, samplerate_hz),
                        kind: WaveformSegmentKind::Activity,
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
