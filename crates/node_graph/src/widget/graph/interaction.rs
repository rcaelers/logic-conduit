use super::{
    NodeGraphWidget, action::ActionEffect, layout::GraphWidgetLayout, menu::build_context_entries,
    minimap,
};
use crate::{
    model::{Connection, FrameId, NodeId, SocketDirection, SocketId},
    support::paint::{bezier_wire_distance, bezier_wire_intersects_rect, wire_intersects_knife},
    widget::{menu::dispatch_menu_shortcut, node::NodeWidget},
};
use egui::{Pos2, Rect, Vec2};
use std::collections::{HashMap, HashSet};
use std::rc::Rc;

const WIRE_SNAP_DISTANCE: f32 = 18.0;
/// Ctrl-held grid size while dragging a node (Phase 6.3), in canvas units.
const GRID_SNAP: f32 = 10.0;

/// Nearest `grid`-unit canvas grid point to `pos`.
fn snap_to_grid(pos: Pos2, grid: f32) -> Pos2 {
    Pos2::new((pos.x / grid).round() * grid, (pos.y / grid).round() * grid)
}

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
        /// Every node with at least one socket compatible with `from` —
        /// computed once when the drag starts (`connectable_nodes`), not
        /// per frame. `render.rs` dims everything else during the drag
        /// (Phase 4.3) so viable targets pop at any zoom.
        connectable: Rc<HashSet<NodeId>>,
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
    /// Freshly added/duplicated/pasted nodes follow the pointer until a
    /// primary-button click confirms placement (Phase 1.2), mirroring
    /// Blender's grab-on-add. Escape/secondary-click cancels by undoing the
    /// snapshot taken when the gesture started. `anchor_canvas` is the
    /// pointer position as of the last processed frame — movement is a
    /// per-frame delta from it, not a fixed offset from gesture start.
    PlacingNodes {
        anchor_canvas: Pos2,
        /// True only for the first `update_placing_nodes` tick after this
        /// state is entered — the same input frame that processed the
        /// triggering click (e.g. clicking a node type in the Add menu).
        /// After an idle frame, egui can deliver a mouse press *and*
        /// release together in one input frame; without this guard, that
        /// same fused event would immediately satisfy the primary-button
        /// confirm check and end placement before the user ever gets to
        /// move the node. Keyboard-triggered entries (Shift+D, Ctrl+V)
        /// don't hit this, since no pointer button is involved at all.
        just_entered: bool,
    },
}

impl InteractionState {
    pub(super) fn is_active(&self) -> bool {
        !matches!(self, Self::Idle)
    }

    pub(super) fn use_fast_rendering(&self) -> bool {
        matches!(
            self,
            Self::Panning { .. }
                | Self::DraggingNode { .. }
                | Self::DraggingFrame { .. }
                | Self::PlacingNodes { .. }
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
    pub(super) fn compatible_wire_target(&self, from: SocketId, to: SocketId) -> bool {
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

    /// Every node with at least one visible socket compatible with `from` —
    /// cached once into `InteractionState::DraggingWire::connectable` when a
    /// wire drag starts (Phase 4.3).
    pub(super) fn connectable_nodes(&self, from: SocketId) -> HashSet<NodeId> {
        self.graph
            .nodes
            .values()
            .filter(|node| {
                let inputs = node
                    .inputs
                    .iter()
                    .enumerate()
                    .filter(|(_, s)| s.visible)
                    .map(|(index, _)| SocketId {
                        node: node.id,
                        index,
                        direction: SocketDirection::Input,
                    });
                let outputs = node
                    .outputs
                    .iter()
                    .enumerate()
                    .filter(|(_, s)| s.visible)
                    .map(|(index, _)| SocketId {
                        node: node.id,
                        index,
                        direction: SocketDirection::Output,
                    });
                inputs
                    .chain(outputs)
                    .any(|candidate| self.compatible_wire_target(from, candidate))
            })
            .map(|node| node.id)
            .collect()
    }

    /// The socket a wire drag started from, if it still exists.
    fn socket_at(&self, id: SocketId) -> Option<&crate::model::Socket> {
        let node = self.graph.nodes.get(&id.node)?;
        match id.direction {
            SocketDirection::Input => node.inputs.get(id.index),
            SocketDirection::Output => node.outputs.get(id.index),
        }
    }

    /// First visible socket on `node_id` compatible with `from` — used to
    /// auto-wire a freshly added node (link-drag search, Phase 1.1).
    pub(super) fn first_compatible_socket(
        &self,
        from: SocketId,
        node_id: NodeId,
    ) -> Option<SocketId> {
        let node = self.graph.nodes.get(&node_id)?;
        let inputs = node
            .inputs
            .iter()
            .enumerate()
            .filter(|(_, s)| s.visible)
            .map(|(index, _)| SocketId {
                node: node_id,
                index,
                direction: SocketDirection::Input,
            });
        let outputs = node
            .outputs
            .iter()
            .enumerate()
            .filter(|(_, s)| s.visible)
            .map(|(index, _)| SocketId {
                node: node_id,
                index,
                direction: SocketDirection::Output,
            });
        inputs
            .chain(outputs)
            .find(|&candidate| self.compatible_wire_target(from, candidate))
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
            // Embedded controls are drawn later in the frame, so they sit on
            // top of this region and still receive their own clicks/drags.
            let body = ui.interact(
                body_rect,
                ui.id().with(("node-body", id.0)),
                egui::Sense::click_and_drag(),
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
                    connectable: Rc::new(self.connectable_nodes(src)),
                };
            }
            return InteractionState::DraggingWire {
                from: sid,
                from_canvas: self.view.screen_to_canvas(origin, spos),
                current_canvas: pc,
                connectable: Rc::new(self.connectable_nodes(sid)),
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
            if (responses.header.drag_started() || responses.body.drag_started())
                && let Some(node) = self.graph.nodes.get(&id)
            {
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

        // Checked before the plain-click deselect below: egui fires both
        // `clicked()` and `double_clicked()` on a double-click's second
        // press, and inserting a reroute shouldn't also clear the selection
        // as a side effect (Phase 6.2).
        if responses.canvas.double_clicked()
            && let Some(idx) = self.wire_near_point(pc, &layout.nodes)
        {
            self.insert_reroute_on_wire(idx, pc);
            return InteractionState::Idle;
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
        layout: &GraphWidgetLayout,
    ) -> InteractionState {
        if ui.input(|i| i.pointer.button_down(egui::PointerButton::Primary)) {
            if let Some(pc) = pointer_canvas {
                let mut new_pos = (pc.to_vec2() - offset).to_pos2();
                // Ctrl is free during an active drag (it only means
                // toggle-select on the click that *starts* one) — reused
                // here for Blender-style grid snap (Phase 6.3). Only the
                // dragged node itself snaps to the grid; every other
                // selected node moves by the same resulting delta, keeping
                // the whole selection's relative layout intact.
                if ui.input(|i| i.modifiers.ctrl) {
                    new_pos = snap_to_grid(new_pos, GRID_SNAP);
                }
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
        self.try_wire_insert(node_id, pointer_canvas, &layout.nodes);
        let selected: Vec<NodeId> = self
            .graph
            .nodes
            .values()
            .filter(|node| node.selected)
            .map(|node| node.id)
            .collect();
        self.resolve_frame_membership_on_drop(&selected, layout);
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

    /// Drives `InteractionState::PlacingNodes` (Phase 1.2): moves every
    /// selected node by the pointer's per-frame delta until the primary
    /// button confirms (with a wire-splice check when exactly one node is
    /// being placed, matching `update_drag_node`'s drop behavior) or
    /// Escape/secondary-click cancels by undoing the add/duplicate/paste.
    /// The confirm/cancel checks are skipped entirely on `just_entered`'s
    /// frame — see the field's doc comment on `InteractionState::PlacingNodes`.
    fn update_placing_nodes(
        &mut self,
        ui: &egui::Ui,
        pointer_canvas: Option<Pos2>,
        anchor_canvas: Pos2,
        just_entered: bool,
        layout: &GraphWidgetLayout,
    ) -> InteractionState {
        if !just_entered {
            if ui.input(|i| i.pointer.button_pressed(egui::PointerButton::Secondary)) {
                self.undo();
                return InteractionState::Idle;
            }
            if ui.input(|i| i.pointer.button_pressed(egui::PointerButton::Primary)) {
                let selected: Vec<NodeId> = self
                    .graph
                    .nodes
                    .values()
                    .filter(|node| node.selected)
                    .map(|node| node.id)
                    .collect();
                if let [only] = selected[..] {
                    self.try_wire_insert(only, pointer_canvas, &layout.nodes);
                }
                self.resolve_frame_membership_on_drop(&selected, layout);
                return InteractionState::Idle;
            }
        }
        let Some(pc) = pointer_canvas else {
            return InteractionState::PlacingNodes {
                anchor_canvas,
                just_entered: false,
            };
        };
        let delta = pc - anchor_canvas;
        if delta != Vec2::ZERO {
            for n in self.graph.nodes.values_mut() {
                if n.selected {
                    n.pos += delta;
                }
            }
        }
        InteractionState::PlacingNodes {
            anchor_canvas: pc,
            just_entered: false,
        }
    }

    fn try_wire_insert(
        &mut self,
        node_id: NodeId,
        pointer_canvas: Option<Pos2>,
        nodes: &HashMap<NodeId, NodeWidget>,
    ) {
        let Some(point) =
            pointer_canvas.or_else(|| Some(nodes.get(&node_id)?.node_rect().center()))
        else {
            return;
        };
        let Some(idx) = self.closest_insert_wire(node_id, point, nodes) else {
            return;
        };
        let conn = self.graph.connections[idx].clone();

        if let Some((ii, oi)) = self.wire_insert_sockets(node_id, &conn) {
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
        pointer: Option<Pos2>,
        pointer_canvas: Option<Pos2>,
        responses: &GraphResponses,
        layout: &GraphWidgetLayout,
        from: SocketId,
        from_canvas: Pos2,
        mut current_canvas: Pos2,
        connectable: Rc<HashSet<NodeId>>,
    ) -> InteractionState {
        if ui.input(|i| i.pointer.button_down(egui::PointerButton::Primary)) {
            if let Some(pc) = pointer_canvas {
                let snapped = self.snapped_wire_target(from, pc, layout);
                current_canvas = snapped.map_or(pc, |(_, pos)| pos);
                // Not over a compatible socket: releasing here opens
                // link-drag search (Phase 1.1) instead of connecting
                // directly. Blender flags this with a "+" cursor; the
                // closest built-in egui cursor with the same badge is Copy.
                if snapped.is_none() {
                    ui.ctx().set_cursor_icon(egui::CursorIcon::Copy);
                }
            }
            return InteractionState::DraggingWire {
                from,
                from_canvas,
                current_canvas,
                connectable,
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
            return InteractionState::Idle;
        }

        // Released on empty canvas: open the link-drag search so a new node
        // can be added and wired in with one gesture (Blender's "link drag
        // search"). Esc/click-outside on the popup just drops the wire.
        if let Some(pointer_screen) = pointer
            && let Some(from_socket) = self.socket_at(from).cloned()
        {
            let canvas_pos = pointer_canvas.unwrap_or(current_canvas);
            self.menu.open_link_drag_search(
                pointer_screen,
                &self.registry,
                canvas_pos,
                from,
                &from_socket,
            );
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
    fn apply_effect(&mut self, effect: ActionEffect, pointer_canvas: Option<Pos2>) {
        match effect {
            ActionEffect::None => {}
            ActionEffect::ResetInteraction => self.interaction_state = InteractionState::Idle,
            ActionEffect::EnterPlacement => {
                // Without a live pointer there is nothing to follow; the
                // action already fell back to fixed-position placement.
                if let Some(anchor_canvas) = pointer_canvas {
                    self.interaction_state = InteractionState::PlacingNodes {
                        anchor_canvas,
                        just_entered: true,
                    };
                }
            }
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

    fn menu_muted_state(&self, context_node: Option<NodeId>) -> bool {
        if let Some(node_id) = context_node {
            return self
                .graph
                .nodes
                .get(&node_id)
                .is_some_and(|node| node.muted);
        }
        self.graph
            .nodes
            .values()
            .any(|node| node.selected && node.muted)
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
            // Blender-style "active" node: the properties panel follows the
            // most recent selection.
            if node.selected {
                self.set_active_node(id);
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
        pointer: Option<Pos2>,
        origin: Pos2,
        layout: &GraphWidgetLayout,
        canvas_rect: Rect,
    ) {
        let response = &responses.canvas;
        let (scroll, zoom_delta, zoom_modifier) = ui.input(|i| {
            (
                i.smooth_scroll_delta,
                i.zoom_delta(),
                i.modifiers.ctrl || i.modifiers.command || i.modifiers.mac_cmd,
            )
        });
        let has_scroll = scroll.length_sq() > 0.01;
        let has_zoom = zoom_modifier && (zoom_delta - 1.0).abs() > 0.001;
        if (has_scroll || has_zoom)
            && !self.menu.blocks_canvas_scroll(ui)
            && let Some(cursor) = pointer
            && canvas_rect.contains(cursor)
        {
            if has_zoom {
                self.view
                    .zoom_around(cursor, origin, zoom_delta.clamp(0.5, 2.0));
            } else if zoom_modifier && scroll.y.abs() > 0.1 {
                self.view
                    .zoom_around(cursor, origin, (1.0_f32 + scroll.y * 0.003).clamp(0.5, 2.0));
            } else if !zoom_modifier {
                self.view.pan += scroll;
            }
        }

        let pointer_canvas = pointer.map(|p| self.view.screen_to_canvas(origin, p));
        let fallback_paste_pos = pointer_canvas
            .or_else(|| pointer.map(|p| self.view.screen_to_canvas(origin, p)))
            .unwrap_or_else(|| self.view.screen_to_canvas(origin, canvas_rect.center()));
        let no_focus = ui.ctx().memory(|m| m.focused().is_none());

        if no_focus
            && pointer.is_some()
            && ui.input_mut(|input| input.consume_key(egui::Modifiers::NONE, egui::Key::Home))
        {
            self.fit_graph_to_viewport(layout, canvas_rect, origin);
            return;
        }

        // Zoom-to-selection (Blender's numpad-`.`) and rename-active (F2)
        // are special-cased here, like Home above, rather than routed
        // through `self.hotkeys`: both need `layout`/`origin` (for viewport
        // fitting and for placing the rename popup at the node's screen
        // position) that the generic action dispatch doesn't carry.
        if no_focus
            && pointer.is_some()
            && ui.input_mut(|input| input.consume_key(egui::Modifiers::NONE, egui::Key::Period))
        {
            self.fit_selection_to_viewport(layout, canvas_rect, origin);
            return;
        }

        if no_focus
            && ui.input_mut(|input| input.consume_key(egui::Modifiers::NONE, egui::Key::F2))
            && let Some(active) = self.active_node
            && let Some(&header_rect) = layout.header_screen_rects.get(&active)
        {
            self.start_renaming_node(active, header_rect.left_bottom());
            return;
        }

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
                self.menu_muted_state(None),
                false,
                any_selected,
                self.can_paste_nodes(),
                self.can_undo(),
                self.can_redo(),
            );
            if let Some(action) = dispatch_menu_shortcut(ui, &shortcut_entries) {
                let effect = self.execute_action(action, ui.ctx(), pointer_canvas);
                self.apply_effect(effect, pointer_canvas);
            }
        }

        // Shift+A opens the Add search at the pointer (Blender's Add menu);
        // plain A/Alt+A (select-all/deselect-all) go through `self.hotkeys`
        // below as ordinary `GraphAction`s. This one stays special-cased
        // because positioning the popup needs the screen pointer/canvas
        // origin the generic action dispatch doesn't carry — but it must
        // run, and *consume* its key event, before that dispatch: egui's
        // `consume_shortcut` matches modifiers with `matches_logically`,
        // which ignores *extra* Shift/Alt held beyond what a binding asks
        // for, so the registry's plain `A` (no modifiers required) would
        // otherwise also match a Shift+A press and fire Select All first,
        // leaving nothing here to see.
        let placing = matches!(
            self.interaction_state,
            InteractionState::PlacingNodes { .. }
        );
        if no_focus
            && !placing
            && ui.input_mut(|input| input.consume_key(egui::Modifiers::SHIFT, egui::Key::A))
        {
            let screen_pos = pointer.unwrap_or(canvas_rect.center());
            let canvas_pos = self.view.screen_to_canvas(origin, screen_pos);
            self.menu
                .open_add_popup(screen_pos, &self.registry, canvas_pos);
        }

        for action in self.hotkeys.dispatch(ui) {
            let effect = self.execute_action(action, ui.ctx(), pointer_canvas);
            self.apply_effect(effect, pointer_canvas);
        }

        let cutting = matches!(self.interaction_state, InteractionState::CuttingWire { .. });

        if let Some(context_screen_pos) =
            self.menu
                .context_trigger_pos(ui, pointer, !cutting && !placing)
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
            let node_muted = self.menu_muted_state(context_node);
            let node_has_derived_cache =
                context_node.is_some_and(|id| self.derived_cache_nodes.contains(&id));
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
                node_muted,
                node_has_derived_cache,
                any_selected,
                can_paste,
                self.can_undo(),
                self.can_redo(),
            );
            self.menu.open_popup(context_screen_pos, entries);
        }

        // Shift+A opens the Add search at the pointer (Blender's Add menu);
        // plain A/Alt+A (select-all/deselect-all) are ordinary `GraphAction`s
        // dispatched through `self.hotkeys` below — this one stays
        // special-cased because positioning the popup needs the screen
        // pointer and canvas origin, which the generic action dispatch
        // doesn't carry.
        if no_focus
            && !placing
            && ui.input(|i| {
                i.key_pressed(egui::Key::A)
                    && i.modifiers.shift
                    && !i.modifiers.ctrl
                    && !i.modifiers.alt
            })
        {
            let screen_pos = pointer.unwrap_or(canvas_rect.center());
            let canvas_pos = self.view.screen_to_canvas(origin, screen_pos);
            self.menu
                .open_add_popup(screen_pos, &self.registry, canvas_pos);
        }

        if let Some(action) = self.menu.update(ui, response, pointer, !cutting) {
            let effect = self.execute_action(action, ui.ctx(), pointer_canvas);
            self.apply_effect(effect, pointer_canvas);
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

    /// Connection nearest `point_canvas`, within snap distance — double-click
    /// to insert a reroute (Phase 6.2). Unlike `closest_insert_wire`, this
    /// isn't gated on overlapping any particular node's rect; it's a plain
    /// point-to-wire hit test.
    fn wire_near_point(
        &self,
        point_canvas: Pos2,
        nodes: &HashMap<NodeId, NodeWidget>,
    ) -> Option<usize> {
        let threshold = WIRE_SNAP_DISTANCE / self.view.zoom;
        let mut best: Option<(usize, f32)> = None;
        for (idx, conn) in self.graph.connections.iter().enumerate() {
            let fp = nodes
                .get(&conn.from.node)
                .and_then(|w| w.output_socket_pos(conn.from.index));
            let tp = nodes
                .get(&conn.to.node)
                .and_then(|w| w.input_socket_pos(conn.to.index));
            let (Some(fp), Some(tp)) = (fp, tp) else {
                continue;
            };
            let dist = bezier_wire_distance(fp, tp, point_canvas);
            if dist <= threshold && dist < best.map_or(f32::INFINITY, |(_, d)| d) {
                best = Some((idx, dist));
            }
        }
        best.map(|(idx, _)| idx)
    }

    /// Splits the connection at `connection_index` by inserting a fresh
    /// `Reroute` node at `pos_canvas` and rewiring both halves through it —
    /// one undo step (Phase 6.2).
    fn insert_reroute_on_wire(&mut self, connection_index: usize, pos_canvas: Pos2) {
        let Some(conn) = self.graph.connections.get(connection_index).cloned() else {
            return;
        };
        self.push_undo_snapshot();
        self.graph.remove_connection_at(connection_index);
        let Some(node_id) = self.add_node_at("Reroute", pos_canvas) else {
            return;
        };
        self.graph.add_connection(
            conn.from,
            SocketId {
                node: node_id,
                index: 0,
                direction: SocketDirection::Input,
            },
        );
        self.graph.add_connection(
            SocketId {
                node: node_id,
                index: 0,
                direction: SocketDirection::Output,
            },
            conn.to,
        );
        self.run_update(conn.from.node);
        self.run_update(node_id);
        self.run_update(conn.to.node);
    }

    /// Wire overlapped by the dragged node's rect, ignoring wires already
    /// attached to `node_id`; when several overlap, the one closest to
    /// `point` (the pointer) wins. Compatibility is deliberately not a
    /// selection criterion — the same wire must be chosen whether or not the
    /// node fits, so the preview (highlight vs. muted) and the actual drop
    /// always agree on the target.
    fn closest_insert_wire(
        &self,
        node_id: NodeId,
        point: Pos2,
        nodes: &HashMap<NodeId, NodeWidget>,
    ) -> Option<usize> {
        let node_rect = nodes.get(&node_id)?.node_rect();
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
            if !bezier_wire_intersects_rect(fp, tp, node_rect) {
                continue;
            }
            let dist = bezier_wire_distance(fp, tp, point);
            if dist < best.map_or(f32::INFINITY, |(_, d)| d) {
                best = Some((idx, dist));
            }
        }
        best.map(|(idx, _)| idx)
    }

    /// Socket indices (input, output) on `node_id` that would splice it into
    /// `conn`, or `None` if the node cannot be inserted there.
    fn wire_insert_sockets(&self, node_id: NodeId, conn: &Connection) -> Option<(usize, usize)> {
        // Only a completely fresh, unconnected node gets spliced into a
        // wire — a node already wired up elsewhere shouldn't have its
        // existing topology silently rearranged by an incidental drag-over.
        if node_has_any_connection(&self.graph.connections, node_id) {
            return None;
        }
        let src_type = self
            .graph
            .nodes
            .get(&conn.from.node)?
            .outputs
            .get(conn.from.index)?
            .effective_type()
            .to_owned();
        let dst_socket = self
            .graph
            .nodes
            .get(&conn.to.node)?
            .inputs
            .get(conn.to.index)?;
        let nn = self.graph.nodes.get(&node_id)?;
        let in_idx = nn
            .inputs
            .iter()
            .position(|s| s.visible && s.accepts(&src_type))?;
        let out_idx = nn
            .outputs
            .iter()
            .position(|s| s.visible && dst_socket.accepts(&s.type_name))?;
        Some((in_idx, out_idx))
    }

    /// Wire the dragged node is hovering, and whether it can be spliced in.
    pub(super) fn compute_insert_candidate_wire(
        &self,
        node_id: NodeId,
        pointer_canvas: Option<Pos2>,
        nodes: &HashMap<NodeId, NodeWidget>,
    ) -> Option<(usize, bool)> {
        let point = pointer_canvas.or_else(|| Some(nodes.get(&node_id)?.node_rect().center()))?;
        let idx = self.closest_insert_wire(node_id, point, nodes)?;
        let conn = self.graph.connections.get(idx)?;
        Some((idx, self.wire_insert_sockets(node_id, conn).is_some()))
    }

    /// Frame that would join `node_id` if it were dropped right now — `None`
    /// if it's already a member of a frame (Phase 1.3): dragging can only
    /// ever *add* a node to a frame, never remove it. Membership only ever
    /// changes the other direction via the explicit "Remove from Frame"
    /// action, so a node that's already in a frame is never a candidate
    /// here — there is nothing to leave-and-rejoin, and no frame ever steals
    /// a node away from another one by drag alone. Run live (not against a
    /// gesture-start snapshot) so the candidate frame can be highlighted
    /// while dragging: a node not yet in any frame doesn't affect any
    /// frame's live bounds, so there's no self-referential loop to guard
    /// against here.
    pub(super) fn compute_drop_target_frame(
        &self,
        node_id: NodeId,
        layout: &GraphWidgetLayout,
    ) -> Option<FrameId> {
        if self
            .graph
            .frames
            .iter()
            .any(|frame| frame.node_ids.contains(&node_id))
        {
            return None;
        }
        let center = layout.nodes.get(&node_id)?.header_rect().center();
        layout
            .frame_rects
            .iter()
            .filter(|(_, rect)| rect.contains(center))
            .min_by(|(a_id, a_rect), (b_id, b_rect)| {
                a_rect
                    .area()
                    .total_cmp(&b_rect.area())
                    .then_with(|| a_id.0.cmp(&b_id.0))
            })
            .map(|(&id, _)| id)
    }

    /// Frame membership follows a drag/placement drop (Phase 1.3) — see
    /// `compute_drop_target_frame` for the rule (join-only; dragging never
    /// removes). Only called once, on gesture confirm; the changes fold
    /// into the undo snapshot the drag/placement already pushed at its
    /// start.
    fn resolve_frame_membership_on_drop(
        &mut self,
        node_ids: &[NodeId],
        layout: &GraphWidgetLayout,
    ) {
        if self.graph.frames.is_empty() {
            return;
        }
        let mut changed = false;
        for &node_id in node_ids {
            let Some(target_id) = self.compute_drop_target_frame(node_id, layout) else {
                continue;
            };
            if let Some(frame) = self.graph.frames.iter_mut().find(|f| f.id == target_id) {
                frame.node_ids.push(node_id);
                changed = true;
            }
        }
        if changed {
            self.graph.cleanup_frames();
        }
    }

    /// Pans the view while the pointer sits within `MARGIN` of (or past) the
    /// canvas edge during a drag (Phase 6.1). `DraggingNode`, `DraggingWire`,
    /// `BoxSelecting`, and `PlacingNodes` all derive their target position
    /// from `pointer_canvas` on the *next* frame, so nudging `view.pan` here
    /// is enough to move the drag correctly — no per-state position math
    /// needed.
    fn edge_auto_pan(&mut self, pointer: Pos2, canvas_rect: Rect) {
        const MARGIN: f32 = 24.0;
        const MAX_SPEED: f32 = 15.0;
        const GAIN: f32 = 0.15;

        let overshoot_left = (canvas_rect.min.x + MARGIN) - pointer.x;
        let overshoot_right = pointer.x - (canvas_rect.max.x - MARGIN);
        let overshoot_top = (canvas_rect.min.y + MARGIN) - pointer.y;
        let overshoot_bottom = pointer.y - (canvas_rect.max.y - MARGIN);

        let mut delta = Vec2::ZERO;
        if overshoot_left > 0.0 {
            delta.x += (overshoot_left * GAIN).min(MAX_SPEED);
        } else if overshoot_right > 0.0 {
            delta.x -= (overshoot_right * GAIN).min(MAX_SPEED);
        }
        if overshoot_top > 0.0 {
            delta.y += (overshoot_top * GAIN).min(MAX_SPEED);
        } else if overshoot_bottom > 0.0 {
            delta.y -= (overshoot_bottom * GAIN).min(MAX_SPEED);
        }

        if delta != Vec2::ZERO {
            self.view.pan += delta;
        }
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
            // Cancelling a placement gesture must revert the add/duplicate/
            // paste it started with, not just drop back to Idle and leave
            // the new nodes stranded.
            if matches!(
                self.interaction_state,
                InteractionState::PlacingNodes { .. }
            ) {
                self.undo();
            }
            self.interaction_state = InteractionState::Idle;
        }

        let ctrl_held = ui.input(|i| i.modifiers.ctrl);
        // `button_down` is global pointer state, not scoped to this widget —
        // without the hover/already-panning check, a middle-drag started
        // over a sibling widget (e.g. the logic analyzer above the graph)
        // would also pan the graph. Once a pan has started, keep following
        // the drag even if the pointer leaves the canvas rect.
        let middle_down = ui.input(|i| i.pointer.button_down(egui::PointerButton::Middle))
            && (pointer.is_some() && response.hovered()
                || matches!(self.interaction_state, InteractionState::Panning { .. }));
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
            && (minimap.response.drag_started() || minimap.response.dragged())
        {
            let canvas_pos = minimap.info.mini_to_canvas(pp);
            self.view.pan = (canvas_rect.center() - origin) - canvas_pos.to_vec2() * self.view.zoom;
            return;
        }

        if let Some(pp) = pointer
            && matches!(
                self.interaction_state,
                InteractionState::DraggingNode { .. }
                    | InteractionState::DraggingWire { .. }
                    | InteractionState::BoxSelecting { .. }
                    | InteractionState::PlacingNodes { .. }
            )
        {
            self.edge_auto_pan(pp, canvas_rect);
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
                self.update_drag_node(ui, pointer_canvas, node_id, offset, layout)
            }
            InteractionState::DraggingFrame {
                frame_id,
                last_canvas,
            } => self.update_drag_frame(ui, pointer_canvas, frame_id, last_canvas),
            InteractionState::DraggingWire {
                from,
                from_canvas,
                current_canvas,
                connectable,
            } => self.update_drag_wire(
                ui,
                pointer,
                pointer_canvas,
                responses,
                layout,
                from,
                from_canvas,
                current_canvas,
                connectable,
            ),
            InteractionState::BoxSelecting {
                start_canvas,
                current_canvas,
            } => self.update_box_select(ui, pointer_canvas, layout, start_canvas, current_canvas),
            InteractionState::CuttingWire { path } => {
                self.update_cut_wire(ui, pointer_canvas, &layout.nodes, path)
            }
            InteractionState::PlacingNodes {
                anchor_canvas,
                just_entered,
            } => self.update_placing_nodes(ui, pointer_canvas, anchor_canvas, just_entered, layout),
        };
    }
}

/// Whether `node_id` is an endpoint of any existing connection.
fn node_has_any_connection(connections: &[Connection], node_id: NodeId) -> bool {
    connections
        .iter()
        .any(|c| c.from.node == node_id || c.to.node == node_id)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn socket(node: u32, index: usize, direction: SocketDirection) -> SocketId {
        SocketId {
            node: NodeId(node),
            index,
            direction,
        }
    }

    #[test]
    fn node_with_no_connections_has_none() {
        let connections = vec![Connection {
            from: socket(1, 0, SocketDirection::Output),
            to: socket(2, 0, SocketDirection::Input),
        }];
        assert!(!node_has_any_connection(&connections, NodeId(3)));
    }

    #[test]
    fn node_as_connection_source_counts() {
        let connections = vec![Connection {
            from: socket(1, 0, SocketDirection::Output),
            to: socket(2, 0, SocketDirection::Input),
        }];
        assert!(node_has_any_connection(&connections, NodeId(1)));
    }

    #[test]
    fn node_as_connection_target_counts() {
        let connections = vec![Connection {
            from: socket(1, 0, SocketDirection::Output),
            to: socket(2, 0, SocketDirection::Input),
        }];
        assert!(node_has_any_connection(&connections, NodeId(2)));
    }

    #[test]
    fn snap_to_grid_rounds_to_the_nearest_grid_point() {
        assert_eq!(
            snap_to_grid(Pos2::new(24.0, 26.0), 10.0),
            Pos2::new(20.0, 30.0)
        );
        assert_eq!(
            snap_to_grid(Pos2::new(-3.0, 5.0), 10.0),
            Pos2::new(0.0, 10.0)
        );
        assert_eq!(
            snap_to_grid(Pos2::new(10.0, 10.0), 10.0),
            Pos2::new(10.0, 10.0)
        );
    }

    #[test]
    fn edge_auto_pan_does_nothing_well_inside_the_canvas() {
        use crate::runtime::NodeTypeRegistry;

        let mut widget = NodeGraphWidget::new(NodeTypeRegistry::new());
        let canvas_rect = Rect::from_min_size(Pos2::ZERO, egui::vec2(800.0, 600.0));
        widget.edge_auto_pan(canvas_rect.center(), canvas_rect);

        assert_eq!(widget.view.pan, Vec2::ZERO);
    }

    #[test]
    fn edge_auto_pan_pans_positive_x_near_the_left_edge() {
        use crate::runtime::NodeTypeRegistry;

        let mut widget = NodeGraphWidget::new(NodeTypeRegistry::new());
        let canvas_rect = Rect::from_min_size(Pos2::ZERO, egui::vec2(800.0, 600.0));
        // Right at the left edge — well past the 24px margin.
        widget.edge_auto_pan(Pos2::new(0.0, canvas_rect.center().y), canvas_rect);

        assert!(widget.view.pan.x > 0.0);
        assert_eq!(widget.view.pan.y, 0.0);
    }

    #[test]
    fn edge_auto_pan_pans_negative_x_near_the_right_edge() {
        use crate::runtime::NodeTypeRegistry;

        let mut widget = NodeGraphWidget::new(NodeTypeRegistry::new());
        let canvas_rect = Rect::from_min_size(Pos2::ZERO, egui::vec2(800.0, 600.0));
        widget.edge_auto_pan(Pos2::new(800.0, canvas_rect.center().y), canvas_rect);

        assert!(widget.view.pan.x < 0.0);
        assert_eq!(widget.view.pan.y, 0.0);
    }

    #[test]
    fn edge_auto_pan_clamps_to_max_speed_past_the_edge() {
        use crate::runtime::NodeTypeRegistry;

        let mut widget = NodeGraphWidget::new(NodeTypeRegistry::new());
        let canvas_rect = Rect::from_min_size(Pos2::ZERO, egui::vec2(800.0, 600.0));
        // Far past the edge — overshoot would blow past MAX_SPEED unclamped.
        widget.edge_auto_pan(Pos2::new(-500.0, canvas_rect.center().y), canvas_rect);

        assert_eq!(widget.view.pan.x, 15.0);
    }

    #[test]
    fn double_click_wire_inserts_a_reroute_and_rewires_both_halves() {
        use crate::runtime::NodeTypeRegistry;

        let mut widget = NodeGraphWidget::new(NodeTypeRegistry::new());
        let a = widget
            .add_node_at("Reroute", Pos2::new(0.0, 0.0))
            .expect("reroute should always be creatable");
        let b = widget
            .add_node_at("Reroute", Pos2::new(200.0, 0.0))
            .expect("reroute should always be creatable");
        let from = socket(a.0, 0, SocketDirection::Output);
        let to = socket(b.0, 0, SocketDirection::Input);
        widget.graph_mut().add_connection(from, to);

        let layout = widget.build_layout(Pos2::ZERO);
        // A and B's reroute sockets sit on a horizontal line at y=12
        // (REROUTE_SIZE/2); this point sits right on that wire.
        let click = Pos2::new(100.0, 12.0);
        let idx = widget
            .wire_near_point(click, &layout.nodes)
            .expect("click should land on the wire");
        widget.insert_reroute_on_wire(idx, click);

        assert_eq!(widget.graph.connections.len(), 2);
        assert_eq!(widget.graph.nodes.len(), 3);
        let new_id = *widget
            .graph
            .nodes
            .keys()
            .find(|&&id| id != a && id != b)
            .expect("a third node should have been inserted");
        assert_eq!(widget.graph.nodes[&new_id].pos, click);
        assert!(
            widget
                .graph
                .connections
                .iter()
                .any(|c| c.from == from && c.to.node == new_id)
        );
        assert!(
            widget
                .graph
                .connections
                .iter()
                .any(|c| c.from.node == new_id && c.to == to)
        );
    }

    #[test]
    fn connectable_nodes_includes_a_node_with_a_compatible_socket() {
        // Reroute sockets are `Any`/`Any`, so B's input is trivially
        // compatible with A's output — the smallest fixture that exercises
        // `connectable_nodes` without needing typed node defs (Phase 4.3).
        use crate::runtime::NodeTypeRegistry;

        let mut widget = NodeGraphWidget::new(NodeTypeRegistry::new());
        let a = widget
            .add_node_at("Reroute", Pos2::new(0.0, 0.0))
            .expect("reroute should always be creatable");
        let b = widget
            .add_node_at("Reroute", Pos2::new(200.0, 0.0))
            .expect("reroute should always be creatable");

        let connectable = widget.connectable_nodes(SocketId {
            node: a,
            index: 0,
            direction: SocketDirection::Output,
        });

        assert!(connectable.contains(&b));
    }

    #[test]
    fn resolve_frame_membership_on_drop_never_ejects_a_current_member() {
        // Regression test: dragging can only ever *add* a node to a frame,
        // never remove it — no matter how far it's dragged. Removing is
        // exclusively the "Remove from Frame" action.
        use crate::runtime::NodeTypeRegistry;

        let mut widget = NodeGraphWidget::new(NodeTypeRegistry::new());
        let a = widget
            .add_node_at("Reroute", Pos2::new(0.0, 0.0))
            .expect("reroute should always be creatable");
        widget
            .graph
            .add_frame("F".to_owned(), egui::Color32::WHITE, vec![a]);

        widget.graph.nodes.get_mut(&a).unwrap().pos = Pos2::new(5000.0, 5000.0);
        let layout = widget.build_layout(Pos2::ZERO);
        widget.resolve_frame_membership_on_drop(&[a], &layout);

        assert_eq!(widget.graph.frames.len(), 1);
        assert!(widget.graph.frames[0].node_ids.contains(&a));
    }

    #[test]
    fn resolve_frame_membership_on_drop_never_moves_a_member_to_a_different_frame() {
        // A node already in frame A must never switch to frame B via drag,
        // even when dropped squarely inside B's bounds — only nodes with no
        // current frame can join one this way.
        use crate::runtime::NodeTypeRegistry;

        let mut widget = NodeGraphWidget::new(NodeTypeRegistry::new());
        let a = widget
            .add_node_at("Reroute", Pos2::new(0.0, 0.0))
            .expect("reroute should always be creatable");
        let frame_a = widget
            .graph
            .add_frame("A".to_owned(), egui::Color32::WHITE, vec![a]);
        let b = widget
            .add_node_at("Reroute", Pos2::new(5000.0, 5000.0))
            .expect("reroute should always be creatable");
        widget
            .graph
            .add_frame("B".to_owned(), egui::Color32::WHITE, vec![b]);

        widget.graph.nodes.get_mut(&a).unwrap().pos = Pos2::new(5000.0, 5000.0);
        let layout = widget.build_layout(Pos2::ZERO);
        widget.resolve_frame_membership_on_drop(&[a], &layout);

        let frame_a = widget
            .graph
            .frames
            .iter()
            .find(|f| f.id == frame_a)
            .expect("frame A should still exist");
        assert!(frame_a.node_ids.contains(&a));
    }

    #[test]
    fn resolve_frame_membership_on_drop_joins_node_dropped_inside_a_frame() {
        use crate::runtime::NodeTypeRegistry;

        let mut widget = NodeGraphWidget::new(NodeTypeRegistry::new());
        let anchor = widget
            .add_node_at("Reroute", Pos2::new(0.0, 0.0))
            .expect("reroute should always be creatable");
        let frame_id = widget
            .graph
            .add_frame("F".to_owned(), egui::Color32::WHITE, vec![anchor]);
        let mover = widget
            .add_node_at("Reroute", Pos2::new(5000.0, 5000.0))
            .expect("reroute should always be creatable");

        widget.graph.nodes.get_mut(&mover).unwrap().pos = Pos2::new(0.0, 0.0);
        let layout = widget.build_layout(Pos2::ZERO);
        widget.resolve_frame_membership_on_drop(&[mover], &layout);

        let frame = widget
            .graph
            .frames
            .iter()
            .find(|f| f.id == frame_id)
            .expect("frame should still exist");
        assert!(frame.node_ids.contains(&mover));
        assert!(frame.node_ids.contains(&anchor));
    }

    #[test]
    fn resolve_frame_membership_on_drop_does_not_join_a_frame_from_outside_its_bounds() {
        use crate::runtime::NodeTypeRegistry;

        let mut widget = NodeGraphWidget::new(NodeTypeRegistry::new());
        let anchor = widget
            .add_node_at("Reroute", Pos2::new(0.0, 0.0))
            .expect("reroute should always be creatable");
        widget
            .graph
            .add_frame("F".to_owned(), egui::Color32::WHITE, vec![anchor]);
        let mover = widget
            .add_node_at("Reroute", Pos2::new(5000.0, 5000.0))
            .expect("reroute should always be creatable");

        // Left well outside F's bounds — should not join.
        let layout = widget.build_layout(Pos2::ZERO);
        widget.resolve_frame_membership_on_drop(&[mover], &layout);

        assert_eq!(widget.graph.frames.len(), 1);
        assert!(!widget.graph.frames[0].node_ids.contains(&mover));
    }
}
