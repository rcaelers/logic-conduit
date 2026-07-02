use super::NodeId;
use egui::Color32;
use serde::{Deserialize, Serialize};

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
