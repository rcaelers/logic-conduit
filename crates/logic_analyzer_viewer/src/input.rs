use crate::types::AnalyzerLayout;
use crate::viewer::LogicAnalyzerViewer;
use egui::Ui;

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
                if self.capture_info.is_some() {
                    self.fit_to_capture = false;
                }
                self.visible_start_us -=
                    scroll_delta.x as f64 / wave_rect.width() as f64 * self.visible_span_us;
                self.visible_start_us = self.visible_start_us.max(0.0);
                self.clamp_to_capture_duration();
            }
            if scroll_delta.y.abs() > SCROLL_INPUT_EPSILON {
                if self.capture_info.is_some() {
                    self.fit_to_capture = false;
                }
                let pointer_x = ui
                    .input(|input| input.pointer.hover_pos())
                    .map_or(0.5, |pos| {
                        ((pos.x - wave_rect.left()) / wave_rect.width()).clamp(0.0, 1.0)
                    }) as f64;

                let old_span = self.visible_span_us;
                let pivot_time = self.visible_start_us + old_span * pointer_x;
                let factor = (1.0_f64 - scroll_delta.y as f64 * 0.0015).clamp(0.35, 2.5);
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
    }
}
