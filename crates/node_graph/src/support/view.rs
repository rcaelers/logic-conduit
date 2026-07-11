use egui::{Pos2, Rect, Vec2};

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

    /// Fits `canvas_bounds` into `viewport`, with the requested screen-space
    /// padding, and centers it without exceeding the default 1x graph zoom.
    pub fn fit_to_rect(&mut self, canvas_bounds: Rect, viewport: Rect, origin: Pos2, padding: f32) {
        let available =
            (viewport.size() - Vec2::splat(padding.max(0.0) * 2.0)).max(Vec2::splat(1.0));
        let size = canvas_bounds.size().max(Vec2::splat(1.0));
        self.zoom = (available.x / size.x)
            .min(available.y / size.y)
            .clamp(0.1, 1.0);
        self.pan = viewport.center() - origin - canvas_bounds.center().to_vec2() * self.zoom;
    }
}

#[cfg(test)]
mod tests {
    use super::ViewState;
    use egui::{Pos2, Rect, Vec2};

    #[test]
    fn fit_to_rect_centers_and_contains_bounds() {
        let mut view = ViewState::default();
        let bounds = Rect::from_min_size(Pos2::new(100.0, 200.0), Vec2::new(400.0, 100.0));
        let viewport = Rect::from_min_size(Pos2::ZERO, Vec2::new(1_000.0, 600.0));
        view.fit_to_rect(bounds, viewport, Pos2::ZERO, 40.0);

        let fitted = Rect::from_min_max(
            view.canvas_to_screen(Pos2::ZERO, bounds.min),
            view.canvas_to_screen(Pos2::ZERO, bounds.max),
        );
        assert!(viewport.shrink(40.0).contains_rect(fitted));
        assert!(fitted.center().distance(viewport.center()) < 0.01);
        assert_eq!(view.zoom, 1.0);
    }
}
