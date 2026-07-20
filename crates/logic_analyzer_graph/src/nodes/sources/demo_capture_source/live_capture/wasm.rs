use serde_json::Value;

use crate::LiveCaptureFeature;

pub(crate) fn feature(_state: &Value) -> Result<Option<Box<dyn LiveCaptureFeature>>, String> {
    Ok(None)
}
