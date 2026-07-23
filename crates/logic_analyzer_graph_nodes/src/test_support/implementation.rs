use egui::Pos2;

use logic_analyzer_graph_api::node::{GraphNodeRegistration, RuntimeBuilder};
use logic_analyzer_graph_api::node_support::LiveCaptureEdit;
use node_graph::{NodeGraphWidget, NodeId, NodeTypeRegistry};

fn registrations() -> impl Iterator<Item = &'static GraphNodeRegistration> {
    inventory::iter::<GraphNodeRegistration>.into_iter()
}

fn registration(stable_id: &str) -> &'static GraphNodeRegistration {
    registrations()
        .find(|registration| registration.stable_id() == stable_id)
        .unwrap_or_else(|| panic!("graph node '{stable_id}' is not registered"))
}

pub fn build_registry() -> NodeTypeRegistry {
    let mut registry = NodeTypeRegistry::new();
    for registration in registrations() {
        registration.apply_node(&mut registry);
    }
    registry
}

pub fn node_name(stable_id: &str) -> &'static str {
    registration(stable_id).name()
}

pub fn registered_node_name(stable_id: &str) -> &'static str {
    node_name(stable_id)
}

pub fn node_builder(stable_id: &str) -> Box<dyn RuntimeBuilder> {
    registration(stable_id)
        .builder()
        .unwrap_or_else(|| panic!("graph node '{stable_id}' has no runtime builder"))
}

pub fn default_node_state(stable_id: &str) -> serde_json::Value {
    let mut widget = NodeGraphWidget::new(build_registry());
    let node = widget
        .add_node_at(node_name(stable_id), Pos2::ZERO)
        .unwrap_or_else(|| panic!("graph node '{stable_id}' could not be instantiated"));
    widget.graph().nodes[&node].state.clone()
}

pub fn apply_registered_live_capture_edit(
    stable_id: &str,
    state: &serde_json::Value,
    edit: &LiveCaptureEdit,
) -> Result<serde_json::Value, String> {
    node_builder(stable_id)
        .apply_live_capture_edit(state, edit)?
        .ok_or_else(|| format!("registered graph node '{stable_id}' rejected its capture edit"))
}

pub fn build_binary_decoder_demo(widget: &mut NodeGraphWidget) {
    crate::nodes::test_graphs_tests::build_binary_decoder_demo(widget);
}

pub fn build_live_binary_test(widget: &mut NodeGraphWidget) -> NodeId {
    crate::nodes::test_graphs_tests::build_live_binary_test(widget)
}

pub fn populate_startup(widget: &mut NodeGraphWidget) {
    crate::nodes::test_graphs_tests::populate_startup(widget);
}

pub fn populate_uart_demo(widget: &mut NodeGraphWidget) {
    crate::nodes::test_graphs_tests::populate_uart_demo(widget);
}
