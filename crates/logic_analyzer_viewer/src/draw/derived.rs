use crate::viewer::LogicAnalyzerViewer;
use dsl::nodes::sinks::MAX_ANNOTATION_NS;
use dsl::{Annotation, DerivedLaneData, Sample};
use egui::{Align2, Color32, FontId, Painter, Pos2, Rect, Shape, Stroke};

impl LogicAnalyzerViewer {
    // ── Derived lanes (§4.9) ─────────────────────────────────────────────────

    /// Rows below the raw channels showing what Viewer nodes collected:
    /// digital levels, decoded-word boxes, trigger markers. Lane colors
    /// follow the socket payload hues (green / orange / amber).
    pub(crate) fn draw_derived_lanes(
        &self,
        painter: &Painter,
        labels_rect: Rect,
        wave_rect: Rect,
        row_height: f32,
        text: Color32,
        grid: Color32,
    ) {
        let Some(store) = &self.derived else {
            return;
        };
        let lanes = store.read();
        if lanes.is_empty() {
            return;
        }
        let clip = painter.with_clip_rect(wave_rect);

        for (offset, lane) in lanes.iter().enumerate() {
            let row = self.channels.len() + offset;
            let y_top = labels_rect.top() + row as f32 * row_height;
            if y_top > labels_rect.bottom() {
                break;
            }
            let row_rect = Rect::from_min_max(
                Pos2::new(labels_rect.left(), y_top),
                Pos2::new(wave_rect.right(), y_top + row_height),
            );
            painter.line_segment(
                [
                    Pos2::new(labels_rect.left(), row_rect.bottom()),
                    Pos2::new(wave_rect.right(), row_rect.bottom()),
                ],
                Stroke::new(1.0, Color32::from_rgb(42, 42, 42)),
            );

            let (badge_color, badge_glyph) = match &lane.data {
                DerivedLaneData::Digital(_) => (Color32::from_rgb(95, 175, 95), "S"),
                DerivedLaneData::Annotations(_) => (Color32::from_rgb(215, 140, 60), "W"),
                DerivedLaneData::Markers(_) => (Color32::from_rgb(230, 190, 80), "T"),
            };
            let badge_rect = Rect::from_min_size(
                Pos2::new(labels_rect.left() + 12.0, row_rect.center().y - 8.0),
                egui::vec2(16.0, 16.0),
            );
            painter.rect_filled(badge_rect, 2.0, badge_color);
            painter.text(
                badge_rect.center(),
                Align2::CENTER_CENTER,
                badge_glyph,
                FontId::monospace(10.0),
                crate::format::badge_text_color(badge_color),
            );
            let name = if lane.dropped > 0 {
                format!("{} ⚠", lane.name)
            } else {
                lane.name.clone()
            };
            painter.with_clip_rect(labels_rect).text(
                Pos2::new(badge_rect.right() + 8.0, row_rect.center().y),
                Align2::LEFT_CENTER,
                name,
                FontId::proportional(12.0),
                text,
            );

            match &lane.data {
                DerivedLaneData::Digital(samples) => {
                    let center_y = row_rect.center().y;
                    clip.line_segment(
                        [
                            Pos2::new(wave_rect.left(), center_y),
                            Pos2::new(wave_rect.right(), center_y),
                        ],
                        Stroke::new(1.0, grid),
                    );
                    self.draw_derived_digital(&clip, wave_rect, y_top, row_height, samples);
                }
                DerivedLaneData::Annotations(annotations) => {
                    self.draw_derived_annotations(&clip, wave_rect, y_top, row_height, annotations);
                }
                DerivedLaneData::Markers(markers) => {
                    self.draw_derived_markers(&clip, wave_rect, y_top, row_height, markers);
                }
            }
        }
    }

    fn draw_derived_digital(
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

    fn draw_derived_annotations(
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

        for annotation in visible {
            if annotation.end_ns < start_ns {
                continue;
            }
            let x0 = self.ns_to_x(wave_rect, annotation.start_ns);
            let x1 = self
                .ns_to_x(wave_rect, annotation.end_ns.max(annotation.start_ns))
                .max(x0 + 2.0);
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

    fn draw_derived_markers(
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
