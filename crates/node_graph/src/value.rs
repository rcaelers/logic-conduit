use egui::{Rect, Ui};
use std::fmt;

/// Type-erased UI control stored in unconnected input sockets and node state.
/// Implementations know how to draw and edit themselves.
/// The graph uses this only at its heterogeneous storage boundary.
pub trait InlineControl: Send + Sync + fmt::Debug {
    /// Draw an inline widget (number button, checkbox, text field, combo box…).
    /// Returns `true` if the value changed.
    fn draw_widget(
        &mut self,
        ui: &mut Ui,
        label: &str,
        rect: Rect,
        zoom: f32,
        clip_rect: Rect,
    ) -> bool;

}
