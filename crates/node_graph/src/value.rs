use egui::{Rect, Ui};
use std::any::Any;
use std::fmt;

/// Type-erased value stored in input sockets and node properties.
/// Implementations know how to draw their own widget.
/// To access the concrete type, downcast via `as_any().downcast_ref::<ConcreteType>()`.
#[typetag::serde(tag = "__type")]
pub trait NodeValue: Send + Sync + fmt::Debug {
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

    fn clone_box(&self) -> Box<dyn NodeValue>;

    fn as_any(&self) -> &dyn Any;
}

impl Clone for Box<dyn NodeValue> {
    fn clone(&self) -> Self {
        self.clone_box()
    }
}
