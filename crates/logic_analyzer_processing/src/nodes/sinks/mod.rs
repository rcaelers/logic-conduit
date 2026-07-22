//! Data-persistence processing nodes.

#[cfg(not(target_arch = "wasm32"))]
pub mod binary_file_writer;
#[cfg(not(target_arch = "wasm32"))]
pub mod csv_word_writer;
pub mod discard_writer;
#[cfg(not(target_arch = "wasm32"))]
pub mod text_file_writer;
pub mod tgck_recorder;
