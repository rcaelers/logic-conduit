mod definition;

pub use definition::FileWriter;
#[cfg(not(target_arch = "wasm32"))]
mod builder;
#[cfg(not(target_arch = "wasm32"))]
pub(crate) use builder::FileWriterBuilder;
#[cfg(not(target_arch = "wasm32"))]
pub use definition::FileWriterState;
