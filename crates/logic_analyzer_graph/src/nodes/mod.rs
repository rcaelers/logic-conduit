//! Concrete graph nodes and their registry infrastructure.

mod catalog;
#[cfg(all(test, not(target_arch = "wasm32")))]
mod platform_registration_tests;
#[cfg(all(test, target_arch = "wasm32"))]
mod platform_registration_web_tests;
mod registration;
mod registry;

pub mod decoders;
pub mod logic;
pub mod sinks;
pub mod sources;

pub(crate) use catalog::standard_builders;
pub use decoders::{
    BinaryDecoder, BinaryDecoderState, I2cDecoder, SpiDecoder, SpiDecoderMetadata, SpiDecoderState,
    UartDecoder, UartDecoderState,
};
pub use logic::{
    Buffer, BufferState, Counter, CounterState, LogicGate, LogicGateState, SrFlipFlop,
    SrFlipFlopState, StringFormatter, StringFormatterState, WordMatcher, WordMatcherState,
};
pub use registration::GraphNodeRegistration;
pub(crate) use registration::graph_node_registrations;
#[cfg(all(test, not(target_arch = "wasm32")))]
pub(crate) use registry::test_graphs_tests;
pub use registry::{
    Number, Signal, Text, TextOpenPath, TextSavePath, Trigger, Words, build_registry,
};
pub use sinks::{
    CsvWriter, CsvWriterState, FileWriter, FileWriterState, TextFileWriter, TgckRecorder, Viewer,
    ViewerState,
};
pub use sources::{
    CaptureDurationValue, DsLogicU3Pro16, DslFileSource, DslFileSourceState, SigrokFileSource,
    SigrokFileSourceState, U3Pro16Metadata, U3Pro16State,
};
#[cfg(any(test, feature = "test-support"))]
pub use sources::{
    TestCaptureSource, TestCaptureSourceState, TestLiveCaptureSource, TestUartSource,
    TestUartSourceState,
};
