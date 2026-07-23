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

#[cfg(any(test, feature = "test-support"))]
pub(crate) use definition::{DsLogicU3Pro16, U3Pro16State};
