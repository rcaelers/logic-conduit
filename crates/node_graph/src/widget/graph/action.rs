pub(super) use super::super::menu::Shortcut;
use super::{FrameRenameState, NodeGraphWidget};
use crate::{
    api::sockets_compatible,
    model::{Connection, FrameId, Node, NodeId, SocketDirection, SocketId},
};
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
}

#[derive(Clone)]
pub(super) enum GraphAction {
    Undo,
    Redo,
    AddNode {
        name: String,
        pos: Pos2,
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
    Save,
    Load,
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
        r.bind(Shortcut::ctrl(egui::Key::S), GraphAction::Save);
        r.bind(Shortcut::ctrl(egui::Key::O), GraphAction::Load);
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
    pub(super) fn execute_action(
        &mut self,
        action: GraphAction,
        egui_ctx: &egui::Context,
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
            GraphAction::AddNode { name, pos } => {
                self.push_undo_snapshot();
                self.add_node_at(&name, pos);
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
                self.paste_nodes(text.as_deref(), pos, egui_ctx);
                ActionEffect::None
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
                self.duplicate_selected();
                ActionEffect::None
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
            GraphAction::Save => {
                self.save_graph(egui_ctx);
                ActionEffect::None
            }
            GraphAction::Load => {
                self.push_undo_snapshot();
                self.load_graph(egui_ctx);
                ActionEffect::ResetInteraction
            }
        }
    }

    fn undo(&mut self) {
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
        if target.is_none() {
            self.graph.frames.retain(|frame| !frame.selected);
        }
        for id in self.target_nodes(target) {
            self.graph.remove_node(id);
            self.runtime.remove(&id);
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
        for connection in rewired {
            self.graph.add_connection(connection.from, connection.to);
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
            .map(|socket| socket.type_name.as_str())
        else {
            return false;
        };
        let Some(to_type) = self
            .graph
            .nodes
            .get(&to.node)
            .and_then(|node| node.inputs.get(to.index))
            .map(|socket| socket.type_name.as_str())
        else {
            return false;
        };
        sockets_compatible(from_type, to_type)
    }

    fn duplicate_selected(&mut self) {
        let selected: Vec<_> = self
            .graph
            .nodes
            .values()
            .filter(|node| node.selected)
            .map(|node| node.id)
            .collect();
        if selected.is_empty() {
            return;
        }

        let payload = self.build_clipboard_payload(&selected);
        self.paste_payload(payload, None);
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
            pasted += 1;
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

    fn save_graph(&mut self, egui_ctx: &egui::Context) {
        let time = egui_ctx.input(|input| input.time);
        for id in self.graph.sorted_node_ids() {
            if let (Some(instance), Some(node)) =
                (self.runtime.get(&id), self.graph.nodes.get_mut(&id))
            {
                node.state = instance.save_state();
            }
        }

        match serde_json::to_string_pretty(&self.graph) {
            Ok(json) => match std::fs::write("pipeline.json", &json) {
                Ok(_) => self.io_status = Some(("Saved  pipeline.json".to_string(), time)),
                Err(error) => self.io_status = Some((format!("Save failed: {error}"), time)),
            },
            Err(error) => self.io_status = Some((format!("Serialization error: {error}"), time)),
        }
    }

    fn load_graph(&mut self, egui_ctx: &egui::Context) {
        let time = egui_ctx.input(|input| input.time);
        match std::fs::read_to_string("pipeline.json") {
            Ok(json) => match serde_json::from_str(&json) {
                Ok(loaded) => {
                    self.graph = loaded;
                    self.runtime.clear();
                    for node in self.graph.nodes.values_mut() {
                        if let Some(instance) = self.registry.restore_node(node) {
                            self.runtime.insert(node.id, instance);
                        }
                    }
                    self.io_status = Some(("Loaded  pipeline.json".to_string(), time));
                }
                Err(error) => self.io_status = Some((format!("Parse error: {error}"), time)),
            },
            Err(error) => self.io_status = Some((format!("Load failed: {error}"), time)),
        }
    }
}
