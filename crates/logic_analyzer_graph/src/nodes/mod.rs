//! Concrete graph nodes and their registry infrastructure.

mod catalog;
#[cfg(feature = "test-support")]
mod node_test_support;
#[cfg(all(test, not(target_arch = "wasm32")))]
mod platform_registration_tests;
#[cfg(all(test, target_arch = "wasm32"))]
mod platform_registration_web_tests;
mod registration;
mod registry;

pub(crate) mod decoders;
pub(crate) mod logic;
pub(crate) mod sinks;
pub(crate) mod sources;

pub(crate) use catalog::standard_builders;
#[cfg(test)]
pub(crate) use decoders::{
    BinaryDecoder, SpiDecoderMetadata, SpiDecoderState, UartDecoder, UartDecoderState,
};
#[cfg(test)]
pub(crate) use logic::{Buffer, Counter, StringFormatter, StringFormatterState, WordMatcherState};
#[cfg(feature = "test-support")]
pub use node_test_support::{
    configure_u3pro16_test_capture, dslogic_u3pro16_name, set_test_capture_trigger_condition,
    test_capture_source_name, test_live_capture_source_name,
};
pub use registration::GraphNodeRegistration;
pub(crate) use registration::{graph_node_registrations, validate_graph_node_payload_requirements};
#[cfg(all(test, not(target_arch = "wasm32")))]
pub(crate) use registry::test_graphs_tests;
pub use registry::{
    Number, Signal, Text, TextOpenPath, TextSavePath, Trigger, Words, build_registry,
};
#[cfg(test)]
pub(crate) use sinks::{FileWriterState, Viewer};
#[cfg(test)]
pub(crate) use sources::TestUartSource;
#[cfg(test)]
pub(crate) use sources::{DslFileSource, DslFileSourceState};
#[cfg(test)]
pub(crate) use sources::{TestCaptureSource, TestLiveCaptureSource};
