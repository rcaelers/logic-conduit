use super::{NodeGraphWidget, interaction::InteractionState, layout::GraphWidgetLayout, minimap};
use crate::{
    model::SocketDirection,
    support::paint::{
        draw_box_select, draw_connections, draw_frames, draw_grid, draw_knife_line, draw_wire,
    },
};
use egui::{Color32, Painter, Pos2, Rect, Stroke};

impl NodeGraphWidget {
    pub(super) fn draw_graph(
        &mut self,
        ui: &mut egui::Ui,
        painter: &Painter,
        rect: Rect,
        origin: Pos2,
        pointer: Option<Pos2>,
        layout: &GraphWidgetLayout,
    ) {
        let pointer_canvas = pointer.map(|p| self.view.screen_to_canvas(origin, p));
        let fast_interaction = self.interaction_state.use_fast_rendering();

        let hovered_wire =
            if let InteractionState::DraggingNode { node_id, .. } = self.interaction_state {
                let has_io = !fast_interaction
                    && !self.graph.nodes[&node_id].inputs.is_empty()
                    && !self.graph.nodes[&node_id].outputs.is_empty();
                if has_io {
                    self.compute_insert_candidate_wire(node_id, &layout.nodes)
                } else {
                    None
                }
            } else {
                self.compute_hovered_wire(pointer_canvas, &layout.nodes)
            };

        draw_grid(painter, rect, &self.view);
        draw_frames(
            painter,
            &self.graph,
            &layout.frame_rects,
            &self.view,
            origin,
        );

        let wire_w = (2.0 * self.view.zoom).clamp(1.0_f32, 4.0_f32);
        draw_connections(painter, &self.graph, &layout.socket_screen_pos, wire_w);
        let mut socket_highlights = Vec::new();

        if let Some(idx) = hovered_wire
            && let Some(conn) = self.graph.connections.get(idx)
            && let (Some(&fp), Some(&tp)) = (
                layout.socket_screen_pos.get(&conn.from),
                layout.socket_screen_pos.get(&conn.to),
            )
        {
            let base = self
                .graph
                .nodes
                .get(&conn.from.node)
                .and_then(|n| n.outputs.get(conn.from.index))
                .map(|s| s.color)
                .unwrap_or(Color32::from_rgb(160, 160, 160));
            let bright = Color32::from_rgba_unmultiplied(
                (base.r() as f32 * 1.5).min(255.0) as u8,
                (base.g() as f32 * 1.5).min(255.0) as u8,
                (base.b() as f32 * 1.5).min(255.0) as u8,
                255,
            );
            draw_wire(painter, fp, tp, bright, wire_w * 2.0);
        }

        if let InteractionState::DraggingWire {
            from,
            from_canvas,
            current_canvas,
        } = &self.interaction_state
        {
            let snap = self.snapped_wire_target(*from, *current_canvas, layout);
            let preview_end_canvas = snap.map_or(*current_canvas, |(_, pos)| pos);
            let color = self
                .graph
                .nodes
                .get(&from.node)
                .and_then(|n| {
                    if from.direction == SocketDirection::Output {
                        n.outputs.get(from.index).map(|s| s.color)
                    } else {
                        n.inputs.get(from.index).map(|s| s.color)
                    }
                })
                .unwrap_or(Color32::from_rgb(160, 160, 160));
            socket_highlights.push((*from_canvas, color));
            draw_wire(
                painter,
                self.view.canvas_to_screen(origin, *from_canvas),
                self.view.canvas_to_screen(origin, preview_end_canvas),
                color,
                wire_w,
            );
            if let Some((target, target_canvas)) = snap {
                let target_socket = self.graph.nodes.get(&target.node).and_then(|node| {
                    match target.direction {
                        SocketDirection::Input => node.inputs.get(target.index),
                        SocketDirection::Output => node.outputs.get(target.index),
                    }
                });
                // Preview what a multi-accepting input would resolve to: the
                // dragged output's type identity instead of the idle look.
                let from_type = self
                    .graph
                    .nodes
                    .get(&from.node)
                    .filter(|_| from.direction == SocketDirection::Output)
                    .and_then(|n| n.outputs.get(from.index))
                    .map(|s| s.effective_type().to_owned());
                let highlight = match (target_socket, from_type) {
                    (Some(socket), Some(ft))
                        if target.direction == SocketDirection::Input
                            && ft != "Any"
                            && ft != socket.type_name =>
                    {
                        self.registry
                            .socket_type_style(&ft)
                            .map(|style| style.color)
                            .unwrap_or(socket.color)
                    }
                    (Some(socket), _) => socket.color,
                    (None, _) => color,
                };
                socket_highlights.push((target_canvas, highlight));
            }
        }

        if let InteractionState::DraggingNode { node_id, .. } = self.interaction_state {
            self.top_node = Some(node_id);
        }
        let mut sorted = self.graph.sorted_node_ids();
        if let Some(top) = self.top_node {
            if sorted.contains(&top) {
                sorted.retain(|id| *id != top);
                sorted.push(top);
            } else {
                self.top_node = None;
            }
        }

        for id in sorted {
            if let (Some(widget), Some(node)) = (layout.nodes.get(&id), self.graph.nodes.get(&id)) {
                widget.draw(painter, node, &self.registry, &self.view, origin);
            }
            if self.view.zoom >= 0.6 && !fast_interaction {
                let changed = if let (Some(widget), Some(node), Some(instance)) = (
                    layout.nodes.get(&id),
                    self.graph.nodes.get(&id),
                    self.runtime.get_mut(&id),
                ) {
                    widget.show_controls(
                        ui,
                        id,
                        node,
                        instance.as_mut(),
                        &self.graph,
                        &self.view,
                        origin,
                    )
                } else {
                    false
                };
                if changed {
                    self.run_update(id);
                }
            }
        }

        for (socket_canvas, highlight) in socket_highlights {
            let center = self.view.canvas_to_screen(origin, socket_canvas);
            let radius = (7.0 * self.view.zoom).clamp(5.0, 10.0);
            painter.circle_filled(
                center,
                radius,
                Color32::from_rgba_premultiplied(highlight.r(), highlight.g(), highlight.b(), 45),
            );
            painter.circle_stroke(center, radius, Stroke::new(1.5_f32, Color32::WHITE));
        }

        if let InteractionState::BoxSelecting {
            start_canvas,
            current_canvas,
        } = &self.interaction_state
        {
            draw_box_select(
                painter,
                self.view.canvas_to_screen(origin, *start_canvas),
                self.view.canvas_to_screen(origin, *current_canvas),
            );
        }

        if let InteractionState::CuttingWire { path } = &self.interaction_state {
            let screen_pts: Vec<egui::Pos2> = path
                .iter()
                .map(|&p| self.view.canvas_to_screen(origin, p))
                .collect();
            if screen_pts.len() >= 2 {
                draw_knife_line(painter, &screen_pts);
            }
        }

        if self.minimap_visible && !fast_interaction {
            let (info, _) = minimap::compute_minimap(layout.node_rects.values().copied(), rect);
            minimap::draw_minimap(
                painter,
                &info,
                &self.graph,
                &layout.node_rects,
                &self.view,
                rect,
            );
        }

        self.draw_io_status(painter, rect, ui.ctx());
    }

    fn draw_io_status(&mut self, painter: &egui::Painter, rect: Rect, ctx: &egui::Context) {
        let Some((msg, start)) = &self.io_status else {
            return;
        };
        let elapsed = (ctx.input(|i| i.time) - start) as f32;
        if elapsed >= 3.0 {
            self.io_status = None;
            return;
        }
        let alpha = ((3.0 - elapsed) / 0.6).clamp(0.0, 1.0);
        let pos = Pos2::new(rect.center().x, rect.min.y + 12.0);
        let msg = msg.clone();
        painter.text(
            pos,
            egui::Align2::CENTER_TOP,
            &msg,
            egui::FontId::proportional(13.0),
            Color32::from_rgba_premultiplied(220, 220, 220, (alpha * 230.0) as u8),
        );
        ctx.request_repaint();
    }

    pub(super) fn show_frame_rename(&mut self, ctx: &egui::Context) {
        let Some(state) = &mut self.frame_rename else {
            return;
        };

        let mut apply = false;
        let mut cancel = false;
        egui::Window::new("Rename Frame")
            .id(egui::Id::new("node_graph_rename_frame"))
            .fixed_pos(state.screen_pos)
            .collapsible(false)
            .resizable(false)
            .show(ctx, |ui| {
                let response = ui.add(
                    egui::TextEdit::singleline(&mut state.text)
                        .desired_width(240.0)
                        .hint_text("Frame name"),
                );
                response.request_focus();
                if response.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                    apply = true;
                }
                ui.horizontal(|ui| {
                    if ui.button("OK").clicked() {
                        apply = true;
                    }
                    if ui.button("Cancel").clicked() {
                        cancel = true;
                    }
                });
            });

        if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
            cancel = true;
        }
        if apply {
            if let Some(state) = self.frame_rename.take() {
                self.push_undo_snapshot();
                if let Some(frame) = self
                    .graph
                    .frames
                    .iter_mut()
                    .find(|frame| frame.id == state.frame_id)
                {
                    frame.label = state.text;
                }
            }
        } else if cancel {
            self.frame_rename = None;
        }
    }
}
