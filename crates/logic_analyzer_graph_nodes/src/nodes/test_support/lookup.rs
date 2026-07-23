use logic_analyzer_graph_api::node::GraphNodeRegistration;

fn registration(stable_id: &str) -> &'static GraphNodeRegistration {
    inventory::iter::<GraphNodeRegistration>
        .into_iter()
        .find(|registration| registration.stable_id() == stable_id)
        .unwrap_or_else(|| panic!("graph node '{stable_id}' is not registered"))
}

pub(crate) fn node_name(stable_id: &str) -> &'static str {
    registration(stable_id).name()
}
