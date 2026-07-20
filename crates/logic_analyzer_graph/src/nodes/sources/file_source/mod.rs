#[cfg(not(target_arch = "wasm32"))]
mod builder;
mod definition;

#[cfg(not(target_arch = "wasm32"))]
pub(crate) use builder::FileSourceBuilder;
pub use definition::{DslFileSource, DslFileSourceState};
