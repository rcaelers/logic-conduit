use egui::Color32;
use serde::{Deserialize, Serialize};

use super::NodeId;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct FrameId(pub u32);

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Frame {
    pub id: FrameId,
    pub label: String,
    pub color: Color32,
    pub node_ids: Vec<NodeId>,
    #[serde(default)]
    pub selected: bool,
}
