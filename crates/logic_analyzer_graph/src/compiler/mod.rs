mod binary_decoder;
mod buffer;
mod counter;
mod errors;
mod formatter;
mod graph;
mod logic_gate;
mod plugin;
mod port_kind;
mod spi_decoder;
mod sr_flip_flop;
mod tgck_recorder;
mod uart_decoder;
mod uart_demo_source;
mod viewer;
mod word_matcher;

std::cfg_select! {
    target_arch = "wasm32" => {
        #[path = "cache_platform_wasm.rs"]
        mod cache_platform;
    }
    _ => {
        #[path = "cache_platform_native.rs"]
        mod cache_platform;
        mod csv_writer;
        mod file_source;
        mod file_writer;
        mod sigrok_file_source;
        mod text_file_writer;
    }
}

pub use errors::{ApplyError, CompileError};
pub use graph::*;
pub use plugin::PluginContext;
pub use port_kind::{PortKind, PortValue};
