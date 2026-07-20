//! Concrete output and viewer graph nodes.

mod file_writer;
mod tgck_recorder;
mod viewer;

#[cfg(not(target_arch = "wasm32"))]
mod csv_writer;
#[cfg(not(target_arch = "wasm32"))]
mod text_file_writer;

#[cfg(not(target_arch = "wasm32"))]
pub(crate) use csv_writer::CsvWriterBuilder;
#[cfg(not(target_arch = "wasm32"))]
pub use csv_writer::{CsvWriter, CsvWriterState};
#[cfg(not(target_arch = "wasm32"))]
pub(crate) use file_writer::FileWriterBuilder;
pub use file_writer::{FileWriter, FileWriterState};
#[cfg(not(target_arch = "wasm32"))]
pub use text_file_writer::TextFileWriter;
#[cfg(not(target_arch = "wasm32"))]
pub(crate) use text_file_writer::TextFileWriterBuilder;
pub use tgck_recorder::TgckRecorder;
pub(crate) use tgck_recorder::TgckRecorderBuilder;
pub(crate) use viewer::ViewerBuilder;
pub use viewer::{Viewer, ViewerState};
