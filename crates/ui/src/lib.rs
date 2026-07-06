#![cfg_attr(target_arch = "wasm32", allow(dead_code))]

mod app;
mod compile;
mod nodes;

pub use app::App;
