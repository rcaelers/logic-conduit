//! Built-in graph-node runtime builder catalog.

use std::collections::HashMap;

use super::binary_decoder::BinaryDecoderBuilder;
use super::buffer::BufferBuilder;
use super::counter::CounterBuilder;
use super::formatter::FormatterBuilder;
use super::logic_gate::LogicGateBuilder;
use super::spi_decoder::SpiDecoderBuilder;
use super::sr_flip_flop::SrFlipFlopBuilder;
use super::tgck_recorder::TgckRecorderBuilder;
use super::uart_decoder::UartDecoderBuilder;
use super::uart_demo_source::UartDemoSourceBuilder;
use super::viewer::ViewerBuilder;
use super::word_matcher::WordMatcherBuilder;
use crate::compiler::RuntimeBuilder;

pub(crate) fn standard_builders() -> HashMap<String, Box<dyn RuntimeBuilder>> {
    let mut builders: HashMap<String, Box<dyn RuntimeBuilder>> = HashMap::new();

    std::cfg_select! {
        target_arch = "wasm32" => {}
        _ => {
            use super::{
                csv_writer::CsvWriterBuilder, file_source::FileSourceBuilder,
                file_writer::FileWriterBuilder, sigrok_file_source::SigrokFileSourceBuilder,
                text_file_writer::TextFileWriterBuilder,
            };

            builders.insert("DSL File Source".into(), Box::new(FileSourceBuilder));
            builders.insert("Sigrok File Source".into(), Box::new(SigrokFileSourceBuilder));
            builders.insert("File Writer".into(), Box::new(FileWriterBuilder));
            builders.insert("Text File Writer".into(), Box::new(TextFileWriterBuilder));
            builders.insert("CSV Writer".into(), Box::new(CsvWriterBuilder));
        }
    }

    builders.insert("UART Demo Source".into(), Box::new(UartDemoSourceBuilder));
    builders.insert("SPI Decoder".into(), Box::new(SpiDecoderBuilder));
    builders.insert("UART Decoder".into(), Box::new(UartDecoderBuilder));
    builders.insert("Binary Decoder".into(), Box::new(BinaryDecoderBuilder));
    builders.insert("Word Matcher".into(), Box::new(WordMatcherBuilder));
    builders.insert("SR Flip-Flop".into(), Box::new(SrFlipFlopBuilder));
    builders.insert("Logic Gate".into(), Box::new(LogicGateBuilder));
    builders.insert("Buffer".into(), Box::new(BufferBuilder));
    builders.insert("Counter".into(), Box::new(CounterBuilder));
    builders.insert("String Formatter".into(), Box::new(FormatterBuilder));
    builders.insert("TGCK Recorder".into(), Box::new(TgckRecorderBuilder));
    builders.insert("Viewer".into(), Box::new(ViewerBuilder));
    builders
}
