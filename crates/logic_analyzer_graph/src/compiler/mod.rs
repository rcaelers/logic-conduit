mod errors;
mod graph;
mod plugin;
mod port_kind;

std::cfg_select! {
    target_arch = "wasm32" => {
        #[path = "cache_platform_wasm.rs"]
        mod cache_platform;
    }
    _ => {
        #[path = "cache_platform_native.rs"]
        mod cache_platform;
    }
}

pub use errors::{ApplyError, CompileError};
pub use graph::*;
pub use plugin::PluginContext;
pub use port_kind::{PortKind, PortValue};
