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

/// A socket on a node instance.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Socket {
    pub name: String,
    pub type_name: String,
    pub color: Color32,
    pub shape: SocketShape,
    /// Controlled by `on_update` — set false to suppress the socket entirely.
    pub visible: bool,
    /// Set true by the user via "Hide Unused"; never touched by `on_update`.
    #[serde(default)]
    pub hidden: bool,
    #[serde(default)]
    pub has_control: bool,
}
