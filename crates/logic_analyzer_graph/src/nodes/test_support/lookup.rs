use logic_analyzer_graph_api::node::RuntimeBuilder;

fn registration(stable_id: &str) -> &'static super::super::GraphNodeRegistration {
    crate::compiler::graph_node_registrations()
        .into_iter()
        .find(|registration| registration.stable_id() == stable_id)
        .unwrap_or_else(|| panic!("graph node '{stable_id}' is not registered"))
}

pub(crate) fn node_name(stable_id: &str) -> &'static str {
    registration(stable_id).name()
}

pub(crate) fn node_builder(stable_id: &str) -> Box<dyn RuntimeBuilder> {
    registration(stable_id)
        .builder()
        .unwrap_or_else(|| panic!("graph node '{stable_id}' has no runtime builder"))
}

pub(crate) fn default_node_state(stable_id: &str) -> serde_json::Value {
    let mut node_types = node_graph::NodeTypeRegistry::new();
    registration(stable_id).apply_node(&mut node_types);
    let mut widget = node_graph::NodeGraphWidget::new(node_types);
    let node = widget
        .add_node_at(node_name(stable_id), egui::Pos2::ZERO)
        .unwrap_or_else(|| panic!("graph node '{stable_id}' could not be instantiated"));
    widget.graph().nodes[&node].state.clone()
}
