//! Concrete graph nodes and their registry infrastructure.

mod catalog;
mod registry;

pub mod decoders;
pub mod logic;
pub mod sinks;
pub mod sources;

#[cfg(not(target_arch = "wasm32"))]
#[path = "registry_native.rs"]
mod registry_platform;
#[cfg(target_arch = "wasm32")]
#[path = "registry_web.rs"]
mod registry_platform;

pub(crate) use catalog::standard_builders;
pub use decoders::{
    BinaryDecoder, BinaryDecoderState, I2cDecoder, SpiDecoder, SpiDecoderMetadata, SpiDecoderState,
    UartDecoder, UartDecoderState,
};
pub use logic::{
    Buffer, BufferState, Counter, CounterState, LogicGate, LogicGateState, SrFlipFlop,
    SrFlipFlopState, StringFormatter, StringFormatterState, WordMatcher, WordMatcherState,
};
#[cfg(all(test, not(target_arch = "wasm32")))]
pub(crate) use registry::test_graphs_tests;
pub use registry::{
    Number, Signal, Text, TextOpenPath, TextSavePath, Trigger, Words, build_registry,
};
#[cfg(not(target_arch = "wasm32"))]
pub use sinks::{CsvWriter, CsvWriterState, TextFileWriter};
pub use sinks::{FileWriter, FileWriterState, TgckRecorder, Viewer, ViewerState};
#[cfg(not(target_arch = "wasm32"))]
pub use sources::{
    CaptureDurationValue, DsLogicU3Pro16, SigrokFileSource, SigrokFileSourceState, U3Pro16Metadata,
    U3Pro16State,
};
pub use sources::{
    DemoCaptureSource, DemoCaptureSourceState, DslFileSource, DslFileSourceState, UartDemoSource,
    UartDemoSourceState,
};
