//! Sink nodes — pipeline endpoints that persist or expose data

mod binary_file_writer;

pub use binary_file_writer::{BinaryFileWriter, WriteWidth};
