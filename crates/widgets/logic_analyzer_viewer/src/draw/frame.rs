use std::collections::{HashMap, HashSet};

use egui::{Align2, Color32, FontId, Painter, Pos2, Rect, Shape, Stroke, StrokeKind, vec2};

use signal_processing::{CollectedLaneSnapshotRequest, DerivedLaneData, LaneSummary};

use super::derived::{DerivedRowGeometry, default_annotation_visual, visible_annotation_range};
use crate::cursor::{cursor_color, cursor_flag_geometry, cursor_flag_label};
use crate::format::{badge_text_color, format_time, nice_step};
use crate::indexed_annotations::IndexedAnnotationSamples;
use crate::lanes::{AnnotationVisual, OpaqueLaneDrawContext, ViewerLaneGroup, ViewerLaneTrackId};
use crate::types::{AnalyzerLayout, RowKey};
use crate::viewer::LogicAnalyzerViewer;

impl LogicAnalyzerViewer {
    pub(crate) fn draw(
        &self,
        painter: &Painter,
        layout: AnalyzerLayout,
        pointer: Option<Pos2>,
        active_cursor: Option<usize>,
    ) {
        let rect = Rect::from_min_max(
            Pos2::new(layout.labels_rect.left(), layout.ruler_rect.top()),
            layout.wave_rect.right_bottom(),
        );
        if rect.width() <= 1.0 || rect.height() <= 1.0 {
            return;
        }

        let background = Color32::from_rgb(22, 22, 22);
        let grid = Color32::from_rgb(52, 52, 52);
        let grid_minor = Color32::from_rgb(38, 38, 38);
        let text = Color32::from_rgb(205, 205, 205);
        let muted = Color32::from_rgb(135, 135, 135);

        painter.rect_filled(rect, 0.0, background);

        let ruler_rect = layout.ruler_rect;
        let labels_rect = layout.labels_rect;
        let wave_rect = layout.wave_rect;
        let row_height = layout.row_height;

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
        self.draw_rows(
            painter,
            labels_rect,
            wave_rect,
            row_height,
            layout.trigger_width,
            layout.name_col_width,
            layout.badge_width,
            text,
            trace,
            grid,
        );
        self.draw_sampling_overlay(painter, layout);
        self.draw_capture_trigger(painter, layout);

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

    fn draw_capture_trigger(&self, painter: &Painter, layout: AnalyzerLayout) {
        let Some(x) = self.capture_trigger_x(layout.wave_rect) else {
            return;
        };
        let color = Color32::from_rgb(238, 72, 72);
        painter.line_segment(
            [
                Pos2::new(x, layout.ruler_rect.top()),
                Pos2::new(x, layout.wave_rect.bottom()),
            ],
            Stroke::new(1.5, color),
        );
        painter.add(Shape::convex_polygon(
            vec![
                Pos2::new(x - 6.0, layout.ruler_rect.top()),
                Pos2::new(x + 6.0, layout.ruler_rect.top()),
                Pos2::new(x, layout.ruler_rect.top() + 8.0),
            ],
            color,
            Stroke::NONE,
        ));
    }

    fn capture_trigger_x(&self, wave_rect: Rect) -> Option<f32> {
        let capture = self.capture_info.as_ref()?;
        let sample = capture.header.trigger_sample?;
        let time_us = sample as f64 * 1_000_000.0 / capture.header.samplerate_hz;
        if !(self.visible_start_us..=self.visible_start_us + self.visible_span_us)
            .contains(&time_us)
        {
            return None;
        }
        Some(self.time_to_x_unclamped(wave_rect, time_us))
    }

    /// Draws every row in `row_order` — a channel or a derived lane, freely
    /// interleaved. The label (name text, then the colored badge) is drawn
    /// identically either way, from `row_label`; only the waveform content
    /// differs by kind (level trace, decoded-word boxes, trigger markers).
    #[allow(clippy::too_many_arguments)]
    fn draw_rows(
        &self,
        painter: &Painter,
        labels_rect: Rect,
        wave_rect: Rect,
        row_height: f32,
        trigger_width: f32,
        name_col_width: f32,
        badge_width: f32,
        text: Color32,
        trace: Color32,
        grid: Color32,
    ) {
        let clip = painter.with_clip_rect(wave_rect);

        let mut y_top = labels_rect.top();
        for key in &self.row_order {
            let display_height = self.display_row_height(key, row_height);
            if y_top > labels_rect.bottom() {
                break;
            }
            let row_rect = Rect::from_min_max(
                Pos2::new(labels_rect.left(), y_top),
                Pos2::new(wave_rect.right(), y_top + display_height),
            );
            painter.line_segment(
                [
                    Pos2::new(labels_rect.left(), row_rect.bottom()),
                    Pos2::new(wave_rect.right(), row_rect.bottom()),
                ],
                Stroke::new(1.0, Color32::from_rgb(42, 42, 42)),
            );

            let Some(label) = self.row_label(key) else {
                y_top += display_height;
                continue;
            };
            if trigger_width > 0.0 {
                let trigger_rect = Rect::from_center_size(
                    Pos2::new(
                        labels_rect.left() + 12.0 + trigger_width * 0.5 - 2.0,
                        row_rect.center().y,
                    ),
                    vec2(20.0, 20.0),
                );
                self.draw_simple_trigger_icon(painter, key, trigger_rect);
            }
            painter.text(
                Pos2::new(
                    labels_rect.left() + 12.0 + trigger_width,
                    row_rect.center().y,
                ),
                Align2::LEFT_CENTER,
                &label.name,
                FontId::proportional(12.0),
                text,
            );
            let badge_rect = Rect::from_min_size(
                Pos2::new(
                    labels_rect.left() + 12.0 + trigger_width + name_col_width + 10.0,
                    row_rect.center().y - 8.0,
                ),
                vec2(badge_width, 16.0),
            );
            painter.rect_filled(badge_rect, 2.0, label.badge_color);
            painter.text(
                badge_rect.center(),
                Align2::CENTER_CENTER,
                &label.badge_text,
                FontId::monospace(10.0),
                badge_text_color(label.badge_color),
            );

            match key {
                RowKey::Channel(index) => {
                    let Some(channel) = self.channels.iter().find(|c| c.index == *index) else {
                        continue;
                    };
                    let center_y = row_rect.center().y;
                    clip.line_segment(
                        [
                            Pos2::new(wave_rect.left(), center_y),
                            Pos2::new(wave_rect.right(), center_y),
                        ],
                        Stroke::new(1.0, grid),
                    );
                    self.draw_channel_waveform(
                        &clip,
                        wave_rect,
                        y_top,
                        display_height,
                        channel,
                        trace,
                    );
                }
                RowKey::Derived(group_id) => {
                    let Some(store) = &self.derived else {
                        continue;
                    };
                    let Some(group) = self
                        .waveform_presentations
                        .read()
                        .iter()
                        .find(|group| &group.id == group_id)
                        .cloned()
                    else {
                        continue;
                    };
                    let uses_legacy_fallback = group
                        .tracks
                        .iter()
                        .any(|track| !group.renderer.uses_opaque_snapshot(track));
                    let annotation_visuals = if uses_legacy_fallback {
                        self.prepare_builtin_annotation_visuals(&group, wave_rect)
                    } else {
                        HashMap::new()
                    };
                    let empty_visuals = HashMap::new();
                    let opaque_lanes = store.opaque_lanes();
                    let (visible_start_ns, visible_end_ns) = self.visible_window_ns();
                    for (track, track_top, track_height) in group.track_rects(y_top, display_height)
                    {
                        if group.renderer.uses_opaque_snapshot(&track)
                            && let Some(query) = opaque_lanes
                                .iter()
                                .find(|lane| lane.name() == track.lane.as_str())
                        {
                            let snapshot = query.snapshot(CollectedLaneSnapshotRequest {
                                start_time_ns: visible_start_ns,
                                end_time_ns: visible_end_ns,
                                max_items: (wave_rect.width().max(1.0) as usize)
                                    .saturating_mul(2)
                                    .max(32),
                            });
                            if group.renderer.draw_opaque_lane(
                                &track,
                                snapshot.as_ref(),
                                OpaqueLaneDrawContext {
                                    painter: &clip,
                                    wave_rect,
                                    top: track_top,
                                    height: track_height,
                                    visible_start_ns,
                                    visible_end_ns,
                                },
                            ) {
                                continue;
                            }
                        }
                        let lanes = store.read();
                        let Some(lane) = lanes.iter().find(|lane| lane.name == track.lane.as_str())
                        else {
                            continue;
                        };
                        let visuals = annotation_visuals.get(&track.id).unwrap_or(&empty_visuals);
                        match &lane.data {
                            DerivedLaneData::Digital(samples) => {
                                let center_y = track_top + track_height * 0.5;
                                clip.line_segment(
                                    [
                                        Pos2::new(wave_rect.left(), center_y),
                                        Pos2::new(wave_rect.right(), center_y),
                                    ],
                                    Stroke::new(1.0, grid),
                                );
                                self.draw_derived_digital(
                                    &clip,
                                    wave_rect,
                                    track_top,
                                    track_height,
                                    samples,
                                );
                            }
                            DerivedLaneData::Annotations(annotations) => {
                                let LaneSummary::Annotations(summary) = &lane.summary else {
                                    continue;
                                };
                                self.draw_derived_annotations(
                                    &clip,
                                    wave_rect,
                                    DerivedRowGeometry {
                                        top: track_top,
                                        height: track_height,
                                    },
                                    annotations,
                                    summary,
                                    visuals,
                                );
                            }
                            DerivedLaneData::IndexedAnnotations(_) => {
                                let Some(cached) = self.indexed_annotation_cache.get(&lane.name)
                                else {
                                    continue;
                                };
                                match cached.samples() {
                                    IndexedAnnotationSamples::Exact {
                                        annotations,
                                        last_timestamp_ns,
                                    } => self.draw_indexed_annotation_exact(
                                        &clip,
                                        wave_rect,
                                        DerivedRowGeometry {
                                            top: track_top,
                                            height: track_height,
                                        },
                                        annotations,
                                        *last_timestamp_ns,
                                        visuals,
                                    ),
                                    IndexedAnnotationSamples::Presence(buckets) => self
                                        .draw_indexed_annotation_presence(
                                            &clip,
                                            wave_rect,
                                            track_top,
                                            track_height,
                                            buckets,
                                        ),
                                    IndexedAnnotationSamples::Error => {}
                                }
                            }
                            DerivedLaneData::Markers(markers) => {
                                self.draw_derived_markers(
                                    &clip,
                                    wave_rect,
                                    track_top,
                                    track_height,
                                    markers,
                                );
                            }
                            DerivedLaneData::Values(values) => {
                                let color = match values.kind {
                                    signal_processing::CollectedValueKind::Number => {
                                        Color32::from_rgb(95, 145, 210)
                                    }
                                    signal_processing::CollectedValueKind::Text => {
                                        Color32::from_rgb(215, 150, 170)
                                    }
                                };
                                self.draw_derived_values(
                                    &clip,
                                    wave_rect,
                                    track_top,
                                    track_height,
                                    &values.values,
                                    color,
                                );
                            }
                        }
                    }
                }
            }
            y_top += display_height;
        }
    }

    /// Prepares visual overrides for the legacy built-in annotation fallback.
    /// Plugin-owned rows receive their own opaque snapshots directly through
    /// [`ViewerLaneRenderer::draw_opaque_lane`].
    fn prepare_builtin_annotation_visuals(
        &self,
        group: &ViewerLaneGroup,
        wave_rect: Rect,
    ) -> HashMap<ViewerLaneTrackId, HashMap<u64, AnnotationVisual>> {
        let exact_limit = (wave_rect.width().max(1.0) as usize)
            .saturating_mul(2)
            .max(32);
        let (start_ns, end_ns) = self.visible_window_ns();
        let mut annotation_values = Vec::<(ViewerLaneTrackId, Vec<u64>, Option<String>)>::new();

        if let Some(store) = &self.derived {
            let lanes = store.read();
            for track in &group.tracks {
                let Some(lane) = lanes.iter().find(|lane| lane.name == track.lane.as_str()) else {
                    continue;
                };
                let values = match &lane.data {
                    DerivedLaneData::Annotations(annotations) => {
                        let (first, last) = visible_annotation_range(annotations, start_ns, end_ns);
                        let visible = &annotations[first..last];
                        if visible.len() > exact_limit {
                            continue;
                        } else {
                            visible.iter().map(|annotation| annotation.value).collect()
                        }
                    }
                    DerivedLaneData::IndexedAnnotations(_) => {
                        let Some(cached) = self.indexed_annotation_cache.get(&lane.name) else {
                            continue;
                        };
                        match cached.samples() {
                            IndexedAnnotationSamples::Exact { annotations, .. } => annotations
                                .iter()
                                .map(|annotation| annotation.value)
                                .collect(),
                            IndexedAnnotationSamples::Presence(_)
                            | IndexedAnnotationSamples::Error => continue,
                        }
                    }
                    DerivedLaneData::Digital(_)
                    | DerivedLaneData::Markers(_)
                    | DerivedLaneData::Values(_) => continue,
                };
                annotation_values.push((
                    track.id.clone(),
                    values,
                    lane.word_display_format.clone(),
                ));
            }
        }

        let mut visuals = HashMap::new();
        for (track, values, format) in annotation_values {
            let format = format.as_deref();
            let mut seen = HashSet::new();
            let track_visuals = values
                .iter()
                .copied()
                .filter(|value| seen.insert(*value))
                .map(|value| {
                    let default = default_annotation_visual(value, format);
                    let visual = group.renderer.annotation_visual(&track, value, default);
                    (value, visual)
                })
                .collect();
            visuals.insert(track, track_visuals);
        }
        visuals
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
            let galley =
                painter.layout_no_wrap(label, egui::FontId::proportional(10.0), Color32::BLACK);
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
                    egui::FontId::proportional(10.0),
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

    pub(crate) fn visible_window_ns(&self) -> (u64, u64) {
        let start_ns = (self.visible_start_us.max(0.0) * 1_000.0) as u64;
        let end_ns =
            ((self.visible_start_us + self.visible_span_us).max(0.0) * 1_000.0).ceil() as u64;
        (start_ns, end_ns)
    }

    pub(crate) fn ns_to_x(&self, rect: Rect, ns: u64) -> f32 {
        self.time_to_x(rect, ns as f64 / 1_000.0)
    }

    pub(crate) fn time_to_x(&self, rect: Rect, time_us: f64) -> f32 {
        let t = ((time_us - self.visible_start_us) / self.visible_span_us).clamp(0.0, 1.0);
        rect.left() + rect.width() * t as f32
    }

    /// Like [`Self::time_to_x`] but without pinning off-screen times to the
    /// viewport edge, so callers can cull (cursors) instead of drawing a
    /// misleading edge line.
    pub(crate) fn time_to_x_unclamped(&self, rect: Rect, time_us: f64) -> f32 {
        let t = (time_us - self.visible_start_us) / self.visible_span_us;
        rect.left() + rect.width() * t as f32
    }

    pub(crate) fn x_to_time(&self, rect: Rect, x: f32) -> f64 {
        let t = ((x - rect.left()) / rect.width()).clamp(0.0, 1.0) as f64;
        self.visible_start_us + self.visible_span_us * t
    }
}

#[cfg(test)]
mod frame_tests {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};

    use signal_processing::{
        Annotation, CaptureMetadata, CollectedLaneQuery, CollectedLaneSnapshotRequest,
        CollectedPayloadRegistry, DerivedLaneData, DerivedLanes, OpaqueCollectedLaneSnapshot,
    };

    use super::*;
    use crate::lanes::{
        DerivedLaneId, ViewerLaneBadge, ViewerLaneGroupId, ViewerLaneRenderer, ViewerLaneTrack,
    };
    use crate::types::CaptureInfo;

    struct ProbingRenderer {
        lanes: DerivedLanes,
        calls: AtomicUsize,
    }

    impl ViewerLaneRenderer for ProbingRenderer {
        fn annotation_visual(
            &self,
            _track: &ViewerLaneTrackId,
            _value: u64,
            default: AnnotationVisual,
        ) -> AnnotationVisual {
            // This takes the store's write lock. The call completes only if
            // frame preparation released its read lock before invoking us.
            self.lanes
                .register("renderer lock probe", DerivedLaneData::Markers(Vec::new()));
            self.calls.fetch_add(1, Ordering::Relaxed);
            default
        }
    }

    struct SnapshotQuery {
        requested: Mutex<Option<CollectedLaneSnapshotRequest>>,
    }

    impl CollectedLaneQuery for SnapshotQuery {
        fn into_any(self: Arc<Self>) -> Arc<dyn std::any::Any + Send + Sync> {
            self
        }

        fn snapshot(
            &self,
            request: CollectedLaneSnapshotRequest,
        ) -> Option<OpaqueCollectedLaneSnapshot> {
            *self.requested.lock().unwrap() = Some(request);
            Some(OpaqueCollectedLaneSnapshot::new(Arc::new(vec![7_u8, 9])))
        }
    }

    struct OpaqueSnapshotRenderer {
        lanes: DerivedLanes,
        values: Mutex<Vec<u8>>,
    }

    impl ViewerLaneRenderer for OpaqueSnapshotRenderer {
        fn uses_opaque_snapshot(&self, _track: &ViewerLaneTrack) -> bool {
            true
        }

        fn draw_opaque_lane(
            &self,
            _track: &ViewerLaneTrack,
            snapshot: Option<&OpaqueCollectedLaneSnapshot>,
            _context: OpaqueLaneDrawContext<'_>,
        ) -> bool {
            let values = snapshot
                .and_then(|snapshot| snapshot.value::<Vec<u8>>())
                .expect("opaque renderer receives its registered snapshot");
            self.values.lock().unwrap().extend(values.iter().copied());
            // Taking this write lock proves that generic rendering released
            // its retained-lane lock before calling plugin code.
            self.lanes.register(
                "opaque renderer lock probe",
                DerivedLaneData::Markers(Vec::new()),
            );
            true
        }
    }

    fn group(renderer: Arc<dyn ViewerLaneRenderer>) -> ViewerLaneGroup {
        ViewerLaneGroup {
            id: ViewerLaneGroupId::new("group"),
            label: "Group".to_owned(),
            badge: ViewerLaneBadge::new("W", Color32::WHITE),
            tracks: vec![ViewerLaneTrack::new(
                "words",
                DerivedLaneId::new("words"),
                1.0,
            )],
            renderer,
        }
    }

    #[test]
    fn capture_trigger_marker_uses_the_raw_sample_time_and_culls_offscreen() {
        let mut viewer = LogicAnalyzerViewer::new();
        viewer.capture_info = Some(CaptureInfo {
            duration_us: 100.0,
            header: CaptureMetadata {
                total_probes: 1,
                samplerate: "1 MHz".into(),
                samplerate_hz: 1_000_000.0,
                sample_period: 0.000_001,
                total_samples: 100,
                total_blocks: 1,
                samples_per_block: 100,
                probe_names: vec!["D0".into()],
                trigger_sample: Some(50),
            },
        });
        viewer.visible_start_us = 0.0;
        viewer.visible_span_us = 100.0;
        let wave_rect = Rect::from_min_max(Pos2::ZERO, Pos2::new(800.0, 100.0));
        assert_eq!(viewer.capture_trigger_x(wave_rect), Some(400.0));

        viewer.visible_start_us = 60.0;
        assert_eq!(viewer.capture_trigger_x(wave_rect), None);
    }

    #[test]
    fn sparse_frame_invokes_renderer_after_releasing_lane_lock() {
        let lanes = DerivedLanes::new();
        lanes.register(
            "words",
            DerivedLaneData::Annotations(vec![
                Annotation {
                    start_ns: 100,
                    end_ns: 200,
                    value: 1,
                },
                Annotation {
                    start_ns: 300,
                    end_ns: 400,
                    value: 2,
                },
            ]),
        );
        let renderer = Arc::new(ProbingRenderer {
            lanes: lanes.clone(),
            calls: AtomicUsize::new(0),
        });
        let mut viewer = LogicAnalyzerViewer::new();
        viewer.set_derived_lanes(lanes.clone());

        let visuals = viewer.prepare_builtin_annotation_visuals(
            &group(renderer.clone()),
            Rect::from_min_size(Pos2::ZERO, egui::vec2(100.0, 30.0)),
        );

        assert_eq!(renderer.calls.load(Ordering::Relaxed), 2);
        assert_eq!(visuals[&ViewerLaneTrackId::new("words")].len(), 2);
        assert!(
            lanes
                .read()
                .iter()
                .any(|lane| lane.name == "renderer lock probe")
        );
    }

    #[test]
    fn dense_frame_is_bounded_activity_and_skips_value_formatter() {
        let lanes = DerivedLanes::new();
        lanes.register(
            "words",
            DerivedLaneData::Annotations(
                (0..1_000)
                    .map(|value| Annotation {
                        start_ns: value * 100,
                        end_ns: value * 100 + 50,
                        value,
                    })
                    .collect(),
            ),
        );
        let renderer = Arc::new(ProbingRenderer {
            lanes: lanes.clone(),
            calls: AtomicUsize::new(0),
        });
        let mut viewer = LogicAnalyzerViewer::new();
        viewer.set_derived_lanes(lanes);

        let visuals = viewer.prepare_builtin_annotation_visuals(
            &group(renderer.clone()),
            Rect::from_min_size(Pos2::ZERO, egui::vec2(10.0, 30.0)),
        );

        assert!(visuals.is_empty());
        assert_eq!(renderer.calls.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn opaque_lane_renderer_receives_a_bounded_snapshot_after_locks_release() {
        let lanes = DerivedLanes::new();
        let mut payloads = CollectedPayloadRegistry::new();
        payloads
            .register::<u8>("org.example.camera-frame/v1")
            .unwrap();
        let query = Arc::new(SnapshotQuery {
            requested: Mutex::new(None),
        });
        lanes.publish_opaque_lane(
            "camera.frames",
            payloads.descriptor::<u8>().unwrap().clone(),
            Arc::clone(&query),
        );
        let renderer = Arc::new(OpaqueSnapshotRenderer {
            lanes: lanes.clone(),
            values: Mutex::new(Vec::new()),
        });
        let presentations = crate::lanes::WaveformPresentationRegistry::new();
        presentations.set_implicit_groups(false);
        presentations.register(ViewerLaneGroup {
            id: ViewerLaneGroupId::new("camera"),
            label: "Camera".to_owned(),
            badge: ViewerLaneBadge::new("CAM", Color32::WHITE),
            tracks: vec![ViewerLaneTrack::new(
                "frames",
                DerivedLaneId::new("camera.frames"),
                1.0,
            )],
            renderer: renderer.clone(),
        });
        let mut viewer = LogicAnalyzerViewer::new();
        viewer.set_derived_lanes(lanes.clone());
        viewer.set_waveform_presentations(presentations);
        viewer.visible_span_us = 100.0;
        viewer.ensure_row_order();

        let context = egui::Context::default();
        let rect = Rect::from_min_size(Pos2::ZERO, egui::vec2(800.0, 120.0));
        context.begin_pass(egui::RawInput {
            screen_rect: Some(rect),
            ..Default::default()
        });
        let painter = context.layer_painter(egui::LayerId::background());
        viewer.draw(
            &painter,
            AnalyzerLayout {
                ruler_rect: Rect::from_min_max(Pos2::ZERO, Pos2::new(800.0, 20.0)),
                labels_rect: Rect::from_min_max(Pos2::new(0.0, 20.0), Pos2::new(200.0, 120.0)),
                wave_rect: Rect::from_min_max(Pos2::new(200.0, 20.0), Pos2::new(800.0, 120.0)),
                row_height: 80.0,
                trigger_width: 0.0,
                name_col_width: 120.0,
                badge_width: 32.0,
            },
            None,
            None,
        );
        let _ = context.end_pass();

        assert_eq!(*renderer.values.lock().unwrap(), vec![7, 9]);
        assert_eq!(
            *query.requested.lock().unwrap(),
            Some(CollectedLaneSnapshotRequest {
                start_time_ns: 0,
                end_time_ns: 100_000,
                max_items: 1_200,
            })
        );
        assert!(
            lanes
                .read()
                .iter()
                .any(|lane| lane.name == "opaque renderer lock probe")
        );
    }
}
