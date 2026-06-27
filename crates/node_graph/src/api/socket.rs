use super::control::InlineControl;
use crate::model::SocketShape;
use egui::Color32;

pub trait SocketDef: 'static + Send + Sync {
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

pub trait SocketWithControlDef: SocketDef {
    type Control: InlineControl;
}

pub fn sockets_compatible(a: &str, b: &str) -> bool {
    a == "Any" || b == "Any" || a == b
}
