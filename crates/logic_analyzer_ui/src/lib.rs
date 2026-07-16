mod about;
mod app;
mod app_platform;
mod demo_signals;
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
    BINDINGS.get_or_init(|| {
        InputBindings::from_json(include_str!("../config/input_bindings.json"))
            .expect("application input binding configuration must be valid")
    })
}
