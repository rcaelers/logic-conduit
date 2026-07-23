#[cfg_attr(target_arch = "wasm32", path = "builder_wasm.rs")]
mod builder;
mod definition;
#[cfg(not(target_arch = "wasm32"))]
mod implementation;
#[cfg(not(target_arch = "wasm32"))]
mod live_capture;
mod registration;
mod trigger;
#[cfg(not(target_arch = "wasm32"))]
mod trigger_lowering;

pub use definition::{CaptureDurationValue, DsLogicU3Pro16, U3Pro16Metadata, U3Pro16State};
