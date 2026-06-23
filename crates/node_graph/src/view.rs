use egui::{Pos2, Vec2};

pub struct ViewState {
    pub pan: Vec2,
    pub zoom: f32,
}

impl Default for ViewState {
    fn default() -> Self {
        Self {
            pan: Vec2::ZERO,
            zoom: 1.0,
        }
    }
}

impl ViewState {
    // canvas → screen: screen = origin + canvas * zoom + pan
    pub fn canvas_to_screen(&self, origin: Pos2, p: Pos2) -> Pos2 {
        origin + p.to_vec2() * self.zoom + self.pan
    }

    // screen → canvas: canvas = (screen - origin - pan) / zoom
    pub fn screen_to_canvas(&self, origin: Pos2, p: Pos2) -> Pos2 {
        ((p - origin - self.pan) / self.zoom).to_pos2()
    }

    pub fn scale(&self, v: f32) -> f32 {
        v * self.zoom
    }

    pub fn zoom_around(&mut self, cursor_screen: Pos2, origin: Pos2, factor: f32) {
        let canvas_cursor = self.screen_to_canvas(origin, cursor_screen);
        self.zoom = (self.zoom * factor).clamp(0.1, 5.0);
        self.pan = cursor_screen - origin - canvas_cursor.to_vec2() * self.zoom;
    }
}
