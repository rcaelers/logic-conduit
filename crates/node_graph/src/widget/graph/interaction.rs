use super::{
    NodeGraphWidget,
    action::ActionEffect,
    layout::GraphWidgetLayout,
    menu::{build_context_entries, build_empty_canvas_entries},
    minimap,
};
use crate::{
    model::{FrameId, NodeId, SocketDirection, SocketId},
    support::paint::{bezier_wire_distance, wire_intersects_knife},
    widget::{menu::dispatch_menu_shortcut, node::NodeWidget},
};
use egui::{Pos2, Rect, Vec2};
use std::collections::HashMap;

const WIRE_INSERT_THRESHOLD: f32 = 40.0;
const WIRE_SNAP_DISTANCE: f32 = 18.0;

#[derive(Default)]
pub(super) enum InteractionState {
    #[default]
    Idle,
    DraggingNode {
        node_id: NodeId,
        offset: Vec2,
    },
    DraggingFrame {
        frame_id: FrameId,
        last_canvas: Pos2,
    },
    DraggingWire {
        from: SocketId,
        from_canvas: Pos2,
        current_canvas: Pos2,
    },
    Panning {
        last_screen: Pos2,
    },
    BoxSelecting {
        start_canvas: Pos2,
        current_canvas: Pos2,
    },
    /// Ctrl+right-drag draws a freeform path; wires it crosses are cut on release.
    CuttingWire {
        path: Vec<Pos2>,
    },
}

impl InteractionState {
    pub(super) fn is_active(&self) -> bool {
        !matches!(self, Self::Idle)
    }

    pub(super) fn use_fast_rendering(&self) -> bool {
        matches!(
            self,
            Self::Panning { .. } | Self::DraggingNode { .. } | Self::DraggingFrame { .. }
        )
    }
}

pub(super) struct NodeResponses {
    pub body: egui::Response,
    pub header: egui::Response,
}

pub(super) struct MinimapResponse {
    pub response: egui::Response,
    pub info: minimap::MinimapInfo,
}

pub(super) struct GraphResponses {
    pub canvas: egui::Response,
    pub frames: HashMap<FrameId, egui::Response>,
    pub nodes: HashMap<NodeId, NodeResponses>,
    pub collapse_toggles: HashMap<NodeId, egui::Response>,
    pub sockets: HashMap<SocketId, egui::Response>,
    pub minimap: Option<MinimapResponse>,
}

enum ContextClickTarget {
    Canvas,
    Node(NodeId),
    Frame(FrameId),
}

impl GraphResponses {
    pub(super) fn canvas_only(canvas: egui::Response) -> Self {
        Self {
            canvas,
            frames: HashMap::new(),
            nodes: HashMap::new(),
            collapse_toggles: HashMap::new(),
            sockets: HashMap::new(),
            minimap: None,
        }
    }
}

impl NodeGraphWidget {
    fn compatible_wire_target(&self, from: SocketId, to: SocketId) -> bool {
        if from == to {
            return false;
        }
        let (output, input) = if from.direction == SocketDirection::Output {
            (from, to)
        } else {
            (to, from)
        };
        if output.direction != SocketDirection::Output || input.direction != SocketDirection::Input
        {
            return false;
        }
        let out_type = self
            .graph
            .nodes
            .get(&output.node)
            .and_then(|n| n.outputs.get(output.index))
            .map(|s| s.effective_type());
        let in_socket = self
            .graph
            .nodes
            .get(&input.node)
            .and_then(|n| n.inputs.get(input.index));
        matches!((out_type, in_socket), (Some(ot), Some(is)) if is.accepts(ot))
    }

    pub(super) fn snapped_wire_target(
        &self,
        from: SocketId,
        pointer_canvas: Pos2,
        layout: &GraphWidgetLayout,
    ) -> Option<(SocketId, Pos2)> {
        let threshold = WIRE_SNAP_DISTANCE / self.view.zoom;
        layout
            .nodes
            .iter()
            .flat_map(|(&node_id, widget)| {
                let input_count = self
                    .graph
                    .nodes
                    .get(&node_id)
                    .map_or(0, |node| node.inputs.len());
                let output_count = self
                    .graph
                    .nodes
                    .get(&node_id)
                    .map_or(0, |node| node.outputs.len());
                let inputs = (0..input_count).filter_map(move |index| {
                    widget.input_socket_pos(index).map(|pos| {
                        (
                            SocketId {
                                node: node_id,
                                index,
                                direction: SocketDirection::Input,
                            },
                            pos,
                        )
                    })
                });
                let outputs = (0..output_count).filter_map(move |index| {
                    widget.output_socket_pos(index).map(|pos| {
                        (
                            SocketId {
                                node: node_id,
                                index,
                                direction: SocketDirection::Output,
                            },
                            pos,
                        )
                    })
                });
                inputs.chain(outputs)
            })
            .filter(|(target, _)| self.compatible_wire_target(from, *target))
            .filter_map(|(target, pos)| {
                let dist = pointer_canvas.distance(pos);
                (dist <= threshold).then_some((target, pos, dist))
            })
            .min_by(|(_, _, a), (_, _, b)| a.total_cmp(b))
            .map(|(target, pos, _)| (target, pos))
    }

    fn add_wire_connection(&mut self, from: SocketId, to: SocketId) {
        let (output, input) = if from.direction == SocketDirection::Output {
            (from, to)
        } else {
            (to, from)
        };
        if self.compatible_wire_target(from, to) {
            self.push_undo_snapshot();
            self.graph.add_connection(output, input);
            self.run_update(output.node);
            self.run_update(input.node);
        }
    }

    pub(super) fn allocate_responses(
        &self,
        ui: &mut egui::Ui,
        canvas_response: egui::Response,
        layout: &GraphWidgetLayout,
        canvas_rect: Rect,
    ) -> GraphResponses {
        let frames = layout
            .frame_screen_rects
            .iter()
            .map(|(&id, &rect)| {
                (
                    id,
                    ui.interact(
                        rect,
                        ui.id().with(("frame", id.0)),
                        egui::Sense::click_and_drag(),
                    ),
                )
            })
            .collect();

        let mut nodes = HashMap::new();
        for (&id, &body_rect) in &layout.node_screen_rects {
            let Some(&header_rect) = layout.header_screen_rects.get(&id) else {
                continue;
            };
            let body = ui.interact(
                body_rect,
                ui.id().with(("node-body", id.0)),
                egui::Sense::click(),
            );
            let header = ui.interact(
                header_rect,
                ui.id().with(("node-header", id.0)),
                egui::Sense::click_and_drag(),
            );
            nodes.insert(id, NodeResponses { body, header });
        }

        let sockets = layout
            .socket_hit_rects
            .iter()
            .map(|(&socket_id, &rect)| {
                (
                    socket_id,
                    ui.interact(
                        rect,
                        ui.id().with((
                            "socket",
                            socket_id.node.0,
                            socket_id.index,
                            socket_id.direction,
                        )),
                        egui::Sense::click_and_drag(),
                    ),
                )
            })
            .collect();
        let collapse_toggles = layout
            .collapse_toggle_screen_rects
            .iter()
            .map(|(&node_id, &rect)| {
                (
                    node_id,
                    ui.interact(
                        rect,
                        ui.id().with(("collapse-toggle", node_id.0)),
                        egui::Sense::click(),
                    ),
                )
            })
            .collect();

        let minimap = self.minimap_visible.then(|| {
            let (info, rect) =
                minimap::compute_minimap(layout.node_rects.values().copied(), canvas_rect);
            let response =
                ui.interact(rect, ui.id().with("minimap"), egui::Sense::click_and_drag());
            MinimapResponse { response, info }
        });

        GraphResponses {
            canvas: canvas_response,
            frames,
            nodes,
            collapse_toggles,
            sockets,
            minimap,
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn idle_transition(
        &mut self,
        ui: &egui::Ui,
        responses: &GraphResponses,
        pointer_canvas: Option<Pos2>,
        origin: Pos2,
        layout: &GraphWidgetLayout,
    ) -> InteractionState {
        let Some(pc) = pointer_canvas else {
            return InteractionState::Idle;
        };

        let current_screen_pos = self.view.canvas_to_screen(origin, pc);
        let press_screen_pos = ui
            .input(|i| i.pointer.press_origin())
            .unwrap_or(current_screen_pos);

        if ui.input(|i| i.pointer.button_down(egui::PointerButton::Secondary)) {
            return InteractionState::Idle;
        }
        let ctrl = ui.input(|i| i.modifiers.ctrl);

        for (&sid, response) in &responses.sockets {
            if !response.drag_started() {
                continue;
            }
            let Some(&spos) = layout.socket_screen_pos.get(&sid) else {
                continue;
            };
            if sid.direction == SocketDirection::Input
                && let Some(src) = self
                    .graph
                    .connections
                    .iter()
                    .find(|c| c.to == sid)
                    .map(|c| c.from)
                && let Some(&src_spos) = layout.socket_screen_pos.get(&src)
            {
                self.push_undo_snapshot();
                self.graph.disconnect_input(sid);
                self.run_update(sid.node);
                return InteractionState::DraggingWire {
                    from: src,
                    from_canvas: self.view.screen_to_canvas(origin, src_spos),
                    current_canvas: pc,
                };
            }
            return InteractionState::DraggingWire {
                from: sid,
                from_canvas: self.view.screen_to_canvas(origin, spos),
                current_canvas: pc,
            };
        }

        for (&id, response) in &responses.collapse_toggles {
            if response.clicked() {
                self.push_undo_snapshot();
                self.toggle_collapsed_for_node(id);
                return InteractionState::Idle;
            }
        }

        for (&id, responses) in &responses.nodes {
            if responses.body.clicked() || responses.header.clicked() {
                self.select_node(id, ctrl);
                return InteractionState::Idle;
            }
            if responses.header.drag_started() {
                if let Some(node) = self.graph.nodes.get(&id) {
                    let node_pos = node.pos.to_vec2();
                    if !node.selected || ctrl {
                        self.select_node(id, ctrl);
                    }
                    self.push_undo_snapshot();
                    return InteractionState::DraggingNode {
                        node_id: id,
                        offset: pc.to_vec2() - node_pos,
                    };
                }
            }
        }

        if responses.frames.values().any(egui::Response::clicked)
            && self
                .node_at_screen_pos(responses, current_screen_pos)
                .is_none()
            && let Some(id) = self.frame_at_screen_pos(responses, layout, current_screen_pos)
        {
            self.select_frame(id, ctrl);
            return InteractionState::Idle;
        }

        if responses.frames.values().any(egui::Response::drag_started) {
            if self
                .node_at_screen_pos(responses, press_screen_pos)
                .is_some()
            {
                return InteractionState::Idle;
            }
            if let Some(id) = self.frame_at_screen_pos(responses, layout, press_screen_pos) {
                self.select_frame(id, ctrl);
                self.push_undo_snapshot();
                return InteractionState::DraggingFrame {
                    frame_id: id,
                    last_canvas: pc,
                };
            }
        }

        if responses.canvas.clicked() && !ctrl {
            for node in self.graph.nodes.values_mut() {
                node.selected = false;
            }
            for frame in &mut self.graph.frames {
                frame.selected = false;
            }
        }

        if responses.canvas.drag_started() {
            return InteractionState::BoxSelecting {
                start_canvas: pc,
                current_canvas: pc,
            };
        }

        InteractionState::Idle
    }

    fn update_panning(
        &mut self,
        response: &egui::Response,
        pointer: Option<Pos2>,
        last_screen: Pos2,
    ) -> InteractionState {
        if response.dragged()
            && let Some(pp) = pointer
        {
            self.view.pan += pp - last_screen;
            return InteractionState::Panning { last_screen: pp };
        }
        InteractionState::Idle
    }

    fn update_drag_node(
        &mut self,
        ui: &egui::Ui,
        pointer_canvas: Option<Pos2>,
        node_id: NodeId,
        offset: Vec2,
        nodes: &HashMap<NodeId, NodeWidget>,
    ) -> InteractionState {
        if ui.input(|i| i.pointer.button_down(egui::PointerButton::Primary)) {
            if let Some(pc) = pointer_canvas {
                let new_pos = (pc.to_vec2() - offset).to_pos2();
                let delta = self
                    .graph
                    .nodes
                    .get(&node_id)
                    .map(|n| new_pos - n.pos)
                    .unwrap_or(Vec2::ZERO);
                for n in self.graph.nodes.values_mut() {
                    if n.selected {
                        n.pos += delta;
                    }
                }
            }
            return InteractionState::DraggingNode { node_id, offset };
        }
        let has_io = !self.graph.nodes[&node_id].inputs.is_empty()
            && !self.graph.nodes[&node_id].outputs.is_empty();
        if has_io {
            self.try_wire_insert(node_id, nodes);
        }
        InteractionState::Idle
    }

    fn update_drag_frame(
        &mut self,
        ui: &egui::Ui,
        pointer_canvas: Option<Pos2>,
        frame_id: FrameId,
        last_canvas: Pos2,
    ) -> InteractionState {
        if ui.input(|i| i.pointer.button_down(egui::PointerButton::Primary)) {
            if let Some(pc) = pointer_canvas {
                let delta = pc - last_canvas;
                self.move_selected_frame_nodes(frame_id, delta);
                return InteractionState::DraggingFrame {
                    frame_id,
                    last_canvas: pc,
                };
            }
            return InteractionState::DraggingFrame {
                frame_id,
                last_canvas,
            };
        }
        InteractionState::Idle
    }

    fn try_wire_insert(&mut self, node_id: NodeId, nodes: &HashMap<NodeId, NodeWidget>) {
        let Some(node_center) = nodes.get(&node_id).map(|w| w.node_rect().center()) else {
            return;
        };
        let mut best: Option<(usize, f32)> = None;
        for (idx, conn) in self.graph.connections.iter().enumerate() {
            if conn.from.node == node_id || conn.to.node == node_id {
                continue;
            }
            let fp = nodes
                .get(&conn.from.node)
                .and_then(|w| w.output_socket_pos(conn.from.index));
            let tp = nodes
                .get(&conn.to.node)
                .and_then(|w| w.input_socket_pos(conn.to.index));
            let (Some(fp), Some(tp)) = (fp, tp) else {
                continue;
            };
            let dist = bezier_wire_distance(fp, tp, node_center);
            if dist < best.map_or(WIRE_INSERT_THRESHOLD, |(_, d)| d) {
                best = Some((idx, dist));
            }
        }
        let Some((idx, _)) = best else { return };
        let conn = self.graph.connections[idx].clone();

        let src_type = self
            .graph
            .nodes
            .get(&conn.from.node)
            .and_then(|n| n.outputs.get(conn.from.index))
            .map(|s| s.effective_type().to_owned());
        let dst_socket = self
            .graph
            .nodes
            .get(&conn.to.node)
            .and_then(|n| n.inputs.get(conn.to.index))
            .cloned();
        let nn = &self.graph.nodes[&node_id];

        let in_idx = src_type.as_deref().and_then(|t| {
            nn.inputs
                .iter()
                .position(|s| s.visible && s.accepts(t))
        });
        let out_idx = dst_socket.as_ref().and_then(|dst| {
            nn.outputs
                .iter()
                .position(|s| s.visible && dst.accepts(&s.type_name))
        });

        if let (Some(ii), Some(oi)) = (in_idx, out_idx) {
            self.push_undo_snapshot();
            self.graph.remove_connection_at(idx);
            self.graph.add_connection(
                conn.from,
                SocketId {
                    node: node_id,
                    index: ii,
                    direction: SocketDirection::Input,
                },
            );
            self.graph.add_connection(
                SocketId {
                    node: node_id,
                    index: oi,
                    direction: SocketDirection::Output,
                },
                conn.to,
            );
            self.run_update(node_id);
            self.run_update(conn.from.node);
            self.run_update(conn.to.node);
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn update_drag_wire(
        &mut self,
        ui: &egui::Ui,
        pointer_canvas: Option<Pos2>,
        responses: &GraphResponses,
        layout: &GraphWidgetLayout,
        from: SocketId,
        from_canvas: Pos2,
        mut current_canvas: Pos2,
    ) -> InteractionState {
        if ui.input(|i| i.pointer.button_down(egui::PointerButton::Primary)) {
            if let Some(pc) = pointer_canvas {
                current_canvas = self
                    .snapped_wire_target(from, pc, layout)
                    .map_or(pc, |(_, pos)| pos);
            }
            return InteractionState::DraggingWire {
                from,
                from_canvas,
                current_canvas,
            };
        }

        if let Some((target, _)) =
            pointer_canvas.and_then(|pc| self.snapped_wire_target(from, pc, layout))
        {
            self.add_wire_connection(from, target);
            return InteractionState::Idle;
        }

        if let Some((&target, _)) = responses
            .sockets
            .iter()
            .find(|(sid, response)| **sid != from && response.hovered())
        {
            self.add_wire_connection(from, target);
        }
        InteractionState::Idle
    }

    fn update_box_select(
        &mut self,
        ui: &egui::Ui,
        pointer_canvas: Option<Pos2>,
        layout: &GraphWidgetLayout,
        start_canvas: Pos2,
        mut current_canvas: Pos2,
    ) -> InteractionState {
        if ui.input(|i| i.pointer.button_down(egui::PointerButton::Primary)) {
            if let Some(pc) = pointer_canvas {
                current_canvas = pc;
            }
            return InteractionState::BoxSelecting {
                start_canvas,
                current_canvas,
            };
        }
        let select_rect = egui::Rect::from_two_pos(start_canvas, current_canvas);
        let shift = ui.input(|i| i.modifiers.shift);
        let ctrl = ui.input(|i| i.modifiers.ctrl);
        if !shift && !ctrl {
            for n in self.graph.nodes.values_mut() {
                n.selected = false;
            }
            for frame in &mut self.graph.frames {
                frame.selected = false;
            }
        }
        for (id, widget) in &layout.nodes {
            if select_rect.intersects(widget.node_rect())
                && let Some(n) = self.graph.nodes.get_mut(id)
            {
                n.selected = !ctrl;
            }
        }
        for (id, rect) in &layout.frame_rects {
            if select_rect.intersects(*rect)
                && let Some(frame) = self.graph.frames.iter_mut().find(|frame| frame.id == *id)
            {
                frame.selected = !ctrl;
            }
        }
        InteractionState::Idle
    }

    fn apply_knife_cut(&mut self, path: &[Pos2], nodes: &HashMap<NodeId, NodeWidget>) {
        if path.len() < 2 {
            return;
        }
        let to_remove: Vec<usize> = self
            .graph
            .connections
            .iter()
            .enumerate()
            .filter_map(|(idx, conn)| {
                let fp = nodes
                    .get(&conn.from.node)
                    .and_then(|w| w.output_socket_pos(conn.from.index))?;
                let tp = nodes
                    .get(&conn.to.node)
                    .and_then(|w| w.input_socket_pos(conn.to.index))?;
                path.windows(2)
                    .any(|w| wire_intersects_knife(fp, tp, w[0], w[1]))
                    .then_some(idx)
            })
            .collect();
        if !to_remove.is_empty() {
            self.push_undo_snapshot();
        }
        let mut touched = Vec::new();
        for idx in to_remove.into_iter().rev() {
            let conn = self.graph.remove_connection_at(idx);
            touched.push(conn.from.node);
            touched.push(conn.to.node);
        }
        touched.sort_unstable_by_key(|id: &NodeId| id.0);
        touched.dedup();
        for node_id in touched {
            self.run_update(node_id);
        }
    }

    fn update_cut_wire(
        &mut self,
        ui: &egui::Ui,
        pointer_canvas: Option<Pos2>,
        nodes: &HashMap<NodeId, NodeWidget>,
        mut path: Vec<Pos2>,
    ) -> InteractionState {
        if ui.input(|i| i.pointer.button_down(egui::PointerButton::Secondary)) {
            if let Some(pc) = pointer_canvas {
                let min_step = 4.0 / self.view.zoom;
                if path.last().is_none_or(|&last| last.distance(pc) > min_step) {
                    path.push(pc);
                }
            }
            return InteractionState::CuttingWire { path };
        }
        self.apply_knife_cut(&path, nodes);
        InteractionState::Idle
    }
    fn apply_effect(&mut self, effect: ActionEffect) {
        if effect == ActionEffect::ResetInteraction {
            self.interaction_state = InteractionState::Idle;
        }
    }

    fn node_has_hidden_sockets(&self, node_id: NodeId) -> bool {
        self.graph.nodes.get(&node_id).is_some_and(|n| {
            n.inputs.iter().any(|s| s.hidden) || n.outputs.iter().any(|s| s.hidden)
        })
    }

    fn menu_collapsed_state(&self, context_node: Option<NodeId>) -> bool {
        if let Some(node_id) = context_node {
            return self
                .graph
                .nodes
                .get(&node_id)
                .is_some_and(|node| node.collapsed);
        }
        self.graph
            .nodes
            .values()
            .any(|node| node.selected && node.collapsed)
    }

    fn node_at_screen_pos(&self, responses: &GraphResponses, screen_pos: Pos2) -> Option<NodeId> {
        if let Some((&id, _)) = responses
            .collapse_toggles
            .iter()
            .find(|(_, response)| response.rect.contains(screen_pos))
        {
            return Some(id);
        }
        if let Some((&id, _)) = responses.nodes.iter().find(|(_, node)| {
            node.header.rect.contains(screen_pos) || node.body.rect.contains(screen_pos)
        }) {
            return Some(id);
        }
        responses
            .sockets
            .iter()
            .find_map(|(&id, response)| response.rect.contains(screen_pos).then_some(id.node))
    }

    fn frame_at_screen_pos(
        &self,
        responses: &GraphResponses,
        layout: &GraphWidgetLayout,
        screen_pos: Pos2,
    ) -> Option<FrameId> {
        responses
            .frames
            .keys()
            .filter(|id| {
                layout
                    .frame_screen_rects
                    .get(id)
                    .is_some_and(|rect| rect.contains(screen_pos))
            })
            .min_by(|a, b| {
                let a_rect = layout.frame_screen_rects[a];
                let b_rect = layout.frame_screen_rects[b];
                a_rect
                    .area()
                    .total_cmp(&b_rect.area())
                    .then_with(|| a.0.cmp(&b.0))
            })
            .copied()
    }

    fn context_click_target_at(
        &self,
        responses: &GraphResponses,
        layout: &GraphWidgetLayout,
        screen_pos: Pos2,
    ) -> Option<ContextClickTarget> {
        if let Some(id) = self.node_at_screen_pos(responses, screen_pos) {
            return Some(ContextClickTarget::Node(id));
        }
        if let Some(id) = self.frame_at_screen_pos(responses, layout, screen_pos) {
            return Some(ContextClickTarget::Frame(id));
        }
        responses
            .canvas
            .rect
            .contains(screen_pos)
            .then_some(ContextClickTarget::Canvas)
    }

    fn select_node(&mut self, id: NodeId, toggle: bool) {
        if !toggle {
            for n in self.graph.nodes.values_mut() {
                n.selected = false;
            }
            for frame in &mut self.graph.frames {
                frame.selected = false;
            }
        }
        if let Some(node) = self.graph.nodes.get_mut(&id) {
            if toggle {
                node.selected = !node.selected;
            } else {
                node.selected = true;
            }
        }
    }

    fn select_frame(&mut self, id: FrameId, toggle: bool) {
        if !toggle {
            for node in self.graph.nodes.values_mut() {
                node.selected = false;
            }
            for frame in &mut self.graph.frames {
                frame.selected = false;
            }
        }
        if let Some(frame) = self.graph.frames.iter_mut().find(|frame| frame.id == id) {
            if toggle {
                frame.selected = !frame.selected;
            } else {
                frame.selected = true;
            }
        }
    }

    fn move_selected_frame_nodes(&mut self, fallback_frame: FrameId, delta: Vec2) {
        let selected_frames: Vec<_> = self
            .graph
            .frames
            .iter()
            .filter(|frame| frame.selected)
            .map(|frame| frame.id)
            .collect();
        let target_frames = if selected_frames.is_empty() {
            vec![fallback_frame]
        } else {
            selected_frames
        };
        let mut moved = std::collections::HashSet::new();
        for frame_id in target_frames {
            let Some(frame) = self.graph.frames.iter().find(|frame| frame.id == frame_id) else {
                continue;
            };
            for &node_id in &frame.node_ids {
                if moved.insert(node_id)
                    && let Some(node) = self.graph.nodes.get_mut(&node_id)
                {
                    node.pos += delta;
                }
            }
        }
    }

    pub(super) fn handle_input(
        &mut self,
        ui: &mut egui::Ui,
        responses: &GraphResponses,
        origin: Pos2,
        layout: &GraphWidgetLayout,
        canvas_rect: Rect,
    ) {
        let response = &responses.canvas;
        let scroll = ui.input(|i| i.smooth_scroll_delta.y);
        if scroll.abs() > 0.1
            && let Some(cursor) = ui.input(|i| i.pointer.hover_pos())
            && canvas_rect.contains(cursor)
        {
            self.view
                .zoom_around(cursor, origin, (1.0_f32 + scroll * 0.003).clamp(0.5, 2.0));
        }

        let pointer = response
            .hover_pos()
            .or_else(|| ui.input(|i| i.pointer.hover_pos()));
        let pointer_canvas = pointer.map(|p| self.view.screen_to_canvas(origin, p));
        let fallback_paste_pos = pointer_canvas
            .or_else(|| pointer.map(|p| self.view.screen_to_canvas(origin, p)))
            .unwrap_or_else(|| self.view.screen_to_canvas(origin, canvas_rect.center()));
        let no_focus = ui.ctx().memory(|m| m.focused().is_none());

        if no_focus {
            let any_selected = self.graph.nodes.values().any(|node| node.selected)
                || self.graph.frames.iter().any(|frame| frame.selected);
            let shortcut_entries = build_context_entries(
                &self.registry,
                fallback_paste_pos,
                pointer.unwrap_or(canvas_rect.center()),
                None,
                None,
                self.graph.frames.iter().any(|frame| frame.selected),
                false,
                self.menu_collapsed_state(None),
                any_selected,
                self.can_paste_nodes(),
                self.can_undo(),
                self.can_redo(),
            );
            if let Some(action) = dispatch_menu_shortcut(ui, &shortcut_entries) {
                let effect = self.execute_action(action, ui.ctx());
                self.apply_effect(effect);
            }
        }

        for action in self.hotkeys.dispatch(ui) {
            let effect = self.execute_action(action, ui.ctx());
            self.apply_effect(effect);
        }

        let cutting = matches!(self.interaction_state, InteractionState::CuttingWire { .. });

        if let Some(context_screen_pos) = self.menu.context_trigger_pos(ui, pointer, !cutting)
            && let Some(context_target) =
                self.context_click_target_at(responses, layout, context_screen_pos)
        {
            let mut context_frame = None;
            let context_node = match context_target {
                ContextClickTarget::Canvas => None,
                ContextClickTarget::Node(id) => Some(id),
                ContextClickTarget::Frame(id) => {
                    if !self
                        .graph
                        .frames
                        .iter()
                        .any(|frame| frame.id == id && frame.selected)
                    {
                        self.select_frame(id, false);
                    }
                    context_frame = Some(id);
                    None
                }
            };
            let canvas_pos = self.view.screen_to_canvas(origin, context_screen_pos);
            let node_hidden = context_node.is_some_and(|id| self.node_has_hidden_sockets(id));
            let node_collapsed = self.menu_collapsed_state(context_node);
            let any_selected = self.graph.nodes.values().any(|n| n.selected)
                || self.graph.frames.iter().any(|frame| frame.selected);
            let can_paste = self.can_paste_nodes();
            let entries = build_context_entries(
                &self.registry,
                canvas_pos,
                context_screen_pos,
                context_node,
                context_frame,
                self.graph.frames.iter().any(|frame| frame.selected),
                node_hidden,
                node_collapsed,
                any_selected,
                can_paste,
                self.can_undo(),
                self.can_redo(),
            );
            self.menu.open_popup(context_screen_pos, entries);
        }

        if no_focus
            && ui.input(|i| {
                i.key_pressed(egui::Key::A)
                    && i.modifiers.shift
                    && !i.modifiers.ctrl
                    && !i.modifiers.alt
            })
        {
            let screen_pos = pointer.unwrap_or(canvas_rect.center());
            let canvas_pos = self.view.screen_to_canvas(origin, screen_pos);
            self.menu.open_popup(
                screen_pos,
                build_empty_canvas_entries(
                    &self.registry,
                    canvas_pos,
                    self.can_paste_nodes(),
                    self.can_undo(),
                    self.can_redo(),
                ),
            );
        }

        if let Some(action) = self.menu.update(ui, response, pointer, !cutting) {
            let effect = self.execute_action(action, ui.ctx());
            self.apply_effect(effect);
        }

        self.update_interaction(
            ui,
            responses,
            pointer,
            pointer_canvas,
            origin,
            canvas_rect,
            layout,
        );
        if self.interaction_state.is_active() {
            ui.ctx()
                .request_repaint_after(std::time::Duration::from_millis(16));
        }
    }

    pub(super) fn compute_hovered_wire(
        &self,
        pointer_canvas: Option<Pos2>,
        nodes: &HashMap<NodeId, NodeWidget>,
    ) -> Option<usize> {
        let pc = pointer_canvas?;
        if matches!(
            self.interaction_state,
            InteractionState::DraggingWire { .. } | InteractionState::DraggingNode { .. }
        ) {
            return None;
        }
        let threshold = 10.0 / self.view.zoom;
        let mut best: Option<(usize, f32)> = None;
        for (idx, conn) in self.graph.connections.iter().enumerate() {
            let Some(fp) = nodes
                .get(&conn.from.node)
                .and_then(|w| w.output_socket_pos(conn.from.index))
            else {
                continue;
            };
            let Some(tp) = nodes
                .get(&conn.to.node)
                .and_then(|w| w.input_socket_pos(conn.to.index))
            else {
                continue;
            };
            let dist = bezier_wire_distance(fp, tp, pc);
            if dist < best.map_or(threshold, |(_, d)| d) {
                best = Some((idx, dist));
            }
        }
        best.map(|(idx, _)| idx)
    }

    pub(super) fn compute_insert_candidate_wire(
        &self,
        node_id: NodeId,
        nodes: &HashMap<NodeId, NodeWidget>,
    ) -> Option<usize> {
        let node_center = nodes.get(&node_id)?.node_rect().center();
        let nn = &self.graph.nodes[&node_id];
        let mut best: Option<(usize, f32)> = None;
        for (idx, conn) in self.graph.connections.iter().enumerate() {
            if conn.from.node == node_id || conn.to.node == node_id {
                continue;
            }
            let fp = nodes
                .get(&conn.from.node)
                .and_then(|w| w.output_socket_pos(conn.from.index));
            let tp = nodes
                .get(&conn.to.node)
                .and_then(|w| w.input_socket_pos(conn.to.index));
            let (Some(fp), Some(tp)) = (fp, tp) else {
                continue;
            };
            let src_t = self
                .graph
                .nodes
                .get(&conn.from.node)
                .and_then(|n| n.outputs.get(conn.from.index))
                .map(|s| s.effective_type());
            let dst_socket = self
                .graph
                .nodes
                .get(&conn.to.node)
                .and_then(|n| n.inputs.get(conn.to.index));
            let ok_in = src_t.is_some_and(|t| {
                nn.inputs.iter().any(|s| s.visible && s.accepts(t))
            });
            let ok_out = dst_socket.is_some_and(|dst| {
                nn.outputs
                    .iter()
                    .any(|s| s.visible && dst.accepts(&s.type_name))
            });
            if !ok_in || !ok_out {
                continue;
            }
            let dist = bezier_wire_distance(fp, tp, node_center);
            if dist < best.map_or(WIRE_INSERT_THRESHOLD, |(_, d)| d) {
                best = Some((idx, dist));
            }
        }
        best.map(|(idx, _)| idx)
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn update_interaction(
        &mut self,
        ui: &mut egui::Ui,
        responses: &GraphResponses,
        pointer: Option<Pos2>,
        pointer_canvas: Option<Pos2>,
        origin: Pos2,
        canvas_rect: Rect,
        layout: &GraphWidgetLayout,
    ) {
        let response = &responses.canvas;
        if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
            self.interaction_state = InteractionState::Idle;
        }

        let ctrl_held = ui.input(|i| i.modifiers.ctrl);
        let middle_down = ui.input(|i| i.pointer.button_down(egui::PointerButton::Middle));
        let right_down = ui.input(|i| i.pointer.button_down(egui::PointerButton::Secondary));

        if middle_down {
            if let Some(pp) = pointer {
                let delta =
                    if let InteractionState::Panning { last_screen } = self.interaction_state {
                        pp - last_screen
                    } else {
                        Vec2::ZERO
                    };
                if ctrl_held {
                    let factor = (1.0_f32 - delta.y * 0.005).clamp(0.5, 2.0);
                    if delta.y.abs() > 0.1 {
                        self.view.zoom_around(pp, origin, factor);
                    }
                } else {
                    self.view.pan += delta;
                }
                self.interaction_state = InteractionState::Panning { last_screen: pp };
            }
            return;
        }
        if matches!(self.interaction_state, InteractionState::Panning { .. }) {
            self.interaction_state = InteractionState::Idle;
        }

        if right_down && ctrl_held {
            if let Some(pc) = pointer_canvas {
                match &mut self.interaction_state {
                    InteractionState::CuttingWire { path } => {
                        let min_step = 4.0 / self.view.zoom;
                        if path.last().is_none_or(|&last| last.distance(pc) > min_step) {
                            path.push(pc);
                        }
                    }
                    _ => self.interaction_state = InteractionState::CuttingWire { path: vec![pc] },
                }
            }
            return;
        }
        if matches!(self.interaction_state, InteractionState::CuttingWire { .. }) {
            let state = std::mem::replace(&mut self.interaction_state, InteractionState::Idle);
            if let InteractionState::CuttingWire { path } = state {
                self.apply_knife_cut(&path, &layout.nodes);
            }
        }

        if matches!(self.interaction_state, InteractionState::Idle)
            && let Some(minimap) = &responses.minimap
            && let Some(pp) = minimap.response.hover_pos()
        {
            if minimap.response.drag_started() || minimap.response.dragged() {
                let canvas_pos = minimap.info.mini_to_canvas(pp);
                self.view.pan =
                    (canvas_rect.center() - origin) - canvas_pos.to_vec2() * self.view.zoom;
                return;
            }
        }

        let state = std::mem::replace(&mut self.interaction_state, InteractionState::Idle);
        self.interaction_state = match state {
            InteractionState::Idle => {
                self.idle_transition(ui, responses, pointer_canvas, origin, layout)
            }
            InteractionState::Panning { last_screen } => {
                self.update_panning(response, pointer, last_screen)
            }
            InteractionState::DraggingNode { node_id, offset } => {
                self.update_drag_node(ui, pointer_canvas, node_id, offset, &layout.nodes)
            }
            InteractionState::DraggingFrame {
                frame_id,
                last_canvas,
            } => self.update_drag_frame(ui, pointer_canvas, frame_id, last_canvas),
            InteractionState::DraggingWire {
                from,
                from_canvas,
                current_canvas,
            } => self.update_drag_wire(
                ui,
                pointer_canvas,
                responses,
                layout,
                from,
                from_canvas,
                current_canvas,
            ),
            InteractionState::BoxSelecting {
                start_canvas,
                current_canvas,
            } => self.update_box_select(ui, pointer_canvas, layout, start_canvas, current_canvas),
            InteractionState::CuttingWire { path } => {
                self.update_cut_wire(ui, pointer_canvas, &layout.nodes, path)
            }
        };
    }
}
