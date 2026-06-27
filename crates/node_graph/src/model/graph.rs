use super::{Connection, Frame, FrameId, Node, NodeId, SocketId};
use egui::Color32;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};

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
        self.connections
            .iter()
            .any(|connection| connection.to == socket)
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
    use crate::model::NodeKind;
    use egui::Pos2;

    #[test]
    fn graph_round_trips_node_data() {
        let mut graph = GraphState::default();
        let id = graph.next_id();
        graph.add_node(Node::new_reroute(id, Pos2::ZERO));

        let json = serde_json::to_string(&graph).expect("graph state should serialize");
        let loaded: GraphState =
            serde_json::from_str(&json).expect("graph state should deserialize");

        assert_eq!(loaded.nodes[&id].kind, NodeKind::Reroute);
    }
}
