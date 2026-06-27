mod action;
mod interaction;
mod layout;
mod menu;
mod minimap;
mod render;

use action::HotkeyRegistry;
use interaction::InteractionState;
use menu::MenuController;

use crate::{
    model::{GraphState, Node, NodeId},
    runtime::NodeTypeRegistry,
    runtime::{NodeInstance, NodeRuntime},
    support::ViewState,
};
use egui::{Pos2, Sense, Ui};
use std::collections::HashMap;

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
            Some(nid)
        } else {
            None
        }
    }

    fn run_update(&mut self, id: NodeId) {
        if let (Some(instance), Some(node)) =
            (self.runtime.get_mut(&id), self.graph.nodes.get_mut(&id))
        {
            instance.update(&mut node.inputs, &mut node.outputs);
            node.state = instance.save_state();
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

        let layout = self.build_layout(origin);
        let responses = self.allocate_responses(ui, response, &layout, rect);
        self.handle_input(ui, &responses, origin, &layout, rect);

        let layout = self.build_layout(origin);
        self.draw_graph(ui, &painter, rect, origin, pointer, &layout);
    }
}
