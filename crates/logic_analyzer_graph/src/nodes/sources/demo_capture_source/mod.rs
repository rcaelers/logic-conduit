mod builder;
mod definition;
#[cfg(not(target_arch = "wasm32"))]
mod implementation;
#[cfg(not(target_arch = "wasm32"))]
mod live_builder;
#[cfg(not(target_arch = "wasm32"))]
mod live_capture;
mod trigger;

pub(crate) use builder::DemoCaptureSourceBuilder;
pub use definition::{DemoCaptureSource, DemoCaptureSourceState, DemoLiveCaptureSource};
#[cfg(not(target_arch = "wasm32"))]
pub(crate) use live_builder::DemoLiveCaptureSourceBuilder;
