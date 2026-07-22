use std::sync::Arc;

use egui::{Color32, CursorIcon, FontId, PointerButton, Pos2, Rect, Response, Ui};

use signal_processing::{Annotation, AnnotationQuery, DerivedLaneData};

use crate::draw::annotation_box_end;
use crate::types::{AnalyzerLayout, CursorInput, RowKey, TimeCursor, Transition};
use crate::viewer::LogicAnalyzerViewer;

enum AnnotationBoundarySource {
    InMemory(Option<f64>),
    Indexed(Arc<dyn AnnotationQuery>),
}

impl LogicAnalyzerViewer {
    /// Drives cursor add / hover / drag / delete for one frame.
    ///
    /// Runs before pan/zoom handling so an active cursor drag can suppress
    /// panning, and before the fit-on-double-click check so a time-canvas
    /// double-click means "add cursor" instead.
    pub(crate) fn handle_cursor_input(
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
        let remove_button = self
            .input_bindings
            .pointer_button(&["logic_analyzer.ruler", "logic_analyzer"], "remove_cursor")
            .unwrap_or(PointerButton::Primary);
        if response.clicked_by(remove_button)
            && let Some(pointer) = pointer
            && let Some(index) = flags.iter().position(|(_, close)| close.contains(pointer))
        {
            self.cursors.remove(index);
            self.drag_cursor = None;
            return state;
        }

        // Double-click anywhere in the time canvas adds a cursor. The label
        // column is excluded because it has no meaningful time coordinate.
        let add_cursor_button = self
            .input_bindings
            .pointer_button(&["logic_analyzer.timeline", "logic_analyzer"], "add_cursor");
        if add_cursor_button.is_some_and(|button| response.double_clicked_by(button))
            && let Some(pointer) = pointer
            && ruler_rect.union(wave_rect).contains(pointer)
        {
            state.add_cursor_double_click = true;
            let time_us = self.x_to_time(wave_rect, pointer.x);
            let number = next_cursor_number(&self.cursors);
            self.cursors.push(TimeCursor { number, time_us });
            return state;
        }

        let over_close_box =
            pointer.is_some_and(|pointer| flags.iter().any(|(_, close)| close.contains(pointer)));
        let hovered_cursor = pointer
            .and_then(|pointer| self.cursor_at_pointer(wave_rect, ruler_rect, &flags, pointer));

        let drag_button = self
            .input_bindings
            .pointer_button(
                &[
                    "logic_analyzer.cursor",
                    "logic_analyzer.ruler",
                    "logic_analyzer",
                ],
                "drag_cursor",
            )
            .unwrap_or(PointerButton::Primary);
        if response.drag_started_by(drag_button) {
            // Hit-test where the button went down, not where the pointer is
            // now: egui reports drag_started only after the pointer moved
            // past the click-vs-drag threshold, by which time it may already
            // have left the narrow line hit zone.
            let grab_pos = ui.input(|input| input.pointer.press_origin()).or(pointer);
            self.drag_cursor =
                grab_pos.and_then(|pos| self.cursor_at_pointer(wave_rect, ruler_rect, &flags, pos));
        }
        if self.drag_cursor.is_some() {
            if response.dragged_by(drag_button) {
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

    /// Snaps `time_us` to the nearest boundary of the row under the pointer:
    /// signal/event toggles or annotation word starts and ends. Over the
    /// ruler or an empty row the time stays free.
    fn snap_cursor_time(&mut self, wave_rect: Rect, pointer: Pos2, time_us: f64) -> f64 {
        const SNAP_DISTANCE_PX: f32 = 8.0;
        if pointer.y < wave_rect.top() || pointer.y > wave_rect.bottom() {
            return time_us;
        }
        let mut channel_row = 0;
        let mut row_top = wave_rect.top();
        while let Some(key) = self.row_order.get(channel_row) {
            let height = self.display_row_height(key, 30.0);
            if pointer.y < row_top + height {
                break;
            }
            row_top += height;
            channel_row += 1;
        }
        let annotation_sources = match self.row_order.get(channel_row) {
            Some(RowKey::Derived(group_id)) => {
                self.derived.as_ref().map_or_else(Vec::new, |store| {
                    let Some(group) = self
                        .waveform_presentations
                        .read()
                        .iter()
                        .find(|group| &group.id == group_id)
                        .cloned()
                    else {
                        return Vec::new();
                    };
                    let row_height = self.display_row_height(
                        self.row_order.get(channel_row).expect("row exists"),
                        30.0,
                    );
                    let pointer_fraction = (pointer.y - row_top) / row_height.max(1.0);
                    let selected = group.renderer.snap_lanes(&group, pointer_fraction);
                    let lanes = store.read();
                    selected
                        .iter()
                        .filter_map(|lane_id| {
                            lanes
                                .iter()
                                .find(|lane| lane.name == lane_id.as_str())
                                .and_then(|lane| match &lane.data {
                                    DerivedLaneData::Annotations(annotations) => {
                                        Some(AnnotationBoundarySource::InMemory(
                                            nearest_annotation_boundary_time(annotations, time_us),
                                        ))
                                    }
                                    DerivedLaneData::IndexedAnnotations(indexed) => {
                                        Some(AnnotationBoundarySource::Indexed(Arc::clone(
                                            indexed.query(),
                                        )))
                                    }
                                    _ => None,
                                })
                        })
                        .collect()
                })
            }
            _ => Vec::new(),
        };
        if !annotation_sources.is_empty() {
            let nearest = annotation_sources
                .into_iter()
                .filter_map(|source| match source {
                    AnnotationBoundarySource::InMemory(nearest) => nearest,
                    AnnotationBoundarySource::Indexed(query) => {
                        let timestamp_ns = (time_us.max(0.0) * 1_000.0).round() as u64;
                        let max_distance_ns =
                            (self.visible_span_us * 1_000.0 * f64::from(SNAP_DISTANCE_PX)
                                / f64::from(wave_rect.width().max(1.0)))
                            .ceil()
                            .max(1.0) as u64;
                        query
                            .nearest_boundary(timestamp_ns, max_distance_ns)
                            .ok()
                            .flatten()
                            .map(|boundary_ns| boundary_ns as f64 / 1_000.0)
                    }
                })
                .min_by(|left, right| (left - time_us).abs().total_cmp(&(right - time_us).abs()));
            let Some(nearest) = nearest else {
                return time_us;
            };
            let distance_px = (self.time_to_x_unclamped(wave_rect, nearest)
                - self.time_to_x_unclamped(wave_rect, time_us))
            .abs();
            return if distance_px <= SNAP_DISTANCE_PX {
                nearest
            } else {
                time_us
            };
        }
        let (channel_index, needs_exact_query, nearest_visible) = {
            let Some(channel) = self.channel_at_row(channel_row) else {
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
        // `waveform` is only ever populated from an indexed capture window
        // (see `channels_from_window`), so `needs_exact_query` is always
        // false when there is no sampler (e.g. on wasm) or the row is a
        // derived lane (`channel_at_row` never sets `waveform` for one),
        // and this always falls through to `nearest_visible`.
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
}

/// Flag box and its embedded close-box for a cursor whose line is at `x`,
/// clamped to stay inside the ruler. Shared by hit-testing and drawing so
/// they can never disagree.
pub(crate) fn cursor_flag_geometry(x: f32, ruler_rect: Rect, label_width: f32) -> (Rect, Rect) {
    const CLOSE_WIDTH: f32 = 15.0;
    const HEIGHT: f32 = 16.0;
    let width = label_width + 12.0 + CLOSE_WIDTH;
    let left = (x - width * 0.5).clamp(
        ruler_rect.left(),
        (ruler_rect.right() - width).max(ruler_rect.left()),
    );
    let top = ruler_rect.top() + 1.0;
    let flag = Rect::from_min_size(Pos2::new(left, top), egui::vec2(width, HEIGHT));
    let close = Rect::from_min_size(
        Pos2::new(flag.right() - CLOSE_WIDTH, top),
        egui::vec2(CLOSE_WIDTH, HEIGHT),
    );
    (flag, close)
}

pub(crate) fn cursor_flag_label(cursor: &TimeCursor) -> String {
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

fn nearest_annotation_boundary_time(annotations: &[Annotation], time_us: f64) -> Option<f64> {
    let time_ns = time_us * 1_000.0;
    let index = annotations.partition_point(|annotation| annotation.start_ns as f64 <= time_ns);
    let first = index.saturating_sub(2);
    let last = (index + 2).min(annotations.len());

    annotations[first..last]
        .iter()
        .enumerate()
        .flat_map(|(offset, annotation)| {
            let annotation_index = first + offset;
            let previous_duration_ns = annotation_index.checked_sub(1).map(|previous_index| {
                let previous = &annotations[previous_index];
                previous.end_ns.saturating_sub(previous.start_ns)
            });
            let end_ns = annotation_box_end(
                annotation,
                annotation_index == annotations.len() - 1,
                previous_duration_ns,
            );
            [annotation.start_ns, end_ns]
        })
        .map(|boundary_ns| boundary_ns as f64 / 1_000.0)
        .min_by(|a, b| (time_us - *a).abs().total_cmp(&(time_us - *b).abs()))
}

pub(crate) fn cursor_color(index: usize) -> Color32 {
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

#[cfg(test)]
mod cursor_tests {
    use signal_processing::{
        DerivedLanes, IndexedAnnotationLane, IndexedAnnotationWriter, LiveStoreConfig, Word,
    };

    use super::*;
    use crate::sampling::pulse_measurement_from_window;

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

    #[test]
    fn nearest_annotation_boundary_picks_word_starts_and_ends() {
        let annotations = [
            Annotation {
                start_ns: 10_000,
                end_ns: 20_000,
                value: 0x12,
            },
            Annotation {
                start_ns: 30_000,
                end_ns: 40_000,
                value: 0x27,
            },
        ];

        assert_eq!(
            nearest_annotation_boundary_time(&annotations, 11.0),
            Some(10.0)
        );
        assert_eq!(
            nearest_annotation_boundary_time(&annotations, 19.0),
            Some(20.0)
        );
        assert_eq!(
            nearest_annotation_boundary_time(&annotations, 29.0),
            Some(30.0)
        );
        assert_eq!(
            nearest_annotation_boundary_time(&annotations, 39.0),
            Some(40.0)
        );
    }

    #[test]
    fn nearest_annotation_boundary_includes_open_words_displayed_end() {
        let annotations = [
            Annotation {
                start_ns: 10_000,
                end_ns: 20_000,
                value: 0x12,
            },
            Annotation {
                start_ns: 30_000,
                end_ns: 30_000,
                value: 0x27,
            },
        ];

        assert_eq!(
            nearest_annotation_boundary_time(&annotations, 39.0),
            Some(40.0)
        );
    }

    #[test]
    fn cursor_snaps_when_dragged_over_an_annotation_row() {
        let mut viewer = LogicAnalyzerViewer::new();
        viewer.visible_span_us = 100.0;
        let lanes = DerivedLanes::new();
        lanes.register(
            "decoded.words",
            DerivedLaneData::Annotations(vec![Annotation {
                start_ns: 10_000,
                end_ns: 20_000,
                value: 0x27,
            }]),
        );
        viewer.set_derived_lanes(lanes);
        viewer.ensure_row_order();
        let wave_rect = Rect::from_min_max(Pos2::new(0.0, 100.0), Pos2::new(1_000.0, 130.0));

        assert_eq!(
            viewer.snap_cursor_time(wave_rect, Pos2::new(205.0, 115.0), 20.5),
            20.0
        );
        assert_eq!(
            viewer.snap_cursor_time(wave_rect, Pos2::new(210.0, 115.0), 21.0),
            21.0,
            "a boundary more than eight pixels away must not capture the cursor"
        );
    }

    #[test]
    fn cursor_snaps_to_indexed_word_starts_and_explicit_ends() {
        let (mut writer, store) =
            IndexedAnnotationWriter::create(LiveStoreConfig::default()).unwrap();
        writer
            .append_batch(&[Word::spanning(0x27, 10_000, 10_000)])
            .unwrap();
        writer.finish().unwrap();
        let lanes = DerivedLanes::new();
        lanes.register(
            "decoded.words",
            DerivedLaneData::IndexedAnnotations(IndexedAnnotationLane::from_store(store)),
        );
        let mut viewer = LogicAnalyzerViewer::new();
        viewer.visible_span_us = 100.0;
        viewer.set_derived_lanes(lanes);
        viewer.ensure_row_order();
        let wave_rect = Rect::from_min_max(Pos2::new(0.0, 100.0), Pos2::new(1_000.0, 130.0));

        assert_eq!(
            viewer.snap_cursor_time(wave_rect, Pos2::new(105.0, 115.0), 10.5),
            10.0
        );
        assert_eq!(
            viewer.snap_cursor_time(wave_rect, Pos2::new(195.0, 115.0), 19.5),
            20.0
        );
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
