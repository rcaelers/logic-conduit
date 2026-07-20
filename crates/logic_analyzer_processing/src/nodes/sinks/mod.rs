//! Sink processing nodes that persist data.

mod tgck_recorder;

#[cfg(not(target_arch = "wasm32"))]
mod binary_file_writer;
#[cfg(not(target_arch = "wasm32"))]
mod csv_word_writer;
#[cfg(not(target_arch = "wasm32"))]
mod text_file_writer;

#[cfg(not(target_arch = "wasm32"))]
pub use binary_file_writer::{BinaryFileWriter, WriteWidth};
#[cfg(not(target_arch = "wasm32"))]
pub use csv_word_writer::{CsvValueFormat, CsvWordWriter};
#[cfg(not(target_arch = "wasm32"))]
pub use text_file_writer::TextFileWriter;
pub use tgck_recorder::{TgckRecord, TgckRecorder};
