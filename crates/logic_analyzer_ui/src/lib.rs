mod about;
mod app;
mod app_platform;
mod demo_signals;
mod input_binding_config;
mod toast;

use std::sync::OnceLock;

pub use app::App;
#[cfg(target_os = "macos")]
pub use app_platform::{
    NativeMenuCommand, dispatch_native_menu_command, set_recent_files_listener,
};
use input_bindings::InputBindings;

pub fn application_input_bindings() -> &'static InputBindings {
    static BINDINGS: OnceLock<InputBindings> = OnceLock::new();
    BINDINGS.get_or_init(input_binding_config::load)
}

/// Standard per-user location for an optional input-binding override.
/// Returns `None` on platforms without filesystem configuration, such as web.
pub fn application_input_bindings_path() -> Option<std::path::PathBuf> {
    input_binding_config::path()
}
