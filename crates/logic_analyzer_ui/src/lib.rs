mod about;
mod app;
mod app_platform;
mod demo_signals;
mod toast;

pub use app::App;
#[cfg(target_os = "macos")]
pub use app_platform::{
    NativeMenuCommand, dispatch_native_menu_command, set_recent_files_listener,
};
