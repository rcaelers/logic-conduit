mod channels;
mod derived;
mod measurement;

use crate::cursor::{cursor_color, cursor_flag_geometry, cursor_flag_label};
use crate::format::{format_duration, format_time, nice_step};
use crate::types::AnalyzerLayout;
use crate::viewer::LogicAnalyzerViewer;
use egui::{Align2, Color32, Painter, Pos2, Rect, Shape, Stroke, StrokeKind, vec2};

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
        self.draw_derived_lanes(painter, labels_rect, wave_rect, row_height, text, grid);

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

    fn visible_window_ns(&self) -> (u64, u64) {
        let start_ns = (self.visible_start_us.max(0.0) * 1_000.0) as u64;
        let end_ns =
            ((self.visible_start_us + self.visible_span_us).max(0.0) * 1_000.0).ceil() as u64;
        (start_ns, end_ns)
    }

    fn ns_to_x(&self, rect: Rect, ns: u64) -> f32 {
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
