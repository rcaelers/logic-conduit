pub(super) use super::super::menu::Shortcut;
use super::{FrameRenameState, NodeGraphWidget, NodeRenameState};
use crate::model::{Connection, FrameId, Node, NodeId, SocketDirection, SocketId};
use egui::{Color32, Pos2, Vec2};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};

static FRAME_COLORS: [Color32; 5] = [
    Color32::from_rgb(50, 90, 160),
    Color32::from_rgb(50, 130, 80),
    Color32::from_rgb(160, 100, 50),
    Color32::from_rgb(110, 60, 160),
    Color32::from_rgb(160, 60, 60),
];

const CLIPBOARD_KIND: &str = "node_graph_clipboard_v1";
const PASTE_OFFSET: Vec2 = Vec2::new(30.0, 30.0);

#[derive(Serialize, Deserialize)]
struct ClipboardPayload {
    kind: String,
    nodes: Vec<Node>,
    connections: Vec<Connection>,
}

#[derive(PartialEq, Eq)]
pub(super) enum ActionEffect {
    None,
    ResetInteraction,
    /// Enter `InteractionState::PlacingNodes` anchored at the pointer, if a
    /// pointer position was available when the action ran — the newly
    /// added/duplicated/pasted nodes then follow the cursor until confirmed
    /// (Phase 1.2).
    EnterPlacement,
}

#[derive(Clone)]
pub(super) enum GraphAction {
    Undo,
    Redo,
    OpenAddSearch,
    AddNode {
        name: String,
        pos: Pos2,
    },
    /// Adds a node and immediately wires `from` to its first compatible
    /// visible socket — the link-drag-search gesture (Phase 1.1): drag a
    /// wire onto empty canvas, pick a node, get it added and connected in
    /// one step.
    AddNodeAndConnect {
        name: String,
        pos: Pos2,
        from: SocketId,
    },
    Cut {
        target: Option<NodeId>,
    },
    Copy {
        target: Option<NodeId>,
    },
    Paste {
        text: Option<String>,
        pos: Pos2,
    },
    Delete {
        target: Option<NodeId>,
    },
    Dissolve {
        target: Option<NodeId>,
    },
    DuplicateSelected,
    AddFrame {
        target: Option<NodeId>,
    },
    RenameFrame {
        target: FrameId,
        screen_pos: Pos2,
    },
    SetFrameColor {
        target: Option<FrameId>,
        color: Color32,
    },
    RemoveFromFrame {
        target: Option<NodeId>,
    },
    ToggleHidden {
        target: Option<NodeId>,
    },
    ToggleCollapsed {
        target: Option<NodeId>,
    },
    ToggleMinimap,
    TogglePanel,
    /// Selects every node and frame (Phase 2, Blender's `A`).
    SelectAll,
    /// Deselects everything (Phase 2, Blender's Alt+A).
    DeselectAll,
    /// Connects the active node to the previously-active one via the first
    /// compatible socket pair found, trying both directions (Phase 2,
    /// Blender's `F` "Make Link"). A no-op if there's no previous active
    /// node, they're the same node, or nothing compatible is found.
    LinkActiveToPrevious,
}

pub(super) struct HotkeyRegistry {
    bindings: Vec<(Shortcut, GraphAction)>,
}

impl HotkeyRegistry {
    pub fn new() -> Self {
        Self {
            bindings: Vec::new(),
        }
    }

    pub fn graph_defaults() -> Self {
        let mut r = Self::new();
        r.bind(
            Shortcut::key(egui::Key::Delete),
            GraphAction::Delete { target: None },
        );
        r.bind(
            Shortcut::key(egui::Key::Backspace),
            GraphAction::Delete { target: None },
        );
        r.bind(Shortcut::key(egui::Key::M), GraphAction::ToggleMinimap);
        r.bind(Shortcut::key(egui::Key::N), GraphAction::TogglePanel);
        r.bind(
            Shortcut::key(egui::Key::H),
            GraphAction::ToggleCollapsed { target: None },
        );
        // Plain `A` (select-all) and Alt+A (deselect-all) live here; Shift+A
        // (open Add search at the pointer) is special-cased in
        // `handle_input` instead, since it needs the screen pointer/canvas
        // origin this registry's dispatch doesn't carry.
        r.bind(Shortcut::key(egui::Key::A), GraphAction::SelectAll);
        r.bind(Shortcut::alt(egui::Key::A), GraphAction::DeselectAll);
        r.bind(Shortcut::key(egui::Key::F), GraphAction::LinkActiveToPrevious);
        r
    }

    pub fn bind(&mut self, shortcut: Shortcut, action: GraphAction) {
        self.bindings.push((shortcut, action));
    }

    /// Dispatch all matching bindings. Suppressed entirely when any widget holds
    /// keyboard focus, e.g. an open menu or inline text edit.
    pub fn dispatch(&self, ui: &egui::Ui) -> Vec<GraphAction> {
        if ui.ctx().memory(|m| m.focused().is_some()) {
            return Vec::new();
        }
        self.bindings
            .iter()
            .filter_map(|(shortcut, action)| {
                ui.input_mut(|i| {
                    i.consume_shortcut(&egui::KeyboardShortcut::new(
                        shortcut.modifiers,
                        shortcut.key,
                    ))
                    .then(|| action.clone())
                })
            })
            .collect()
    }
}

impl NodeGraphWidget {
    /// `pointer_canvas`: the current pointer position in canvas space, when
    /// known. `AddNode`, `Paste` and `DuplicateSelected` use it both to spawn
    /// their nodes under the cursor and, if present, to enter placement mode
    /// (Phase 1.2) so the nodes follow the pointer until confirmed; without a
    /// pointer (e.g. a touch-driven menu) they fall back to their previous
    /// fixed-position behavior.
    pub(super) fn execute_action(
        &mut self,
        action: GraphAction,
        egui_ctx: &egui::Context,
        pointer_canvas: Option<Pos2>,
    ) -> ActionEffect {
        match action {
            GraphAction::Undo => {
                self.undo();
                ActionEffect::ResetInteraction
            }
            GraphAction::Redo => {
                self.redo();
                ActionEffect::ResetInteraction
            }
            GraphAction::OpenAddSearch => ActionEffect::None,
            GraphAction::AddNode { name, pos } => {
                self.push_undo_snapshot();
                let added = self.add_node_at(&name, pointer_canvas.unwrap_or(pos));
                if let Some(new_id) = added {
                    // `add_node_at` doesn't touch selection — without this,
                    // placement mode would move whatever was selected
                    // *before* the add (or nothing) instead of the node
                    // that's actually following the pointer.
                    self.select_only(new_id);
                }
                if added.is_some() && pointer_canvas.is_some() {
                    ActionEffect::EnterPlacement
                } else {
                    ActionEffect::None
                }
            }
            GraphAction::AddNodeAndConnect { name, pos, from } => {
                self.push_undo_snapshot();
                if let Some(new_id) = self.add_node_at(&name, pos) {
                    self.select_only(new_id);
                    if let Some(target) = self.first_compatible_socket(from, new_id) {
                        let (output, input) = if from.direction == SocketDirection::Output {
                            (from, target)
                        } else {
                            (target, from)
                        };
                        self.graph.add_connection(output, input);
                        self.run_update(output.node);
                        self.run_update(input.node);
                    }
                }
                ActionEffect::None
            }
            GraphAction::Cut { target } => {
                if self.copy_nodes(target, egui_ctx) {
                    self.push_undo_snapshot();
                    self.delete_nodes(target);
                }
                ActionEffect::None
            }
            GraphAction::Copy { target } => {
                self.copy_nodes(target, egui_ctx);
                ActionEffect::None
            }
            GraphAction::Paste { text, pos } => {
                if self.can_paste_nodes() || text.is_some() {
                    self.push_undo_snapshot();
                }
                let placed_pos = pointer_canvas.unwrap_or(pos);
                let pasted = self.paste_nodes(text.as_deref(), placed_pos, egui_ctx);
                if pasted && pointer_canvas.is_some() {
                    ActionEffect::EnterPlacement
                } else {
                    ActionEffect::None
                }
            }
            GraphAction::Delete { target } => {
                self.push_undo_snapshot();
                self.delete_nodes(target);
                ActionEffect::None
            }
            GraphAction::Dissolve { target } => {
                self.push_undo_snapshot();
                self.dissolve_nodes(target);
                ActionEffect::None
            }
            GraphAction::DuplicateSelected => {
                self.push_undo_snapshot();
                let duplicated = self.duplicate_selected(pointer_canvas);
                if duplicated && pointer_canvas.is_some() {
                    ActionEffect::EnterPlacement
                } else {
                    ActionEffect::None
                }
            }
            GraphAction::AddFrame { target } => {
                self.push_undo_snapshot();
                self.add_frame(target);
                ActionEffect::None
            }
            GraphAction::RenameFrame { target, screen_pos } => {
                self.start_renaming_frame(target, screen_pos);
                ActionEffect::None
            }
            GraphAction::SetFrameColor { target, color } => {
                self.push_undo_snapshot();
                self.set_frame_color(target, color);
                ActionEffect::None
            }
            GraphAction::RemoveFromFrame { target } => {
                self.push_undo_snapshot();
                self.remove_from_frame(target);
                ActionEffect::None
            }
            GraphAction::ToggleHidden { target } => {
                self.push_undo_snapshot();
                self.toggle_hidden_sockets(target);
                ActionEffect::None
            }
            GraphAction::ToggleCollapsed { target } => {
                self.push_undo_snapshot();
                self.toggle_collapsed(target);
                ActionEffect::None
            }
            GraphAction::ToggleMinimap => {
                self.minimap_visible = !self.minimap_visible;
                ActionEffect::None
            }
            GraphAction::TogglePanel => {
                self.toggle_panel();
                ActionEffect::None
            }
            GraphAction::SelectAll => {
                for node in self.graph.nodes.values_mut() {
                    node.selected = true;
                }
                for frame in &mut self.graph.frames {
                    frame.selected = true;
                }
                // Give the panel something sane to show, matching a normal
                // click-select — the newest node, if any exist.
                if let Some(id) = self.graph.nodes.keys().max_by_key(|id| id.0).copied() {
                    self.set_active_node(id);
                }
                ActionEffect::None
            }
            GraphAction::DeselectAll => {
                for node in self.graph.nodes.values_mut() {
                    node.selected = false;
                }
                for frame in &mut self.graph.frames {
                    frame.selected = false;
                }
                ActionEffect::None
            }
            GraphAction::LinkActiveToPrevious => {
                if let (Some(previous), Some(active)) =
                    (self.previous_active_node, self.active_node)
                    && previous != active
                    && let Some((from, to)) = self.first_compatible_link(previous, active)
                {
                    self.push_undo_snapshot();
                    self.graph.add_connection(from, to);
                    self.run_update(from.node);
                    self.run_update(to.node);
                }
                ActionEffect::None
            }
        }
    }

    /// First (output, input) socket pair — visible sockets only — that
    /// directly connects `a` and `b`, tried in both directions (`a`→`b`
    /// first, then `b`→`a`). Used by `LinkActiveToPrevious` (Phase 2).
    fn first_compatible_link(&self, a: NodeId, b: NodeId) -> Option<(SocketId, SocketId)> {
        self.first_compatible_link_directed(a, b)
            .or_else(|| self.first_compatible_link_directed(b, a))
    }

    fn first_compatible_link_directed(
        &self,
        from_node: NodeId,
        to_node: NodeId,
    ) -> Option<(SocketId, SocketId)> {
        let from = self.graph.nodes.get(&from_node)?;
        let to = self.graph.nodes.get(&to_node)?;
        for (out_idx, output) in from.outputs.iter().enumerate() {
            if !output.visible {
                continue;
            }
            for (in_idx, input) in to.inputs.iter().enumerate() {
                if input.visible && input.accepts(output.effective_type()) {
                    return Some((
                        SocketId {
                            node: from_node,
                            index: out_idx,
                            direction: SocketDirection::Output,
                        },
                        SocketId {
                            node: to_node,
                            index: in_idx,
                            direction: SocketDirection::Input,
                        },
                    ));
                }
            }
        }
        None
    }

    /// `pub(super)`: also called from `interaction.rs` to cancel a placement
    /// gesture (Phase 1.2) by reverting the snapshot taken when it started.
    pub(super) fn undo(&mut self) {
        let Some(previous) = self.undo_stack.pop() else {
            return;
        };
        self.sync_all_node_state();
        self.redo_stack.push(self.graph.clone());
        self.graph = previous;
        self.restore_runtime();
    }

    fn redo(&mut self) {
        let Some(next) = self.redo_stack.pop() else {
            return;
        };
        self.sync_all_node_state();
        self.undo_stack.push(self.graph.clone());
        self.graph = next;
        self.restore_runtime();
    }

    fn delete_nodes(&mut self, target: Option<NodeId>) {
        let targets = self.target_nodes(target);
        if target.is_none() {
            self.graph.frames.retain(|frame| !frame.selected);
        }
        for id in targets {
            self.graph.remove_node(id);
            self.runtime.remove(&id);
        }
        if self
            .active_node
            .is_some_and(|id| !self.graph.nodes.contains_key(&id))
        {
            self.active_node = None;
        }
        self.graph.cleanup_frames();
    }

    fn dissolve_nodes(&mut self, target: Option<NodeId>) {
        let targets = self.target_nodes(target);
        if targets.is_empty() {
            return;
        }
        let target_set: HashSet<_> = targets.iter().copied().collect();
        let mut rewired = Vec::new();

        for &id in &targets {
            let incoming: Vec<_> = self
                .graph
                .connections
                .iter()
                .filter(|connection| {
                    connection.to.node == id && !target_set.contains(&connection.from.node)
                })
                .cloned()
                .collect();
            let outgoing: Vec<_> = self
                .graph
                .connections
                .iter()
                .filter(|connection| {
                    connection.from.node == id && !target_set.contains(&connection.to.node)
                })
                .cloned()
                .collect();

            for input_connection in &incoming {
                for output_connection in &outgoing {
                    if self
                        .direct_connection_compatible(input_connection.from, output_connection.to)
                    {
                        rewired.push(Connection {
                            from: input_connection.from,
                            to: output_connection.to,
                        });
                    }
                }
            }
        }

        for id in targets {
            self.graph.remove_node(id);
            self.runtime.remove(&id);
        }
        if self
            .active_node
            .is_some_and(|id| !self.graph.nodes.contains_key(&id))
        {
            self.active_node = None;
        }
        let mut touched: Vec<NodeId> = Vec::new();
        for connection in rewired {
            self.graph.add_connection(connection.from, connection.to);
            touched.push(connection.from.node);
            touched.push(connection.to.node);
        }
        touched.sort_unstable_by_key(|id| id.0);
        touched.dedup();
        for id in touched {
            self.run_update(id);
        }
        self.graph.cleanup_frames();
    }

    fn direct_connection_compatible(&self, from: SocketId, to: SocketId) -> bool {
        if from.direction != SocketDirection::Output || to.direction != SocketDirection::Input {
            return false;
        }
        let Some(from_type) = self
            .graph
            .nodes
            .get(&from.node)
            .and_then(|node| node.outputs.get(from.index))
            .map(|socket| socket.effective_type())
        else {
            return false;
        };
        self.graph
            .nodes
            .get(&to.node)
            .and_then(|node| node.inputs.get(to.index))
            .is_some_and(|socket| socket.accepts(from_type))
    }

    /// Clears every node/frame selection and selects only `node_id` — what
    /// clicking a node normally does, needed explicitly wherever a node is
    /// added programmatically (`add_node_at` itself leaves selection
    /// untouched).
    fn select_only(&mut self, node_id: NodeId) {
        for node in self.graph.nodes.values_mut() {
            node.selected = node.id == node_id;
        }
        for frame in &mut self.graph.frames {
            frame.selected = false;
        }
    }

    /// Returns whether anything was duplicated. `pointer_canvas`, when
    /// present, places the duplicates under the cursor (Phase 1.2 placement
    /// mode then takes over); otherwise they land at the usual fixed offset.
    fn duplicate_selected(&mut self, pointer_canvas: Option<Pos2>) -> bool {
        let selected: Vec<_> = self
            .graph
            .nodes
            .values()
            .filter(|node| node.selected)
            .map(|node| node.id)
            .collect();
        if selected.is_empty() {
            return false;
        }

        let payload = self.build_clipboard_payload(&selected);
        self.paste_payload(payload, pointer_canvas);
        true
    }

    fn sync_node_state(&mut self, id: NodeId) {
        if let (Some(instance), Some(node)) = (self.runtime.get(&id), self.graph.nodes.get_mut(&id))
        {
            node.state = instance.save_state();
        }
    }

    fn build_clipboard_payload(&mut self, node_ids: &[NodeId]) -> ClipboardPayload {
        for &id in node_ids {
            self.sync_node_state(id);
        }
        let selected: HashSet<_> = node_ids.iter().copied().collect();
        let nodes = node_ids
            .iter()
            .filter_map(|id| self.graph.nodes.get(id).cloned())
            .collect();
        let connections = self
            .graph
            .connections
            .iter()
            .filter(|connection| {
                selected.contains(&connection.from.node) && selected.contains(&connection.to.node)
            })
            .cloned()
            .collect();
        ClipboardPayload {
            kind: CLIPBOARD_KIND.to_owned(),
            nodes,
            connections,
        }
    }

    fn copy_nodes(&mut self, target: Option<NodeId>, egui_ctx: &egui::Context) -> bool {
        let targets = self.target_nodes(target);
        if targets.is_empty() {
            return false;
        }
        let payload = self.build_clipboard_payload(&targets);
        let Ok(text) = serde_json::to_string(&payload) else {
            return false;
        };
        egui_ctx.copy_text(text.clone());
        self.clipboard_cache = Some(text);
        self.io_status = Some((
            format!("Copied {} node(s)", payload.nodes.len()),
            egui_ctx.input(|i| i.time),
        ));
        true
    }

    fn paste_nodes(
        &mut self,
        pasted_text: Option<&str>,
        pos: Pos2,
        egui_ctx: &egui::Context,
    ) -> bool {
        let text = pasted_text
            .map(str::to_owned)
            .or_else(|| self.clipboard_cache.clone());
        let Some(text) = text else {
            return false;
        };
        let Ok(payload) = serde_json::from_str::<ClipboardPayload>(&text) else {
            return false;
        };
        if payload.kind != CLIPBOARD_KIND || payload.nodes.is_empty() {
            return false;
        }
        self.clipboard_cache = Some(text);
        let pasted = self.paste_payload(payload, Some(pos));
        if pasted > 0 {
            self.io_status = Some((
                format!("Pasted {pasted} node(s)"),
                egui_ctx.input(|i| i.time),
            ));
            return true;
        }
        false
    }

    pub(super) fn can_paste_nodes(&self) -> bool {
        self.clipboard_cache
            .as_deref()
            .and_then(|text| serde_json::from_str::<ClipboardPayload>(text).ok())
            .is_some_and(|payload| payload.kind == CLIPBOARD_KIND && !payload.nodes.is_empty())
    }

    fn paste_payload(&mut self, payload: ClipboardPayload, pos: Option<Pos2>) -> usize {
        let mut id_map = HashMap::new();
        let min_pos = payload
            .nodes
            .iter()
            .fold(Pos2::new(f32::INFINITY, f32::INFINITY), |min, node| {
                Pos2::new(min.x.min(node.pos.x), min.y.min(node.pos.y))
            });
        let offset = pos.map_or(PASTE_OFFSET, |pos| pos - min_pos);

        for node in self.graph.nodes.values_mut() {
            node.selected = false;
        }
        for frame in &mut self.graph.frames {
            frame.selected = false;
        }

        let mut pasted = 0usize;
        let mut last_pasted = None;
        for mut node in payload.nodes {
            let old_id = node.id;
            let new_id = self.graph.next_id();
            id_map.insert(old_id, new_id);
            node.id = new_id;
            node.pos += offset;
            node.selected = true;
            if let Some(instance) = self.registry.restore_node(&mut node) {
                self.runtime.insert(node.id, instance);
            }
            self.graph.add_node(node);
            last_pasted = Some(new_id);
            pasted += 1;
        }
        if let Some(id) = last_pasted {
            self.set_active_node(id);
        }

        let new_connections: Vec<_> = payload
            .connections
            .iter()
            .filter(|connection| {
                id_map.contains_key(&connection.from.node)
                    && id_map.contains_key(&connection.to.node)
            })
            .map(|connection| Connection {
                from: SocketId {
                    node: id_map[&connection.from.node],
                    ..connection.from
                },
                to: SocketId {
                    node: id_map[&connection.to.node],
                    ..connection.to
                },
            })
            .collect();
        self.graph.connections.extend(new_connections);
        let pasted_ids: Vec<NodeId> = id_map.values().copied().collect();
        self.graph.prune_unconnected_resolutions(&pasted_ids);
        pasted
    }

    fn add_frame(&mut self, target: Option<NodeId>) {
        let targets = self.target_nodes(target);
        if targets.is_empty() {
            return;
        }
        let color = FRAME_COLORS[self.graph.frames.len() % FRAME_COLORS.len()];
        self.graph.add_frame("Frame".to_string(), color, targets);
    }

    fn set_frame_color(&mut self, target: Option<FrameId>, color: Color32) {
        if let Some(frame_id) = target {
            if let Some(frame) = self
                .graph
                .frames
                .iter_mut()
                .find(|frame| frame.id == frame_id)
            {
                frame.color = color;
            }
            return;
        }
        for frame in &mut self.graph.frames {
            if frame.selected {
                frame.color = color;
            }
        }
    }

    fn start_renaming_frame(&mut self, target: FrameId, screen_pos: Pos2) {
        let Some(frame) = self.graph.frames.iter().find(|frame| frame.id == target) else {
            return;
        };
        self.frame_rename = Some(FrameRenameState {
            frame_id: target,
            text: frame.label.clone(),
            screen_pos,
        });
    }

    /// Opens the inline rename overlay for `target` (Phase 2, F2) — same
    /// mechanism as `start_renaming_frame`, writing to `node.title` instead.
    pub(super) fn start_renaming_node(&mut self, target: NodeId, screen_pos: Pos2) {
        let Some(node) = self.graph.nodes.get(&target) else {
            return;
        };
        self.node_rename = Some(NodeRenameState {
            node_id: target,
            text: node.title.clone(),
            screen_pos,
        });
    }

    fn target_nodes(&self, target: Option<NodeId>) -> Vec<NodeId> {
        if let Some(node_id) = target {
            if self
                .graph
                .nodes
                .get(&node_id)
                .is_some_and(|node| node.selected)
            {
                let selected: Vec<_> = self
                    .graph
                    .nodes
                    .values()
                    .filter(|node| node.selected)
                    .map(|node| node.id)
                    .collect();
                if !selected.is_empty() {
                    return selected;
                }
            }
            return vec![node_id];
        }
        self.graph
            .nodes
            .values()
            .filter(|node| node.selected)
            .map(|node| node.id)
            .collect()
    }

    fn remove_from_frame(&mut self, target: Option<NodeId>) {
        let targets: HashSet<_> = self.target_nodes(target).into_iter().collect();
        if targets.is_empty() {
            return;
        }
        for frame in &mut self.graph.frames {
            frame.node_ids.retain(|node_id| !targets.contains(node_id));
        }
        self.graph.cleanup_frames();
    }

    fn toggle_hidden_sockets(&mut self, target: Option<NodeId>) {
        if let Some(node_id) = target {
            self.toggle_hidden_sockets_for_node(node_id);
            return;
        }
        let selected: Vec<_> = self
            .graph
            .nodes
            .values()
            .filter(|node| node.selected)
            .map(|node| node.id)
            .collect();
        for node_id in selected {
            self.toggle_hidden_sockets_for_node(node_id);
        }
    }

    fn toggle_hidden_sockets_for_node(&mut self, node_id: NodeId) {
        let has_hidden = self.graph.nodes.get(&node_id).is_some_and(|node| {
            node.inputs.iter().any(|socket| socket.hidden)
                || node.outputs.iter().any(|socket| socket.hidden)
        });
        if has_hidden {
            if let Some(node) = self.graph.nodes.get_mut(&node_id) {
                for socket in &mut node.inputs {
                    socket.hidden = false;
                }
                for socket in &mut node.outputs {
                    socket.hidden = false;
                }
            }
            return;
        }

        let connected_inputs: HashSet<usize> = self
            .graph
            .connections
            .iter()
            .filter(|connection| connection.to.node == node_id)
            .map(|connection| connection.to.index)
            .collect();
        let connected_outputs: HashSet<usize> = self
            .graph
            .connections
            .iter()
            .filter(|connection| connection.from.node == node_id)
            .map(|connection| connection.from.index)
            .collect();

        if let Some(node) = self.graph.nodes.get_mut(&node_id) {
            for (i, socket) in node.inputs.iter_mut().enumerate() {
                if !socket.has_control && !connected_inputs.contains(&i) {
                    socket.hidden = true;
                }
            }
            for (i, socket) in node.outputs.iter_mut().enumerate() {
                if !connected_outputs.contains(&i) {
                    socket.hidden = true;
                }
            }
        }
    }

    fn toggle_collapsed(&mut self, target: Option<NodeId>) {
        if let Some(node_id) = target {
            self.toggle_collapsed_for_node(node_id);
            return;
        }
        let selected: Vec<_> = self
            .graph
            .nodes
            .values()
            .filter(|node| node.selected)
            .map(|node| node.id)
            .collect();
        for node_id in selected {
            self.toggle_collapsed_for_node(node_id);
        }
    }

    pub(super) fn toggle_collapsed_for_node(&mut self, node_id: NodeId) {
        if let Some(node) = self.graph.nodes.get_mut(&node_id)
            && node.kind != crate::model::NodeKind::Reroute
        {
            node.collapsed = !node.collapsed;
        }
    }
}

#[cfg(test)]
mod action_tests {
    use super::*;
    use crate::api::{FloatSocket, InputDef, NodeDef, OutputDef};
    use crate::model::SocketDirection;
    use crate::runtime::NodeTypeRegistry;

    #[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
    struct SourceState;
    struct SourceNode;
    impl NodeDef for SourceNode {
        type State = SourceState;
        fn name() -> &'static str {
            "Source"
        }
        fn category() -> &'static str {
            "Test"
        }
        fn inputs() -> Vec<InputDef<SourceState>> {
            vec![]
        }
        fn outputs() -> Vec<OutputDef<SourceState>> {
            vec![OutputDef::new::<FloatSocket>("Out")]
        }
        fn state() -> SourceState {
            SourceState
        }
    }

    #[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
    struct SinkState;
    struct SinkNode;
    impl NodeDef for SinkNode {
        type State = SinkState;
        fn name() -> &'static str {
            "Sink"
        }
        fn category() -> &'static str {
            "Test"
        }
        fn inputs() -> Vec<InputDef<SinkState>> {
            vec![InputDef::new::<FloatSocket>("In")]
        }
        fn outputs() -> Vec<OutputDef<SinkState>> {
            vec![]
        }
        fn state() -> SinkState {
            SinkState
        }
    }

    fn test_widget() -> NodeGraphWidget {
        let mut registry = NodeTypeRegistry::new();
        registry.register::<SourceNode>();
        registry.register::<SinkNode>();
        NodeGraphWidget::new(registry)
    }

    #[test]
    fn add_node_and_connect_wires_first_compatible_socket() {
        let mut widget = test_widget();
        let source_id = widget
            .add_node_at("Source", Pos2::new(0.0, 0.0))
            .expect("source node should be created");
        let from = SocketId {
            node: source_id,
            index: 0,
            direction: SocketDirection::Output,
        };

        let effect = widget.execute_action(
            GraphAction::AddNodeAndConnect {
                name: "Sink".to_owned(),
                pos: Pos2::new(200.0, 0.0),
                from,
            },
            &egui::Context::default(),
            None,
        );

        assert!(effect == ActionEffect::None);
        assert_eq!(widget.graph.nodes.len(), 2);
        assert_eq!(widget.graph.connections.len(), 1);
        let connection = &widget.graph.connections[0];
        assert_eq!(connection.from, from);
        assert_eq!(connection.to.direction, SocketDirection::Input);
        assert_ne!(connection.to.node, source_id);
        // A single undo step covers both the add and the connect.
        assert!(widget.can_undo());
        assert_eq!(widget.undo_stack.len(), 1);
    }

    #[test]
    fn add_node_and_connect_leaves_node_unconnected_when_incompatible() {
        let mut widget = test_widget();
        let sink_id = widget
            .add_node_at("Sink", Pos2::new(0.0, 0.0))
            .expect("sink node should be created");
        // Sink has no outputs, so this is an Input socket dragged from Sink;
        // wiring it to another Sink (also no outputs) should find nothing.
        let from = SocketId {
            node: sink_id,
            index: 0,
            direction: SocketDirection::Input,
        };

        widget.execute_action(
            GraphAction::AddNodeAndConnect {
                name: "Sink".to_owned(),
                pos: Pos2::new(200.0, 0.0),
                from,
            },
            &egui::Context::default(),
            None,
        );

        assert_eq!(widget.graph.nodes.len(), 2);
        assert!(widget.graph.connections.is_empty());
    }

    #[test]
    fn add_node_with_pointer_spawns_there_and_enters_placement() {
        let mut widget = test_widget();
        let pointer = Pos2::new(120.0, 80.0);

        let effect = widget.execute_action(
            GraphAction::AddNode {
                name: "Source".to_owned(),
                pos: Pos2::new(0.0, 0.0),
            },
            &egui::Context::default(),
            Some(pointer),
        );

        assert!(effect == ActionEffect::EnterPlacement);
        let node = widget
            .graph
            .nodes
            .values()
            .find(|n| n.def_name() == "Source")
            .expect("source node should exist");
        assert_eq!(node.pos, pointer);
    }

    #[test]
    fn add_node_selects_only_the_new_node_not_a_prior_selection() {
        // Regression test: placement mode moves whatever is `selected`. If
        // adding a node left an older selection in place instead of
        // selecting the new node, placement mode would drag that old node
        // around instead of the one that was just added.
        let mut widget = test_widget();
        let existing = widget
            .add_node_at("Source", Pos2::new(0.0, 0.0))
            .expect("source node should be created");
        widget.graph.nodes.get_mut(&existing).unwrap().selected = true;

        widget.execute_action(
            GraphAction::AddNode {
                name: "Sink".to_owned(),
                pos: Pos2::new(0.0, 0.0),
            },
            &egui::Context::default(),
            Some(Pos2::new(120.0, 80.0)),
        );

        let new_id = widget
            .graph
            .nodes
            .values()
            .find(|n| n.def_name() == "Sink")
            .expect("sink node should exist")
            .id;
        assert!(widget.graph.nodes[&new_id].selected);
        assert!(!widget.graph.nodes[&existing].selected);
    }

    #[test]
    fn add_node_without_pointer_uses_given_pos_and_skips_placement() {
        let mut widget = test_widget();
        let pos = Pos2::new(50.0, 60.0);

        let effect = widget.execute_action(
            GraphAction::AddNode {
                name: "Source".to_owned(),
                pos,
            },
            &egui::Context::default(),
            None,
        );

        assert!(effect == ActionEffect::None);
        let node = widget
            .graph
            .nodes
            .values()
            .find(|n| n.def_name() == "Source")
            .expect("source node should exist");
        assert_eq!(node.pos, pos);
    }

    #[test]
    fn duplicate_selected_with_pointer_enters_placement() {
        let mut widget = test_widget();
        let source_id = widget
            .add_node_at("Source", Pos2::new(10.0, 10.0))
            .expect("source node should be created");
        widget.graph.nodes.get_mut(&source_id).unwrap().selected = true;
        let pointer = Pos2::new(300.0, 40.0);

        let effect = widget.execute_action(
            GraphAction::DuplicateSelected,
            &egui::Context::default(),
            Some(pointer),
        );

        assert!(effect == ActionEffect::EnterPlacement);
        assert_eq!(widget.graph.nodes.len(), 2);
    }

    #[test]
    fn duplicate_selected_with_nothing_selected_does_not_enter_placement() {
        let mut widget = test_widget();
        widget
            .add_node_at("Source", Pos2::new(10.0, 10.0))
            .expect("source node should be created");
        // Freshly added node is not selected by default in this harness path
        // (add_node_at itself doesn't mark it selected), so nothing to
        // duplicate.
        for node in widget.graph.nodes.values_mut() {
            node.selected = false;
        }

        let effect = widget.execute_action(
            GraphAction::DuplicateSelected,
            &egui::Context::default(),
            Some(Pos2::new(300.0, 40.0)),
        );

        assert!(effect == ActionEffect::None);
        assert_eq!(widget.graph.nodes.len(), 1);
    }

    #[test]
    fn select_all_selects_every_node_and_frame() {
        let mut widget = test_widget();
        let a = widget.add_node_at("Source", Pos2::new(0.0, 0.0)).unwrap();
        let b = widget.add_node_at("Sink", Pos2::new(0.0, 0.0)).unwrap();
        widget
            .graph
            .add_frame("F".to_owned(), Color32::WHITE, vec![a]);

        widget.execute_action(GraphAction::SelectAll, &egui::Context::default(), None);

        assert!(widget.graph.nodes[&a].selected);
        assert!(widget.graph.nodes[&b].selected);
        assert!(widget.graph.frames[0].selected);
    }

    #[test]
    fn deselect_all_clears_every_selection() {
        let mut widget = test_widget();
        let a = widget.add_node_at("Source", Pos2::new(0.0, 0.0)).unwrap();
        widget.graph.nodes.get_mut(&a).unwrap().selected = true;
        let frame_id = widget
            .graph
            .add_frame("F".to_owned(), Color32::WHITE, vec![a]);
        widget
            .graph
            .frames
            .iter_mut()
            .find(|f| f.id == frame_id)
            .unwrap()
            .selected = true;

        widget.execute_action(GraphAction::DeselectAll, &egui::Context::default(), None);

        assert!(!widget.graph.nodes[&a].selected);
        assert!(!widget.graph.frames[0].selected);
    }

    #[test]
    fn link_active_to_previous_connects_compatible_sockets() {
        let mut widget = test_widget();
        let source = widget
            .add_node_at("Source", Pos2::new(0.0, 0.0))
            .expect("source node should be created");
        let sink = widget
            .add_node_at("Sink", Pos2::new(100.0, 0.0))
            .expect("sink node should be created");
        // `add_node_at` makes each new node the active one, so after adding
        // both, `previous_active_node` is `source` and `active_node` is
        // `sink` — exactly the "select A, then B, press F" sequence.
        assert_eq!(widget.previous_active_node, Some(source));
        assert_eq!(widget.active_node, Some(sink));

        widget.execute_action(
            GraphAction::LinkActiveToPrevious,
            &egui::Context::default(),
            None,
        );

        assert_eq!(widget.graph.connections.len(), 1);
        let connection = &widget.graph.connections[0];
        assert_eq!(connection.from.node, source);
        assert_eq!(connection.to.node, sink);
    }

    #[test]
    fn link_active_to_previous_is_a_noop_without_two_distinct_nodes() {
        let mut widget = test_widget();
        widget
            .add_node_at("Source", Pos2::new(0.0, 0.0))
            .expect("source node should be created");
        // Only one node was ever added, so there's no previous active node.

        widget.execute_action(
            GraphAction::LinkActiveToPrevious,
            &egui::Context::default(),
            None,
        );

        assert!(widget.graph.connections.is_empty());
    }
}
