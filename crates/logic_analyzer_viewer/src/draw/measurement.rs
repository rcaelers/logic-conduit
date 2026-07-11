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
        let marker_y = row_top + row_height * 0.5;
        // Segment 1 (start_us..end_us) sits at the hovered level; segment 2
        // (end_us..period_end_us), when known, is the *other* level — each
        // bracket is drawn at the height that level is actually at, so a
        // long High next to a brief Low gets its own visible arrow instead
        // of being reduced to a Duty Cycle percentage in the tooltip. An
        // event has no level at all, so both stay at the row's mid-height.
        let (level1_y, level2_y) = if measurement.is_event {
            (marker_y, marker_y)
        } else if measurement.value {
            (high_y, low_y)
        } else {
            (low_y, high_y)
        };

        let has_period = measurement.period_end_us.is_some();
        let x0_raw = self.time_to_x_unclamped(wave_rect, measurement.start_us);
        let x1_raw = self.time_to_x_unclamped(wave_rect, measurement.end_us);
        let x2_raw = measurement
            .period_end_us
            .map(|period_end_us| self.time_to_x_unclamped(wave_rect, period_end_us));

        let in_range = |x: f32| x >= wave_rect.left() && x <= wave_rect.right();
        let x0 = x0_raw.clamp(wave_rect.left(), wave_rect.right());
        let x1 = x1_raw.clamp(wave_rect.left(), wave_rect.right());
        let start_in_view = !measurement.start_open && in_range(x0_raw);
        // The midpoint toggle is a real edge whenever a period was found
        // (it's what closes it); otherwise it's only real if `end_us`
        // itself wasn't just the open window edge.
        let mid_in_view = (has_period || !measurement.end_open) && in_range(x1_raw);

        self.draw_measurement_bracket(
            painter,
            x0,
            x1,
            level1_y,
            start_in_view,
            mid_in_view,
            yellow,
        );

        if let Some(x2_raw) = x2_raw {
            let x2 = x2_raw.clamp(wave_rect.left(), wave_rect.right());
            let end_in_view = in_range(x2_raw);
            self.draw_measurement_bracket(
                painter,
                x1,
                x2,
                level2_y,
                mid_in_view,
                end_in_view,
                yellow,
            );
            // Ties the two brackets together at the toggle they share.
            if mid_in_view {
                painter.line_segment([Pos2::new(x1, level1_y), Pos2::new(x1, level2_y)], stroke);
            }
        }

        let row_bottom = row_top + row_height;
        self.draw_measurement_tooltip(painter, wave_rect, row_top, row_bottom, measurement);
    }

    /// One arrow-tipped bracket from `x_start` to `x_end` at height `y`;
    /// arrowheads only draw on the ends that land on a real, in-view toggle
    /// — an open end simply runs the plain line off the viewport edge.
    #[allow(clippy::too_many_arguments)]
    fn draw_measurement_bracket(
        &self,
        painter: &Painter,
        x_start: f32,
        x_end: f32,
        y: f32,
        start_in_view: bool,
        end_in_view: bool,
        color: Color32,
    ) {
        if (x_end - x_start).abs() < 2.0 {
            return;
        }
        let stroke = Stroke::new(1.2, color);
        painter.line_segment([Pos2::new(x_start, y), Pos2::new(x_end, y)], stroke);
        if start_in_view {
            self.draw_measurement_arrow_end(painter, Pos2::new(x_start, y), 1.0, color);
        }
        if end_in_view {
            self.draw_measurement_arrow_end(painter, Pos2::new(x_end, y), -1.0, color);
        }
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
        row_top: f32,
        row_bottom: f32,
        measurement: PulseMeasurement,
    ) {
        // An event lane (§`PulseMeasurement::is_event`) has no level, so
        // only the gap to the neighboring event is meaningful — no
        // High/Low split, no Period, no Frequency, no Duty Cycle.
        let (label, other_label) = if measurement.is_event {
            ("Time between events", None)
        } else if measurement.value {
            ("High", Some("Low"))
        } else {
            ("Low", Some("High"))
        };
        let width_line = if measurement.start_open || measurement.end_open {
            // One or both toggles lie beyond the examined window; the width
            // only says how long the run provably is.
            format!(
                "{label}: > {}",
                format_delta(measurement.width_us()).trim_start_matches('+')
            )
        } else {
            format!("{label}: {}", format_delta(measurement.width_us()))
        };
        let mut lines = vec![width_line];
        // `period_end_us` is already cleared for events (§`sample_hover_measurement`),
        // so `period_us`/`duty_cycle` are `None` here and these stay hidden.
        if let Some(period_us) = measurement.period_us() {
            // The opposite level's own duration — otherwise a long High next
            // to a brief Low only shows up buried in a Duty Cycle percentage.
            if let Some(other_label) = other_label {
                lines.push(format!(
                    "{other_label}: {}",
                    format_delta(period_us - measurement.width_us())
                ));
            }
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
        // Below the whole row (not just the hovered level) so the box never
        // covers the brackets/trace it's reporting on. When there isn't
        // room below (the row is near the bottom of the visible area), go
        // above the row instead of clamping back down over it — a clamp
        // here would silently reintroduce the exact overlap this is trying
        // to avoid.
        let below = row_bottom + 4.0;
        let top = if below + height <= wave_rect.bottom() {
            below
        } else {
            (row_top - 4.0 - height).max(wave_rect.top())
        };
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
