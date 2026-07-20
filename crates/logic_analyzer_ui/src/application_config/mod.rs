mod implementation;

#[cfg(not(target_arch = "wasm32"))]
#[path = "native.rs"]
mod platform;
#[cfg(target_arch = "wasm32")]
#[path = "wasm.rs"]
mod platform;

pub(crate) use implementation::{ApplicationConfig, embedded_defaults};
pub(crate) use platform::load;
