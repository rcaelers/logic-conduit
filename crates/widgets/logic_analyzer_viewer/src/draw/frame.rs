use std::collections::{HashMap, HashSet};

use egui::{Align2, Color32, FontId, Painter, Pos2, Rect, Shape, Stroke, StrokeKind, vec2};

use signal_processing::{DerivedLaneData, LaneSummary};

use super::derived::{DerivedRowGeometry, default_annotation_visual, visible_annotation_range};
use crate::cursor::{cursor_color, cursor_flag_geometry, cursor_flag_label};
use crate::format::{badge_text_color, format_duration, format_time, nice_step};
use crate::indexed_annotations::IndexedAnnotationSamples;
use crate::lanes::{
    AnnotationVisual, ViewerLaneFrame, ViewerLaneGroup, ViewerLaneTrackFrame, ViewerLaneTrackId,
};
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
            egui::FontId::proportional(13.0),
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
            egui::FontId::proportional(11.0),
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
        self.draw_rows(
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
            painter.text(
                Pos2::new(labels_rect.left() + 12.0, row_rect.center().y),
                Align2::LEFT_CENTER,
                &label.name,
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
                        .viewer_lanes
                        .read()
                        .iter()
                        .find(|group| &group.id == group_id)
                        .cloned()
                    else {
                        continue;
                    };
                    let (_frame, annotation_visuals) =
                        self.prepare_viewer_lane_frame(&group, wave_rect);
                    let empty_visuals = HashMap::new();
                    let lanes = store.read();
                    for (track, track_top, track_height) in group.track_rects(y_top, display_height)
                    {
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
                                match &cached.samples {
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
                        }
                    }
                }
            }
            y_top += display_height;
        }
    }

    /// Captures the bounded semantic inputs a concrete renderer may inspect,
    /// releases `DerivedLanes`, and only then invokes renderer/plugin code.
    fn prepare_viewer_lane_frame(
        &self,
        group: &ViewerLaneGroup,
        wave_rect: Rect,
    ) -> (
        ViewerLaneFrame,
        HashMap<ViewerLaneTrackId, HashMap<u64, AnnotationVisual>>,
    ) {
        let exact_limit = (wave_rect.width().max(1.0) as usize)
            .saturating_mul(2)
            .max(32);
        let (start_ns, end_ns) = self.visible_window_ns();
        let mut frame = ViewerLaneFrame::default();
        let mut formats = HashMap::<ViewerLaneTrackId, Option<String>>::new();

        if let Some(store) = &self.derived {
            let lanes = store.read();
            for track in &group.tracks {
                let Some(lane) = lanes.iter().find(|lane| lane.name == track.lane.as_str()) else {
                    continue;
                };
                let (values, dense) = match &lane.data {
                    DerivedLaneData::Annotations(annotations) => {
                        let (first, last) = visible_annotation_range(annotations, start_ns, end_ns);
                        let visible = &annotations[first..last];
                        if visible.len() > exact_limit {
                            (Vec::new(), true)
                        } else {
                            (
                                visible.iter().map(|annotation| annotation.value).collect(),
                                false,
                            )
                        }
                    }
                    DerivedLaneData::IndexedAnnotations(_) => self
                        .indexed_annotation_cache
                        .get(&lane.name)
                        .map_or((Vec::new(), true), |cached| match &cached.samples {
                            IndexedAnnotationSamples::Exact { annotations, .. } => (
                                annotations
                                    .iter()
                                    .map(|annotation| annotation.value)
                                    .collect(),
                                false,
                            ),
                            IndexedAnnotationSamples::Presence(_)
                            | IndexedAnnotationSamples::Error => (Vec::new(), true),
                        }),
                    DerivedLaneData::Digital(_) | DerivedLaneData::Markers(_) => continue,
                };
                formats.insert(track.id.clone(), lane.word_display_format.clone());
                frame.tracks.push(ViewerLaneTrackFrame {
                    track: track.id.clone(),
                    annotation_values: values,
                    dense,
                });
            }
        }

        let mut visuals = HashMap::new();
        for track_frame in &frame.tracks {
            if track_frame.dense {
                continue;
            }
            let format = formats
                .get(&track_frame.track)
                .and_then(|format| format.as_deref());
            let mut seen = HashSet::new();
            let track_visuals = track_frame
                .annotation_values
                .iter()
                .copied()
                .filter(|value| seen.insert(*value))
                .map(|value| {
                    let default = default_annotation_visual(value, format);
                    let visual =
                        group
                            .renderer
                            .annotation_visual(&track_frame.track, value, default);
                    (value, visual)
                })
                .collect();
            visuals.insert(track_frame.track.clone(), track_visuals);
        }
        (frame, visuals)
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

    pub(super) fn ns_to_x(&self, rect: Rect, ns: u64) -> f32 {
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
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use signal_processing::{Annotation, DerivedLaneData, DerivedLanes};

    use super::*;
    use crate::lanes::{
        DerivedLaneId, ViewerLaneBadge, ViewerLaneGroupId, ViewerLaneRenderer, ViewerLaneTrack,
    };

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

        let (frame, visuals) = viewer.prepare_viewer_lane_frame(
            &group(renderer.clone()),
            Rect::from_min_size(Pos2::ZERO, egui::vec2(100.0, 30.0)),
        );

        assert_eq!(frame.tracks[0].annotation_values, [1, 2]);
        assert!(!frame.tracks[0].dense);
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

        let (frame, visuals) = viewer.prepare_viewer_lane_frame(
            &group(renderer.clone()),
            Rect::from_min_size(Pos2::ZERO, egui::vec2(10.0, 30.0)),
        );

        assert!(frame.tracks[0].dense);
        assert!(frame.tracks[0].annotation_values.is_empty());
        assert!(visuals.is_empty());
        assert_eq!(renderer.calls.load(Ordering::Relaxed), 0);
    }
}
