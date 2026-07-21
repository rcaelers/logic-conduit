//! Test-only deterministic capture graph nodes.

mod builder;
mod definition;
#[cfg(not(target_arch = "wasm32"))]
mod implementation;
#[cfg_attr(target_arch = "wasm32", path = "live_builder_wasm.rs")]
mod live_builder;
#[cfg(not(target_arch = "wasm32"))]
mod live_capture;
mod trigger;

pub(crate) use builder::TestCaptureSourceBuilder;
pub use definition::{TestCaptureSource, TestCaptureSourceState, TestLiveCaptureSource};
pub(crate) use live_builder::TestLiveCaptureSourceBuilder;
