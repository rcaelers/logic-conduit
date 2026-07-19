//! Platform-specific application/document state.

std::cfg_select! {
    target_arch = "wasm32" => {
        #[path = "wasm.rs"]
        mod imp;

        pub(crate) use imp::PlatformState;
    }
    _ => {
        #[path = "native.rs"]
        mod imp;

        pub(crate) use imp::{
            FileCommand, GuardedAction, PlatformState, capture_session_directory,
            derived_cache_directory,
        };
    }
}

#[cfg(target_os = "macos")]
pub(crate) use imp::notify_recent_files_changed;
#[cfg(target_os = "macos")]
pub use imp::{NativeMenuCommand, dispatch_native_menu_command, set_recent_files_listener};
