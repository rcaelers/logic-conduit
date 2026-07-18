use egui::Ui;

use crate::types::AnalyzerLayout;
use crate::viewer::LogicAnalyzerViewer;

const SCROLL_INPUT_EPSILON: f32 = 0.5;

impl LogicAnalyzerViewer {
    pub(crate) fn handle_input(
        &mut self,
        ui: &Ui,
        layout: AnalyzerLayout,
        hovered: bool,
        dragging: bool,
    ) {
        let wave_rect = layout.wave_rect;
        if wave_rect.width() <= 1.0 {
            return;
        }

        if dragging {
            self.leave_live_edge();
            if self.capture_info.is_some() {
                self.fit_to_capture = false;
            }
            let delta = ui.input(|input| input.pointer.delta());
            self.visible_start_us -=
                delta.x as f64 / wave_rect.width() as f64 * self.visible_span_us;
            self.visible_start_us = self.visible_start_us.max(0.0);
            self.clamp_to_capture_duration();
        }

        if hovered {
            let scroll_delta = ui.input(|input| input.smooth_scroll_delta);
            if scroll_delta.x.abs() > SCROLL_INPUT_EPSILON {
                self.leave_live_edge();
                if self.capture_info.is_some() {
                    self.fit_to_capture = false;
                }
                self.visible_start_us -=
                    scroll_delta.x as f64 / wave_rect.width() as f64 * self.visible_span_us;
                self.visible_start_us = self.visible_start_us.max(0.0);
                self.clamp_to_capture_duration();
            }

            let pointer_x = ui
                .input(|input| input.pointer.hover_pos())
                .map_or(0.5, |pos| {
                    ((pos.x - wave_rect.left()) / wave_rect.width()).clamp(0.0, 1.0)
                }) as f64;

            // Vertical scroll always zooms here (Saleae/PulseView
            // convention — deliberately unconditional on Ctrl, so the
            // graph-editor habit of holding Ctrl to zoom still works and
            // never does something surprising, per the Phase 6.4 decision).
            if scroll_delta.y.abs() > SCROLL_INPUT_EPSILON {
                let factor = (1.0_f64 - scroll_delta.y as f64 * 0.0015).clamp(0.35, 2.5);
                self.zoom_time_axis(factor, pointer_x);
            }

            // Trackpad pinch/magnify: `zoom_delta` isn't carried by
            // `smooth_scroll_delta` at all, so without this a pinch gesture
            // did nothing here even though the graph editor already
            // supports it (Phase 6.4).
            let zoom_gesture = ui.input(|input| input.zoom_delta()) as f64;
            if (zoom_gesture - 1.0).abs() > 0.001 {
                self.zoom_time_axis((1.0 / zoom_gesture).clamp(0.35, 2.5), pointer_x);
            }
        }
    }

    /// Zooms the visible time window by `factor` (< 1 zooms in, > 1 zooms
    /// out) around `pointer_x` (0..1, fraction across `wave_rect`).
    fn zoom_time_axis(&mut self, factor: f64, pointer_x: f64) {
        self.leave_live_edge();
        if self.capture_info.is_some() {
            self.fit_to_capture = false;
        }
        let old_span = self.visible_span_us;
        let pivot_time = self.visible_start_us + old_span * pointer_x;
        let max_span = self
            .capture_info
            .as_ref()
            .map_or(f64::MAX, |capture| capture.duration_us.max(1.0));
        self.visible_span_us = (self.visible_span_us * factor).clamp(0.001, max_span);
        self.visible_start_us = pivot_time - self.visible_span_us * pointer_x;
        self.visible_start_us = self.visible_start_us.max(0.0);
        self.clamp_to_capture_duration();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zoom_time_axis_shrinks_span_and_keeps_pivot_centered() {
        let mut viewer = LogicAnalyzerViewer::new();
        viewer.visible_start_us = 0.0;
        viewer.visible_span_us = 1000.0;

        viewer.zoom_time_axis(0.5, 0.5);

        assert!((viewer.visible_span_us - 500.0).abs() < 1e-9);
        let pivot = viewer.visible_start_us + viewer.visible_span_us * 0.5;
        assert!((pivot - 500.0).abs() < 1e-9);
    }

    #[test]
    fn zoom_time_axis_grows_span_when_factor_exceeds_one() {
        let mut viewer = LogicAnalyzerViewer::new();
        viewer.visible_start_us = 0.0;
        viewer.visible_span_us = 1000.0;

        viewer.zoom_time_axis(2.0, 0.5);

        assert!((viewer.visible_span_us - 2000.0).abs() < 1e-9);
    }

    #[test]
    fn zoom_time_axis_zooms_toward_the_pointer_not_the_center() {
        // Pivot at us=200 (pointer_x=0.2 of a 1000us window) should stay
        // fixed under the pointer after zooming in, not drift toward the
        // window's center.
        let mut viewer = LogicAnalyzerViewer::new();
        viewer.visible_start_us = 0.0;
        viewer.visible_span_us = 1000.0;

        viewer.zoom_time_axis(0.5, 0.2);

        let pivot = viewer.visible_start_us + viewer.visible_span_us * 0.2;
        assert!((pivot - 200.0).abs() < 1e-9);
    }
}
