use super::NodeGraphWidget;
use crate::{
    model::{NodeId, SocketDirection, SocketId},
    support::paint::{SOCKET_RADIUS, to_screen_rect},
    widget::node::NodeWidget,
};
use egui::{Pos2, Rect};
use std::collections::HashMap;

const SOCKET_HIT_PADDING: f32 = 5.0;

pub(super) struct GraphWidgetLayout {
    pub nodes: HashMap<NodeId, NodeWidget>,
    pub node_rects: HashMap<NodeId, Rect>,
    pub node_screen_rects: HashMap<NodeId, Rect>,
    pub header_screen_rects: HashMap<NodeId, Rect>,
    pub collapse_toggle_screen_rects: HashMap<NodeId, Rect>,
    pub socket_screen_pos: HashMap<SocketId, Pos2>,
    pub socket_hit_rects: HashMap<SocketId, Rect>,
}

impl NodeGraphWidget {
    pub(super) fn build_layout(&self, origin: Pos2) -> GraphWidgetLayout {
        let nodes: HashMap<NodeId, NodeWidget> = self
            .graph
            .nodes
            .iter()
            .map(|(&id, node)| (id, NodeWidget::new(node)))
            .collect();

        let node_rects: HashMap<NodeId, Rect> = nodes
            .iter()
            .map(|(&id, widget)| (id, widget.node_rect()))
            .collect();
        let node_screen_rects: HashMap<NodeId, Rect> = nodes
            .iter()
            .map(|(&id, widget)| (id, to_screen_rect(widget.node_rect(), &self.view, origin)))
            .collect();
        let header_screen_rects: HashMap<NodeId, Rect> = nodes
            .iter()
            .map(|(&id, widget)| (id, to_screen_rect(widget.header_rect(), &self.view, origin)))
            .collect();
        let collapse_toggle_screen_rects: HashMap<NodeId, Rect> = nodes
            .iter()
            .filter_map(|(&id, widget)| {
                widget
                    .collapse_toggle_rect()
                    .map(|rect| (id, to_screen_rect(rect, &self.view, origin)))
            })
            .collect();

        let mut socket_screen_pos = HashMap::new();
        let mut socket_hit_rects = HashMap::new();
        let socket_hit_radius = SOCKET_RADIUS * self.view.zoom + SOCKET_HIT_PADDING;
        for (&id, widget) in &nodes {
            let Some(node) = self.graph.nodes.get(&id) else {
                continue;
            };
            for i in 0..node.inputs.len() {
                if let Some(pos) = widget.input_socket_pos(i) {
                    let socket_id = SocketId {
                        node: id,
                        index: i,
                        direction: SocketDirection::Input,
                    };
                    let screen_pos = self.view.canvas_to_screen(origin, pos);
                    socket_screen_pos.insert(socket_id, screen_pos);
                    socket_hit_rects.insert(
                        socket_id,
                        Rect::from_center_size(
                            screen_pos,
                            egui::Vec2::splat(socket_hit_radius * 2.0),
                        ),
                    );
                }
            }
            for i in 0..node.outputs.len() {
                if let Some(pos) = widget.output_socket_pos(i) {
                    let socket_id = SocketId {
                        node: id,
                        index: i,
                        direction: SocketDirection::Output,
                    };
                    let screen_pos = self.view.canvas_to_screen(origin, pos);
                    socket_screen_pos.insert(socket_id, screen_pos);
                    socket_hit_rects.insert(
                        socket_id,
                        Rect::from_center_size(
                            screen_pos,
                            egui::Vec2::splat(socket_hit_radius * 2.0),
                        ),
                    );
                }
            }
        }

        GraphWidgetLayout {
            nodes,
            node_rects,
            node_screen_rects,
            header_screen_rects,
            collapse_toggle_screen_rects,
            socket_screen_pos,
            socket_hit_rects,
        }
    }
}
