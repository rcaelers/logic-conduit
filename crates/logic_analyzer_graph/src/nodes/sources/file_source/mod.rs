#[cfg_attr(target_arch = "wasm32", path = "builder_wasm.rs")]
mod builder;
mod definition;
mod registration;

#[cfg(all(test, not(target_arch = "wasm32")))]
pub(crate) use builder::FileSourceBuilder;
pub use definition::{DslFileSource, DslFileSourceState};
