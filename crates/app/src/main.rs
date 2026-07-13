#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

#[cfg_attr(target_arch = "wasm32", path = "main_wasm.rs")]
#[cfg_attr(not(target_arch = "wasm32"), path = "main_native.rs")]
mod platform;

fn main() -> platform::MainResult {
    platform::run()
}
