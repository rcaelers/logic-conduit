use std::collections::{BTreeMap, HashMap, HashSet};

use egui::Color32;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

use super::{
    Connection, Frame, FrameId, Node, NodeId, NodeKind, Socket, SocketDirection, SocketId,
    VariadicInfo,
};

#[derive(Clone, Default, Serialize, Deserialize)]
pub struct GraphState {
    pub nodes: HashMap<NodeId, Node>,
    pub connections: Vec<Connection>,
    pub frames: Vec<Frame>,
    #[serde(flatten)]
    pub metadata: GraphMetadata,
}

#[derive(Clone, Default, Serialize, Deserialize)]
pub struct GraphMetadata {
    next_id: u32,
    next_frame_id: u32,
    /// Namespaced, host-owned document state. Generic graph code preserves
    /// these values without interpreting their contents.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    extensions: BTreeMap<String, serde_json::Value>,
}

impl GraphState {
    pub fn extension<T: DeserializeOwned>(
        &self,
        key: &str,
    ) -> Result<Option<T>, serde_json::Error> {
        self.metadata
            .extensions
            .get(key)
            .cloned()
            .map(serde_json::from_value)
            .transpose()
    }

    pub fn set_extension<T: Serialize>(
        &mut self,
        key: impl Into<String>,
        value: T,
    ) -> Result<(), serde_json::Error> {
        self.metadata
            .extensions
            .insert(key.into(), serde_json::to_value(value)?);
        Ok(())
    }

    pub fn remove_extension(&mut self, key: &str) {
        self.metadata.extensions.remove(key);
    }

    pub fn next_id(&mut self) -> NodeId {
        let id = NodeId(self.metadata.next_id);
        self.metadata.next_id += 1;
        id
    }

    pub fn add_node(&mut self, node: Node) {
        self.nodes.insert(node.id, node);
    }

    pub fn remove_node(&mut self, id: NodeId) {
        self.nodes.remove(&id);
        self.connections
            .retain(|connection| connection.to.node != id);
        // Outgoing connections are dropped one by one so each downstream
        // input reverts properly (resolution cleared, variadic members
        // removed with index fixup).
        while let Some(position) = self
            .connections
            .iter()
            .position(|connection| connection.from.node == id)
        {
            self.remove_connection_at(position);
        }
    }

    /// Adds a connection, replacing any existing one into `to`, and resolves
    /// the input socket's type to the output's type when they differ. When
    /// `to` is a variadic placeholder, it becomes a member and a new
    /// placeholder is spawned (until the group's max).
    pub fn add_connection(&mut self, from: SocketId, to: SocketId) {
        self.connections.retain(|connection| connection.to != to);
        self.connections.push(Connection { from, to });
        self.resolve_input(from, to);
        self.grow_variadic_group(to);
        self.propagate_reroute_output(to.node);
    }

    /// Removes the connection feeding `to`, if any, and reverts the input
    /// socket: back to its native type, or removed entirely if it is a
    /// variadic member. Returns whether a connection was removed.
    pub fn disconnect_input(&mut self, to: SocketId) -> bool {
        let before = self.connections.len();
        self.connections.retain(|connection| connection.to != to);
        let removed = self.connections.len() != before;
        if removed {
            self.on_input_disconnected(to);
        }
        removed
    }

    /// Removes the connection at `index` and reverts its input socket.
    pub fn remove_connection_at(&mut self, index: usize) -> Connection {
        let connection = self.connections.remove(index);
        self.on_input_disconnected(connection.to);
        connection
    }

    fn on_input_disconnected(&mut self, to: SocketId) {
        let is_member = self
            .nodes
            .get(&to.node)
            .and_then(|node| node.inputs.get(to.index))
            .is_some_and(Socket::is_variadic_member);
        if is_member {
            self.collapse_variadic_member(to);
        } else {
            self.clear_input_resolution(to);
        }
        self.propagate_reroute_output(to.node);
    }

    fn resolve_input(&mut self, from: SocketId, to: SocketId) {
        let out_type = self
            .nodes
            .get(&from.node)
            .and_then(|node| node.outputs.get(from.index))
            .map(|socket| socket.effective_type().to_owned());
        let Some(node) = self.nodes.get_mut(&to.node) else {
            return;
        };
        let Some(socket) = node.inputs.get_mut(to.index) else {
            return;
        };
        socket.resolved_type = match out_type {
            Some(t) if t != "Any" && t != socket.type_name => Some(t),
            _ => None,
        };
    }

    fn clear_input_resolution(&mut self, to: SocketId) {
        if let Some(socket) = self
            .nodes
            .get_mut(&to.node)
            .and_then(|node| node.inputs.get_mut(to.index))
        {
            socket.resolved_type = None;
        }
    }

    /// A reroute is transparent — its output should always mirror whatever
    /// flows into its input. Unlike a regular node's sockets (kept in sync by
    /// its own `on_update`), nothing else does this for a reroute, so its
    /// output's `resolved_type` — and therefore its socket-dot color *and*
    /// the color of any wire leaving it, both of which fall back to the
    /// static idle color while unresolved — used to just stay at the
    /// default gray forever. Called after a reroute's own input resolution
    /// changes; cascades forward through whatever the output feeds,
    /// including further chained reroutes.
    fn propagate_reroute_output(&mut self, node_id: NodeId) {
        let Some(node) = self.nodes.get(&node_id) else {
            return;
        };
        if node.kind != NodeKind::Reroute {
            return;
        }
        let input_type = node.inputs[0].effective_type().to_owned();
        let Some(node) = self.nodes.get_mut(&node_id) else {
            return;
        };
        node.outputs[0].resolved_type = (input_type != "Any").then_some(input_type);

        let output = SocketId {
            node: node_id,
            index: 0,
            direction: SocketDirection::Output,
        };
        let downstream: Vec<SocketId> = self
            .connections
            .iter()
            .filter(|c| c.from == output)
            .map(|c| c.to)
            .collect();
        for to in downstream {
            self.resolve_input(output, to);
            self.propagate_reroute_output(to.node);
        }
    }

    /// Recomputes every reroute's output resolution from its current input —
    /// a one-time correction after loading a graph that may have been saved
    /// before reroute outputs propagated their resolved type at all (they
    /// used to just render flat gray, wire included).
    pub(crate) fn fixup_reroute_outputs(&mut self) {
        let reroutes: Vec<NodeId> = self
            .nodes
            .iter()
            .filter(|(_, node)| node.kind == NodeKind::Reroute)
            .map(|(&id, _)| id)
            .collect();
        for id in reroutes {
            self.propagate_reroute_output(id);
        }
    }

    // ── Variadic groups ───────────────────────────────────────────────────────

    /// Converts a just-connected placeholder into a member and spawns a fresh
    /// placeholder after it while the group is below its max.
    fn grow_variadic_group(&mut self, to: SocketId) {
        let (def_index, info, template) = {
            let Some(socket) = self
                .nodes
                .get(&to.node)
                .and_then(|node| node.inputs.get(to.index))
            else {
                return;
            };
            let Some(info) = socket.variadic.clone() else {
                return;
            };
            if !info.placeholder {
                return;
            }
            (socket.def_index, info, socket.clone())
        };
        let Some(node) = self.nodes.get_mut(&to.node) else {
            return;
        };
        let members = node
            .inputs
            .iter()
            .filter(|socket| socket.def_index == def_index && socket.is_variadic_member())
            .count();
        let number = members + 1;
        let socket = &mut node.inputs[to.index];
        socket.variadic = Some(VariadicInfo {
            placeholder: false,
            ..info.clone()
        });
        socket.name = format!("{} {}", info.base, number);
        if number < info.max {
            let mut placeholder = template;
            placeholder.resolved_type = None;
            placeholder.name = info.base;
            self.insert_input_socket(to.node, to.index + 1, placeholder);
        }
    }

    /// Removes a disconnected variadic member, renumbers the remaining
    /// members, and restores the trailing placeholder if the group had been
    /// at its max.
    fn collapse_variadic_member(&mut self, to: SocketId) {
        let (def_index, info) = {
            let Some(socket) = self
                .nodes
                .get(&to.node)
                .and_then(|node| node.inputs.get(to.index))
            else {
                return;
            };
            let Some(info) = socket.variadic.clone() else {
                return;
            };
            if info.placeholder {
                return;
            }
            (socket.def_index, info)
        };
        let Some(removed) = self.remove_input_socket(to.node, to.index) else {
            return;
        };

        let Some(node) = self.nodes.get_mut(&to.node) else {
            return;
        };
        let mut members = 0usize;
        let mut group_end = None;
        let mut has_placeholder = false;
        for (index, socket) in node.inputs.iter_mut().enumerate() {
            if socket.def_index != def_index {
                continue;
            }
            group_end = Some(index);
            if socket.is_variadic_member() {
                members += 1;
                socket.name = format!("{} {}", info.base, members);
            } else if socket.is_variadic_placeholder() {
                has_placeholder = true;
            }
        }
        if !has_placeholder && members < info.max {
            let mut placeholder = removed;
            placeholder.resolved_type = None;
            placeholder.name = info.base.clone();
            placeholder.variadic = Some(VariadicInfo {
                placeholder: true,
                ..info
            });
            let insert_at = group_end.map_or(to.index, |index| index + 1);
            self.insert_input_socket(to.node, insert_at, placeholder);
        }
    }

    /// Inserts an input socket, shifting the indices of existing connections
    /// into this node accordingly.
    fn insert_input_socket(&mut self, node_id: NodeId, index: usize, socket: Socket) {
        let Some(node) = self.nodes.get_mut(&node_id) else {
            return;
        };
        let index = index.min(node.inputs.len());
        node.inputs.insert(index, socket);
        for connection in &mut self.connections {
            if connection.to.node == node_id && connection.to.index >= index {
                connection.to.index += 1;
            }
        }
    }

    /// Removes an input socket, shifting the indices of existing connections
    /// into this node accordingly. Any connection to the removed socket is
    /// dropped.
    fn remove_input_socket(&mut self, node_id: NodeId, index: usize) -> Option<Socket> {
        let node = self.nodes.get_mut(&node_id)?;
        if index >= node.inputs.len() {
            return None;
        }
        let socket = node.inputs.remove(index);
        self.connections
            .retain(|connection| !(connection.to.node == node_id && connection.to.index == index));
        for connection in &mut self.connections {
            if connection.to.node == node_id && connection.to.index > index {
                connection.to.index -= 1;
            }
        }
        Some(socket)
    }

    /// Reverts inputs of `ids` that have no incoming connection — used after
    /// pasting, where a socket may have been copied resolved (or as a grown
    /// variadic member) while its feeding connection was not part of the
    /// payload.
    pub fn prune_unconnected_resolutions(&mut self, ids: &[NodeId]) {
        for &id in ids {
            // Collapse unconnected variadic members one at a time: each
            // removal shifts indices, so recompute between iterations.
            loop {
                let connected: HashSet<usize> = self
                    .connections
                    .iter()
                    .filter(|connection| connection.to.node == id)
                    .map(|connection| connection.to.index)
                    .collect();
                let Some(node) = self.nodes.get(&id) else {
                    break;
                };
                let victim = node.inputs.iter().enumerate().find_map(|(index, socket)| {
                    (socket.is_variadic_member() && !connected.contains(&index)).then_some(index)
                });
                let Some(index) = victim else {
                    break;
                };
                self.collapse_variadic_member(SocketId {
                    node: id,
                    index,
                    direction: crate::model::SocketDirection::Input,
                });
            }

            let connected: HashSet<usize> = self
                .connections
                .iter()
                .filter(|connection| connection.to.node == id)
                .map(|connection| connection.to.index)
                .collect();
            let Some(node) = self.nodes.get_mut(&id) else {
                continue;
            };
            for (index, socket) in node.inputs.iter_mut().enumerate() {
                if socket.resolved_type.is_some() && !connected.contains(&index) {
                    socket.resolved_type = None;
                }
            }
        }
    }

    pub fn is_input_connected(&self, socket: SocketId) -> bool {
        self.connections
            .iter()
            .any(|connection| connection.to == socket)
    }

    pub fn is_output_connected(&self, socket: SocketId) -> bool {
        self.connections
            .iter()
            .any(|connection| connection.from == socket)
    }

    pub fn sorted_node_ids(&self) -> Vec<NodeId> {
        let mut ids: Vec<NodeId> = self.nodes.keys().copied().collect();
        ids.sort_by_key(|id| id.0);
        ids
    }

    pub fn add_frame(&mut self, label: String, color: Color32, node_ids: Vec<NodeId>) -> FrameId {
        let id = FrameId(self.metadata.next_frame_id);
        self.metadata.next_frame_id += 1;
        self.frames.push(Frame {
            id,
            label,
            color,
            node_ids,
            selected: false,
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
    use egui::{Color32, Pos2};

    use super::*;
    use crate::model::{NodeKind, Socket, SocketDirection, SocketShape};

    fn socket(type_name: &str, allowed: &[&str]) -> Socket {
        Socket {
            name: String::new(),
            type_name: type_name.to_owned(),
            color: Color32::GRAY,
            shape: SocketShape::Circle,
            allowed: allowed.iter().map(|s| s.to_string()).collect(),
            resolved_type: None,
            def_index: 0,
            variadic: None,
            visible: true,
            hidden: false,
            has_control: false,
            show_in_view: false,
        }
    }

    fn node_with_sockets(id: NodeId, inputs: Vec<Socket>, outputs: Vec<Socket>) -> Node {
        let mut node = Node::new_reroute(id, Pos2::ZERO);
        node.kind = NodeKind::Regular;
        node.inputs = inputs;
        node.outputs = outputs;
        node
    }

    fn sid(node: NodeId, index: usize, direction: SocketDirection) -> SocketId {
        SocketId {
            node,
            index,
            direction,
        }
    }

    #[test]
    fn socket_accepts_native_allowed_and_any() {
        let s = socket("Signal", &["Float", "Int"]);
        assert!(s.accepts("Signal"));
        assert!(s.accepts("Float"));
        assert!(s.accepts("Int"));
        assert!(s.accepts("Any"));
        assert!(!s.accepts("Protocol"));
    }

    #[test]
    fn connect_resolves_and_disconnect_reverts() {
        let mut graph = GraphState::default();
        let src = graph.next_id();
        let dst = graph.next_id();
        graph.add_node(node_with_sockets(src, vec![], vec![socket("Float", &[])]));
        graph.add_node(node_with_sockets(
            dst,
            vec![socket("Signal", &["Float"])],
            vec![],
        ));

        let from = sid(src, 0, SocketDirection::Output);
        let to = sid(dst, 0, SocketDirection::Input);
        assert!(!graph.is_output_connected(from));
        graph.add_connection(from, to);
        assert!(graph.is_output_connected(from));
        assert_eq!(
            graph.nodes[&dst].inputs[0].resolved_type.as_deref(),
            Some("Float")
        );
        assert_eq!(graph.nodes[&dst].inputs[0].effective_type(), "Float");

        assert!(graph.disconnect_input(to));
        assert!(!graph.is_output_connected(from));
        assert_eq!(graph.nodes[&dst].inputs[0].resolved_type, None);
        assert_eq!(graph.nodes[&dst].inputs[0].effective_type(), "Signal");
    }

    #[test]
    fn reroute_output_mirrors_its_input_when_connected_and_disconnected() {
        let mut graph = GraphState::default();
        let src = graph.next_id();
        let reroute_id = graph.next_id();
        graph.add_node(node_with_sockets(src, vec![], vec![socket("Float", &[])]));
        graph.add_node(Node::new_reroute(reroute_id, Pos2::ZERO));

        graph.add_connection(
            sid(src, 0, SocketDirection::Output),
            sid(reroute_id, 0, SocketDirection::Input),
        );
        assert_eq!(
            graph.nodes[&reroute_id].outputs[0].resolved_type.as_deref(),
            Some("Float"),
            "the reroute's output should mirror its resolved input type"
        );
        assert_eq!(
            graph.nodes[&reroute_id].outputs[0].effective_type(),
            "Float"
        );

        graph.disconnect_input(sid(reroute_id, 0, SocketDirection::Input));
        assert_eq!(
            graph.nodes[&reroute_id].outputs[0].resolved_type, None,
            "disconnecting the input should revert the output back to Any"
        );
    }

    #[test]
    fn reroute_output_propagation_cascades_through_a_chain() {
        let mut graph = GraphState::default();
        let src = graph.next_id();
        let reroute_a = graph.next_id();
        let reroute_b = graph.next_id();
        graph.add_node(node_with_sockets(src, vec![], vec![socket("Words", &[])]));
        graph.add_node(Node::new_reroute(reroute_a, Pos2::ZERO));
        graph.add_node(Node::new_reroute(reroute_b, Pos2::ZERO));

        graph.add_connection(
            sid(src, 0, SocketDirection::Output),
            sid(reroute_a, 0, SocketDirection::Input),
        );
        graph.add_connection(
            sid(reroute_a, 0, SocketDirection::Output),
            sid(reroute_b, 0, SocketDirection::Input),
        );

        assert_eq!(graph.nodes[&reroute_a].outputs[0].effective_type(), "Words");
        assert_eq!(
            graph.nodes[&reroute_b].inputs[0].effective_type(),
            "Words",
            "reroute B's input should have resolved from A's now-correct output"
        );
        assert_eq!(
            graph.nodes[&reroute_b].outputs[0].effective_type(),
            "Words",
            "propagation should cascade all the way through the chain"
        );
    }

    #[test]
    fn fixup_reroute_outputs_corrects_a_stale_load() {
        // Simulates a graph saved before reroute outputs propagated at all:
        // the connection and the input's resolution are both present and
        // correct, but the output was never updated to match.
        let mut graph = GraphState::default();
        let src = graph.next_id();
        let reroute_id = graph.next_id();
        graph.add_node(node_with_sockets(src, vec![], vec![socket("Trigger", &[])]));
        graph.add_node(Node::new_reroute(reroute_id, Pos2::ZERO));
        graph.add_connection(
            sid(src, 0, SocketDirection::Output),
            sid(reroute_id, 0, SocketDirection::Input),
        );
        // Force it back to the stale/buggy state after the (correct) connect.
        graph.nodes.get_mut(&reroute_id).unwrap().outputs[0].resolved_type = None;
        assert_eq!(graph.nodes[&reroute_id].outputs[0].effective_type(), "Any");

        graph.fixup_reroute_outputs();

        assert_eq!(
            graph.nodes[&reroute_id].outputs[0].effective_type(),
            "Trigger"
        );
    }

    #[test]
    fn connect_same_type_does_not_resolve() {
        let mut graph = GraphState::default();
        let src = graph.next_id();
        let dst = graph.next_id();
        graph.add_node(node_with_sockets(src, vec![], vec![socket("Signal", &[])]));
        graph.add_node(node_with_sockets(
            dst,
            vec![socket("Signal", &["Float"])],
            vec![],
        ));

        graph.add_connection(
            sid(src, 0, SocketDirection::Output),
            sid(dst, 0, SocketDirection::Input),
        );
        assert_eq!(graph.nodes[&dst].inputs[0].resolved_type, None);
    }

    #[test]
    fn removing_source_node_reverts_downstream_inputs() {
        let mut graph = GraphState::default();
        let src = graph.next_id();
        let dst = graph.next_id();
        graph.add_node(node_with_sockets(src, vec![], vec![socket("Int", &[])]));
        graph.add_node(node_with_sockets(
            dst,
            vec![socket("Signal", &["Int"])],
            vec![],
        ));

        graph.add_connection(
            sid(src, 0, SocketDirection::Output),
            sid(dst, 0, SocketDirection::Input),
        );
        assert!(graph.nodes[&dst].inputs[0].resolved_type.is_some());

        graph.remove_node(src);
        assert!(graph.connections.is_empty());
        assert_eq!(graph.nodes[&dst].inputs[0].resolved_type, None);
    }

    #[test]
    fn prune_clears_resolution_without_connection() {
        let mut graph = GraphState::default();
        let dst = graph.next_id();
        let mut node = node_with_sockets(dst, vec![socket("Signal", &["Float"])], vec![]);
        node.inputs[0].resolved_type = Some("Float".to_owned());
        graph.add_node(node);

        graph.prune_unconnected_resolutions(&[dst]);
        assert_eq!(graph.nodes[&dst].inputs[0].resolved_type, None);
    }

    fn variadic_placeholder(type_name: &str, base: &str, max: usize) -> Socket {
        let mut s = socket(type_name, &[]);
        s.name = base.to_owned();
        s.variadic = Some(VariadicInfo {
            base: base.to_owned(),
            max,
            placeholder: true,
        });
        s
    }

    /// Source node with `count` Signal outputs.
    fn source(graph: &mut GraphState, count: usize) -> NodeId {
        let id = graph.next_id();
        let outputs = (0..count).map(|_| socket("Signal", &[])).collect();
        graph.add_node(node_with_sockets(id, vec![], outputs));
        id
    }

    #[test]
    fn connecting_placeholder_grows_group() {
        let mut graph = GraphState::default();
        let src = source(&mut graph, 2);
        let dst = graph.next_id();
        graph.add_node(node_with_sockets(
            dst,
            vec![variadic_placeholder("Signal", "Ch", 4)],
            vec![],
        ));

        graph.add_connection(
            sid(src, 0, SocketDirection::Output),
            sid(dst, 0, SocketDirection::Input),
        );

        let inputs = &graph.nodes[&dst].inputs;
        assert_eq!(inputs.len(), 2);
        assert!(inputs[0].is_variadic_member());
        assert_eq!(inputs[0].name, "Ch 1");
        assert!(inputs[1].is_variadic_placeholder());
        assert_eq!(inputs[1].name, "Ch");

        graph.add_connection(
            sid(src, 1, SocketDirection::Output),
            sid(dst, 1, SocketDirection::Input),
        );
        let inputs = &graph.nodes[&dst].inputs;
        assert_eq!(inputs.len(), 3);
        assert_eq!(inputs[1].name, "Ch 2");
        assert!(inputs[2].is_variadic_placeholder());
    }

    #[test]
    fn group_stops_growing_at_max() {
        let mut graph = GraphState::default();
        let src = source(&mut graph, 2);
        let dst = graph.next_id();
        graph.add_node(node_with_sockets(
            dst,
            vec![variadic_placeholder("Signal", "Ch", 2)],
            vec![],
        ));

        graph.add_connection(
            sid(src, 0, SocketDirection::Output),
            sid(dst, 0, SocketDirection::Input),
        );
        graph.add_connection(
            sid(src, 1, SocketDirection::Output),
            sid(dst, 1, SocketDirection::Input),
        );

        let inputs = &graph.nodes[&dst].inputs;
        assert_eq!(inputs.len(), 2);
        assert!(inputs.iter().all(Socket::is_variadic_member));
    }

    #[test]
    fn disconnecting_member_removes_it_and_renumbers() {
        let mut graph = GraphState::default();
        let src = source(&mut graph, 3);
        let dst = graph.next_id();
        graph.add_node(node_with_sockets(
            dst,
            vec![variadic_placeholder("Signal", "Ch", 4)],
            vec![],
        ));
        for i in 0..3 {
            graph.add_connection(
                sid(src, i, SocketDirection::Output),
                sid(dst, i, SocketDirection::Input),
            );
        }
        assert_eq!(graph.nodes[&dst].inputs.len(), 4);

        // Remove the middle member; the two remaining members renumber and
        // the connection into "Ch 3" shifts down to keep pointing at it.
        graph.disconnect_input(sid(dst, 1, SocketDirection::Input));
        let inputs = &graph.nodes[&dst].inputs;
        assert_eq!(inputs.len(), 3);
        assert_eq!(inputs[0].name, "Ch 1");
        assert_eq!(inputs[1].name, "Ch 2");
        assert!(inputs[2].is_variadic_placeholder());
        assert_eq!(graph.connections.len(), 2);
        assert!(
            graph
                .connections
                .iter()
                .any(|c| c.from.index == 2 && c.to.index == 1)
        );
    }

    #[test]
    fn disconnecting_member_at_max_restores_placeholder() {
        let mut graph = GraphState::default();
        let src = source(&mut graph, 2);
        let dst = graph.next_id();
        graph.add_node(node_with_sockets(
            dst,
            vec![variadic_placeholder("Signal", "Ch", 2)],
            vec![],
        ));
        graph.add_connection(
            sid(src, 0, SocketDirection::Output),
            sid(dst, 0, SocketDirection::Input),
        );
        graph.add_connection(
            sid(src, 1, SocketDirection::Output),
            sid(dst, 1, SocketDirection::Input),
        );

        graph.disconnect_input(sid(dst, 0, SocketDirection::Input));
        let inputs = &graph.nodes[&dst].inputs;
        assert_eq!(inputs.len(), 2);
        assert!(inputs[0].is_variadic_member());
        assert_eq!(inputs[0].name, "Ch 1");
        assert!(inputs[1].is_variadic_placeholder());
    }

    #[test]
    fn removing_source_collapses_variadic_members() {
        let mut graph = GraphState::default();
        let src = source(&mut graph, 2);
        let dst = graph.next_id();
        graph.add_node(node_with_sockets(
            dst,
            vec![variadic_placeholder("Signal", "Ch", 4)],
            vec![],
        ));
        graph.add_connection(
            sid(src, 0, SocketDirection::Output),
            sid(dst, 0, SocketDirection::Input),
        );
        graph.add_connection(
            sid(src, 1, SocketDirection::Output),
            sid(dst, 1, SocketDirection::Input),
        );

        graph.remove_node(src);
        let inputs = &graph.nodes[&dst].inputs;
        assert_eq!(inputs.len(), 1);
        assert!(inputs[0].is_variadic_placeholder());
        assert!(graph.connections.is_empty());
    }

    #[test]
    fn variadic_grow_shifts_connections_of_later_inputs() {
        let mut graph = GraphState::default();
        let src = source(&mut graph, 2);
        let dst = graph.next_id();
        // Variadic group followed by a static input.
        let mut static_input = socket("Signal", &[]);
        static_input.def_index = 1;
        graph.add_node(node_with_sockets(
            dst,
            vec![variadic_placeholder("Signal", "Ch", 4), static_input],
            vec![],
        ));

        // Connect the static input first (index 1), then grow the group.
        graph.add_connection(
            sid(src, 0, SocketDirection::Output),
            sid(dst, 1, SocketDirection::Input),
        );
        graph.add_connection(
            sid(src, 1, SocketDirection::Output),
            sid(dst, 0, SocketDirection::Input),
        );

        // The placeholder insert shifted the static input to index 2.
        let inputs = &graph.nodes[&dst].inputs;
        assert_eq!(inputs.len(), 3);
        assert!(inputs[1].is_variadic_placeholder());
        assert!(
            graph
                .connections
                .iter()
                .any(|c| c.from.index == 0 && c.to.index == 2)
        );
    }

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

    #[test]
    fn namespaced_document_extensions_round_trip_but_empty_maps_stay_compatible() {
        let mut graph = GraphState::default();
        assert!(
            !serde_json::to_value(&graph)
                .unwrap()
                .as_object()
                .unwrap()
                .contains_key("extensions")
        );

        graph.set_extension("example.selection", NodeId(7)).unwrap();
        let json = serde_json::to_string(&graph).unwrap();
        let loaded: GraphState = serde_json::from_str(&json).unwrap();
        assert_eq!(
            loaded.extension::<NodeId>("example.selection").unwrap(),
            Some(NodeId(7))
        );

        let legacy: GraphState = serde_json::from_str(
            r#"{"nodes":{},"connections":[],"frames":[],"next_id":0,"next_frame_id":0}"#,
        )
        .unwrap();
        assert_eq!(
            legacy.extension::<NodeId>("example.selection").unwrap(),
            None
        );
    }
}
