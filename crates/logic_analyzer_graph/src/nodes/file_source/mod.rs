mod definition;

pub use definition::DslFileSource;
#[cfg(not(target_arch = "wasm32"))]
mod builder;
#[cfg(not(target_arch = "wasm32"))]
pub(crate) use builder::FileSourceBuilder;
#[cfg(not(target_arch = "wasm32"))]
pub use definition::DslFileSourceState;
