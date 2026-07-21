//! Concrete capture source nodes, formats, and device adapters.

mod synthetic_capture_source;
mod synthetic_uart_source;

#[cfg(not(target_arch = "wasm32"))]
mod buffered_fake;
#[cfg(not(target_arch = "wasm32"))]
mod capture_archive;
#[cfg(all(test, not(target_arch = "wasm32")))]
mod conformance_tests;
#[cfg(not(target_arch = "wasm32"))]
mod dsl_file;
#[cfg(not(target_arch = "wasm32"))]
mod dslogic_u3pro16;
#[cfg(not(target_arch = "wasm32"))]
mod logic_analyzer;
mod logic_trigger;
#[cfg(not(target_arch = "wasm32"))]
mod sigrok_file;

#[cfg(not(target_arch = "wasm32"))]
pub use buffered_fake::{BufferedFakeConfig, BufferedFakeController, BufferedFakeProvider};
#[cfg(not(target_arch = "wasm32"))]
pub use dsl_file::{
    DeferredDslFileSource, DslCaptureReader, DslChunkedCaptureReader, DslFileCaptureDataSource,
    DslFileSource, open_dsl_chunked_capture, open_dsl_chunked_capture_with_progress,
};
#[cfg(not(target_arch = "wasm32"))]
pub use dslogic_u3pro16::{
    DsLogicCapturePlan, DsLogicTriggerHeader, DsLogicU3Pro16, DsLogicU3Pro16BufferedProvider,
    DsLogicU3Pro16Source, DsLogicU3Pro16StreamingProvider, LinkSpeed, RusbTransport, UsbError,
    UsbTransport, u3pro16_buffered_plan, u3pro16_streaming_plan,
};
#[cfg(not(target_arch = "wasm32"))]
pub use logic_analyzer::{
    CaptureMode, ClockEdge, ClockSource, LogicAnalyzer, LogicAnalyzerError, LogicAnalyzerInfo,
    LogicAnalyzerResult, LogicAnalyzerSource, LogicCaptureConfig, LogicChunk, LogicEncoding,
    LogicEncodingRequest,
};
pub use logic_trigger::{LogicTrigger, LogicTriggerStage, TriggerCondition, TriggerLogic};
#[cfg(not(target_arch = "wasm32"))]
pub use sigrok_file::{
    SigrokCaptureReader, SigrokChunkedCaptureReader, SigrokFileCaptureDataSource, SigrokFileSource,
    open_sigrok_chunked_capture,
};
pub use synthetic_capture_source::SyntheticCaptureSource;
#[cfg(not(target_arch = "wasm32"))]
pub use synthetic_capture_source::{
    DeterministicFakeConfig, DeterministicFakeController, DeterministicFakeProvider,
    DeterministicTrigger, DeterministicTriggerCount, DeterministicTriggerCountMode,
    DeterministicTriggerLogic, DeterministicTriggerPredicate, DeterministicTriggerStage,
};
pub use synthetic_uart_source::SyntheticUartSource;
