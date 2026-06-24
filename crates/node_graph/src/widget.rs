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
                hidden: false,
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
                hidden: false,
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
    status: Option<(String, f64)>,
    /// Position where secondary button was pressed; used for custom tablet movement threshold.
    sec_press_screen_pos: Option<Pos2>,
    show_add_menu: bool,
    add_menu_screen_pos: Pos2,
    /// Keyboard navigation state for the standalone add popup.
    add_nav_cat: Option<usize>,    // highlighted category; cats.len() = Reroute
    add_nav_in_sub: bool,
    add_nav_sub_item: usize,
    add_cat_ids: Vec<egui::Id>,    // captured category SubMenuButton IDs (refreshed each frame)
    /// Node that should always render on top (last moved/dragged).
    top_node: Option<NodeId>,
    /// Response ID of the "+ Add" SubMenuButton in the right-click context menu (stable across
    /// frames); used to programmatically open that submenu when 'A' is pressed while the menu
    /// is already visible.
    add_btn_id: Option<egui::Id>,
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
            status: None,
            sec_press_screen_pos: None,
            show_add_menu: false,
            add_menu_screen_pos: Pos2::ZERO,
            add_nav_cat: None,
            add_nav_in_sub: false,
            add_nav_sub_item: 0,
            add_cat_ids: Vec::new(),
            top_node: None,
            add_btn_id: None,
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

        if let InteractionState::DraggingNode { node_id, .. } = self.interaction {
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

        if let InteractionState::CuttingWire { path } = &self.interaction {
            let screen_pts: Vec<egui::Pos2> = path.iter()
                .map(|&p| self.view.canvas_to_screen(origin, p))
                .collect();
            if screen_pts.len() >= 2 {
                draw_knife_line(&painter, &screen_pts);
            }
        }

        self.handle_input(ui, &response, origin, &layouts, &socket_screen_pos, rect);

        if self.minimap_visible {
            let (info, _) = minimap::compute_minimap(&layouts, rect);
            minimap::draw_minimap(&painter, &info, &self.graph, &layouts, &self.view, rect);
        }

        self.draw_status(&painter, rect, ui.ctx());
    }

    // ── Registry helpers ──────────────────────────────────────────────────────

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

            let sock_name = sock.name.clone();
            let changed = ui.push_id((id.0, i), |ui| {
                value.draw_widget(ui, &sock_name, ws, self.view.zoom, node_screen_rect)
            }).inner;
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

            let changed = ui.push_id((id.0, pi), |ui| {
                prop.value
                    .draw_widget(ui, &label, ws, self.view.zoom, node_screen_rect)
            }).inner;
            if changed {
                any_changed = true;
            }
        }

        if any_changed && let Some(node) = self.graph.nodes.get_mut(&id) {
            node.run_update();
        }
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
            // Record the screen position so we can apply a custom movement threshold on release.
            self.sec_press_screen_pos = pointer;
        }

        if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
            self.interaction = InteractionState::Idle;
            self.show_add_menu = false;
        }
        if ui.input(|i| i.key_pressed(egui::Key::Delete) || i.key_pressed(egui::Key::Backspace)) {
            self.delete_selected();
        }
        if ui.input(|i| i.key_pressed(egui::Key::D) && i.modifiers.shift) {
            self.duplicate_selected();
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

        let no_focus = ui.ctx().memory(|m| m.focused().is_none());

        // Ctrl+H: toggle hidden sockets on selected nodes
        if no_focus && ui.input(|i| i.key_pressed(egui::Key::H) && i.modifiers.ctrl) {
            let selected: Vec<NodeId> = self.graph.nodes.values()
                .filter(|n| n.selected)
                .map(|n| n.id)
                .collect();
            for id in selected {
                self.toggle_hidden_sockets(id);
            }
        }

        // X: delete selected nodes
        if no_focus && ui.input(|i| i.key_pressed(egui::Key::X) && !i.modifiers.any()) {
            self.delete_selected();
        }

        if response.clicked_by(egui::PointerButton::Primary) && !ui.ctx().egui_is_using_pointer() {
            self.handle_selection_click(ui, pointer, layouts, origin);
        }

        let context_screen = self.view.canvas_to_screen(origin, self.context_pos);
        let context_node = self.node_at_screen(context_screen, layouts, origin);
        let context_node_hidden = context_node.is_some_and(|id| self.node_has_hidden_sockets(id));
        let any_selected = self.graph.nodes.values().any(|n| n.selected);
        // Show node-action items when the cursor was over a node or nodes are selected.
        let node_ctx = context_node.is_some() || any_selected;

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

        // 'a' key: open Add submenu inside the context menu, or open standalone popup.
        let a_pressed = ui.input(|i| i.key_pressed(egui::Key::A) && i.modifiers.shift && !i.modifiers.ctrl && !i.modifiers.alt);
        if a_pressed && !self.show_add_menu {
            if response.context_menu_opened() {
                // Don't require no_focus here — egui may mark the popup as focused internally.
                if !node_ctx {
                    if let Some(btn_id) = self.add_btn_id {
                        let popup_id = egui::Popup::default_response_id(&response);
                        let sub_id = egui::containers::menu::SubMenu::id_from_widget_id(btn_id);
                        // MenuState::from_id resets open_item if the submenu wasn't shown last
                        // frame.  Mark it shown first so the staleness check passes.
                        egui::containers::menu::MenuState::mark_shown(ui.ctx(), sub_id);
                        egui::containers::menu::MenuState::from_id(ui.ctx(), popup_id, |state| {
                            state.open_item = Some(sub_id);
                        });
                    }
                }
            } else if no_focus {
                // Only open standalone popup when no text widget has focus.
                let pos = pointer.unwrap_or_else(|| canvas_rect.center());
                self.add_menu_screen_pos = pos;
                self.context_pos = self.view.screen_to_canvas(origin, pos);
                self.show_add_menu = true;
                self.add_nav_cat = None;
                self.add_nav_in_sub = false;
                self.add_nav_sub_item = 0;
            }
        }

        let mut add_kind: Option<String> = None;
        let mut close_add_menu = false;

        // Keyboard navigation for the standalone add-node popup.
        if self.show_add_menu {
            let n_cats = cats_and_items.len();
            let total = n_cats + 1; // categories + Reroute

            if ui.input(|i| i.key_pressed(egui::Key::ArrowDown)) {
                if self.add_nav_in_sub {
                    let n = self.add_nav_cat
                        .and_then(|ci| cats_and_items.get(ci))
                        .map_or(0, |(_, v)| v.len());
                    self.add_nav_sub_item = (self.add_nav_sub_item + 1).min(n.saturating_sub(1));
                } else {
                    self.add_nav_cat = Some(match self.add_nav_cat {
                        None => 0,
                        Some(i) => (i + 1).min(total - 1),
                    });
                }
            }
            if ui.input(|i| i.key_pressed(egui::Key::ArrowUp)) {
                if self.add_nav_in_sub {
                    self.add_nav_sub_item = self.add_nav_sub_item.saturating_sub(1);
                } else {
                    self.add_nav_cat = Some(match self.add_nav_cat {
                        None => total - 1,
                        Some(i) => i.saturating_sub(1),
                    });
                }
            }
            if ui.input(|i| i.key_pressed(egui::Key::ArrowRight)) && !self.add_nav_in_sub {
                if let Some(i) = self.add_nav_cat {
                    if i < n_cats {
                        self.add_nav_in_sub = true;
                        self.add_nav_sub_item = 0;
                    }
                }
            }
            if ui.input(|i| i.key_pressed(egui::Key::ArrowLeft)) {
                if self.add_nav_in_sub {
                    self.add_nav_in_sub = false;
                } else {
                    close_add_menu = true;
                }
            }
            if ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                if self.add_nav_in_sub {
                    if let Some(ci) = self.add_nav_cat {
                        if let Some((_, names)) = cats_and_items.get(ci) {
                            if let Some(name) = names.get(self.add_nav_sub_item) {
                                add_kind = Some(name.clone());
                                close_add_menu = true;
                            }
                        }
                    }
                } else if let Some(i) = self.add_nav_cat {
                    if i == n_cats {
                        add_kind = Some("Reroute".to_string());
                        close_add_menu = true;
                    } else {
                        self.add_nav_in_sub = true;
                        self.add_nav_sub_item = 0;
                    }
                }
            }

            // Programmatically open the keyboard-selected category submenu.
            if let Some(cat_i) = self.add_nav_cat {
                let menu_id = egui::Id::new("dsl_add_node_popup");
                if cat_i < n_cats {
                    if let Some(&btn_id) = self.add_cat_ids.get(cat_i) {
                        let sub_id = egui::containers::menu::SubMenu::id_from_widget_id(btn_id);
                        egui::containers::menu::MenuState::mark_shown(ui.ctx(), sub_id);
                        egui::containers::menu::MenuState::mark_shown(ui.ctx(), menu_id);
                        egui::containers::menu::MenuState::from_id(ui.ctx(), menu_id, |state| {
                            state.open_item = Some(sub_id);
                        });
                    }
                } else {
                    // Reroute highlighted — close any open submenu.
                    egui::containers::menu::MenuState::from_id(ui.ctx(), menu_id, |state| {
                        state.open_item = None;
                    });
                }
            }
        }

        // Needed for tablet trigger below and for direct button handling.
        let ctrl_held = ui.input(|i| i.modifiers.ctrl);

        // Close the 'A' add-menu whenever the user presses the secondary button (context menu about
        // to open) so both can't be visible at the same time.
        if ui.input(|i| i.pointer.button_pressed(egui::PointerButton::Secondary)) {
            self.show_add_menu = false;
        }

        // Tablet: egui's secondary_clicked() has a ~6 px movement threshold.
        // On secondary release, if movement is within our larger threshold AND egui didn't
        // already open the menu, open the Popup directly so response.context_menu() renders it.
        let sec_released = ui.input(|i| i.pointer.button_released(egui::PointerButton::Secondary));
        if sec_released
            && !ctrl_held
            && !matches!(self.interaction, InteractionState::CuttingWire { .. })
            && !response.secondary_clicked()
        {
            if let Some(press) = self.sec_press_screen_pos
                && let Some(curr) = pointer
                && press.distance(curr) < 30.0
            {
                let popup_id = egui::Popup::default_response_id(&response);
                #[allow(deprecated)]
                ui.ctx().memory_mut(|mem| mem.open_popup_at(popup_id, curr));
            }
        }
        if sec_released {
            self.sec_press_screen_pos = None;
        }

        let mut do_delete = false;
        let mut do_toggle_hide = false;

        // Right-click context menu — uses egui's built-in mechanism for proper hover-to-open submenus.
        response.context_menu(|ui| {
            ui.set_min_width(200.0);
            if node_ctx {
                if ui.add(egui::Button::new("Delete").right_text("X")).clicked() {
                    do_delete = true;
                    ui.close();
                }
                let arrow = egui::containers::menu::SubMenuButton::RIGHT_ARROW;
                let _ = egui::containers::menu::SubMenuButton::from_button(
                    egui::Button::new("Show/Hide").right_text(arrow),
                )
                .ui(ui, |ui| {
                    let chk = if context_node_hidden { "✓  " } else { "    " };
                    if ui
                        .add(
                            egui::Button::new(format!("{chk}Unconnected Sockets"))
                                .right_text("Ctrl+H"),
                        )
                        .clicked()
                    {
                        do_toggle_hide = true;
                        ui.close();
                    }
                });
            } else {
                let (add_resp, _) = egui::containers::menu::SubMenuButton::from_button(
                    egui::Button::new("+ Add").right_text("A  ⏵"),
                )
                .ui(ui, |ui| {
                    for (cat, names) in &cats_and_items {
                        let arrow = egui::containers::menu::SubMenuButton::RIGHT_ARROW;
                        let _ = egui::containers::menu::SubMenuButton::from_button(
                            egui::Button::new(cat.as_str()).right_text(arrow),
                        )
                        .ui(ui, |ui| {
                            for name in names {
                                if ui.button(name.as_str()).clicked() {
                                    add_kind = Some(name.clone());
                                    ui.close();
                                }
                            }
                        });
                    }
                    ui.separator();
                    if ui.button("Reroute").clicked() {
                        add_kind = Some("Reroute".to_string());
                        ui.close();
                    }
                });
                self.add_btn_id = Some(add_resp.id);
                // Paste (future)
            }
        });
        // 'A' key: submenu-style add popup (same hierarchy as the right-click Add submenu)
        if self.show_add_menu {
            // Snapshot nav state so closures stay borrow-clean.
            let nav_cat = self.add_nav_cat;
            let nav_in_sub = self.add_nav_in_sub;
            let nav_sub = self.add_nav_sub_item;
            let n_cats = cats_and_items.len();
            let sel_bg = ui.visuals().selection.bg_fill;
            let mut new_cat_ids: Vec<egui::Id> = Vec::new();

            let area_resp = egui::Area::new(egui::Id::new("dsl_add_node_popup"))
                .fixed_pos(self.add_menu_screen_pos)
                .order(egui::Order::Foreground)
                .layout(egui::Layout::top_down_justified(egui::Align::Min))
                .info(
                    egui::UiStackInfo::new(egui::UiKind::Menu).with_tag_value(
                        egui::containers::menu::MenuConfig::MENU_CONFIG_TAG,
                        egui::containers::menu::MenuConfig::new(),
                    ),
                )
                .show(ui.ctx(), |ui| {
                    egui::containers::menu::menu_style(ui.style_mut());
                    egui::Frame::menu(ui.style()).show(ui, |ui| {
                        ui.set_min_width(150.0);
                        for (cat_i, (cat, names)) in cats_and_items.iter().enumerate() {
                            let arrow = egui::containers::menu::SubMenuButton::RIGHT_ARROW;
                            let (cat_resp, _) = egui::containers::menu::SubMenuButton::from_button(
                                egui::Button::new(cat.as_str()).right_text(arrow),
                            )
                            .ui(ui, |ui| {
                                for (item_i, name) in names.iter().enumerate() {
                                    let selected = nav_in_sub
                                        && nav_cat == Some(cat_i)
                                        && item_i == nav_sub;
                                    let resp = ui.add(
                                        egui::Button::new(name.as_str())
                                            .fill(if selected { sel_bg } else { egui::Color32::TRANSPARENT }),
                                    );
                                    if resp.clicked() {
                                        add_kind = Some(name.clone());
                                        close_add_menu = true;
                                    }
                                }
                            });
                            new_cat_ids.push(cat_resp.id);
                        }
                        ui.separator();
                        let reroute_selected = nav_cat == Some(n_cats);
                        let resp = ui.add(
                            egui::Button::new("Reroute")
                                .fill(if reroute_selected { sel_bg } else { egui::Color32::TRANSPARENT }),
                        );
                        if resp.clicked() {
                            add_kind = Some("Reroute".to_string());
                            close_add_menu = true;
                        }
                    });
                });

            self.add_cat_ids = new_cat_ids;

            if !close_add_menu {
                if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                    close_add_menu = true;
                } else if ui.input(|i| i.pointer.button_released(egui::PointerButton::Primary))
                    && !area_resp.response.hovered()
                {
                    close_add_menu = true;
                }
            }
        }
        if close_add_menu {
            self.show_add_menu = false;
        }

        if do_delete {
            if let Some(id) = context_node {
                self.graph.remove_node(id);
                self.graph.cleanup_frames();
            }
            self.delete_selected();
        }
        if do_toggle_hide {
            if let Some(id) = context_node {
                self.toggle_hidden_sockets(id);
            } else {
                let selected: Vec<NodeId> = self.graph.nodes.values()
                    .filter(|n| n.selected).map(|n| n.id).collect();
                for id in selected {
                    self.toggle_hidden_sockets(id);
                }
            }
        }
        if let Some(kind) = add_kind {
            let pos = self.context_pos;
            self.add_from_registry(&kind, pos);
        }

        // ── Direct non-primary button handling ────────────────────────────────────
        // drag_started() is Primary-only in egui, so middle/secondary must be
        // detected via button_down() every frame instead of through the state machine.
        // ctrl_held is already computed above.

        let middle_down = ui.input(|i| i.pointer.button_down(egui::PointerButton::Middle));
        let right_down  = ui.input(|i| i.pointer.button_down(egui::PointerButton::Secondary));

        if middle_down {
            if let Some(pp) = pointer {
                let delta = if let InteractionState::Panning { last_screen } = self.interaction {
                    pp - last_screen
                } else {
                    Vec2::ZERO
                };
                if ctrl_held {
                    // Ctrl+middle: zoom around cursor using vertical mouse movement.
                    // Moving up (negative delta.y) zooms in; moving down zooms out.
                    let factor = (1.0_f32 - delta.y * 0.005).clamp(0.5, 2.0);
                    if delta.y.abs() > 0.1 {
                        self.view.zoom_around(pp, origin, factor);
                    }
                } else {
                    self.view.pan += delta;
                }
                self.interaction = InteractionState::Panning { last_screen: pp };
            }
            return;
        }
        if matches!(self.interaction, InteractionState::Panning { .. }) {
            self.interaction = InteractionState::Idle;
        }

        if right_down && ctrl_held {
            // Ctrl+right: grow the knife-cut path every frame
            if let Some(pc) = pointer_canvas {
                match &mut self.interaction {
                    InteractionState::CuttingWire { path } => {
                        let min_step = 4.0 / self.view.zoom;
                        if path.last().is_none_or(|&last| last.distance(pc) > min_step) {
                            path.push(pc);
                        }
                    }
                    _ => self.interaction = InteractionState::CuttingWire { path: vec![pc] },
                }
            }
            return;
        }
        if matches!(self.interaction, InteractionState::CuttingWire { .. }) {
            // Ctrl or right released: apply the cut
            let state = std::mem::replace(&mut self.interaction, InteractionState::Idle);
            if let InteractionState::CuttingWire { path } = state {
                self.apply_knife_cut(&path, layouts);
            }
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
            InteractionState::CuttingWire { path } =>
                self.update_cut_wire(response, pointer_canvas, layouts, path),
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

    fn node_has_hidden_sockets(&self, node_id: NodeId) -> bool {
        self.graph.nodes.get(&node_id).is_some_and(|n| {
            n.inputs.iter().any(|s| s.hidden) || n.outputs.iter().any(|s| s.hidden)
        })
    }

    fn toggle_hidden_sockets(&mut self, node_id: NodeId) {
        if self.node_has_hidden_sockets(node_id) {
            if let Some(node) = self.graph.nodes.get_mut(&node_id) {
                for inp in node.inputs.iter_mut() { inp.hidden = false; }
                for out in node.outputs.iter_mut() { out.hidden = false; }
            }
        } else {
            use std::collections::HashSet;
            let connected_in: HashSet<usize> = self.graph.connections.iter()
                .filter(|c| c.to.node == node_id)
                .map(|c| c.to.index)
                .collect();
            let connected_out: HashSet<usize> = self.graph.connections.iter()
                .filter(|c| c.from.node == node_id)
                .map(|c| c.from.index)
                .collect();
            if let Some(node) = self.graph.nodes.get_mut(&node_id) {
                for (i, inp) in node.inputs.iter_mut().enumerate() {
                    if inp.value.is_none() && !connected_in.contains(&i) {
                        inp.hidden = true;
                    }
                }
                for (i, out) in node.outputs.iter_mut().enumerate() {
                    if !connected_out.contains(&i) {
                        out.hidden = true;
                    }
                }
            }
        }
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

        // Middle and Ctrl+right are handled in handle_input directly.
        // Guard: if the secondary button somehow triggers drag_started(), don't box-select.
        let right_down = ui.input(|i| i.pointer.button_down(egui::PointerButton::Secondary));
        if right_down {
            return InteractionState::Idle;
        }
        let ctrl = ui.input(|i| i.modifiers.ctrl);

        // Primary button from here on — check socket then node header, then box-select
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

        // Primary drag on empty canvas → box select; modifiers read at release
        InteractionState::BoxSelecting {
            start_canvas: pc,
            current_canvas: pc,
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
        let shift = ui.input(|i| i.modifiers.shift);
        let ctrl  = ui.input(|i| i.modifiers.ctrl);
        if !shift && !ctrl {
            for n in self.graph.nodes.values_mut() {
                n.selected = false;
            }
        }
        for (&id, layout) in layouts {
            if select_rect.intersects(layout.node_rect)
                && let Some(n) = self.graph.nodes.get_mut(&id)
            {
                n.selected = !ctrl; // ctrl = remove mode, shift/none = add/replace mode
            }
        }
        InteractionState::Idle
    }

    fn apply_knife_cut(&mut self, path: &[Pos2], layouts: &HashMap<NodeId, NodeLayout>) {
        if path.len() < 2 {
            return;
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
                path.windows(2)
                    .any(|w| wire_intersects_knife(fp, tp, w[0], w[1]))
                    .then_some(idx)
            })
            .collect();
        for idx in to_remove.into_iter().rev() {
            self.graph.connections.remove(idx);
        }
    }

    fn update_cut_wire(
        &mut self,
        response: &egui::Response,
        pointer_canvas: Option<Pos2>,
        layouts: &HashMap<NodeId, NodeLayout>,
        mut path: Vec<Pos2>,
    ) -> InteractionState {
        // This is a fallback path for systems where drag_started() fires for Secondary.
        // The normal path is handled directly in handle_input via button_down.
        if response.dragged() {
            if let Some(pc) = pointer_canvas {
                let min_step = 4.0 / self.view.zoom;
                if path.last().is_none_or(|&last| last.distance(pc) > min_step) {
                    path.push(pc);
                }
            }
            return InteractionState::CuttingWire { path };
        }
        self.apply_knife_cut(&path, layouts);
        InteractionState::Idle
    }
}
