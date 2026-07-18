use serde_json::Value;

use crate::compiler::LiveCaptureFeature;

std::cfg_select! {
    target_arch = "wasm32" => {
        #[path = "wasm.rs"]
        mod imp;
    }
    _ => {
        #[path = "native.rs"]
        mod imp;
    }
}

pub(super) fn feature(state: &Value) -> Result<Option<Box<dyn LiveCaptureFeature>>, String> {
    imp::feature(state)
}
