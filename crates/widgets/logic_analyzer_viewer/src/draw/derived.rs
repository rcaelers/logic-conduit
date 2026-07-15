use std::collections::HashMap;

use egui::{Align2, Color32, FontId, Painter, Pos2, Rect, Shape, Stroke};

use signal_processing::events::MAX_ANNOTATION_NS;
use signal_processing::{Annotation, AnnotationFold, ChunkedMipmap, Sample, WordPresenceBucket};

use crate::lanes::AnnotationVisual;
use crate::viewer::LogicAnalyzerViewer;

#[derive(Clone, Copy)]
pub(super) struct DerivedRowGeometry {
    pub top: f32,
    pub height: f32,
}

impl LogicAnalyzerViewer {
    // ── Derived lanes ─────────────────────────────────────────────────
    //
    // Content only — the label (name, badge) is drawn once for every row
    // kind by `draw_rows`, not here.

    pub(crate) fn draw_derived_digital(
        &self,
        painter: &Painter,
        wave_rect: Rect,
        y_top: f32,
        row_height: f32,
        samples: &[Sample],
    ) {
        const DIRECT_EDGE_LIMIT: usize = 4096;
        let color = Color32::from_rgb(95, 175, 95);
        let stroke = Stroke::new(1.4, color);
        let high_y = y_top + row_height * 0.28;
        let low_y = y_top + row_height * 0.72;
        let (start_ns, end_ns) = self.visible_window_ns();

        let first = samples.partition_point(|sample| sample.start_time_ns < start_ns);
        let last = samples.partition_point(|sample| sample.start_time_ns <= end_ns);
        let mut level = if first > 0 {
            samples[first - 1].value
        } else {
            false
        };
        let y_of = |value: bool| if value { high_y } else { low_y };

        if last - first <= DIRECT_EDGE_LIMIT {
            let mut prev_x = wave_rect.left();
            for sample in &samples[first..last] {
                let x = self.ns_to_x(wave_rect, sample.start_time_ns);
                painter.line_segment(
                    [Pos2::new(prev_x, y_of(level)), Pos2::new(x, y_of(level))],
                    stroke,
                );
                painter.line_segment(
                    [Pos2::new(x, y_of(level)), Pos2::new(x, y_of(sample.value))],
                    stroke,
                );
                level = sample.value;
                prev_x = x;
            }
            painter.line_segment(
                [
                    Pos2::new(prev_x, y_of(level)),
                    Pos2::new(wave_rect.right(), y_of(level)),
                ],
                stroke,
            );
            return;
        }

        // Dense: one pass per pixel column — a column containing edges is a
        // solid band (same rule as the channel renderer: never invent edge
        // positions), columns without edges extend the current level.
        let span_ns = (end_ns - start_ns).max(1);
        let width = wave_rect.width().max(1.0);
        let mut index = first;
        let mut run_start_x = wave_rect.left();
        let mut column = 0u32;
        while (column as f32) < width {
            let x0 = wave_rect.left() + column as f32;
            let column_end_ns =
                start_ns + ((column + 1) as u64).saturating_mul(span_ns) / width as u64;
            let step =
                samples[index..last].partition_point(|sample| sample.start_time_ns < column_end_ns);
            if step > 0 {
                painter.line_segment(
                    [
                        Pos2::new(run_start_x, y_of(level)),
                        Pos2::new(x0, y_of(level)),
                    ],
                    stroke,
                );
                painter.rect_filled(
                    Rect::from_min_max(Pos2::new(x0, high_y), Pos2::new(x0 + 1.0, low_y)),
                    0.0,
                    color,
                );
                index += step;
                level = samples[index - 1].value;
                run_start_x = x0 + 1.0;
            }
            column += 1;
        }
        painter.line_segment(
            [
                Pos2::new(run_start_x, y_of(level)),
                Pos2::new(wave_rect.right(), y_of(level)),
            ],
            stroke,
        );
    }

    pub(super) fn draw_derived_annotations(
        &self,
        painter: &Painter,
        wave_rect: Rect,
        row: DerivedRowGeometry,
        annotations: &[Annotation],
        summary: &ChunkedMipmap<Annotation, AnnotationFold>,
        visuals: &HashMap<u64, AnnotationVisual>,
    ) {
        let band_color = Color32::from_rgb(215, 140, 60);
        let box_top = row.top + row.height * 0.12;
        let box_bottom = row.top + row.height * 0.88;
        let (start_ns, end_ns) = self.visible_window_ns();

        // Bounded lanes retain exact values only for their newest window.
        // Render older, summarized entries as activity bands so the full
        // recording remains visible without materializing billions of
        // annotation structs in UI memory.
        let exact_start_ns = annotations
            .first()
            .map_or(end_ns.saturating_add(1), |annotation| annotation.start_ns);
        if start_ns < exact_start_ns {
            let summary_end_ns = end_ns.min(exact_start_ns.saturating_sub(1));
            let records = summary.sampled_window(
                start_ns,
                summary_end_ns,
                wave_rect.width().max(1.0) as usize,
            );
            for record in records {
                let record_start = record.start_ns.max(start_ns);
                let record_end = record.end_ns.min(summary_end_ns);
                if record_start > record_end {
                    continue;
                }
                let x0 = self.ns_to_x(wave_rect, record_start);
                let x1 = self
                    .ns_to_x(wave_rect, record_end)
                    .max(x0 + 1.0)
                    .min(wave_rect.right());
                painter.rect_filled(
                    Rect::from_min_max(Pos2::new(x0, box_top), Pos2::new(x1, box_bottom)),
                    0.0,
                    band_color,
                );
            }
        }

        self.draw_annotation_slice(
            painter,
            wave_rect,
            box_top,
            box_bottom,
            annotations,
            annotations.last().map(|annotation| annotation.start_ns),
            visuals,
        );
    }

    pub(crate) fn draw_indexed_annotation_presence(
        &self,
        painter: &Painter,
        wave_rect: Rect,
        y_top: f32,
        row_height: f32,
        buckets: &[WordPresenceBucket],
    ) {
        let color = Color32::from_rgb(215, 140, 60);
        let top = y_top + row_height * 0.12;
        let bottom = y_top + row_height * 0.88;
        let (visible_start, visible_end) = self.visible_window_ns();
        for bucket in buckets {
            let start_ns = bucket.start_ns.max(visible_start);
            let end_ns = bucket.end_ns.min(visible_end);
            if start_ns > end_ns || bucket.word_count == 0 {
                continue;
            }
            let x0 = self.ns_to_x(wave_rect, start_ns);
            let x1 = self
                .ns_to_x(wave_rect, end_ns)
                .max(x0 + 1.0)
                .min(wave_rect.right());
            painter.rect_filled(
                Rect::from_min_max(Pos2::new(x0, top), Pos2::new(x1, bottom)),
                0.0,
                color,
            );
        }
    }

    pub(super) fn draw_indexed_annotation_exact(
        &self,
        painter: &Painter,
        wave_rect: Rect,
        geometry: DerivedRowGeometry,
        annotations: &[Annotation],
        last_timestamp_ns: Option<u64>,
        visuals: &HashMap<u64, AnnotationVisual>,
    ) {
        self.draw_annotation_slice(
            painter,
            wave_rect,
            geometry.top + geometry.height * 0.12,
            geometry.top + geometry.height * 0.88,
            annotations,
            last_timestamp_ns,
            visuals,
        );
    }

    #[allow(clippy::too_many_arguments)]
    fn draw_annotation_slice(
        &self,
        painter: &Painter,
        wave_rect: Rect,
        box_top: f32,
        box_bottom: f32,
        annotations: &[Annotation],
        lane_last_timestamp_ns: Option<u64>,
        visuals: &HashMap<u64, AnnotationVisual>,
    ) {
        let band_color = Color32::from_rgb(215, 140, 60);
        let (start_ns, end_ns) = self.visible_window_ns();
        let (first, last) = visible_annotation_range(annotations, start_ns, end_ns);
        let visible = &annotations[first..last];

        if visible.len() > wave_rect.width() as usize * 2 {
            // Dense windows are a bounded activity snapshot. Exact values
            // and protocol formatting cannot be legible at this density, so
            // every occupied run is rendered as a presence band. Zooming in
            // produces a sparse frame with exact boxes and labels.
            let span_ns = (end_ns - start_ns).max(1);
            let width = wave_rect.width().max(1.0);
            let runs = dense_annotation_runs(visible, start_ns, span_ns, width as u32);
            for run in &runs {
                let x0 = wave_rect.left() + run.start_column as f32;
                let x1 = wave_rect.left() + (run.end_column + 1) as f32;
                painter.rect_filled(
                    Rect::from_min_max(Pos2::new(x0, box_top), Pos2::new(x1, box_bottom)),
                    0.0,
                    band_color,
                );
            }
            return;
        }

        for (offset, annotation) in visible.iter().enumerate() {
            if annotation.end_ns < start_ns {
                continue;
            }
            let is_last_ever = first + offset == annotations.len() - 1
                && lane_last_timestamp_ns == Some(annotation.start_ns);
            // Every earlier annotation is already closed (only the very
            // last one can still have `end_ns == start_ns`), so its width
            // is a fair estimate of how long this open-ended one likely is.
            let previous_duration_ns = (first + offset > 0).then(|| {
                let previous = &annotations[first + offset - 1];
                previous.end_ns.saturating_sub(previous.start_ns)
            });
            self.draw_annotation_box(
                painter,
                wave_rect,
                box_top,
                box_bottom,
                annotation,
                is_last_ever,
                previous_duration_ns,
                visuals,
            );
        }
    }

    /// One word as a bordered value box. The hexadecimal label is included
    /// only when it fits within the word's actual displayed time span.
    #[allow(clippy::too_many_arguments)]
    fn draw_annotation_box(
        &self,
        painter: &Painter,
        wave_rect: Rect,
        box_top: f32,
        box_bottom: f32,
        annotation: &Annotation,
        is_last_ever: bool,
        previous_duration_ns: Option<u64>,
        visuals: &HashMap<u64, AnnotationVisual>,
    ) {
        let effective_end = annotation_box_end(annotation, is_last_ever, previous_duration_ns);
        let x0 = self.ns_to_x(wave_rect, annotation.start_ns);
        let natural_x1 = self.ns_to_x(wave_rect, effective_end).max(x0 + 2.0);
        let visual = visuals
            .get(&annotation.value)
            .cloned()
            .unwrap_or_else(|| default_annotation_visual(annotation.value, None));
        let label = visual.label;
        let label_width = annotation_label_width(&label);
        let rect = Rect::from_min_max(Pos2::new(x0, box_top), Pos2::new(natural_x1, box_bottom));
        // Keep the angled ends shallow and consistent. A large bevel turns
        // short annotations into pointy hexagons instead of PulseView-style
        // data boxes.
        let bevel = (rect.height() * 0.20)
            .min(rect.width() * 0.18)
            .clamp(1.0, 10.0);
        painter.add(Shape::convex_polygon(
            vec![
                Pos2::new(rect.left() + bevel, rect.top()),
                Pos2::new(rect.right() - bevel, rect.top()),
                Pos2::new(rect.right(), rect.center().y),
                Pos2::new(rect.right() - bevel, rect.bottom()),
                Pos2::new(rect.left() + bevel, rect.bottom()),
                Pos2::new(rect.left(), rect.center().y),
            ],
            visual.fill,
            visual.border,
        ));
        if let Some(label_position) = annotation_label_position(rect, wave_rect, label_width) {
            painter.text(
                label_position,
                Align2::CENTER_CENTER,
                label,
                FontId::monospace(10.0),
                Color32::from_rgb(235, 220, 200),
            );
        }
    }

    pub(crate) fn draw_derived_markers(
        &self,
        painter: &Painter,
        wave_rect: Rect,
        y_top: f32,
        row_height: f32,
        markers: &[u64],
    ) {
        let color = Color32::from_rgb(230, 190, 80);
        let (start_ns, end_ns) = self.visible_window_ns();
        let first = markers.partition_point(|&ts| ts < start_ns);
        let last = markers.partition_point(|&ts| ts <= end_ns);
        let visible = &markers[first..last];
        let top = y_top + row_height * 0.18;
        let bottom = y_top + row_height * 0.82;

        if visible.len() > wave_rect.width() as usize {
            let span_ns = (end_ns - start_ns).max(1);
            let width = wave_rect.width().max(1.0);
            let mut index = 0usize;
            let mut column = 0u32;
            while (column as f32) < width {
                let column_end_ns =
                    start_ns + ((column + 1) as u64).saturating_mul(span_ns) / width as u64;
                let step = visible[index..].partition_point(|&ts| ts < column_end_ns);
                if step > 0 {
                    let x0 = wave_rect.left() + column as f32;
                    painter.rect_filled(
                        Rect::from_min_max(Pos2::new(x0, top), Pos2::new(x0 + 1.0, bottom)),
                        0.0,
                        color,
                    );
                    index += step;
                }
                column += 1;
            }
            return;
        }

        for &ts in visible {
            let x = self.ns_to_x(wave_rect, ts);
            painter.line_segment(
                [Pos2::new(x, top), Pos2::new(x, bottom)],
                Stroke::new(1.4, color),
            );
            painter.add(Shape::convex_polygon(
                vec![
                    Pos2::new(x - 4.0, top),
                    Pos2::new(x + 4.0, top),
                    Pos2::new(x, top + 6.0),
                ],
                color,
                Stroke::NONE,
            ));
        }
    }
}

pub(super) fn default_annotation_visual(
    value: u64,
    display_format: Option<&str>,
) -> AnnotationVisual {
    AnnotationVisual {
        label: format_value(value, display_format),
        fill: Color32::from_rgb(88, 58, 28),
        border: Stroke::new(1.0, Color32::from_rgb(215, 140, 60)),
    }
}

fn format_value(value: u64, format: Option<&str>) -> String {
    match format.unwrap_or("Hex") {
        "Binary" => format!("{value:b}"),
        "Octal" => format!("{value:o}"),
        "Decimal" => value.to_string(),
        "ASCII" => char::from_u32(value as u32)
            .filter(|character| !character.is_control())
            .map_or_else(|| ".".to_string(), |character| character.to_string()),
        "Hex + ASCII" => {
            let ascii = char::from_u32(value as u32)
                .filter(|character| !character.is_control())
                .unwrap_or('.');
            format!("{value:02X} '{ascii}'")
        }
        _ => format!("{value:02X}"),
    }
}

/// One contiguous stretch of pixel columns containing annotation starts, in
/// the dense (more words than pixels) rendering path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct DenseRun {
    start_column: u32,
    /// Inclusive.
    end_column: u32,
    /// Index (into the visible slice) of the run's first word.
    first_index: usize,
    /// Words whose start falls inside the run.
    count: usize,
}

/// Buckets `visible` (sorted by `start_ns`, all within the window) into
/// per-pixel-column runs: adjacent columns that each contain at least one
/// annotation start merge into one run with a total word count.
fn dense_annotation_runs(
    visible: &[Annotation],
    start_ns: u64,
    span_ns: u64,
    width: u32,
) -> Vec<DenseRun> {
    let mut runs: Vec<DenseRun> = Vec::new();
    let mut index = 0usize;
    for column in 0..width {
        let column_end_ns =
            start_ns + ((column + 1) as u64).saturating_mul(span_ns) / width.max(1) as u64;
        let step =
            visible[index..].partition_point(|annotation| annotation.start_ns < column_end_ns);
        if step == 0 {
            continue;
        }
        match runs.last_mut() {
            Some(run) if run.end_column + 1 == column => {
                run.end_column = column;
                run.count += step;
            }
            _ => runs.push(DenseRun {
                start_column: column,
                end_column: column,
                first_index: index,
                count: step,
            }),
        }
        index += step;
    }
    runs
}

/// Effective right edge for one annotation box. The most recent word ever
/// decoded has no successor to patch its `end_ns` (see `append_word` in
/// `viewer_sink.rs`) — `start_ns == end_ns` forever, not just until the next
/// word arrives — so it's rendered open-ended using the previous word's
/// width as a same-framing estimate (falling back to the burst cap when
/// there's no previous word to measure), rather than collapsing to an
/// unreadable sliver.
pub(crate) fn annotation_box_end(
    annotation: &Annotation,
    is_last_ever: bool,
    previous_duration_ns: Option<u64>,
) -> u64 {
    if is_last_ever && annotation.end_ns == annotation.start_ns {
        let duration = previous_duration_ns
            .unwrap_or(MAX_ANNOTATION_NS)
            .min(MAX_ANNOTATION_NS);
        annotation.start_ns + duration
    } else {
        annotation.end_ns.max(annotation.start_ns)
    }
}

fn annotation_label_width(label: &str) -> f32 {
    (label.chars().count() as f32 * 6.2 + 10.0).max(26.0)
}

fn annotation_label_position(rect: Rect, wave_rect: Rect, label_width: f32) -> Option<Pos2> {
    let visible_rect = rect.intersect(wave_rect);
    (visible_rect.width() >= label_width).then(|| visible_rect.center())
}

/// Annotation starts are ordered and instantaneous parallel words are closed
/// at the next word's start, so at most the immediately preceding annotation
/// can overlap the left edge of the visible window.
pub(super) fn visible_annotation_range(
    annotations: &[Annotation],
    start_ns: u64,
    end_ns: u64,
) -> (usize, usize) {
    let first_in_window = annotations.partition_point(|annotation| annotation.start_ns < start_ns);
    let mut first = first_in_window.saturating_sub(1);
    if first < first_in_window {
        let annotation = &annotations[first];
        let previous_duration_ns = (first > 0).then(|| {
            let previous = &annotations[first - 1];
            previous.end_ns.saturating_sub(previous.start_ns)
        });
        let end = annotation_box_end(
            annotation,
            first == annotations.len() - 1,
            previous_duration_ns,
        );
        if end < start_ns {
            first = first_in_window;
        }
    }
    let last = annotations.partition_point(|annotation| annotation.start_ns <= end_ns);
    (first, last.max(first))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn last_ever_open_annotation_matches_the_previous_words_width() {
        // "HELLO\n": every earlier byte was ~10 bit-times wide, so the
        // dangling '\n' should render about that wide too, not the full
        // 1ms burst cap.
        let annotation = Annotation {
            start_ns: 100_000,
            end_ns: 100_000,
            value: 0x0A,
        };
        assert_eq!(annotation_box_end(&annotation, true, Some(10_000)), 110_000);
    }

    #[test]
    fn last_ever_open_annotation_falls_back_to_the_burst_cap_with_no_history() {
        let annotation = Annotation {
            start_ns: 1_000,
            end_ns: 1_000,
            value: 0x4F,
        };
        assert_eq!(
            annotation_box_end(&annotation, true, None),
            1_000 + MAX_ANNOTATION_NS
        );
    }

    #[test]
    fn last_ever_open_annotation_never_exceeds_the_burst_cap() {
        let annotation = Annotation {
            start_ns: 1_000,
            end_ns: 1_000,
            value: 0x4F,
        };
        assert_eq!(
            annotation_box_end(&annotation, true, Some(MAX_ANNOTATION_NS * 10)),
            1_000 + MAX_ANNOTATION_NS
        );
    }

    #[test]
    fn closed_annotation_keeps_its_patched_end() {
        let annotation = Annotation {
            start_ns: 1_000,
            end_ns: 1_500,
            value: 0x4F,
        };
        assert_eq!(annotation_box_end(&annotation, true, Some(10_000)), 1_500);
        assert_eq!(annotation_box_end(&annotation, false, Some(10_000)), 1_500);
    }

    #[test]
    fn annotation_label_width_scales_with_hex_digits() {
        assert!(annotation_label_width("600081") > annotation_label_width("4F"));
        assert!(annotation_label_width("4F") >= 26.0);
    }

    #[test]
    fn clipped_annotation_uses_its_visible_width_for_the_label() {
        let wave_rect = Rect::from_min_max(Pos2::new(100.0, 0.0), Pos2::new(300.0, 20.0));
        let clipped_left = Rect::from_min_max(Pos2::new(50.0, 2.0), Pos2::new(180.0, 18.0));
        let clipped_right = Rect::from_min_max(Pos2::new(260.0, 2.0), Pos2::new(350.0, 18.0));
        let too_narrow = Rect::from_min_max(Pos2::new(290.0, 2.0), Pos2::new(350.0, 18.0));
        let label_width = annotation_label_width("27");

        assert_eq!(
            annotation_label_position(clipped_left, wave_rect, label_width),
            Some(Pos2::new(140.0, 10.0))
        );
        assert_eq!(
            annotation_label_position(clipped_right, wave_rect, label_width),
            Some(Pos2::new(280.0, 10.0))
        );
        assert_eq!(
            annotation_label_position(too_narrow, wave_rect, label_width),
            None
        );
    }

    #[test]
    fn visible_range_includes_a_long_word_starting_before_the_window() {
        let annotations = [
            Annotation {
                start_ns: 1_000,
                end_ns: 10_000_000,
                value: 0x12,
            },
            Annotation {
                start_ns: 10_000_000,
                end_ns: 11_000_000,
                value: 0x27,
            },
        ];

        assert_eq!(
            visible_annotation_range(&annotations, 9_000_000, 10_500_000),
            (0, 2)
        );
        assert_eq!(
            visible_annotation_range(&annotations, 5_000_000, 6_000_000),
            (0, 1)
        );
    }

    #[test]
    fn visible_range_excludes_a_preceding_word_that_ended_before_the_window() {
        let annotations = [
            Annotation {
                start_ns: 1_000,
                end_ns: 2_000,
                value: 0x12,
            },
            Annotation {
                start_ns: 10_000_000,
                end_ns: 11_000_000,
                value: 0x27,
            },
        ];

        assert_eq!(
            visible_annotation_range(&annotations, 5_000_000, 6_000_000),
            (1, 1)
        );
    }

    /// A lone word and a real cluster must come out as separate runs — the
    /// lone one (count 1) is what the dense path still renders as an exact
    /// value box instead of folding it into an unrelated cluster.
    #[test]
    fn dense_runs_keep_isolated_words_separable_from_clusters() {
        // Window: 0..1_000_000ns over 100 columns → 10_000ns per column.
        // Burst: 3 words inside column 1; lone word in column 50; pair in
        // column 80.
        let word = |start_ns: u64| Annotation {
            start_ns,
            end_ns: start_ns,
            value: 0,
        };
        let visible = [
            word(10_000),
            word(12_000),
            word(14_000),
            word(500_000),
            word(800_000),
            word(805_000),
        ];
        let runs = dense_annotation_runs(&visible, 0, 1_000_000, 100);
        assert_eq!(
            runs,
            vec![
                DenseRun {
                    start_column: 1,
                    end_column: 1,
                    first_index: 0,
                    count: 3
                },
                DenseRun {
                    start_column: 50,
                    end_column: 50,
                    first_index: 3,
                    count: 1
                },
                DenseRun {
                    start_column: 80,
                    end_column: 80,
                    first_index: 4,
                    count: 2
                },
            ]
        );
    }

    /// A burst spanning several adjacent columns is one run, not one run
    /// per pixel.
    #[test]
    fn dense_runs_merge_adjacent_columns() {
        let word = |start_ns: u64| Annotation {
            start_ns,
            end_ns: start_ns,
            value: 0,
        };
        // 100 columns over 0..100_000ns → 1_000ns per column; words every
        // 250ns across columns 10..=13.
        let visible: Vec<Annotation> = (0..16).map(|i| word(10_000 + i * 250)).collect();
        let runs = dense_annotation_runs(&visible, 0, 100_000, 100);
        assert_eq!(
            runs,
            vec![DenseRun {
                start_column: 10,
                end_column: 13,
                first_index: 0,
                count: 16
            }]
        );
    }

    #[test]
    fn open_annotation_that_is_not_the_last_one_ever_is_left_alone() {
        // Shouldn't happen in practice (append_word always patches the
        // second-to-last entry), but the fallback must not invent a box.
        let annotation = Annotation {
            start_ns: 1_000,
            end_ns: 1_000,
            value: 0x4F,
        };
        assert_eq!(annotation_box_end(&annotation, false, Some(10_000)), 1_000);
    }
}
