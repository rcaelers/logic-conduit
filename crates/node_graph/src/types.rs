use crate::value::InlineControl;
use egui::Color32;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum SocketShape {
    #[default]
    Circle,
    Diamond,
    Square,
    Triangle,
}

/// Trait for defining socket types — implement for custom types.
/// Built-in types (`BoolSocket`, `IntSocket`, etc.) use this same API.
pub trait SocketDef: 'static + Send + Sync {
    /// Concrete data carried by connections of this socket type.
    type Value: 'static + Send + Sync;

    fn type_name() -> &'static str
    where
        Self: Sized;
    fn color() -> Color32
    where
        Self: Sized;
    fn shape() -> SocketShape
    where
        Self: Sized,
    {
        SocketShape::Circle
    }
}

/// A socket definition that supports an editable inline control when unconnected.
pub trait SocketWithControlDef: SocketDef {
    type Control: InlineControl;
}

pub fn sockets_compatible(a: &str, b: &str) -> bool {
    a == "Any" || b == "Any" || a == b
}
