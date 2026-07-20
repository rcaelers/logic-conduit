use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver};

use egui::{FontId, Pos2, Rect, Sense, Ui};

use input_bindings::InputBindings;
use signal_processing::{CaptureDataSource, CaptureIndex, CaptureIndexFactory, DerivedLanes};

use crate::channel::LogicChannel;
use crate::indexed_annotations::IndexedAnnotationCacheEntry;
use crate::lanes::{ViewerLaneGroupId, ViewerLaneRegistry};
use crate::sampling_overlay::SamplingOverlay;
use crate::simple_trigger::{SimpleTriggerEdit, SimpleTriggerLane, SimpleTriggerPopup};
use crate::types::{
    AnalyzerLayout, CaptureInfo, ColorProfile, IndexBuildProgress, PulseMeasurement, RowDragState,
    RowKey, RowRenameState, TimeCursor, Transition,
};

const DEFAULT_VISIBLE_SPAN_US: f64 = 900.0;

/// One channel's digital waveform as raw (time, level) transitions — the
/// generic way for a host application to hand [`LogicAnalyzerViewer::set_channels`]
/// waveform data it already has in memory.
pub struct ChannelSignal {
    pub index: usize,
    pub name: String,
    pub initial: bool,
    /// `(time_us, level after this transition)`, in increasing time order.
    pub transitions: Vec<(f64, bool)>,
}

pub struct LogicAnalyzerViewer {
    pub(crate) input_bindings: Arc<InputBindings>,
    pub(crate) channels: Vec<LogicChannel>,
    /// Display order across both `channels` and `derived` lanes — the only
    /// source of truth for row order, kept in sync by `ensure_row_order`.
    pub(crate) row_order: Vec<RowKey>,
    pub(crate) row_order_changed: bool,
    pub(crate) row_drag: Option<RowDragState>,
    pub(crate) channel_names: HashMap<usize, String>,
    pub(crate) derived_names: HashMap<ViewerLaneGroupId, String>,
    pub(crate) row_rename: Option<RowRenameState>,
    /// Synchronous sampler over the waveform index; present once the index
    /// build (which runs on a worker thread) has completed. Sampling the
    /// visible window happens on the UI thread every frame the view changes,
    /// so what is drawn is always the current view at the current zoom —
    /// there is no asynchronous refinement that could disagree with it.
    pub(crate) sampler: Option<Box<dyn CaptureIndex>>,
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
    pub(crate) capture_path: Option<PathBuf>,
    pub(crate) capture_info: Option<CaptureInfo>,
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
    pub(crate) viewer_lanes: ViewerLaneRegistry,
    pub(crate) indexed_annotation_cache: HashMap<String, IndexedAnnotationCacheEntry>,
    pub(crate) sampling_overlay: Option<SamplingOverlay>,
    pub(crate) sampling_overlay_channels: Option<Vec<LogicChannel>>,
    pub(crate) sampling_overlay_key: Option<(u64, u64, u64)>,
    pub(crate) growing_capture: Option<GrowingCaptureView>,
    pub(crate) simple_trigger_lanes: HashMap<usize, SimpleTriggerLane>,
    pub(crate) simple_trigger_popup: Option<SimpleTriggerPopup>,
    pub(crate) pending_simple_trigger_edit: Option<SimpleTriggerEdit>,
    pub(crate) simple_trigger_editing_enabled: bool,
    pub(crate) hovered_input_context: &'static str,
}

pub(crate) struct GrowingCaptureView {
    pub(crate) generation: u64,
    pub(crate) paused: bool,
    pub(crate) follow_newest: bool,
    pub(crate) complete: bool,
    pub(crate) planned_span_us: Option<f64>,
}

impl Default for LogicAnalyzerViewer {
    fn default() -> Self {
        Self::new()
    }
}

impl LogicAnalyzerViewer {
    pub fn new() -> Self {
        Self {
            input_bindings: Arc::new(
                InputBindings::from_json(r#"{"bindings":[]}"#)
                    .expect("empty input binding configuration is valid"),
            ),
            row_order: Vec::new(),
            row_order_changed: false,
            channels: Vec::new(),
            row_drag: None,
            channel_names: HashMap::new(),
            derived_names: HashMap::new(),
            row_rename: None,
            sampler: None,
            sampled_key: None,
            hover_measurement: None,
            visible_start_us: 0.0,
            visible_span_us: DEFAULT_VISIBLE_SPAN_US,
            capture_path: None,
            capture_info: None,
            worker_responses: None,
            status: "No capture loaded".to_string(),
            index_progress: None,
            fit_to_capture: false,
            cursors: Vec::new(),
            drag_cursor: None,
            color_profile: ColorProfile::DsView,
            derived: None,
            viewer_lanes: ViewerLaneRegistry::new(),
            indexed_annotation_cache: HashMap::new(),
            sampling_overlay: None,
            sampling_overlay_channels: None,
            sampling_overlay_key: None,
            growing_capture: None,
            simple_trigger_lanes: HashMap::new(),
            simple_trigger_popup: None,
            pending_simple_trigger_edit: None,
            simple_trigger_editing_enabled: true,
            hovered_input_context: "logic_analyzer",
        }
    }

    pub fn set_input_bindings(&mut self, input_bindings: Arc<InputBindings>) {
        self.input_bindings = input_bindings;
    }

    pub fn set_color_profile(&mut self, color_profile: ColorProfile) {
        self.color_profile = color_profile;
    }

    pub fn status_summary(&self) -> String {
        format!(
            "{} channels · {} span · {}",
            self.channels.len(),
            crate::format::format_duration(self.visible_span_us),
            self.status
        )
    }

    pub fn index_progress_fraction(&self) -> Option<f32> {
        self.index_progress.map(IndexBuildProgress::fraction)
    }

    pub fn hovered_input_context(&self) -> &'static str {
        self.hovered_input_context
    }

    /// Replaces the derived-lane store: the viewer renders whatever the
    /// running pipeline pushes into it, live, in rows below `channels` —
    /// which stay exactly as they are; a run only adds lanes, it never
    /// removes what was already on screen. A fresh (empty) store clears the
    /// previous run's lanes.
    pub fn set_derived_lanes(&mut self, lanes: DerivedLanes) {
        self.derived = Some(lanes);
        self.indexed_annotation_cache.clear();
    }

    /// Replaces the explicit presentation registry paired with the current
    /// derived-lane store. The registry may be populated by graph compilation
    /// after this call; clones share the same per-run contents.
    pub fn set_viewer_lanes(&mut self, lanes: ViewerLaneRegistry) {
        self.viewer_lanes = lanes;
    }

    /// Replaces the protocol-neutral sampling markers drawn over raw capture
    /// rows. The graph/application layer resolves decoder inputs to channel
    /// indices; this widget only renders the resulting electrical sampling
    /// relationship.
    pub fn set_sampling_overlay(&mut self, overlay: Option<SamplingOverlay>) {
        self.sampling_overlay = overlay;
        self.sampling_overlay_channels = None;
        self.sampling_overlay_key = None;
    }

    pub fn set_simple_trigger_lanes(&mut self, lanes: Vec<SimpleTriggerLane>) {
        self.simple_trigger_lanes = lanes.into_iter().map(|lane| (lane.channel, lane)).collect();
        if self
            .simple_trigger_popup
            .as_ref()
            .is_some_and(|popup| !self.simple_trigger_lanes.contains_key(&popup.channel))
        {
            self.simple_trigger_popup = None;
        }
    }

    pub fn set_simple_trigger_editing_enabled(&mut self, enabled: bool) {
        self.simple_trigger_editing_enabled = enabled;
        if !enabled {
            self.simple_trigger_popup = None;
        }
    }

    pub fn take_simple_trigger_edit(&mut self) -> Option<SimpleTriggerEdit> {
        self.pending_simple_trigger_edit.take()
    }

    /// Replaces the raw channel rows with `signals` — the generic way for a
    /// host application to hand the viewer waveform data it already has in
    /// memory, independent of opening a capture file or wiring up a live
    /// pipeline. `derived` lanes are untouched and keep sitting below
    /// whatever channels are here.
    pub fn set_channels(&mut self, signals: Vec<ChannelSignal>) {
        self.channels = signals
            .into_iter()
            .map(|signal| LogicChannel {
                index: signal.index,
                name: signal.name,
                initial: signal.initial,
                transitions: signal
                    .transitions
                    .into_iter()
                    .map(|(time_us, value)| Transition { time_us, value })
                    .collect(),
                waveform: Vec::new(),
            })
            .collect();
        self.ensure_row_order();
    }

    /// Replaces the raw channel rows for a finite in-memory capture and fits
    /// the initial view to its full duration. This is the memory-backed
    /// counterpart of [`Self::set_capture_path`].
    pub fn set_channels_with_duration(&mut self, signals: Vec<ChannelSignal>, duration_us: f64) {
        self.growing_capture = None;
        self.capture_path = None;
        self.capture_info = None;
        self.channel_names.clear();
        self.row_rename = None;
        self.sampler = None;
        self.sampled_key = None;
        self.sampling_overlay_channels = None;
        self.sampling_overlay_key = None;
        self.worker_responses = None;
        self.index_progress = None;
        self.cursors.clear();
        self.drag_cursor = None;
        self.hover_measurement = None;
        self.set_channels(signals);
        self.visible_start_us = 0.0;
        self.visible_span_us = duration_us.max(1.0);
        self.fit_to_capture = true;
        self.status = "In-memory capture ready".to_owned();
    }

    /// `open` constructs the capture-specific [`CaptureDataSource`] for
    /// `path` — the viewer only knows the generic trait, not which concrete
    /// source (file format, live capture, …) produced it.
    pub fn set_capture_path<S: CaptureDataSource>(
        &mut self,
        path: impl AsRef<Path>,
        open: impl FnOnce(&Path) -> Result<S, String>,
    ) {
        let path = path.as_ref();
        if path.as_os_str().is_empty() {
            return;
        }

        if self.capture_path.as_deref() == Some(path) {
            return;
        }

        let path = path.to_path_buf();
        self.growing_capture = None;
        let data_source = match open(&path) {
            Ok(data_source) => data_source,
            Err(err) => {
                self.capture_path = Some(path.clone());
                self.capture_info = None;
                // Stale channel rows drop out of `row_order` on the next
                // `ensure_row_order` pass; any derived-lane rows from an
                // active run are left exactly where they are.
                self.channels.clear();
                self.row_drag = None;
                self.channel_names.clear();
                self.row_rename = None;
                self.sampler = None;
                self.sampled_key = None;
                self.sampling_overlay_channels = None;
                self.sampling_overlay_key = None;
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
        self.row_drag = None;
        self.channel_names.clear();
        self.row_rename = None;
        self.sampler = None;
        self.sampled_key = None;
        self.sampling_overlay_channels = None;
        self.sampling_overlay_key = None;
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

    /// Attaches a deferred, format-neutral indexed capture source.
    pub fn set_capture_factory(
        &mut self,
        identity: impl Into<PathBuf>,
        factory: Box<dyn CaptureIndexFactory>,
    ) {
        let identity = identity.into();
        if self.capture_path.as_ref() == Some(&identity) {
            return;
        }
        let display_name = factory.display_name();
        self.clear_capture();
        self.capture_path = Some(identity.clone());
        self.fit_to_capture = true;
        self.status = format!("Opening {display_name}");
        let (response_tx, response_rx) = mpsc::channel();
        crate::worker::spawn_capture_factory_worker(identity, factory, response_tx);
        self.worker_responses = Some(response_rx);
    }

    /// Clear a capture when no file-backed source remains in the graph.
    pub fn clear_capture(&mut self) {
        self.growing_capture = None;
        self.capture_path = None;
        self.capture_info = None;
        self.channels.clear();
        self.channel_names.clear();
        self.row_rename = None;
        self.sampler = None;
        self.sampled_key = None;
        self.sampling_overlay_channels = None;
        self.sampling_overlay_key = None;
        self.worker_responses = None;
        self.index_progress = None;
        self.cursors.clear();
        self.hover_measurement = None;
        self.status = "No capture loaded".to_string();
    }

    /// Attaches a provider-neutral growing waveform query. Acquisition and
    /// index construction remain owned by the host; this widget only follows
    /// published generations and samples visible windows.
    pub fn set_growing_capture(&mut self, sampler: Box<dyn CaptureIndex>) {
        self.set_growing_capture_with_planned_span(sampler, None);
    }

    /// Attaches a growing waveform while preserving a host-provided capture
    /// window. The planned span is generic presentation metadata and may be
    /// longer than the samples committed so far. The initial visible span is
    /// capped to elapsed capture time so the time axis never starts below
    /// zero.
    pub fn set_growing_capture_with_planned_span(
        &mut self,
        sampler: Box<dyn CaptureIndex>,
        planned_span_us: Option<f64>,
    ) {
        let metadata = sampler.current_metadata();
        let generation = sampler.generation();
        let complete = sampler.is_complete();
        self.capture_path = None;
        self.capture_info = Some(CaptureInfo {
            duration_us: metadata.duration_us(),
            header: metadata.clone(),
        });
        self.channels = crate::channel::placeholder_channels(&metadata);
        self.channel_names.clear();
        self.row_rename = None;
        self.sampler = Some(sampler);
        self.sampled_key = None;
        self.sampling_overlay_channels = None;
        self.sampling_overlay_key = None;
        self.worker_responses = None;
        self.index_progress = None;
        self.cursors.clear();
        self.drag_cursor = None;
        self.hover_measurement = None;
        let duration_us = metadata.duration_us();
        let planned_span_us = planned_span_us.filter(|span| span.is_finite() && *span > 0.0);
        self.visible_span_us = planned_span_us
            .unwrap_or(DEFAULT_VISIBLE_SPAN_US)
            .min(duration_us.max(1.0));
        self.visible_start_us = (duration_us - self.visible_span_us).max(0.0);
        self.fit_to_capture = false;
        self.growing_capture = Some(GrowingCaptureView {
            generation,
            paused: false,
            follow_newest: true,
            complete,
            planned_span_us,
        });
        self.status = if complete {
            "Captured waveform ready".into()
        } else {
            "Following live capture".into()
        };
        self.ensure_row_order();
    }

    pub fn has_growing_capture(&self) -> bool {
        self.growing_capture.is_some()
    }

    pub fn growing_capture_complete(&self) -> bool {
        self.growing_capture
            .as_ref()
            .is_none_or(|capture| capture.complete)
    }

    pub fn display_paused(&self) -> bool {
        self.growing_capture
            .as_ref()
            .is_some_and(|capture| capture.paused)
    }

    pub fn follows_newest(&self) -> bool {
        self.growing_capture
            .as_ref()
            .is_some_and(|capture| capture.follow_newest)
    }

    pub fn set_follow_newest(&mut self, follow: bool) {
        if let Some(capture) = &mut self.growing_capture {
            capture.follow_newest = follow;
            if follow {
                capture.paused = false;
                capture.generation = capture.generation.wrapping_sub(1);
            }
        }
    }

    pub fn toggle_pause_display(&mut self) {
        if let Some(capture) = &mut self.growing_capture {
            capture.paused = !capture.paused;
            if !capture.paused {
                capture.generation = capture.generation.wrapping_sub(1);
            }
        }
    }

    pub fn go_live(&mut self) {
        self.set_follow_newest(true);
    }

    pub(crate) fn leave_live_edge(&mut self) {
        if let Some(capture) = &mut self.growing_capture {
            capture.follow_newest = false;
        }
    }

    fn refresh_growing_capture(&mut self) {
        let Some((paused, follow_newest, known_generation)) = self
            .growing_capture
            .as_ref()
            .map(|view| (view.paused, view.follow_newest, view.generation))
        else {
            return;
        };
        let Some(sampler) = self.sampler.as_ref() else {
            return;
        };
        let generation = sampler.generation();
        let complete = sampler.is_complete();
        if let Some(view) = &mut self.growing_capture {
            view.complete = complete;
        }
        if paused || generation == known_generation {
            return;
        }

        let metadata = sampler.current_metadata();
        if metadata.total_samples == 0 {
            return;
        }
        let duration_us = metadata.duration_us();
        if let Some(capture) = &mut self.capture_info {
            capture.header = metadata;
            capture.duration_us = duration_us;
        }
        if follow_newest {
            self.visible_span_us = self.visible_span_us.min(duration_us.max(1.0));
            self.visible_start_us = (duration_us - self.visible_span_us).max(0.0);
        } else {
            self.clamp_to_capture_duration();
        }
        if let Some(view) = &mut self.growing_capture {
            view.generation = generation;
        }
        self.sampled_key = None;
        self.status = if complete {
            "Captured waveform ready".into()
        } else if follow_newest {
            "Following live capture".into()
        } else {
            "Live capture available".into()
        };
    }

    /// One-line hint of available controls, for a status bar (Phase 4.1).
    pub fn status_hint(&self) -> &'static str {
        "Drag Pan · Scroll Zoom · Double-click ruler to add a cursor · Home Fit"
    }

    pub fn show(&mut self, ui: &mut Ui) {
        let rect = ui.available_rect_before_wrap();
        let response = ui.allocate_rect(rect, Sense::click_and_drag());
        let painter = ui.painter_at(rect);

        self.process_worker_responses();
        self.refresh_growing_capture();
        // Reconciles `row_order` against the current channels and derived
        // lanes (drops stale rows, appends new ones) before anything this
        // frame does row-position math, so hit-testing, drag, and layout
        // all see the same order.
        self.ensure_row_order();
        let mut layout = self.layout(ui, rect);
        self.hovered_input_context = response
            .hover_pos()
            .map(|pointer| {
                if layout.ruler_rect.contains(pointer) {
                    "logic_analyzer.ruler"
                } else if layout.labels_rect.contains(pointer) {
                    "logic_analyzer.channel"
                } else {
                    "logic_analyzer"
                }
            })
            .unwrap_or("logic_analyzer");
        let trigger_input = self.handle_simple_trigger_input(ui, &response, layout);
        let row_rename_started =
            !trigger_input && self.handle_row_label_input(ui, &response, layout);
        let row_dragging = !trigger_input && self.handle_row_reorder(ui, &response, layout);
        let cursor_input = self.handle_cursor_input(ui, &response, layout);
        if cursor_input.active.is_some() {
            self.hovered_input_context = "logic_analyzer.cursor";
        }
        let home_pressed = response.hovered()
            && ui.ctx().memory(|memory| memory.focused().is_none())
            && self
                .input_bindings
                .consume_shortcut_ctx(ui.ctx(), &["logic_analyzer"], "fit");
        if home_pressed {
            self.reset_time_view();
        } else if (self
            .input_bindings
            .pointer_button(&["logic_analyzer"], "fit_pointer")
            .is_some_and(|button| response.double_clicked_by(button))
            && !cursor_input.ruler_double_click
            && !row_rename_started)
            || (response.hovered()
                && self.input_bindings.consume_shortcut_ctx(
                    ui.ctx(),
                    &["logic_analyzer"],
                    "fit_alternate",
                ))
        {
            self.fit_capture();
        }
        self.handle_input(
            ui,
            layout,
            response.hovered(),
            self.input_bindings
                .pointer_button(&["logic_analyzer"], "pan")
                .is_some_and(|button| response.dragged_by(button))
                && !cursor_input.blocks_pan
                && !row_dragging,
        );
        self.sample_visible_window(layout);
        self.sample_sampling_overlay();
        layout = self.layout(ui, rect);
        self.sample_indexed_annotations(layout);
        let hover_pointer = if cursor_input.blocks_pan {
            None
        } else {
            response.hover_pos()
        };
        self.sample_hover_measurement(layout, hover_pointer);
        self.draw(&painter, layout, hover_pointer, cursor_input.active);
        self.show_simple_trigger_popup(ui.ctx());
        self.show_row_rename(ui.ctx());
        if self.has_live_indexed_annotations() {
            ui.ctx()
                .request_repaint_after(std::time::Duration::from_millis(50));
        }
        if self
            .growing_capture
            .as_ref()
            .is_some_and(|capture| !capture.complete)
        {
            ui.ctx()
                .request_repaint_after(std::time::Duration::from_millis(8));
        }
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

    fn layout(&self, ui: &Ui, rect: Rect) -> AnalyzerLayout {
        let ruler_height = 34.0;
        let row_height = 30.0;
        let label_pad = 12.0;
        let trigger_width = if self.simple_trigger_lanes.is_empty() {
            0.0
        } else {
            26.0
        };
        let name_badge_gap = 10.0;
        let label_right_pad = 10.0;
        let name_font = FontId::proportional(12.0);
        let badge_font = FontId::monospace(10.0);
        // Measured from the same `row_label` the draw pass uses, so a
        // derived lane's text column is exactly as wide as what's actually
        // drawn in it — text and badge share one label layout regardless of
        // row kind (§ below).
        let labels: Vec<_> = self
            .row_order
            .iter()
            .filter_map(|key| self.row_label(key))
            .collect();
        let (name_col_width, badge_width) = ui.ctx().fonts_mut(|fonts| {
            let name_col_width = labels
                .iter()
                .map(|label| {
                    fonts
                        .layout_no_wrap(label.name.clone(), name_font.clone(), egui::Color32::WHITE)
                        .size()
                        .x
                })
                .fold(0.0, f32::max);
            let badge_width = labels
                .iter()
                .map(|label| {
                    fonts
                        .layout_no_wrap(
                            label.badge_text.clone(),
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
        let desired_left_width = label_pad
            + trigger_width
            + name_col_width
            + name_badge_gap
            + badge_width
            + label_right_pad;
        let left_width = desired_left_width.max(72.0).min(rect.width().max(0.0));

        let ruler_rect = Rect::from_min_max(
            Pos2::new(rect.left() + left_width, rect.top()),
            Pos2::new(rect.right(), rect.top() + ruler_height),
        );
        let labels_rect = Rect::from_min_max(
            Pos2::new(rect.left(), rect.top() + ruler_height),
            Pos2::new(rect.left() + left_width, rect.bottom()),
        );
        let wave_rect = Rect::from_min_max(
            Pos2::new(rect.left() + left_width, rect.top() + ruler_height),
            rect.max,
        );

        AnalyzerLayout {
            ruler_rect,
            labels_rect,
            wave_rect,
            row_height,
            trigger_width,
            name_col_width,
            badge_width,
        }
    }

    pub(crate) fn fit_capture(&mut self) {
        self.leave_live_edge();
        if let Some(capture) = self.capture_info.as_ref() {
            self.visible_start_us = 0.0;
            self.visible_span_us = capture.duration_us.max(1.0);
            self.fit_to_capture = true;
        }
    }

    /// Returns the time viewport to its origin and fits the complete
    /// recording. Uses capture metadata when available, otherwise the latest
    /// timestamp in loaded channel transitions or derived lanes.
    fn reset_time_view(&mut self) {
        self.leave_live_edge();
        self.visible_start_us = 0.0;
        if let Some(capture) = self.capture_info.as_ref() {
            self.visible_span_us = capture.duration_us.max(1.0);
            self.fit_to_capture = true;
        } else {
            self.visible_span_us = self
                .channels
                .iter()
                .filter_map(|channel| channel.transitions.last())
                .map(|transition| transition.time_us)
                .chain(std::iter::once(self.derived_timeline_end_us()))
                .fold(0.0_f64, f64::max)
                .max(1.0);
            self.fit_to_capture = false;
        }
    }

    fn derived_timeline_end_us(&self) -> f64 {
        let Some(derived) = self.derived.as_ref() else {
            return 0.0;
        };
        let lanes = derived.read();
        let end_ns = lanes
            .iter()
            .filter_map(|lane| match &lane.data {
                signal_processing::DerivedLaneData::Digital(samples) => {
                    samples.last().map(|sample| sample.start_time_ns)
                }
                signal_processing::DerivedLaneData::Annotations(annotations) => annotations
                    .iter()
                    .map(|annotation| annotation.end_ns.max(annotation.start_ns))
                    .max(),
                signal_processing::DerivedLaneData::IndexedAnnotations(indexed) => {
                    indexed.metadata().extent_end_ns
                }
                signal_processing::DerivedLaneData::Markers(markers) => markers.last().copied(),
                signal_processing::DerivedLaneData::Values(values) => {
                    values.values.last().map(|value| value.start_time_ns)
                }
            })
            .max()
            .unwrap_or(0);
        end_ns as f64 / 1_000.0
    }

    fn has_live_indexed_annotations(&self) -> bool {
        self.derived.as_ref().is_some_and(|derived| {
            derived.read().iter().any(|lane| {
                matches!(
                    &lane.data,
                    signal_processing::DerivedLaneData::IndexedAnnotations(indexed)
                        if indexed.status()
                            == signal_processing::StoreStatus::Live
                )
            })
        })
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

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU64, Ordering};

    use signal_processing::{
        CaptureIndex, CaptureMetadata, CaptureSampledChannel, CaptureSampledWindow,
        DerivedLaneData, DerivedLanes, IndexedAnnotationLane, IndexedAnnotationWriter,
        LiveStoreConfig, Word,
    };

    use super::{ChannelSignal, LogicAnalyzerViewer};
    use crate::{SamplingEdge, SamplingOverlay};

    struct GrowingTestIndex {
        header: CaptureMetadata,
        total_samples: Arc<AtomicU64>,
        generation: Arc<AtomicU64>,
        path: PathBuf,
    }

    impl CaptureIndex for GrowingTestIndex {
        fn display_name(&self) -> String {
            "Growing test".into()
        }

        fn index_path(&self) -> &Path {
            &self.path
        }

        fn header(&self) -> &CaptureMetadata {
            &self.header
        }

        fn current_metadata(&self) -> CaptureMetadata {
            let mut metadata = self.header.clone();
            metadata.total_samples = self.total_samples.load(Ordering::Relaxed);
            metadata
        }

        fn generation(&self) -> u64 {
            self.generation.load(Ordering::Relaxed)
        }

        fn is_complete(&self) -> bool {
            false
        }

        fn capture_duration_us(&self) -> f64 {
            self.current_metadata().duration_us()
        }

        fn sampled_window(
            &mut self,
            channels: &[usize],
            start_sample: u64,
            end_sample: u64,
            _target_points: usize,
        ) -> signal_processing::Result<CaptureSampledWindow> {
            Ok(CaptureSampledWindow {
                start_sample,
                end_sample,
                sample_step: 1,
                channels: channels
                    .iter()
                    .map(|&channel| CaptureSampledChannel {
                        channel,
                        name: channel.to_string(),
                        initial: false,
                        transitions: Vec::new(),
                        waveform: Vec::new(),
                    })
                    .collect(),
            })
        }
    }

    fn growing_test_index(
        total_samples: Arc<AtomicU64>,
        generation: Arc<AtomicU64>,
    ) -> GrowingTestIndex {
        GrowingTestIndex {
            header: CaptureMetadata {
                total_probes: 1,
                samplerate: "1 MHz".into(),
                samplerate_hz: 1_000_000.0,
                sample_period: 0.000_001,
                total_samples: 0,
                total_blocks: 0,
                samples_per_block: 64,
                probe_names: vec!["D0".into()],
                trigger_sample: None,
            },
            total_samples,
            generation,
            path: PathBuf::from("growing-test"),
        }
    }

    #[test]
    fn reset_time_view_fits_in_memory_channels_without_a_capture() {
        let mut viewer = LogicAnalyzerViewer::new();
        viewer.set_channels(vec![ChannelSignal {
            index: 0,
            name: "D0".to_owned(),
            initial: false,
            transitions: vec![(20.0, true), (240.0, false)],
        }]);
        viewer.visible_start_us = 120.0;
        viewer.visible_span_us = 12.0;
        viewer.reset_time_view();

        assert_eq!(viewer.visible_start_us, 0.0);
        assert_eq!(viewer.visible_span_us, 240.0);
    }

    #[test]
    fn reset_time_view_fits_indexed_annotations_without_a_capture() {
        let (mut writer, store) =
            IndexedAnnotationWriter::create(LiveStoreConfig::default()).unwrap();
        writer
            .append_batch(&[Word::spanning(0x27, 250_000, 50_000)])
            .unwrap();
        writer.finish().unwrap();
        let lanes = DerivedLanes::new();
        lanes.register(
            "words",
            DerivedLaneData::IndexedAnnotations(IndexedAnnotationLane::from_store(store)),
        );
        let mut viewer = LogicAnalyzerViewer::new();
        viewer.set_derived_lanes(lanes);
        viewer.visible_start_us = 120.0;
        viewer.visible_span_us = 12.0;

        viewer.reset_time_view();

        assert_eq!(viewer.visible_start_us, 0.0);
        assert_eq!(viewer.visible_span_us, 300.0);
    }

    #[test]
    fn pause_display_freezes_the_view_and_go_live_catches_up() {
        let total_samples = Arc::new(AtomicU64::new(1_000));
        let generation = Arc::new(AtomicU64::new(1));
        let mut viewer = LogicAnalyzerViewer::new();
        viewer.set_growing_capture(Box::new(growing_test_index(
            Arc::clone(&total_samples),
            Arc::clone(&generation),
        )));

        assert!(viewer.follows_newest());
        assert_eq!(viewer.visible_span_us, 900.0);
        assert_eq!(viewer.visible_start_us, 100.0);

        total_samples.store(2_000, Ordering::Relaxed);
        generation.store(2, Ordering::Relaxed);
        viewer.refresh_growing_capture();
        assert_eq!(
            viewer.capture_info.as_ref().unwrap().header.total_samples,
            2_000
        );
        assert_eq!(viewer.visible_start_us, 1_100.0);

        viewer.toggle_pause_display();
        total_samples.store(3_000, Ordering::Relaxed);
        generation.store(3, Ordering::Relaxed);
        viewer.refresh_growing_capture();
        assert!(viewer.display_paused());
        assert_eq!(
            viewer.capture_info.as_ref().unwrap().header.total_samples,
            2_000
        );
        assert_eq!(viewer.visible_start_us, 1_100.0);

        viewer.go_live();
        viewer.refresh_growing_capture();
        assert!(!viewer.display_paused());
        assert!(viewer.follows_newest());
        assert_eq!(
            viewer.capture_info.as_ref().unwrap().header.total_samples,
            3_000
        );
        assert_eq!(viewer.visible_start_us, 2_100.0);

        viewer.leave_live_edge();
        assert!(!viewer.follows_newest());
    }

    #[test]
    fn planned_live_span_never_places_the_view_before_capture_start() {
        let total_samples = Arc::new(AtomicU64::new(10_000));
        let generation = Arc::new(AtomicU64::new(1));
        let mut viewer = LogicAnalyzerViewer::new();
        viewer.set_growing_capture_with_planned_span(
            Box::new(growing_test_index(
                Arc::clone(&total_samples),
                Arc::clone(&generation),
            )),
            Some(1_000_000.0),
        );

        assert_eq!(viewer.visible_span_us, 10_000.0);
        assert_eq!(viewer.visible_start_us, 0.0);

        total_samples.store(20_000, Ordering::Relaxed);
        generation.store(2, Ordering::Relaxed);
        viewer.refresh_growing_capture();

        assert_eq!(viewer.visible_span_us, 10_000.0);
        assert_eq!(viewer.visible_start_us, 10_000.0);
    }

    #[test]
    fn sampling_overlay_uses_an_exact_cached_window_up_to_one_hundred_ms() {
        let total_samples = Arc::new(AtomicU64::new(200_000));
        let generation = Arc::new(AtomicU64::new(1));
        let mut viewer = LogicAnalyzerViewer::new();
        viewer.set_growing_capture(Box::new(growing_test_index(
            Arc::clone(&total_samples),
            Arc::clone(&generation),
        )));
        viewer.set_sampling_overlay(Some(SamplingOverlay {
            clock_channel: 0,
            sampled_channels: vec![0],
            edge: SamplingEdge::Rising,
            qualifiers: Vec::new(),
            activities: Vec::new(),
        }));
        viewer.visible_start_us = 0.0;
        viewer.visible_span_us = 100_000.0;

        viewer.sample_sampling_overlay();

        assert!(viewer.sampling_overlay_channels.is_some());
        assert_eq!(viewer.sampling_overlay_key, Some((0, 100_000, 1)));

        viewer.visible_span_us = 100_001.0;
        viewer.sample_sampling_overlay();
        assert!(viewer.sampling_overlay_channels.is_none());
        assert!(viewer.sampling_overlay_key.is_none());
    }
}
