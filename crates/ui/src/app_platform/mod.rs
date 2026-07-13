//! Platform-specific application/document state.

#[cfg_attr(target_arch = "wasm32", path = "wasm.rs")]
#[cfg_attr(not(target_arch = "wasm32"), path = "native.rs")]
mod imp;

pub(crate) use imp::PlatformState;
#[cfg(target_os = "macos")]
pub(crate) use imp::notify_recent_files_changed;
#[cfg(not(target_arch = "wasm32"))]
pub(crate) use imp::{FileCommand, GuardedAction};
#[cfg(target_os = "macos")]
pub use imp::{NativeMenuCommand, dispatch_native_menu_command, set_recent_files_listener};
