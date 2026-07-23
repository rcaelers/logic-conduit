//! Test-only deterministic capture graph nodes.

mod builder;
mod definition;
#[cfg(not(target_arch = "wasm32"))]
mod implementation;
#[cfg_attr(target_arch = "wasm32", path = "live_builder_wasm.rs")]
mod live_builder;
#[cfg(not(target_arch = "wasm32"))]
mod live_capture;
mod registration;
mod trigger;

#[cfg(all(test, not(target_arch = "wasm32")))]
pub(crate) use builder::TestCaptureSourceBuilder;
#[cfg(any(test, feature = "test-support"))]
pub(crate) use definition::{TestCaptureSource, TestCaptureSourceState, TestLiveCaptureSource};
