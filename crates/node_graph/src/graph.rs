use crate::socket::SocketShape;
use egui::{Color32, Pos2};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{HashMap, HashSet};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct NodeId(pub u32);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SocketId {
    pub node: NodeId,
    pub index: usize,
    pub direction: SocketDirection,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum SocketDirection {
    Input,
    Output,
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
            state: Value::Null,
            property_count: 0,
            selected: false,
        }
    }

}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Connection {
    pub from: SocketId,
    pub to: SocketId,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct FrameId(pub u32);

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Frame {
    pub id: FrameId,
    pub label: String,
    pub color: Color32,
    pub node_ids: Vec<NodeId>,
}

#[derive(Default, Serialize, Deserialize)]
pub struct GraphState {
    pub nodes: HashMap<NodeId, Node>,
    pub connections: Vec<Connection>,
    pub frames: Vec<Frame>,
    next_id: u32,
    next_frame_id: u32,
}

impl GraphState {
    pub fn next_id(&mut self) -> NodeId {
        let id = NodeId(self.next_id);
        self.next_id += 1;
        id
    }

    pub fn add_node(&mut self, node: Node) {
        self.nodes.insert(node.id, node);
    }

    pub fn remove_node(&mut self, id: NodeId) {
        self.nodes.remove(&id);
        self.connections
            .retain(|connection| connection.from.node != id && connection.to.node != id);
    }

    pub fn add_connection(&mut self, from: SocketId, to: SocketId) {
        self.connections.retain(|connection| connection.to != to);
        self.connections.push(Connection { from, to });
    }

    pub fn is_input_connected(&self, socket: SocketId) -> bool {
        self.connections.iter().any(|connection| connection.to == socket)
    }

    pub fn sorted_node_ids(&self) -> Vec<NodeId> {
        let mut ids: Vec<NodeId> = self.nodes.keys().copied().collect();
        ids.sort_by_key(|id| id.0);
        ids
    }

    pub fn add_frame(&mut self, label: String, color: Color32, node_ids: Vec<NodeId>) -> FrameId {
        let id = FrameId(self.next_frame_id);
        self.next_frame_id += 1;
        self.frames.push(Frame {
            id,
            label,
            color,
            node_ids,
        });
        id
    }

    pub fn cleanup_frames(&mut self) {
        let alive: HashSet<NodeId> = self.nodes.keys().copied().collect();
        for frame in &mut self.frames {
            frame.node_ids.retain(|id| alive.contains(id));
        }
        self.frames.retain(|frame| !frame.node_ids.is_empty());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn graph_round_trips_node_data() {
        let mut graph = GraphState::default();
        let id = graph.next_id();
        graph.add_node(Node::new_reroute(id, Pos2::ZERO));

        let json = serde_json::to_string(&graph).expect("graph state should serialize");
        let loaded: GraphState = serde_json::from_str(&json).expect("graph state should deserialize");

        assert_eq!(loaded.nodes[&id].kind, NodeKind::Reroute);
    }
}
