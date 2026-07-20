#[cfg(not(target_arch = "wasm32"))]
mod builder;
mod definition;

#[cfg(not(target_arch = "wasm32"))]
pub(crate) use builder::FileWriterBuilder;
pub use definition::{FileWriter, FileWriterState};
