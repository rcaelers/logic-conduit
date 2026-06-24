use egui::{Rect, Ui};
use std::fmt;

/// Editable inline UI state bound to a node-state field.
pub trait InlineControl: Send + Sync + fmt::Debug {
    fn draw_widget(
        &mut self,
        ui: &mut Ui,
        label: &str,
        rect: Rect,
        zoom: f32,
        clip_rect: Rect,
    ) -> bool;
}
