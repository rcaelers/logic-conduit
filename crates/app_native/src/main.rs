#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

std::cfg_select! {
    target_arch = "wasm32" => {
        fn main() {}
    }
    _ => {
        mod native;

        fn main() -> native::MainResult {
            native::run()
        }
    }
}
