use serde_json::Value;

use crate::compiler::LiveCaptureFeature;

pub(super) fn feature(
    _state: &Value,
) -> Result<Option<Box<dyn LiveCaptureFeature>>, String> {
    Ok(None)
}
