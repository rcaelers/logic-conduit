//! Sink processing nodes that persist data.

mod tgck_recorder;

pub use tgck_recorder::{TgckRecord, TgckRecorder};

std::cfg_select! {
    target_arch = "wasm32" => {}
    _ => {
        mod binary_file_writer;
        mod csv_word_writer;
        mod text_file_writer;

        pub use binary_file_writer::{BinaryFileWriter, WriteWidth};
        pub use csv_word_writer::{CsvValueFormat, CsvWordWriter};
        pub use text_file_writer::TextFileWriter;
    }
}
