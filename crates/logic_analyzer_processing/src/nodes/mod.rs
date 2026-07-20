//! Concrete processing nodes used by logic-analyzer graphs.

pub mod decoders;
mod demo_capture_source;
pub mod logic;
pub mod sinks;
mod uart_demo_source;

#[cfg(not(target_arch = "wasm32"))]
mod capture_archive;
#[cfg(not(target_arch = "wasm32"))]
mod dsl_file;
#[cfg(not(target_arch = "wasm32"))]
mod dslogic_u3pro16;
#[cfg(not(target_arch = "wasm32"))]
mod logic_analyzer;
#[cfg(not(target_arch = "wasm32"))]
mod sigrok_file;

pub use demo_capture_source::DemoCaptureSource;
#[cfg(not(target_arch = "wasm32"))]
pub use dsl_file::{
    DeferredDslFileSource, DslCaptureReader, DslChunkedCaptureReader, DslFileCaptureDataSource,
    DslFileSource, open_dsl_chunked_capture, open_dsl_chunked_capture_with_progress,
};
#[cfg(not(target_arch = "wasm32"))]
pub use dslogic_u3pro16::{
    DsLogicCapturePlan, DsLogicTriggerHeader, DsLogicU3Pro16, DsLogicU3Pro16Source, LinkSpeed,
    RusbTransport, UsbError, UsbTransport, u3pro16_buffered_plan, u3pro16_streaming_plan,
};
#[cfg(not(target_arch = "wasm32"))]
pub use logic_analyzer::{
    CaptureMode, ClockEdge, ClockSource, LogicAnalyzer, LogicAnalyzerError, LogicAnalyzerInfo,
    LogicAnalyzerResult, LogicAnalyzerSource, LogicCaptureConfig, LogicChunk, LogicEncoding,
    LogicEncodingRequest, LogicTrigger, LogicTriggerStage, TriggerCondition, TriggerLogic,
};
#[cfg(not(target_arch = "wasm32"))]
pub use sigrok_file::{
    SigrokCaptureReader, SigrokChunkedCaptureReader, SigrokFileCaptureDataSource, SigrokFileSource,
    open_sigrok_chunked_capture,
};
pub use uart_demo_source::UartDemoSource;
