//! Concrete processing nodes used by logic-analyzer graphs.

pub mod decoders;
pub mod logic;
pub mod sinks;
pub mod sources;

#[cfg(not(target_arch = "wasm32"))]
pub use sources::{
    CaptureMode, ClockEdge, ClockSource, DeferredDslFileSource, DsLogicCapturePlan,
    DsLogicTriggerHeader, DsLogicU3Pro16, DsLogicU3Pro16Source, DslCaptureReader,
    DslChunkedCaptureReader, DslFileCaptureDataSource, DslFileSource, LinkSpeed, LogicAnalyzer,
    LogicAnalyzerError, LogicAnalyzerInfo, LogicAnalyzerResult, LogicAnalyzerSource,
    LogicCaptureConfig, LogicChunk, LogicEncoding, LogicEncodingRequest, LogicTrigger,
    LogicTriggerStage, RusbTransport, SigrokCaptureReader, SigrokChunkedCaptureReader,
    SigrokFileCaptureDataSource, SigrokFileSource, TriggerCondition, TriggerLogic, UsbError,
    UsbTransport, open_dsl_chunked_capture, open_dsl_chunked_capture_with_progress,
    open_sigrok_chunked_capture, u3pro16_buffered_plan, u3pro16_streaming_plan,
};
pub use sources::{SyntheticCaptureSource, SyntheticUartSource};
