use crate::format::{format_delta, format_frequency};
use crate::types::PulseMeasurement;
use crate::viewer::LogicAnalyzerViewer;
use egui::{Align2, Color32, FontId, Painter, Pos2, Rect, Stroke, vec2};

impl LogicAnalyzerViewer {
    pub(crate) fn draw_pulse_measurement(
        &self,
        painter: &Painter,
        wave_rect: Rect,
        row_height: f32,
        measurement: PulseMeasurement,
    ) {
        let yellow = Color32::from_rgb(255, 190, 0);
        let stroke = Stroke::new(1.2, yellow);
        let row_top = wave_rect.top() + measurement.channel_row as f32 * row_height;
        let high_y = row_top + row_height * 0.28;
        let low_y = row_top + row_height * 0.72;
        let signal_y = if measurement.value { high_y } else { low_y };
        let marker_y = row_top + row_height * 0.5;

        let x0_raw = self.time_to_x_unclamped(wave_rect, measurement.start_us);
        let x1 = self.time_to_x_unclamped(wave_rect, measurement.end_us);
        // Without a following transition to close a full period, fall back to
        // a Width-only bracket spanning just the measured pulse.
        let has_period = measurement.period_end_us.is_some();
        let x2_raw = measurement.period_end_us.map_or(x1, |period_end_us| {
            self.time_to_x_unclamped(wave_rect, period_end_us)
        });
        if (x2_raw - x0_raw).abs() < 2.0 {
            return;
        }

        // Edges can fall outside the visible window (or, for an open run,
        // outside the examined window entirely); clamp the line to what's on
        // screen. The arrowhead and the vertical connector down to the
        // signal only draw for a real toggle that is actually in view — on
        // any other side the plain line simply runs off the viewport edge.
        let x0 = x0_raw.clamp(wave_rect.left(), wave_rect.right());
        let x2 = x2_raw.clamp(wave_rect.left(), wave_rect.right());
        let start_edge_in_view =
            !measurement.start_open && x0_raw >= wave_rect.left() && x0_raw <= wave_rect.right();
        let end_edge_in_view = !(measurement.end_open && !has_period)
            && x2_raw >= wave_rect.left()
            && x2_raw <= wave_rect.right();

        painter.line_segment([Pos2::new(x0, marker_y), Pos2::new(x2, marker_y)], stroke);
        if start_edge_in_view {
            self.draw_measurement_arrow_end(painter, Pos2::new(x0, marker_y), 1.0, yellow);
            painter.line_segment(
                [Pos2::new(x0, marker_y - 4.0), Pos2::new(x0, signal_y)],
                stroke,
            );
        }
        if end_edge_in_view {
            self.draw_measurement_arrow_end(painter, Pos2::new(x2, marker_y), -1.0, yellow);
            painter.line_segment(
                [Pos2::new(x2, marker_y - 4.0), Pos2::new(x2, signal_y)],
                stroke,
            );
        }

        if has_period && x1 > wave_rect.left() && x1 < wave_rect.right() {
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
        let width_line = if measurement.start_open || measurement.end_open {
            // One or both toggles lie beyond the examined window; the width
            // only says how long the run provably is.
            format!(
                "Width: > {}",
                format_delta(measurement.width_us()).trim_start_matches('+')
            )
        } else {
            format!("Width: {}", format_delta(measurement.width_us()))
        };
        let mut lines = vec![width_line];
        if let Some(period_us) = measurement.period_us() {
            lines.push(format!("Period: {}", format_delta(period_us)));
            lines.push(format!("Frequency: {}", format_frequency(period_us)));
        }
        if let Some(duty_cycle) = measurement.duty_cycle() {
            lines.push(format!("Duty Cycle: {:.2}%", duty_cycle * 100.0));
        }

        let width = 175.0_f32.min(wave_rect.width().max(1.0));
        let height = (20.0 * lines.len() as f32 + 16.0).min(wave_rect.height().max(1.0));
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
}
