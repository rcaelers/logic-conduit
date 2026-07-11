mod action;
mod interaction;
mod layout;
mod menu;
mod minimap;
mod panel;
mod render;

use action::HotkeyRegistry;
use interaction::{GraphResponses, InteractionState};
use menu::MenuController;
use panel::PanelState;

use crate::{
    model::{FrameId, GraphState, Node, NodeBadge, NodeId},
    runtime::NodeTypeRegistry,
    runtime::{NodeInstance, NodeRuntime},
    support::ViewState,
};
use egui::{Pos2, Sense, Ui};
use std::collections::HashMap;
use std::path::Path;

// ── Main widget ───────────────────────────────────────────────────────────────

pub struct NodeGraphWidget {
    graph: GraphState,
    runtime: HashMap<NodeId, Box<dyn NodeInstance>>,
    view: ViewState,
    interaction_state: InteractionState,
    registry: NodeTypeRegistry,
    minimap_visible: bool,
    top_node: Option<NodeId>,
    menu: MenuController,
    io_status: Option<(String, f64)>,
    hotkeys: HotkeyRegistry,
    clipboard_cache: Option<String>,
    undo_stack: Vec<GraphState>,
    redo_stack: Vec<GraphState>,
    frame_rename: Option<FrameRenameState>,
    /// Most recently clicked/added node; the properties panel shows it.
    active_node: Option<NodeId>,
    panel: PanelState,
    /// Badges set from outside the graph (compiler errors, runtime status);
    /// they take precedence over def-driven badges.
    external_badges: HashMap<NodeId, NodeBadge>,
    /// Short live-status texts (e.g. items-produced counters) drawn small
    /// in the node header.
    node_statuses: HashMap<NodeId, String>,
}

struct FrameRenameState {
    frame_id: FrameId,
    text: String,
    screen_pos: Pos2,
}

fn graph_pointer(
    pointer: Option<Pos2>,
    panel_rect: Option<egui::Rect>,
    tab_bar_rect: egui::Rect,
) -> Option<Pos2> {
    pointer.filter(|pointer| {
        !tab_bar_rect.contains(*pointer) && !panel_rect.is_some_and(|rect| rect.contains(*pointer))
    })
}

impl NodeGraphWidget {
    pub fn new(registry: NodeTypeRegistry) -> Self {
        Self {
            graph: GraphState::default(),
            runtime: HashMap::new(),
            view: ViewState::default(),
            interaction_state: InteractionState::default(),
            registry,
            minimap_visible: true,
            top_node: None,
            menu: MenuController::new(),
            io_status: None,
            hotkeys: HotkeyRegistry::graph_defaults(),
            clipboard_cache: None,
            undo_stack: Vec::new(),
            redo_stack: Vec::new(),
            frame_rename: None,
            active_node: None,
            panel: PanelState::default(),
            external_badges: HashMap::new(),
            node_statuses: HashMap::new(),
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
        if let Some(NodeRuntime { node, instance }) = self.registry.instantiate(name, id, pos) {
            let nid = node.id;
            self.runtime.insert(nid, instance);
            self.graph.add_node(node);
            self.active_node = Some(nid);
            Some(nid)
        } else {
            None
        }
    }

    /// Replaces a node's state wholesale and re-runs its def (sockets,
    /// visibility, badge) — the programmatic equivalent of editing its
    /// controls. Returns false when the node or its def is unknown or the
    /// state fails to restore.
    pub fn set_node_state(&mut self, id: NodeId, state: serde_json::Value) -> bool {
        let Some(node) = self.graph.nodes.get_mut(&id) else {
            return false;
        };
        node.state = state;
        let Some(instance) = self.registry.restore_node(node) else {
            return false;
        };
        self.runtime.insert(id, instance);
        true
    }

    /// Sets (or clears, with `None`) an externally owned badge on a node —
    /// compile errors, runtime status. External badges render instead of the
    /// def's own badge while present.
    pub fn set_node_badge(&mut self, id: NodeId, badge: Option<NodeBadge>) {
        match badge {
            Some(badge) => {
                self.external_badges.insert(id, badge);
            }
            None => {
                self.external_badges.remove(&id);
            }
        }
    }

    /// Sets (or clears) the short live-status text drawn in a node's header
    /// (e.g. "1.2M" items while a pipeline runs).
    pub fn set_node_status(&mut self, id: NodeId, status: Option<String>) {
        match status {
            Some(status) => {
                self.node_statuses.insert(id, status);
            }
            None => {
                self.node_statuses.remove(&id);
            }
        }
    }

    /// Clears every live-status text (e.g. when a new run starts).
    pub fn clear_node_statuses(&mut self) {
        self.node_statuses.clear();
    }

    fn fit_graph_to_viewport(
        &mut self,
        layout: &layout::GraphWidgetLayout,
        viewport: egui::Rect,
        origin: Pos2,
    ) {
        let bounds = layout
            .node_rects
            .values()
            .chain(layout.frame_rects.values())
            .copied()
            .reduce(|bounds, rect| bounds.union(rect));
        if let Some(bounds) = bounds {
            self.view.fit_to_rect(bounds, viewport, origin, 48.0);
        } else {
            self.view = ViewState::default();
        }
    }

    /// Replaces the whole graph and rebuilds every node's runtime instance
    /// from the registry — the programmatic equivalent of loading a saved
    /// file. State restore runs through the same reconcile path as
    /// file loading (`restore_node`): sockets validated against current defs,
    /// `on_update` re-run, badges recomputed.
    pub fn set_graph(&mut self, graph: GraphState) {
        self.graph = graph;
        self.external_badges.clear();
        self.node_statuses.clear();
        self.active_node = None;
        self.restore_runtime();
    }

    /// Saves the current graph as formatted JSON.
    pub fn save_to_path(&mut self, path: impl AsRef<Path>) -> Result<(), String> {
        let json = serde_json::to_string_pretty(&self.snapshot_value()?)
            .map_err(|error| format!("could not serialize graph: {error}"))?;
        std::fs::write(path.as_ref(), json)
            .map_err(|error| format!("could not write {}: {error}", path.as_ref().display()))
    }

    /// Captures the current graph, including state still held by inline node
    /// controls. Used by document persistence and dirty-state tracking.
    pub fn snapshot_value(&mut self) -> Result<serde_json::Value, String> {
        self.sync_all_node_state();
        serde_json::to_value(&self.graph)
            .map_err(|error| format!("could not serialize graph: {error}"))
    }

    /// Loads a graph from JSON and rebuilds its runtime node instances.
    /// The current graph is left untouched if reading or parsing fails.
    pub fn load_from_path(&mut self, path: impl AsRef<Path>) -> Result<(), String> {
        let json = std::fs::read_to_string(path.as_ref())
            .map_err(|error| format!("could not read {}: {error}", path.as_ref().display()))?;
        let graph = serde_json::from_str(&json)
            .map_err(|error| format!("could not parse {}: {error}", path.as_ref().display()))?;
        self.set_graph(graph);
        self.undo_stack.clear();
        self.redo_stack.clear();
        Ok(())
    }

    fn run_update(&mut self, id: NodeId) {
        if let (Some(instance), Some(node)) =
            (self.runtime.get_mut(&id), self.graph.nodes.get_mut(&id))
        {
            instance.update(&mut node.inputs, &mut node.outputs);
            node.state = instance.save_state();
            node.badge = instance.badge();
        }
    }

    fn sync_all_node_state(&mut self) {
        for id in self.graph.sorted_node_ids() {
            if let (Some(instance), Some(node)) =
                (self.runtime.get(&id), self.graph.nodes.get_mut(&id))
            {
                node.state = instance.save_state();
            }
        }
    }

    pub(super) fn push_undo_snapshot(&mut self) {
        self.sync_all_node_state();
        self.undo_stack.push(self.graph.clone());
        self.redo_stack.clear();
    }

    pub(super) fn can_undo(&self) -> bool {
        !self.undo_stack.is_empty()
    }

    pub(super) fn can_redo(&self) -> bool {
        !self.redo_stack.is_empty()
    }

    pub(super) fn restore_runtime(&mut self) {
        self.runtime.clear();
        for node in self.graph.nodes.values_mut() {
            if let Some(instance) = self.registry.restore_node(node) {
                self.runtime.insert(node.id, instance);
            }
        }
    }

    // ── Viewport render ───────────────────────────────────────────────────────

    pub fn show(&mut self, ui: &mut Ui) {
        let rect = ui.available_rect_before_wrap();
        let response = ui.allocate_rect(rect, Sense::click_and_drag());
        let painter = ui.painter_at(rect);
        let origin = rect.min;

        let pointer = response
            .hover_pos()
            .or_else(|| ui.input(|i| i.pointer.hover_pos()));

        // The right-side tab strip is always present. The optional panel body
        // floats over the graph and only claims input where it is visible.
        let tab_bar_rect = self.panel_tab_bar_rect(rect);
        let panel_rect = self.panel_rect(rect);
        let content_rect =
            egui::Rect::from_min_max(rect.min, Pos2::new(tab_bar_rect.left(), rect.max.y));
        let layout = self.build_layout(origin);
        let responses = if self.interaction_state.use_fast_rendering() {
            GraphResponses::canvas_only(response)
        } else {
            self.allocate_responses(ui, response, &layout, content_rect)
        };

        // Register the floating UI after every graph hit target so it owns
        // overlapping clicks and drags in egui's interaction z-order.
        if let Some(panel_rect) = panel_rect {
            self.update_panel_interaction(ui, panel_rect);
        }
        self.update_panel_tab_bar_interaction(ui, tab_bar_rect);

        let graph_pointer = graph_pointer(pointer, panel_rect, tab_bar_rect);
        let hovered_socket = graph_pointer.and_then(|_| self.hovered_socket(&responses));
        self.handle_input(ui, &responses, graph_pointer, origin, &layout, content_rect);

        let layout = self.build_layout(origin);
        self.draw_graph(
            ui,
            &painter,
            content_rect,
            origin,
            pointer,
            &layout,
            hovered_socket,
        );
        self.show_socket_tooltip(&responses, hovered_socket);
        if let Some(panel_rect) = panel_rect {
            self.show_active_panel(ui, panel_rect);
        }
        self.show_panel_tab_bar(ui, tab_bar_rect);
        self.show_frame_rename(ui.ctx());
    }
}

#[cfg(test)]
mod tests {
    use super::graph_pointer;
    use egui::{Pos2, Rect};

    #[test]
    fn floating_panel_blocks_graph_pointer_only_inside_its_bounds() {
        let panel = Rect::from_min_max(Pos2::new(600.0, 0.0), Pos2::new(900.0, 400.0));
        let tabs = Rect::from_min_max(Pos2::new(900.0, 0.0), Pos2::new(924.0, 800.0));

        assert_eq!(
            graph_pointer(Some(Pos2::new(700.0, 200.0)), Some(panel), tabs),
            None
        );
        assert_eq!(
            graph_pointer(Some(Pos2::new(910.0, 200.0)), Some(panel), tabs),
            None
        );
        assert_eq!(
            graph_pointer(Some(Pos2::new(700.0, 500.0)), Some(panel), tabs),
            Some(Pos2::new(700.0, 500.0))
        );
        assert_eq!(
            graph_pointer(Some(Pos2::new(300.0, 200.0)), Some(panel), tabs),
            Some(Pos2::new(300.0, 200.0))
        );
    }
}
