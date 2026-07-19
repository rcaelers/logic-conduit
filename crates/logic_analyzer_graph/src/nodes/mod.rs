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
mod registry;
mod spi_decoder;
mod sr_flip_flop;
mod tgck_recorder;
mod uart_decoder;
mod uart_demo_source;
mod viewer;
mod word_matcher;

std::cfg_select! {
    target_arch = "wasm32" => {
        #[path = "registry_web.rs"]
        mod registry_platform;
    }
    _ => {
        mod csv_writer;
        mod dslogic_u3pro16;
        #[path = "file_source/builder.rs"]
        mod file_source_builder;
        #[path = "file_writer/builder.rs"]
        mod file_writer_builder;
        #[path = "registry_native.rs"]
        mod registry_platform;
        mod sigrok_file_source;
        mod text_file_writer;

        pub use csv_writer::{CsvWriter, CsvWriterState};
        pub use dslogic_u3pro16::{DsLogicU3Pro16, U3Pro16State};
        #[allow(unused_imports)]
        pub(crate) use file_source_builder::FileSourceBuilder;
        pub use registry_platform::{CaptureFileSource, capture_file_source};
        pub use sigrok_file_source::{SigrokFileSource, SigrokFileSourceState};
        pub use text_file_writer::TextFileWriter;
    }
}

#[cfg(test)]
pub(crate) use binary_decoder::BinaryDecoderBuilder;
pub use binary_decoder::{BinaryDecoder, BinaryDecoderState};
pub use buffer::{Buffer, BufferState};
pub(crate) use catalog::standard_builders;
pub use counter::{Counter, CounterState};
#[cfg(test)]
pub(crate) use demo_capture_source::DemoCaptureSourceBuilder;
pub use demo_capture_source::{CapturePreviewSignal, DemoCaptureSource, DemoCaptureSourceState};
pub use file_source::{DslFileSource, DslFileSourceState};
pub use file_writer::{FileWriter, FileWriterState};
pub use formatter::{StringFormatter, StringFormatterState};
pub use i2c_decoder::I2cDecoder;
pub use logic_gate::{LogicGate, LogicGateState};
pub use registry::*;
pub use spi_decoder::{SpiDecoder, SpiDecoderState};
pub use sr_flip_flop::{SrFlipFlop, SrFlipFlopState};
pub use tgck_recorder::TgckRecorder;
pub(crate) use uart_decoder::selected_baud_rate;
pub use uart_decoder::{UartDecoder, UartDecoderState};
pub use uart_demo_source::{UartDemoSource, UartDemoSourceState};
pub use viewer::{Viewer, ViewerState};
pub use word_matcher::{WordMatcher, WordMatcherState};

/// Finds an in-memory raw-capture preview supplied by a concrete source node.
pub fn capture_preview(
    graph: &node_graph::GraphState,
) -> Option<(node_graph::NodeId, Vec<CapturePreviewSignal>)> {
    graph.nodes.iter().find_map(|(&id, node)| {
        demo_capture_source::capture_preview(node).map(|preview| (id, preview))
    })
}
