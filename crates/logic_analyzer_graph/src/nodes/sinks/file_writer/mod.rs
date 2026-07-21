#[cfg_attr(target_arch = "wasm32", path = "builder_wasm.rs")]
mod builder;
mod definition;

pub(crate) use builder::FileWriterBuilder;
pub use definition::{FileWriter, FileWriterState};
