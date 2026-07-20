//! Platform-neutral application runtime manager facade.

#[cfg(not(target_arch = "wasm32"))]
mod native;
#[cfg(target_arch = "wasm32")]
mod wasm;

#[cfg(not(target_arch = "wasm32"))]
pub use native::AppManager;
#[cfg(target_arch = "wasm32")]
pub use wasm::AppManager;
