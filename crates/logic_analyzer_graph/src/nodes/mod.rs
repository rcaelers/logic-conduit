mod binary_decoder;
mod buffer;
mod catalog;
mod counter;
mod demo_capture_source;
mod file_source;
mod file_writer;
mod formatter;
mod i2c_decoder;
mod logic_gate;
mod preview;
mod registry;
mod spi_decoder;
mod sr_flip_flop;
mod tgck_recorder;
mod uart_decoder;
mod uart_demo_source;
mod viewer;
mod word_matcher;

#[cfg(not(target_arch = "wasm32"))]
mod csv_writer;
#[cfg(not(target_arch = "wasm32"))]
mod dslogic_u3pro16;
#[cfg(not(target_arch = "wasm32"))]
#[path = "file_source/builder.rs"]
mod file_source_builder;
#[cfg(not(target_arch = "wasm32"))]
#[path = "file_writer/builder.rs"]
mod file_writer_builder;
#[cfg(not(target_arch = "wasm32"))]
#[path = "registry_native.rs"]
mod registry_platform;
#[cfg(target_arch = "wasm32")]
#[path = "registry_web.rs"]
mod registry_platform;
#[cfg(not(target_arch = "wasm32"))]
mod sigrok_file_source;
#[cfg(not(target_arch = "wasm32"))]
mod text_file_writer;

#[cfg(test)]
pub(crate) use binary_decoder::BinaryDecoderBuilder;
pub use binary_decoder::{BinaryDecoder, BinaryDecoderState};
pub use buffer::{Buffer, BufferState};
pub(crate) use catalog::standard_builders;
pub use counter::{Counter, CounterState};
#[cfg(not(target_arch = "wasm32"))]
pub use csv_writer::{CsvWriter, CsvWriterState};
#[cfg(test)]
pub(crate) use demo_capture_source::DemoCaptureSourceBuilder;
pub use demo_capture_source::{CapturePreviewSignal, DemoCaptureSource, DemoCaptureSourceState};
#[cfg(not(target_arch = "wasm32"))]
pub use dslogic_u3pro16::{DsLogicU3Pro16, U3Pro16Metadata, U3Pro16State};
pub use file_source::{DslFileSource, DslFileSourceState};
#[cfg(not(target_arch = "wasm32"))]
#[allow(unused_imports)]
pub(crate) use file_source_builder::FileSourceBuilder;
pub use file_writer::{FileWriter, FileWriterState};
pub use formatter::{StringFormatter, StringFormatterState};
pub use i2c_decoder::I2cDecoder;
pub use logic_gate::{LogicGate, LogicGateState};
pub use preview::capture_preview;
#[cfg(test)]
pub(crate) use registry::test_graphs_tests;
pub use registry::{
    Number, Signal, Text, TextOpenPath, TextSavePath, Trigger, Words, build_registry,
};
#[cfg(not(target_arch = "wasm32"))]
pub use registry_platform::{CaptureFileSource, capture_file_source};
#[cfg(not(target_arch = "wasm32"))]
pub use sigrok_file_source::{SigrokFileSource, SigrokFileSourceState};
pub use spi_decoder::{SpiDecoder, SpiDecoderMetadata, SpiDecoderState};
pub use sr_flip_flop::{SrFlipFlop, SrFlipFlopState};
#[cfg(not(target_arch = "wasm32"))]
pub use text_file_writer::TextFileWriter;
pub use tgck_recorder::TgckRecorder;
pub(crate) use uart_decoder::selected_baud_rate;
pub use uart_decoder::{UartDecoder, UartDecoderState};
pub use uart_demo_source::{UartDemoSource, UartDemoSourceState};
pub use viewer::{Viewer, ViewerState};
pub use word_matcher::{WordMatcher, WordMatcherState};
