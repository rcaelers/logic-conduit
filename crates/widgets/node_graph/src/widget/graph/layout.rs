use std::collections::HashMap;

use egui::{Pos2, Rect};

use super::widget::NodeGraphWidget;
use crate::model::{FrameId, NodeId, SocketDirection, SocketId};
use crate::support::{SOCKET_RADIUS, to_screen_rect};
use crate::widget::node::NodeWidget;

const SOCKET_HIT_PADDING: f32 = 5.0;
const FRAME_PADDING: f32 = 20.0;
const FRAME_TITLE_PADDING: f32 = 44.0;

pub(super) struct GraphWidgetLayout {
    pub nodes: HashMap<NodeId, NodeWidget>,
    pub node_rects: HashMap<NodeId, Rect>,
    pub frame_rects: HashMap<FrameId, Rect>,
    pub frame_screen_rects: HashMap<FrameId, Rect>,
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
        let mut frame_order: Vec<_> = self.graph.frames.iter().collect();
        frame_order.sort_by_key(|frame| frame.node_ids.len());
        let mut frame_rects = HashMap::new();
        for frame in frame_order {
            let mut bounds = frame
                .node_ids
                .iter()
                .filter_map(|id| node_rects.get(id).copied())
                .reduce(|bounds, rect| bounds.union(rect));
            for child in &self.graph.frames {
                if child.id == frame.id
                    || child.node_ids.len() >= frame.node_ids.len()
                    || !child.node_ids.iter().all(|id| frame.node_ids.contains(id))
                {
                    continue;
                }
                if let Some(&child_rect) = frame_rects.get(&child.id) {
                    bounds = Some(bounds.map_or(child_rect, |bounds| bounds.union(child_rect)));
                }
            }
            if let Some(bounds) = bounds {
                frame_rects.insert(
                    frame.id,
                    Rect::from_min_max(
                        Pos2::new(
                            bounds.min.x - FRAME_PADDING,
                            bounds.min.y - FRAME_TITLE_PADDING,
                        ),
                        Pos2::new(bounds.max.x + FRAME_PADDING, bounds.max.y + FRAME_PADDING),
                    ),
                );
            }
        }
        let frame_screen_rects: HashMap<FrameId, Rect> = frame_rects
            .iter()
            .map(|(&id, &rect)| (id, to_screen_rect(rect, &self.view, origin)))
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
            frame_rects,
            frame_screen_rects,
            node_screen_rects,
            header_screen_rects,
            collapse_toggle_screen_rects,
            socket_screen_pos,
            socket_hit_rects,
        }
    }
}
