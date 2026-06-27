use super::{NodeId, Socket, SocketShape};
use egui::{Color32, Pos2};
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub enum NodeKind {
    #[default]
    Regular,
    Reroute,
}

#[derive(Serialize, Deserialize)]
pub struct Node {
    pub id: NodeId,
    pub kind: NodeKind,
    pub title: String,
    pub header_color: Color32,
    pub pos: Pos2,
    pub inputs: Vec<Socket>,
    pub outputs: Vec<Socket>,
    #[serde(default)]
    pub collapsed: bool,
    #[serde(default)]
    pub state: Value,
    #[serde(skip)]
    pub(crate) property_count: usize,
    pub selected: bool,
}

impl Clone for Node {
    fn clone(&self) -> Self {
        Self {
            id: self.id,
            kind: self.kind.clone(),
            title: self.title.clone(),
            header_color: self.header_color,
            pos: self.pos,
            inputs: self.inputs.clone(),
            outputs: self.outputs.clone(),
            collapsed: self.collapsed,
            state: self.state.clone(),
            property_count: self.property_count,
            selected: self.selected,
        }
    }
}

impl Node {
    pub fn new_reroute(id: NodeId, pos: Pos2) -> Self {
        let input = Socket {
            name: String::new(),
            type_name: "Any".to_string(),
            color: Color32::from_rgb(150, 150, 150),
            shape: SocketShape::Circle,
            visible: true,
            hidden: false,
            has_control: false,
        };
        let output = Socket {
            name: String::new(),
            type_name: "Any".to_string(),
            color: Color32::from_rgb(150, 150, 150),
            shape: SocketShape::Circle,
            visible: true,
            hidden: false,
            has_control: false,
        };
        Self {
            id,
            kind: NodeKind::Reroute,
            title: String::new(),
            header_color: Color32::from_rgb(80, 80, 80),
            pos,
            inputs: vec![input],
            outputs: vec![output],
            collapsed: false,
            state: Value::Null,
            property_count: 0,
            selected: false,
        }
    }
}
