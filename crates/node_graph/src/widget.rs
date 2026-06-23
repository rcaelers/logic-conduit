use crate::{
    draw::{
        NodeLayout, SOCKET_RADIUS, bezier_wire_distance, compute_node_layout, draw_box_select,
        draw_connections, draw_frames, draw_grid, draw_knife_line, draw_node, draw_wire,
        to_screen_rect, wire_intersects_knife,
    },
    graph::{
        Connection, GraphState, InputSocket, Node, NodeClassDef, NodeDef, NodeId, NodeKind, Prop,
        Socket, SocketId,
    },
    interaction::InteractionState,
    minimap,
    types::sockets_compatible,
    view::ViewState,
};
use egui::{Color32, Pos2, Rect, Sense, Ui, Vec2};
use std::collections::HashMap;

const WIRE_INSERT_THRESHOLD: f32 = 40.0;

static FRAME_COLORS: [Color32; 5] = [
    Color32::from_rgb(50, 90, 160),
    Color32::from_rgb(50, 130, 80),
    Color32::from_rgb(160, 100, 50),
    Color32::from_rgb(110, 60, 160),
    Color32::from_rgb(160, 60, 60),
];

// ── Node type registry ────────────────────────────────────────────────────────

#[derive(Default)]
pub struct NodeTypeRegistry {
    types: Vec<NodeClassDef>,
}

impl NodeTypeRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register<T: NodeDef>(&mut self) -> &mut Self {
        self.types.push(NodeClassDef::from_def::<T>());
        self
    }

    pub(crate) fn all(&self) -> &[NodeClassDef] {
        &self.types
    }

    pub(crate) fn find(&self, name: &str) -> Option<&NodeClassDef> {
        self.types.iter().find(|d| d.name == name)
    }

    pub fn instantiate(&self, name: &str, id: NodeId, pos: Pos2) -> Option<Node> {
        let def = self.find(name)?;

        let inputs = def
            .inputs
            .iter()
            .map(|d| InputSocket {
                name: d.name.clone(),
                type_name: d.type_name.clone(),
                color: d.color,
                shape: d.shape,
                visible: true,
                value: d.value.clone(),
            })
            .collect();

        let outputs = def
            .outputs
            .iter()
            .map(|d| Socket {
                name: d.name.clone(),
                type_name: d.type_name.clone(),
                color: d.color,
                shape: d.shape,
                visible: true,
            })
            .collect();

        let props = def
            .props
            .iter()
            .map(|p| Prop {
                id: p.id.clone(),
                label: p.label.clone(),
                value: p.value.clone(),
            })
            .collect();

        let mut node = Node {
            id,
            kind: NodeKind::Regular,
            title: def.name.clone(),
            header_color: def.header_color,
            pos,
            inputs,
            outputs,
            props,
            selected: false,
            update_fn: def.update_fn,
        };
        node.run_update();
        Some(node)
    }
}

// ── Main widget ───────────────────────────────────────────────────────────────

pub struct NodeGraphWidget {
    graph: GraphState,
    view: ViewState,
    interaction: InteractionState,
    registry: NodeTypeRegistry,
    context_pos: Pos2,
    minimap_visible: bool,
    show_search: bool,
    search_text: String,
    search_selected: usize,
    search_insert_pos: Pos2,
    search_just_opened: bool,
    status: Option<(String, f64)>,
}

impl NodeGraphWidget {
    pub fn new(registry: NodeTypeRegistry) -> Self {
        Self {
            graph: GraphState::default(),
            view: ViewState::default(),
            interaction: InteractionState::default(),
            registry,
            context_pos: Pos2::ZERO,
            minimap_visible: true,
            show_search: false,
            search_text: String::new(),
            search_selected: 0,
            search_insert_pos: Pos2::ZERO,
            search_just_opened: false,
            status: None,
        }
    }

    pub fn graph(&self) -> &GraphState {
        &self.graph
    }
    pub fn graph_mut(&mut self) -> &mut GraphState {
        &mut self.graph
    }

    pub fn add_node_at(&mut self, name: &str, pos: Pos2) -> Option<NodeId> {
        let id = self.graph.next_id();
        if name == "Reroute" {
            let n = Node::new_reroute(id, pos);
            let nid = n.id;
            self.graph.add_node(n);
            return Some(nid);
        }
        if let Some(node) = self.registry.instantiate(name, id, pos) {
            let nid = node.id;
            self.graph.add_node(node);
            Some(nid)
        } else {
            None
        }
    }

    pub fn show(&mut self, ui: &mut Ui) {
        let rect = ui.available_rect_before_wrap();
        let response = ui.allocate_rect(rect, Sense::click_and_drag());
        let painter = ui.painter_at(rect);
        let origin = rect.min;

        draw_grid(&painter, rect, &self.view);

        let pointer = response
            .hover_pos()
            .or_else(|| ui.input(|i| i.pointer.hover_pos()));
        let pointer_canvas = pointer.map(|p| self.view.screen_to_canvas(origin, p));

        let mut socket_screen_pos: HashMap<SocketId, Pos2> = HashMap::new();
        let mut layouts: HashMap<NodeId, NodeLayout> = HashMap::new();
        for id in self.graph.sorted_node_ids() {
            let node = &self.graph.nodes[&id];
            let layout = compute_node_layout(node);
            for (i, _) in node.inputs.iter().enumerate() {
                if let Some(pos) = layout.input_socket_pos[i] {
                    socket_screen_pos.insert(
                        SocketId {
                            node: id,
                            index: i,
                            is_output: false,
                        },
                        self.view.canvas_to_screen(origin, pos),
                    );
                }
            }
            for (i, _) in node.outputs.iter().enumerate() {
                if let Some(pos) = layout.output_socket_pos[i] {
                    socket_screen_pos.insert(
                        SocketId {
                            node: id,
                            index: i,
                            is_output: true,
                        },
                        self.view.canvas_to_screen(origin, pos),
                    );
                }
            }
            layouts.insert(id, layout);
        }

        let hovered_wire = if let InteractionState::DraggingNode { node_id, .. } = self.interaction
        {
            let has_io = !self.graph.nodes[&node_id].inputs.is_empty()
                && !self.graph.nodes[&node_id].outputs.is_empty();
            if has_io {
                self.compute_insert_candidate_wire(node_id, &layouts)
            } else {
                None
            }
        } else {
            self.compute_hovered_wire(pointer_canvas, &layouts)
        };

        draw_frames(&painter, &self.graph, &layouts, &self.view, origin);

        let wire_w = (2.0 * self.view.zoom).clamp(1.0_f32, 4.0_f32);
        draw_connections(&painter, &self.graph, &socket_screen_pos, wire_w);

        if let Some(idx) = hovered_wire
            && let Some(conn) = self.graph.connections.get(idx)
            && let (Some(&fp), Some(&tp)) = (
                socket_screen_pos.get(&conn.from),
                socket_screen_pos.get(&conn.to),
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
            draw_wire(&painter, fp, tp, bright, wire_w * 2.0);
        }

        if let InteractionState::DraggingWire {
            from,
            from_canvas,
            current_canvas,
        } = &self.interaction
        {
            let color = self
                .graph
                .nodes
                .get(&from.node)
                .and_then(|n| {
                    if from.is_output {
                        n.outputs.get(from.index).map(|s| s.color)
                    } else {
                        n.inputs.get(from.index).map(|s| s.color)
                    }
                })
                .unwrap_or(Color32::from_rgb(160, 160, 160));
            draw_wire(
                &painter,
                self.view.canvas_to_screen(origin, *from_canvas),
                self.view.canvas_to_screen(origin, *current_canvas),
                color,
                wire_w,
            );
        }

        for id in self.graph.sorted_node_ids() {
            if let (Some(layout), Some(node)) = (layouts.get(&id), self.graph.nodes.get(&id)) {
                draw_node(&painter, node, layout, &self.view, origin);
            }
            if self.view.zoom >= 0.6 {
                self.draw_node_inline_widgets(ui, origin, &layouts, id);
            }
        }

        if let InteractionState::BoxSelecting {
            start_canvas,
            current_canvas,
        } = &self.interaction
        {
            draw_box_select(
                &painter,
                self.view.canvas_to_screen(origin, *start_canvas),
                self.view.canvas_to_screen(origin, *current_canvas),
            );
        }

        if let InteractionState::CuttingWire {
            start_canvas,
            current_canvas,
        } = &self.interaction
        {
            draw_knife_line(
                &painter,
                self.view.canvas_to_screen(origin, *start_canvas),
                self.view.canvas_to_screen(origin, *current_canvas),
            );
        }

        self.handle_input(ui, &response, origin, &layouts, &socket_screen_pos, rect);

        if self.minimap_visible {
            let (info, _) = minimap::compute_minimap(&layouts, rect);
            minimap::draw_minimap(&painter, &info, &self.graph, &layouts, &self.view, rect);
        }

        self.draw_search_overlay(ui, origin);
        self.draw_status(&painter, rect, ui.ctx());
    }

    // ── Registry helpers ──────────────────────────────────────────────────────

    fn filtered_types(&self, query: &str) -> Vec<(String, String)> {
        let q = query.to_lowercase();
        let mut result: Vec<(String, String)> = self
            .registry
            .all()
            .iter()
            .filter(|d| {
                q.is_empty()
                    || d.name.to_lowercase().contains(&q)
                    || d.category.to_lowercase().contains(&q)
            })
            .map(|d| (d.name.clone(), d.category.clone()))
            .collect();
        if q.is_empty() || "reroute".contains(&q) || "utility".contains(&q) {
            result.push(("Reroute".to_string(), "Utility".to_string()));
        }
        result
    }

    fn add_from_registry(&mut self, name: &str, pos: Pos2) {
        self.add_node_at(name, pos);
    }

    // ── Inline widgets ────────────────────────────────────────────────────────

    fn draw_node_inline_widgets(
        &mut self,
        ui: &mut Ui,
        origin: Pos2,
        layouts: &HashMap<NodeId, NodeLayout>,
        id: NodeId,
    ) {
        let node_screen_rect = to_screen_rect(layouts[&id].node_rect, &self.view, origin);

        // Input socket default-value widgets
        let n_inputs = self.graph.nodes[&id].inputs.len();
        for i in 0..n_inputs {
            let sid = SocketId {
                node: id,
                index: i,
                is_output: false,
            };
            if self.graph.is_input_connected(sid) {
                continue;
            }
            let Some(wr) = layouts[&id].input_widget_rects.get(i).and_then(|r| *r) else {
                continue;
            };
            let ws = to_screen_rect(wr, &self.view, origin);
            if ws.width() < 30.0 {
                continue;
            }

            let node = self.graph.nodes.get_mut(&id).unwrap();
            let Some(sock) = node.inputs.get_mut(i) else {
                continue;
            };
            let Some(value) = sock.value.as_mut() else {
                continue;
            };

            let changed =
                value.draw_widget(ui, &sock.name.clone(), ws, self.view.zoom, node_screen_rect);
            if changed && let Some(node) = self.graph.nodes.get_mut(&id) {
                node.run_update();
            }
        }

        // Node property widgets
        let n_props = self.graph.nodes[&id].props.len();
        let mut any_changed = false;
        for pi in 0..n_props {
            let Some(pr) = layouts[&id].prop_rects.get(pi).copied() else {
                continue;
            };
            let ws = to_screen_rect(pr, &self.view, origin);
            if ws.width() < 40.0 {
                continue;
            }

            let node = self.graph.nodes.get_mut(&id).unwrap();
            let prop = &mut node.props[pi];
            let label = prop.label.clone();

            let changed = prop
                .value
                .draw_widget(ui, &label, ws, self.view.zoom, node_screen_rect);
            if changed {
                any_changed = true;
            }
        }

        if any_changed && let Some(node) = self.graph.nodes.get_mut(&id) {
            node.run_update();
        }
    }

    // ── Search overlay ────────────────────────────────────────────────────────

    fn draw_search_overlay(&mut self, ui: &mut Ui, origin: Pos2) {
        if !self.show_search {
            return;
        }

        let screen_pos = self.view.canvas_to_screen(origin, self.search_insert_pos);
        let win_size = Vec2::new(230.0, 270.0);
        let pos = Pos2::new(
            screen_pos
                .x
                .min(origin.x + ui.available_width() - win_size.x - 10.0)
                .max(origin.x + 10.0),
            screen_pos
                .y
                .min(origin.y + ui.available_height() - win_size.y - 10.0)
                .max(origin.y + 10.0),
        );

        let filtered = self.filtered_types(&self.search_text);
        let count = filtered.len();
        self.search_selected = self.search_selected.min(count.saturating_sub(1));

        let mut select_idx: Option<usize> = None;
        let mut close = false;

        egui::Area::new(egui::Id::new("node_search_overlay"))
            .fixed_pos(pos)
            .order(egui::Order::Foreground)
            .show(ui.ctx(), |ui| {
                egui::Frame::window(ui.style()).show(ui, |ui| {
                    ui.set_width(220.0);
                    ui.label(egui::RichText::new("Add Node  (Shift+A)").strong());
                    ui.separator();

                    let te = ui.add(
                        egui::TextEdit::singleline(&mut self.search_text)
                            .hint_text("Search…")
                            .desired_width(f32::INFINITY),
                    );
                    if self.search_just_opened {
                        te.request_focus();
                        self.search_just_opened = false;
                    }
                    ui.separator();

                    egui::ScrollArea::vertical()
                        .max_height(180.0)
                        .show(ui, |ui| {
                            for (i, (name, cat)) in filtered.iter().enumerate() {
                                let selected = i == self.search_selected;
                                let label = egui::RichText::new(format!("{name}  ({cat})"));
                                let label = if selected {
                                    label.strong().color(Color32::from_rgb(180, 210, 255))
                                } else {
                                    label
                                };
                                if ui.selectable_label(selected, label).clicked() {
                                    select_idx = Some(i);
                                    close = true;
                                }
                            }
                            if count == 0 {
                                ui.label(egui::RichText::new("No results").weak().italics());
                            }
                        });
                });
            });

        if close {
            self.show_search = false;
            self.search_text.clear();
            self.search_selected = 0;
        }
        if let Some(idx) = select_idx {
            let (name, _) = filtered[idx].clone();
            let pos = self.search_insert_pos;
            self.add_from_registry(&name, pos);
        }
    }

    fn commit_search(&mut self) {
        let filtered = self.filtered_types(&self.search_text);
        let idx = self.search_selected.min(filtered.len().saturating_sub(1));
        if let Some((name, _)) = filtered.get(idx).cloned() {
            let pos = self.search_insert_pos;
            self.add_from_registry(&name, pos);
        }
        self.show_search = false;
        self.search_text.clear();
        self.search_selected = 0;
    }

    // ── Status display ────────────────────────────────────────────────────────

    fn draw_status(&mut self, painter: &egui::Painter, rect: Rect, ctx: &egui::Context) {
        let Some((msg, start)) = &self.status else {
            return;
        };
        let elapsed = (ctx.input(|i| i.time) - start) as f32;
        if elapsed >= 3.0 {
            self.status = None;
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

    // ── Save / Load ───────────────────────────────────────────────────────────

    fn save_graph(&mut self, ctx: &egui::Context) {
        let t = ctx.input(|i| i.time);
        match serde_json::to_string_pretty(&self.graph) {
            Ok(json) => match std::fs::write("pipeline.json", &json) {
                Ok(_) => self.status = Some(("Saved  pipeline.json".to_string(), t)),
                Err(e) => self.status = Some((format!("Save failed: {e}"), t)),
            },
            Err(e) => self.status = Some((format!("Serialization error: {e}"), t)),
        }
    }

    fn load_graph(&mut self, ctx: &egui::Context) {
        let t = ctx.input(|i| i.time);
        match std::fs::read_to_string("pipeline.json") {
            Ok(json) => match serde_json::from_str(&json) {
                Ok(graph) => {
                    self.graph = graph;
                    let names: Vec<(NodeId, String)> = self
                        .graph
                        .nodes
                        .values()
                        .map(|n| (n.id, n.title.clone()))
                        .collect();
                    for (node_id, title) in names {
                        let update_fn = self.registry.find(&title).and_then(|d| d.update_fn);
                        if let Some(n) = self.graph.nodes.get_mut(&node_id) {
                            n.update_fn = update_fn;
                        }
                    }
                    self.interaction = InteractionState::Idle;
                    self.status = Some(("Loaded  pipeline.json".to_string(), t));
                }
                Err(e) => self.status = Some((format!("Parse error: {e}"), t)),
            },
            Err(e) => self.status = Some((format!("Load failed: {e}"), t)),
        }
    }

    // ── Input handling ────────────────────────────────────────────────────────

    fn handle_input(
        &mut self,
        ui: &mut Ui,
        response: &egui::Response,
        origin: Pos2,
        layouts: &HashMap<NodeId, NodeLayout>,
        socket_screen_pos: &HashMap<SocketId, Pos2>,
        canvas_rect: Rect,
    ) {
        let scroll = ui.input(|i| i.smooth_scroll_delta.y);
        if scroll.abs() > 0.1
            && let Some(cursor) = ui.input(|i| i.pointer.hover_pos())
            && canvas_rect.contains(cursor)
        {
            self.view.zoom_around(cursor, origin, (1.0_f32 + scroll * 0.003).clamp(0.5, 2.0));
        }

        let pointer = response
            .hover_pos()
            .or_else(|| ui.input(|i| i.pointer.hover_pos()));
        let pointer_canvas = pointer.map(|p| self.view.screen_to_canvas(origin, p));

        if ui.input(|i| i.pointer.button_pressed(egui::PointerButton::Secondary))
            && let Some(pc) = pointer_canvas
        {
            self.context_pos = pc;
        }

        if self.show_search {
            let count = self.filtered_types(&self.search_text).len();
            if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                self.show_search = false;
                self.search_text.clear();
                self.search_selected = 0;
            } else if ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                self.commit_search();
            } else if ui.input(|i| i.key_pressed(egui::Key::ArrowDown)) {
                self.search_selected = (self.search_selected + 1).min(count.saturating_sub(1));
            } else if ui.input(|i| i.key_pressed(egui::Key::ArrowUp)) {
                self.search_selected = self.search_selected.saturating_sub(1);
            }
            let state = std::mem::replace(&mut self.interaction, InteractionState::Idle);
            self.interaction = match state {
                InteractionState::Panning { last_screen } => {
                    self.update_panning(response, pointer, last_screen)
                }
                InteractionState::DraggingNode { node_id, offset } => {
                    self.update_drag_node(response, pointer_canvas, node_id, offset, layouts)
                }
                InteractionState::DraggingWire {
                    from,
                    from_canvas,
                    current_canvas,
                } => self.update_drag_wire(
                    response,
                    pointer,
                    pointer_canvas,
                    socket_screen_pos,
                    from,
                    from_canvas,
                    current_canvas,
                ),
                other => other,
            };
            return;
        }

        if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
            self.interaction = InteractionState::Idle;
        }
        if ui.input(|i| i.key_pressed(egui::Key::Delete) || i.key_pressed(egui::Key::Backspace)) {
            self.delete_selected();
        }
        if ui.input(|i| i.key_pressed(egui::Key::D) && i.modifiers.shift) {
            self.duplicate_selected();
        }

        if ui.input(|i| i.key_pressed(egui::Key::A) && i.modifiers.shift) {
            self.search_insert_pos = pointer_canvas.unwrap_or(self.context_pos);
            self.show_search = true;
            self.search_just_opened = true;
            self.search_text.clear();
            self.search_selected = 0;
        }

        if ui.input(|i| i.key_pressed(egui::Key::J) && i.modifiers.ctrl) {
            let selected: Vec<NodeId> = self
                .graph
                .nodes
                .values()
                .filter(|n| n.selected)
                .map(|n| n.id)
                .collect();
            if !selected.is_empty() {
                let idx = self.graph.frames.len() % FRAME_COLORS.len();
                let color = FRAME_COLORS[idx];
                self.graph.add_frame("Frame".to_string(), color, selected);
            }
        }

        if ui.input(|i| i.key_pressed(egui::Key::M)) {
            self.minimap_visible = !self.minimap_visible;
        }

        if ui.input(|i| i.key_pressed(egui::Key::S) && i.modifiers.ctrl) {
            let ctx = ui.ctx().clone();
            self.save_graph(&ctx);
        }
        if ui.input(|i| i.key_pressed(egui::Key::O) && i.modifiers.ctrl) {
            let ctx = ui.ctx().clone();
            self.load_graph(&ctx);
        }

        if response.clicked() && !ui.ctx().egui_is_using_pointer() {
            self.handle_selection_click(ui, pointer, layouts, origin);
        }

        let cats_and_items: Vec<(String, Vec<String>)> = {
            let mut cats: Vec<String> = Vec::new();
            let mut map: HashMap<String, Vec<String>> = HashMap::new();
            for def in self.registry.all() {
                map.entry(def.category.clone())
                    .or_default()
                    .push(def.name.clone());
                if !cats.contains(&def.category) {
                    cats.push(def.category.clone());
                }
            }
            cats.into_iter()
                .map(|c| {
                    let names = map.remove(&c).unwrap_or_default();
                    (c, names)
                })
                .collect()
        };

        let mut add_kind: Option<String> = None;
        response.context_menu(|ui| {
            ui.set_min_width(180.0);
            ui.label(egui::RichText::new("Add Node").strong());
            ui.separator();
            for (cat, names) in &cats_and_items {
                ui.label(egui::RichText::new(cat.as_str()).weak());
                for name in names {
                    if ui.button(name.as_str()).clicked() {
                        add_kind = Some(name.clone());
                        ui.close();
                    }
                }
                ui.separator();
            }
            ui.label(egui::RichText::new("Utility").weak());
            if ui.button("Reroute").clicked() {
                add_kind = Some("Reroute".to_string());
                ui.close();
            }
        });
        if let Some(kind) = add_kind {
            let pos = self.context_pos;
            self.add_from_registry(&kind, pos);
        }

        if matches!(self.interaction, InteractionState::Idle)
            && self.minimap_visible
            && let Some(pp) = pointer
        {
            let (info, mini_rect) = minimap::compute_minimap(layouts, canvas_rect);
            if mini_rect.contains(pp) && (response.drag_started() || response.dragged()) {
                let canvas_pos = info.mini_to_canvas(pp);
                self.view.pan =
                    (canvas_rect.center() - origin) - canvas_pos.to_vec2() * self.view.zoom;
                return;
            }
        }

        let state = std::mem::replace(&mut self.interaction, InteractionState::Idle);
        self.interaction = match state {
            InteractionState::Idle => self.idle_transition(
                ui,
                response,
                pointer,
                pointer_canvas,
                origin,
                layouts,
                socket_screen_pos,
            ),
            InteractionState::Panning { last_screen } => {
                self.update_panning(response, pointer, last_screen)
            }
            InteractionState::DraggingNode { node_id, offset } => {
                self.update_drag_node(response, pointer_canvas, node_id, offset, layouts)
            }
            InteractionState::DraggingWire {
                from,
                from_canvas,
                current_canvas,
            } => self.update_drag_wire(
                response,
                pointer,
                pointer_canvas,
                socket_screen_pos,
                from,
                from_canvas,
                current_canvas,
            ),
            InteractionState::BoxSelecting {
                start_canvas,
                current_canvas,
            } => self.update_box_select(
                ui,
                response,
                pointer_canvas,
                layouts,
                start_canvas,
                current_canvas,
            ),
            InteractionState::CuttingWire {
                start_canvas,
                current_canvas,
            } => self.update_cut_wire(response, pointer_canvas, layouts, start_canvas, current_canvas),
        };
    }

    // ── Keyboard actions ──────────────────────────────────────────────────────

    fn delete_selected(&mut self) {
        let to_delete: Vec<NodeId> = self
            .graph
            .nodes
            .values()
            .filter(|n| n.selected)
            .map(|n| n.id)
            .collect();
        for id in to_delete {
            self.graph.remove_node(id);
        }
        self.graph.cleanup_frames();
    }

    fn duplicate_selected(&mut self) {
        let selected: Vec<NodeId> = self
            .graph
            .nodes
            .values()
            .filter(|n| n.selected)
            .map(|n| n.id)
            .collect();
        if selected.is_empty() {
            return;
        }

        let mut id_map: HashMap<NodeId, NodeId> = HashMap::new();
        let clones: Vec<Node> = selected
            .iter()
            .map(|&old| {
                let new_id = self.graph.next_id();
                id_map.insert(old, new_id);
                let mut node = self.graph.nodes[&old].clone();
                node.id = new_id;
                node.pos += Vec2::new(30.0, 30.0);
                node.selected = true;
                node
            })
            .collect();

        for id in &selected {
            if let Some(n) = self.graph.nodes.get_mut(id) {
                n.selected = false;
            }
        }
        for node in clones {
            self.graph.add_node(node);
        }

        let new_conns: Vec<Connection> = self
            .graph
            .connections
            .iter()
            .filter(|c| id_map.contains_key(&c.from.node) && id_map.contains_key(&c.to.node))
            .map(|c| Connection {
                from: SocketId {
                    node: id_map[&c.from.node],
                    ..c.from
                },
                to: SocketId {
                    node: id_map[&c.to.node],
                    ..c.to
                },
            })
            .collect();
        self.graph.connections.extend(new_conns);
    }

    // ── Click / selection ─────────────────────────────────────────────────────

    fn handle_selection_click(
        &mut self,
        ui: &mut Ui,
        pointer: Option<Pos2>,
        layouts: &HashMap<NodeId, NodeLayout>,
        origin: Pos2,
    ) {
        let Some(pp) = pointer else { return };
        let ctrl = ui.input(|i| i.modifiers.ctrl);
        let hit = self.node_at_screen(pp, layouts, origin);
        if let Some(id) = hit {
            if !ctrl {
                for n in self.graph.nodes.values_mut() {
                    n.selected = false;
                }
            }
            let node = self.graph.nodes.get_mut(&id).unwrap();
            if ctrl {
                node.selected = !node.selected;
            } else {
                node.selected = true;
            }
        } else if !ctrl {
            for n in self.graph.nodes.values_mut() {
                n.selected = false;
            }
        }
    }

    fn node_at_screen(
        &self,
        pp: Pos2,
        layouts: &HashMap<NodeId, NodeLayout>,
        origin: Pos2,
    ) -> Option<NodeId> {
        for (&id, layout) in layouts {
            if to_screen_rect(layout.node_rect, &self.view, origin).contains(pp) {
                return Some(id);
            }
        }
        None
    }

    fn compute_hovered_wire(
        &self,
        pointer_canvas: Option<Pos2>,
        layouts: &HashMap<NodeId, NodeLayout>,
    ) -> Option<usize> {
        let pc = pointer_canvas?;
        if matches!(
            self.interaction,
            InteractionState::DraggingWire { .. } | InteractionState::DraggingNode { .. }
        ) {
            return None;
        }
        let threshold = 10.0 / self.view.zoom;
        let mut best: Option<(usize, f32)> = None;
        for (idx, conn) in self.graph.connections.iter().enumerate() {
            let Some(fl) = layouts.get(&conn.from.node) else {
                continue;
            };
            let Some(fp) = fl.output_socket_pos.get(conn.from.index).and_then(|p| *p) else {
                continue;
            };
            let Some(tl) = layouts.get(&conn.to.node) else {
                continue;
            };
            let Some(tp) = tl.input_socket_pos.get(conn.to.index).and_then(|p| *p) else {
                continue;
            };
            let dist = bezier_wire_distance(fp, tp, pc);
            if dist < best.map_or(threshold, |(_, d)| d) {
                best = Some((idx, dist));
            }
        }
        best.map(|(idx, _)| idx)
    }

    fn compute_insert_candidate_wire(
        &self,
        node_id: NodeId,
        layouts: &HashMap<NodeId, NodeLayout>,
    ) -> Option<usize> {
        let node_center = layouts.get(&node_id)?.node_rect.center();
        let nn = &self.graph.nodes[&node_id];
        let mut best: Option<(usize, f32)> = None;
        for (idx, conn) in self.graph.connections.iter().enumerate() {
            if conn.from.node == node_id || conn.to.node == node_id {
                continue;
            }
            let fp = layouts
                .get(&conn.from.node)
                .and_then(|l| l.output_socket_pos.get(conn.from.index).and_then(|p| *p));
            let tp = layouts
                .get(&conn.to.node)
                .and_then(|l| l.input_socket_pos.get(conn.to.index).and_then(|p| *p));
            let (Some(fp), Some(tp)) = (fp, tp) else {
                continue;
            };
            let src_t = self
                .graph
                .nodes
                .get(&conn.from.node)
                .and_then(|n| n.outputs.get(conn.from.index))
                .map(|s| s.type_name.as_str());
            let dst_t = self
                .graph
                .nodes
                .get(&conn.to.node)
                .and_then(|n| n.inputs.get(conn.to.index))
                .map(|s| s.type_name.as_str());
            let ok_in = src_t.is_some_and(|t| {
                nn.inputs
                    .iter()
                    .any(|s| s.visible && sockets_compatible(&s.type_name, t))
            });
            let ok_out = dst_t.is_some_and(|t| {
                nn.outputs
                    .iter()
                    .any(|s| s.visible && sockets_compatible(&s.type_name, t))
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

    // ── Drag state machine ────────────────────────────────────────────────────

    #[allow(clippy::too_many_arguments)]
    fn idle_transition(
        &mut self,
        ui: &mut Ui,
        response: &egui::Response,
        pointer: Option<Pos2>,
        pointer_canvas: Option<Pos2>,
        origin: Pos2,
        layouts: &HashMap<NodeId, NodeLayout>,
        socket_screen_pos: &HashMap<SocketId, Pos2>,
    ) -> InteractionState {
        if !response.drag_started() {
            return InteractionState::Idle;
        }
        let Some(pp) = pointer else {
            return InteractionState::Idle;
        };
        let Some(pc) = pointer_canvas else {
            return InteractionState::Idle;
        };

        let middle = ui.input(|i| i.pointer.button_pressed(egui::PointerButton::Middle));
        let right = ui.input(|i| i.pointer.button_pressed(egui::PointerButton::Secondary));
        if middle || right {
            return InteractionState::Panning { last_screen: pp };
        }

        let hit_r = SOCKET_RADIUS * self.view.zoom + 5.0;
        for (&sid, &spos) in socket_screen_pos {
            if pp.distance(spos) >= hit_r {
                continue;
            }

            if !sid.is_output
                && let Some(src) = self.graph.connections.iter().find(|c| c.to == sid).map(|c| c.from)
                && let Some(&src_spos) = socket_screen_pos.get(&src)
            {
                self.graph.connections.retain(|c| c.to != sid);
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

        for (&id, layout) in layouts {
            if to_screen_rect(layout.header_rect, &self.view, origin).contains(pp) {
                let node_pos = self.graph.nodes[&id].pos.to_vec2();
                let ctrl = ui.input(|i| i.modifiers.ctrl);
                if ctrl {
                    self.graph.nodes.get_mut(&id).unwrap().selected = true;
                } else if !self.graph.nodes[&id].selected {
                    for n in self.graph.nodes.values_mut() {
                        n.selected = false;
                    }
                    self.graph.nodes.get_mut(&id).unwrap().selected = true;
                }
                return InteractionState::DraggingNode {
                    node_id: id,
                    offset: pc.to_vec2() - node_pos,
                };
            }
        }

        let ctrl = ui.input(|i| i.modifiers.ctrl);
        if ctrl {
            InteractionState::BoxSelecting {
                start_canvas: pc,
                current_canvas: pc,
            }
        } else {
            InteractionState::CuttingWire {
                start_canvas: pc,
                current_canvas: pc,
            }
        }
    }

    fn update_panning(
        &mut self,
        response: &egui::Response,
        pointer: Option<Pos2>,
        last_screen: Pos2,
    ) -> InteractionState {
        if response.dragged() && let Some(pp) = pointer {
            self.view.pan += pp - last_screen;
            return InteractionState::Panning { last_screen: pp };
        }
        InteractionState::Idle
    }

    fn update_drag_node(
        &mut self,
        response: &egui::Response,
        pointer_canvas: Option<Pos2>,
        node_id: NodeId,
        offset: Vec2,
        layouts: &HashMap<NodeId, NodeLayout>,
    ) -> InteractionState {
        if response.dragged() {
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
            self.try_wire_insert(node_id, layouts);
        }
        InteractionState::Idle
    }

    fn try_wire_insert(&mut self, node_id: NodeId, layouts: &HashMap<NodeId, NodeLayout>) {
        let node_center = compute_node_layout(&self.graph.nodes[&node_id])
            .node_rect
            .center();
        let mut best: Option<(usize, f32)> = None;
        for (idx, conn) in self.graph.connections.iter().enumerate() {
            if conn.from.node == node_id || conn.to.node == node_id {
                continue;
            }
            let fp = layouts
                .get(&conn.from.node)
                .and_then(|l| l.output_socket_pos.get(conn.from.index).and_then(|p| *p));
            let tp = layouts
                .get(&conn.to.node)
                .and_then(|l| l.input_socket_pos.get(conn.to.index).and_then(|p| *p));
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
            .map(|s| s.type_name.clone());
        let dst_type = self
            .graph
            .nodes
            .get(&conn.to.node)
            .and_then(|n| n.inputs.get(conn.to.index))
            .map(|s| s.type_name.clone());
        let nn = &self.graph.nodes[&node_id];

        let in_idx = src_type.as_deref().and_then(|t| {
            nn.inputs
                .iter()
                .position(|s| s.visible && sockets_compatible(&s.type_name, t))
        });
        let out_idx = dst_type.as_deref().and_then(|t| {
            nn.outputs
                .iter()
                .position(|s| s.visible && sockets_compatible(&s.type_name, t))
        });

        if let (Some(ii), Some(oi)) = (in_idx, out_idx) {
            self.graph.connections.remove(idx);
            self.graph.add_connection(
                conn.from,
                SocketId {
                    node: node_id,
                    index: ii,
                    is_output: false,
                },
            );
            self.graph.add_connection(
                SocketId {
                    node: node_id,
                    index: oi,
                    is_output: true,
                },
                conn.to,
            );
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn update_drag_wire(
        &mut self,
        response: &egui::Response,
        pointer: Option<Pos2>,
        pointer_canvas: Option<Pos2>,
        socket_screen_pos: &HashMap<SocketId, Pos2>,
        from: SocketId,
        from_canvas: Pos2,
        mut current_canvas: Pos2,
    ) -> InteractionState {
        if response.dragged() {
            if let Some(pc) = pointer_canvas {
                current_canvas = pc;
            }
            return InteractionState::DraggingWire {
                from,
                from_canvas,
                current_canvas,
            };
        }
        if let Some(pp) = pointer {
            let hit_r = SOCKET_RADIUS * self.view.zoom + 5.0;
            for (&sid, &spos) in socket_screen_pos {
                if sid == from || pp.distance(spos) >= hit_r {
                    continue;
                }
                let (output, input) = if from.is_output {
                    (from, sid)
                } else {
                    (sid, from)
                };
                if output.is_output && !input.is_output {
                    let out_type = self
                        .graph
                        .nodes
                        .get(&output.node)
                        .and_then(|n| n.outputs.get(output.index))
                        .map(|s| s.type_name.as_str());
                    let in_type = self
                        .graph
                        .nodes
                        .get(&input.node)
                        .and_then(|n| n.inputs.get(input.index))
                        .map(|s| s.type_name.as_str());
                    if let (Some(ot), Some(it)) = (out_type, in_type) && sockets_compatible(ot, it) {
                        self.graph.add_connection(output, input);
                    }
                }
                break;
            }
        }
        InteractionState::Idle
    }

    fn update_box_select(
        &mut self,
        ui: &Ui,
        response: &egui::Response,
        pointer_canvas: Option<Pos2>,
        layouts: &HashMap<NodeId, NodeLayout>,
        start_canvas: Pos2,
        mut current_canvas: Pos2,
    ) -> InteractionState {
        if response.dragged() {
            if let Some(pc) = pointer_canvas {
                current_canvas = pc;
            }
            return InteractionState::BoxSelecting {
                start_canvas,
                current_canvas,
            };
        }
        let select_rect = Rect::from_two_pos(start_canvas, current_canvas);
        let ctrl = ui.input(|i| i.modifiers.ctrl);
        if !ctrl {
            for n in self.graph.nodes.values_mut() {
                n.selected = false;
            }
        }
        for (&id, layout) in layouts {
            if select_rect.intersects(layout.node_rect)
                && let Some(n) = self.graph.nodes.get_mut(&id)
            {
                n.selected = true;
            }
        }
        InteractionState::Idle
    }

    fn update_cut_wire(
        &mut self,
        response: &egui::Response,
        pointer_canvas: Option<Pos2>,
        layouts: &HashMap<NodeId, NodeLayout>,
        start_canvas: Pos2,
        mut current_canvas: Pos2,
    ) -> InteractionState {
        if response.dragged() {
            if let Some(pc) = pointer_canvas {
                current_canvas = pc;
            }
            return InteractionState::CuttingWire {
                start_canvas,
                current_canvas,
            };
        }
        let to_remove: Vec<usize> = self
            .graph
            .connections
            .iter()
            .enumerate()
            .filter_map(|(idx, conn)| {
                let fp = layouts
                    .get(&conn.from.node)
                    .and_then(|l| l.output_socket_pos.get(conn.from.index).and_then(|p| *p))?;
                let tp = layouts
                    .get(&conn.to.node)
                    .and_then(|l| l.input_socket_pos.get(conn.to.index).and_then(|p| *p))?;
                wire_intersects_knife(fp, tp, start_canvas, current_canvas).then_some(idx)
            })
            .collect();
        for idx in to_remove.into_iter().rev() {
            self.graph.connections.remove(idx);
        }
        InteractionState::Idle
    }
}
