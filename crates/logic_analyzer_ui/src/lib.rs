mod about;
mod app;
mod app_platform;
mod application_config;
mod decoder_panel;
mod input_binding_config;
mod live_capture;
mod plugin;
mod plugin_panel;
mod product;
mod toast;

use std::sync::OnceLock;

pub use app::App;
#[cfg(target_os = "macos")]
pub use app_platform::{
    NativeMenuCommand, dispatch_native_menu_command, set_recent_files_listener,
};
use input_bindings::InputBindings;
pub use plugin::PluginContext;
pub use plugin_panel::{
    PluginPanel, PluginPanelContext, PluginPanelDescriptor, PluginPanelIcon, UiPanelRegistration,
};
pub use product::{APPLICATION_ID, APPLICATION_NAME};

pub fn application_input_bindings() -> &'static InputBindings {
    static BINDINGS: OnceLock<InputBindings> = OnceLock::new();
    BINDINGS.get_or_init(input_binding_config::load)
}
