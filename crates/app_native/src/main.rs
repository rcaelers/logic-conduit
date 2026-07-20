#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

std::cfg_select! {
    target_arch = "wasm32" => {
        fn main() {}
    }
    _ => {
        #[cfg(target_os = "macos")]
        mod macos_menu;
        mod native;

        fn main() -> native::MainResult {
            native::run()
        }
    }
}
