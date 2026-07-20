use serde_json::Value;

use crate::LiveCaptureFeature;

pub(crate) fn feature(state: &Value) -> Result<Option<Box<dyn LiveCaptureFeature>>, String> {
    super::platform::feature(state)
}
