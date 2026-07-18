use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::Arc;

use egui::{Pos2, Sense, Ui};

use input_bindings::InputBindings;

use super::action::HotkeyRegistry;
use super::interaction::{GraphResponses, InteractionState};
use super::menu::MenuController;
use super::panel::{PanelState, PanelTab};
use super::{layout, render};
use crate::model::{FrameId, GraphState, Node, NodeBadge, NodeId};
use crate::runtime::{NodeInstance, NodeRuntime, NodeTypeRegistry};
use crate::support::ViewState;

// ── Main widget ───────────────────────────────────────────────────────────────

pub struct NodeGraphWidget {
    pub(super) graph: GraphState,
    pub(super) runtime: HashMap<NodeId, Box<dyn NodeInstance>>,
    pub(super) view: ViewState,
    pub(super) interaction_state: InteractionState,
    pub(super) registry: NodeTypeRegistry,
    pub(super) minimap_visible: bool,
    pub(super) top_node: Option<NodeId>,
    pub(super) menu: MenuController,
    /// Pending copy/paste confirmation ("Copied 3 node(s)"), taken and
    /// cleared by the host app's `take_io_status` — the host's own toast
    /// system (Phase 4.2) owns display and timing, not the widget.
    pub(super) io_status: Option<String>,
    pub(super) hotkeys: HotkeyRegistry,
    pub(super) input_bindings: Arc<InputBindings>,
    pub(super) clipboard_cache: Option<String>,
    pub(super) undo_stack: Vec<GraphState>,
    pub(super) redo_stack: Vec<GraphState>,
    pub(super) frame_rename: Option<FrameRenameState>,
    pub(super) node_rename: Option<NodeRenameState>,
    /// Most recently clicked/added node; the properties panel shows it.
    pub(super) active_node: Option<NodeId>,
    pub(super) panel: PanelState,
    /// Badges set from outside the graph (compiler errors, runtime status);
    /// they take precedence over def-driven badges.
    pub(super) external_badges: HashMap<NodeId, NodeBadge>,
    /// Short live-status texts (e.g. items-produced counters) drawn small
    /// in the node header.
    pub(super) node_statuses: HashMap<NodeId, String>,
    /// Nodes whose host-owned derived data can be cleared from the context
    /// menu. The widget only queues a request; the host performs the I/O.
    pub(super) derived_cache_nodes: HashSet<NodeId>,
    pub(super) clear_derived_cache_request: Option<NodeId>,
    /// Host-provided, application-neutral node context actions.
    pub(super) node_context_actions: HashMap<NodeId, Vec<NodeContextAction>>,
    pub(super) node_context_action_request: Option<(NodeId, String)>,
    /// Host-controlled edit gate. View navigation, selection, inspection,
    /// and copy remain available while graph mutations are disabled.
    pub(super) editing_enabled: bool,
}

/// A context-menu action contributed by the host application. Both the ID
/// and its meaning are opaque to the node graph widget.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NodeContextAction {
    pub id: String,
    pub label: String,
    pub icon: Option<String>,
    pub checked: bool,
}

impl NodeContextAction {
    pub fn new(id: impl Into<String>, label: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            label: label.into(),
            icon: None,
            checked: false,
        }
    }

    pub fn with_icon(mut self, icon: impl Into<String>) -> Self {
        self.icon = Some(icon.into());
        self
    }

    pub fn with_checkmark(mut self, checked: bool) -> Self {
        self.checked = checked;
        self
    }
}

pub(super) struct FrameRenameState {
    pub(super) frame_id: FrameId,
    pub(super) text: String,
    pub(super) screen_pos: Pos2,
}

pub(super) struct NodeRenameState {
    pub(super) node_id: NodeId,
    pub(super) text: String,
    pub(super) screen_pos: Pos2,
}

/// Public mirror of the internal `PanelTab` — kept separate so the widget's
/// internal panel module doesn't need to be part of the crate's API surface.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum GraphPanelTab {
    Node,
    View,
}

impl From<PanelTab> for GraphPanelTab {
    fn from(tab: PanelTab) -> Self {
        match tab {
            PanelTab::Node => Self::Node,
            PanelTab::View => Self::View,
        }
    }
}

impl From<GraphPanelTab> for PanelTab {
    fn from(tab: GraphPanelTab) -> Self {
        match tab {
            GraphPanelTab::Node => Self::Node,
            GraphPanelTab::View => Self::View,
        }
    }
}

/// Persistable UI state that isn't part of the graph document itself —
/// N-panel width/tab and minimap visibility (Phase 5.2). The host app reads
/// this via [`NodeGraphWidget::ui_prefs`] to save it and restores it via
/// [`NodeGraphWidget::set_ui_prefs`] on the next launch.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct GraphUiPrefs {
    pub panel_width: f32,
    pub panel_tab: Option<GraphPanelTab>,
    pub minimap_visible: bool,
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
        let input_bindings = Arc::new(
            InputBindings::from_json(r#"{"bindings":[]}"#)
                .expect("empty input binding configuration is valid"),
        );
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
            input_bindings,
            clipboard_cache: None,
            undo_stack: Vec::new(),
            redo_stack: Vec::new(),
            frame_rename: None,
            node_rename: None,
            active_node: None,
            panel: PanelState::default(),
            external_badges: HashMap::new(),
            node_statuses: HashMap::new(),
            derived_cache_nodes: HashSet::new(),
            clear_derived_cache_request: None,
            node_context_actions: HashMap::new(),
            node_context_action_request: None,
            editing_enabled: true,
        }
    }

    /// Installs the host application's bindings. Context and action names are
    /// opaque to the binding manager and interpreted only by this widget.
    pub fn set_input_bindings(&mut self, input_bindings: Arc<InputBindings>) {
        self.input_bindings = input_bindings;
    }

    pub fn graph(&self) -> &GraphState {
        &self.graph
    }

    pub fn graph_mut(&mut self) -> &mut GraphState {
        &mut self.graph
    }

    pub fn editing_enabled(&self) -> bool {
        self.editing_enabled
    }

    /// Enables or disables graph mutations initiated through the widget.
    /// Disabling during a modal edit restores its pre-edit snapshot.
    pub fn set_editing_enabled(&mut self, enabled: bool) {
        if self.editing_enabled == enabled {
            return;
        }
        self.editing_enabled = enabled;
        if enabled {
            return;
        }

        let restore_snapshot = match self.interaction_state {
            InteractionState::DraggingNode { .. }
            | InteractionState::DraggingFrame { .. }
            | InteractionState::PlacingNodes { .. } => true,
            InteractionState::DraggingWire {
                restore_on_cancel, ..
            } => restore_on_cancel,
            InteractionState::Idle
            | InteractionState::Panning { .. }
            | InteractionState::BoxSelecting { .. }
            | InteractionState::CuttingWire { .. } => false,
        };
        if restore_snapshot {
            self.cancel_undo_snapshot();
        }
        if !matches!(
            self.interaction_state,
            InteractionState::Panning { .. } | InteractionState::BoxSelecting { .. }
        ) {
            self.interaction_state = InteractionState::Idle;
        }
        self.frame_rename = None;
        self.node_rename = None;
    }

    /// Flushes inline control state into the graph before an external
    /// operation snapshots or validates it.
    pub fn sync_node_states(&mut self) {
        self.sync_all_node_state();
    }

    /// Takes the pending copy/paste confirmation message, if any — call
    /// once per frame and feed the result into the host app's toast system
    /// (Phase 4.2). Returns `None` most frames.
    pub fn take_io_status(&mut self) -> Option<String> {
        self.io_status.take()
    }

    pub fn set_derived_cache_nodes(&mut self, nodes: impl IntoIterator<Item = NodeId>) {
        self.derived_cache_nodes = nodes.into_iter().collect();
    }

    pub fn take_clear_derived_cache_request(&mut self) -> Option<NodeId> {
        self.clear_derived_cache_request.take()
    }

    pub fn set_node_context_actions(&mut self, actions: HashMap<NodeId, Vec<NodeContextAction>>) {
        self.node_context_actions = actions;
    }

    pub fn take_node_context_action(&mut self) -> Option<(NodeId, String)> {
        self.node_context_action_request.take()
    }

    /// Current UI prefs (N-panel width/tab, minimap visibility) — for the
    /// host app to persist across launches (Phase 5.2).
    pub fn ui_prefs(&self) -> GraphUiPrefs {
        GraphUiPrefs {
            panel_width: self.panel.width,
            panel_tab: self.panel.active_tab.map(GraphPanelTab::from),
            minimap_visible: self.minimap_visible,
        }
    }

    /// Restores UI prefs saved via [`Self::ui_prefs`] — call once after
    /// construction, before the first `show`.
    pub fn set_ui_prefs(&mut self, prefs: GraphUiPrefs) {
        self.panel.width = prefs.panel_width;
        self.panel.active_tab = prefs.panel_tab.map(PanelTab::from);
        self.minimap_visible = prefs.minimap_visible;
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
            self.set_active_node(nid);
            Some(nid)
        } else {
            None
        }
    }

    pub(super) fn set_active_node(&mut self, id: NodeId) {
        self.active_node = Some(id);
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

    /// Applies one host-initiated state edit as an undoable graph mutation.
    pub fn edit_node_state(&mut self, id: NodeId, state: serde_json::Value) -> bool {
        if !self.editing_enabled {
            return false;
        }
        self.sync_all_node_state();
        if self.graph.nodes.get(&id).map(|node| &node.state) == Some(&state) {
            return true;
        }
        let previous = self.graph.clone();
        if self.set_node_state(id, state) {
            self.undo_stack.push(previous);
            self.redo_stack.clear();
            true
        } else {
            self.graph = previous;
            self.restore_runtime();
            false
        }
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

    pub(super) fn fit_graph_to_viewport(
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

    /// Zooms to fit the current selection (Phase 2, Blender's numpad-`.`) —
    /// falls back to fitting the whole graph, matching `Home`, when nothing
    /// is selected.
    pub(super) fn fit_selection_to_viewport(
        &mut self,
        layout: &layout::GraphWidgetLayout,
        viewport: egui::Rect,
        origin: Pos2,
    ) {
        let node_bounds = self
            .graph
            .nodes
            .values()
            .filter(|node| node.selected)
            .filter_map(|node| layout.node_rects.get(&node.id).copied());
        let frame_bounds = self
            .graph
            .frames
            .iter()
            .filter(|frame| frame.selected)
            .filter_map(|frame| layout.frame_rects.get(&frame.id).copied());
        let bounds = node_bounds.chain(frame_bounds).reduce(|a, b| a.union(b));
        match bounds {
            Some(bounds) => self.view.fit_to_rect(bounds, viewport, origin, 48.0),
            None => self.fit_graph_to_viewport(layout, viewport, origin),
        }
    }

    /// Replaces the whole graph and rebuilds every node's runtime instance
    /// from the registry — the programmatic equivalent of loading a saved
    /// file. State restore runs through the same reconcile path as
    /// file loading (`restore_node`): sockets validated against current defs,
    /// `on_update` re-run, badges recomputed.
    pub fn set_graph(&mut self, graph: GraphState) {
        self.graph = graph;
        self.graph.fixup_reroute_outputs();
        self.external_badges.clear();
        self.node_statuses.clear();
        self.active_node = None;
        self.restore_runtime();
    }

    /// Resets to a fresh, empty graph — the programmatic equivalent of
    /// File → New (Phase 5.1). Clears undo/redo along with graph content;
    /// UI prefs (panel width, minimap) and the runtime registry are
    /// untouched.
    pub fn new_graph(&mut self) {
        self.set_graph(GraphState::default());
        self.undo_stack.clear();
        self.redo_stack.clear();
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

    pub(super) fn run_update(&mut self, id: NodeId) {
        if let (Some(instance), Some(node)) =
            (self.runtime.get_mut(&id), self.graph.nodes.get_mut(&id))
        {
            instance.update(&mut node.inputs, &mut node.outputs);
            node.state = instance.save_state();
            node.badge = instance.badge();
        }
    }

    pub(super) fn sync_all_node_state(&mut self) {
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
            render::GraphRenderContext {
                rect: content_rect,
                origin,
                pointer,
                layout: &layout,
                hovered_socket,
            },
        );
        self.show_socket_tooltip(&responses, hovered_socket);
        if let Some(panel_rect) = panel_rect {
            self.show_active_panel(ui, panel_rect);
        }
        self.show_panel_tab_bar(ui, tab_bar_rect);
        self.show_frame_rename(ui.ctx());
        self.show_node_rename(ui.ctx());
    }

    /// One-line hint of available actions for the current interaction
    /// state, for a status bar (Phase 4.1). Static strings only — cheap
    /// enough to call every frame.
    pub fn status_hint(&self) -> &'static str {
        match &self.interaction_state {
            InteractionState::DraggingWire { .. } => {
                "Release on a socket to connect · release on canvas to search for a node"
            }
            InteractionState::PlacingNodes { .. } => "Click to place · Esc to cancel",
            InteractionState::CuttingWire { .. } => "Release to cut the crossed wires",
            InteractionState::DraggingNode { .. } => "Drop inside a frame to join it",
            InteractionState::DraggingFrame { .. }
            | InteractionState::BoxSelecting { .. }
            | InteractionState::Panning { .. } => "",
            InteractionState::Idle => {
                let any_selected = self.graph.nodes.values().any(|node| node.selected)
                    || self.graph.frames.iter().any(|frame| frame.selected);
                if any_selected {
                    "Shift+D Duplicate · F2 Rename · H Collapse · X Delete · . Zoom to Selection"
                } else {
                    "Shift+A Add · A Select All · RMB Menu · MMB Pan"
                }
            }
        }
    }

    /// Most-specific input-binding context for an active graph interaction.
    ///
    /// The strings are opaque to the generic binding manager. Returning only
    /// active interactions lets the host keep ordinary hover context handling
    /// separate while ensuring a drag remains active after leaving the graph
    /// rectangle.
    pub fn active_input_context(&self) -> Option<&'static str> {
        match self.interaction_state {
            InteractionState::DraggingNode { .. } => Some("node_graph.drag_node"),
            InteractionState::DraggingWire { .. } => Some("node_graph.drag_wire"),
            _ => None,
        }
    }

    /// Current zoom level as a whole-number percentage, for a status bar.
    pub fn zoom_percent(&self) -> i32 {
        (self.view.zoom * 100.0).round() as i32
    }

    /// `"n nodes"` or `"m/n selected"`, for a status bar.
    pub fn selection_summary(&self) -> String {
        let total = self.graph.nodes.len();
        let selected = self
            .graph
            .nodes
            .values()
            .filter(|node| node.selected)
            .count();
        if selected > 0 {
            format!("{selected}/{total} selected")
        } else {
            format!("{total} node{}", if total == 1 { "" } else { "s" })
        }
    }
}

#[cfg(test)]
mod tests {
    use egui::{Pos2, Rect, Vec2};

    use super::{GraphPanelTab, GraphUiPrefs, NodeGraphWidget, graph_pointer};
    use crate::model::NodeId;
    use crate::runtime::NodeTypeRegistry;
    use crate::widget::graph::action::GraphAction;
    use crate::widget::graph::interaction::InteractionState;

    #[test]
    fn node_panel_is_open_by_default_and_restored_preferences_win() {
        let mut widget = NodeGraphWidget::new(NodeTypeRegistry::new());
        assert_eq!(widget.ui_prefs().panel_tab, Some(GraphPanelTab::Node));

        widget.set_ui_prefs(GraphUiPrefs {
            panel_width: 280.0,
            panel_tab: None,
            minimap_visible: true,
        });
        assert_eq!(widget.ui_prefs().panel_tab, None);
    }

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

    #[test]
    fn node_drag_reports_a_specific_input_context() {
        let mut widget = NodeGraphWidget::new(NodeTypeRegistry::new());
        assert_eq!(widget.active_input_context(), None);

        widget.interaction_state = InteractionState::DraggingNode {
            node_id: NodeId(1),
            offset: Vec2::ZERO,
            constraint: None,
        };
        assert_eq!(widget.active_input_context(), Some("node_graph.drag_node"));
    }

    #[test]
    fn read_only_mode_blocks_mutations_but_keeps_selection_actions() {
        let mut widget = NodeGraphWidget::new(NodeTypeRegistry::new());
        let node = widget.add_node_at("Reroute", Pos2::ZERO).unwrap();
        widget.set_editing_enabled(false);

        widget.execute_action(
            GraphAction::Delete { target: Some(node) },
            &egui::Context::default(),
            None,
        );
        assert!(widget.graph().nodes.contains_key(&node));

        widget.execute_action(GraphAction::SelectAll, &egui::Context::default(), None);
        assert!(widget.graph().nodes[&node].selected);
    }

    #[test]
    fn entering_read_only_mode_reverts_an_active_node_drag() {
        let mut widget = NodeGraphWidget::new(NodeTypeRegistry::new());
        let node = widget
            .add_node_at("Reroute", Pos2::new(10.0, 20.0))
            .unwrap();
        widget.push_undo_snapshot();
        widget.graph.nodes.get_mut(&node).unwrap().pos = Pos2::new(80.0, 90.0);
        widget.interaction_state = InteractionState::DraggingNode {
            node_id: node,
            offset: Vec2::ZERO,
            constraint: None,
        };

        widget.set_editing_enabled(false);

        assert!(!widget.editing_enabled());
        assert_eq!(widget.graph().nodes[&node].pos, Pos2::new(10.0, 20.0));
        assert!(matches!(widget.interaction_state, InteractionState::Idle));
    }
}
