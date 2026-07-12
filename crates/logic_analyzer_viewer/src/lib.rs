#![cfg_attr(target_arch = "wasm32", allow(dead_code))]

mod channel;
mod cursor;
mod draw;
mod format;
#[cfg(not(target_arch = "wasm32"))]
mod indexed_annotations;
mod input;
mod sampling;
mod types;
mod viewer;
#[cfg(not(target_arch = "wasm32"))]
mod worker;

pub use viewer::{ChannelSignal, LogicAnalyzerViewer};
