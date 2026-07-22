//! Parallel bus decoder and its platform-specific execution backend.

#[cfg(not(target_arch = "wasm32"))]
#[path = "native.rs"]
mod implementation;
#[cfg(target_arch = "wasm32")]
#[path = "wasm.rs"]
mod implementation;
mod types;

pub use implementation::ParallelDecoder;
pub use types::{ParallelInputStrategy, StrobeMode};
