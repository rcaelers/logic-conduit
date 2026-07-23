//! Shared test live-capture feature implementation.

use serde_json::Value;

use logic_analyzer_graph_api::node::LiveCaptureFeature;

pub(crate) fn feature(state: &Value) -> Result<Option<Box<dyn LiveCaptureFeature>>, String> {
    super::platform::feature(state)
}
