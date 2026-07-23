use crate::LiveCaptureEdit;

fn node_registration(stable_id: &str) -> &'static super::GraphNodeRegistration {
    super::graph_node_registrations()
        .into_iter()
        .find(|registration| registration.stable_id() == stable_id)
        .unwrap_or_else(|| panic!("test-support graph node '{stable_id}' is not registered"))
}

pub fn registered_node_name(stable_id: &str) -> &'static str {
    node_registration(stable_id).name()
}

pub fn apply_registered_live_capture_edit(
    stable_id: &str,
    state: &serde_json::Value,
    edit: &LiveCaptureEdit,
) -> Result<serde_json::Value, String> {
    node_registration(stable_id)
        .builder()
        .ok_or_else(|| format!("registered graph node '{stable_id}' is not runnable"))?
        .apply_live_capture_edit(state, edit)?
        .ok_or_else(|| format!("registered graph node '{stable_id}' rejected its capture edit"))
}
