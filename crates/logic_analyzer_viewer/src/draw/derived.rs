use crate::viewer::LogicAnalyzerViewer;
use dsl::nodes::sinks::MAX_ANNOTATION_NS;
use dsl::{Annotation, Sample};
use egui::{Align2, Color32, FontId, Painter, Pos2, Rect, Shape, Stroke};

impl LogicAnalyzerViewer {
    // ── Derived lanes (§4.9) ─────────────────────────────────────────────────
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

        let first = samples.partition_point(|sample| sample.start_time < start_ns);
        let last = samples.partition_point(|sample| sample.start_time <= end_ns);
        let mut level = if first > 0 {
            samples[first - 1].value
        } else {
            false
        };
        let y_of = |value: bool| if value { high_y } else { low_y };

        if last - first <= DIRECT_EDGE_LIMIT {
            let mut prev_x = wave_rect.left();
            for sample in &samples[first..last] {
                let x = self.ns_to_x(wave_rect, sample.start_time);
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
                samples[index..last].partition_point(|sample| sample.start_time < column_end_ns);
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

    pub(crate) fn draw_derived_annotations(
        &self,
        painter: &Painter,
        wave_rect: Rect,
        y_top: f32,
        row_height: f32,
        annotations: &[Annotation],
    ) {
        let box_color = Color32::from_rgb(88, 58, 28);
        let border = Stroke::new(1.0, Color32::from_rgb(215, 140, 60));
        let box_top = y_top + row_height * 0.18;
        let box_bottom = y_top + row_height * 0.82;
        let (start_ns, end_ns) = self.visible_window_ns();

        // Boxes are at most MAX_ANNOTATION_NS long, so anything starting
        // earlier than that before the window cannot reach into it.
        let first = annotations.partition_point(|annotation| {
            annotation.start_ns < start_ns.saturating_sub(MAX_ANNOTATION_NS)
        });
        let last = annotations.partition_point(|annotation| annotation.start_ns <= end_ns);
        let visible = &annotations[first..last];

        if visible.len() > wave_rect.width() as usize * 2 {
            // Dense: per-column presence band.
            let span_ns = (end_ns - start_ns).max(1);
            let width = wave_rect.width().max(1.0);
            let mut index = 0usize;
            let mut column = 0u32;
            while (column as f32) < width {
                let column_end_ns =
                    start_ns + ((column + 1) as u64).saturating_mul(span_ns) / width as u64;
                let step = visible[index..]
                    .partition_point(|annotation| annotation.start_ns < column_end_ns);
                if step > 0 {
                    let x0 = wave_rect.left() + column as f32;
                    painter.rect_filled(
                        Rect::from_min_max(Pos2::new(x0, box_top), Pos2::new(x0 + 1.0, box_bottom)),
                        0.0,
                        border.color,
                    );
                    index += step;
                }
                column += 1;
            }
            return;
        }

        for (offset, annotation) in visible.iter().enumerate() {
            if annotation.end_ns < start_ns {
                continue;
            }
            let is_last_ever = first + offset == annotations.len() - 1;
            // Every earlier annotation is already closed (only the very
            // last one can still have `end_ns == start_ns`), so its width
            // is a fair estimate of how long this open-ended one likely is.
            let previous_duration_ns = (first + offset > 0).then(|| {
                let previous = &annotations[first + offset - 1];
                previous.end_ns.saturating_sub(previous.start_ns)
            });
            let effective_end = annotation_box_end(annotation, is_last_ever, previous_duration_ns);
            let x0 = self.ns_to_x(wave_rect, annotation.start_ns);
            let x1 = self.ns_to_x(wave_rect, effective_end).max(x0 + 2.0);
            let rect = Rect::from_min_max(Pos2::new(x0, box_top), Pos2::new(x1, box_bottom));
            painter.rect_filled(rect, 2.0, box_color);
            painter.rect_stroke(rect, 2.0, border, egui::StrokeKind::Inside);
            if rect.width() >= 26.0 {
                painter.text(
                    rect.center(),
                    Align2::CENTER_CENTER,
                    format!("{:02X}", annotation.value),
                    FontId::monospace(10.0),
                    Color32::from_rgb(235, 220, 200),
                );
            }
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

/// Effective right edge for one annotation box. The most recent word ever
/// decoded has no successor to patch its `end_ns` (see `append_word` in
/// `viewer_sink.rs`) — `start_ns == end_ns` forever, not just until the next
/// word arrives — so it's rendered open-ended using the previous word's
/// width as a same-framing estimate (falling back to the burst cap when
/// there's no previous word to measure), rather than collapsing to an
/// unreadable sliver.
fn annotation_box_end(
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
        assert_eq!(
            annotation_box_end(&annotation, true, Some(10_000)),
            110_000
        );
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
