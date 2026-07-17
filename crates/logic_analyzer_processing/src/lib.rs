//! Concrete, UI-independent logic-analyzer processing nodes.

pub mod live_capture;
pub mod nodes;

pub use live_capture::{
    AcquisitionContext, AcquisitionError, AcquisitionOutcome, AcquisitionResult, LogicCaptureEvent,
    PreparedAcquisition,
};
pub use nodes::decoders::{
    CsPolarity, ParallelDecoder, ParallelDecoderMetrics, ParallelDecoderMetricsSnapshot,
    ParallelInputStrategy, SpiDecoder, SpiMode, StrobeMode,
};
pub use nodes::logic::{
    BufferNode, GateOp, LogicGate, MatchOp, SrLatch, TextFormatter, TriggerAt, TriggerCounter,
    WordMatcher,
};
pub use nodes::sinks::TgckRecorder;
pub use nodes::{DemoCaptureSource, UartDemoSource};

std::cfg_select! {
    target_arch = "wasm32" => {}
    _ => {
        pub use live_capture::{
            DeterministicFakeConfig, DeterministicFakeController, DeterministicFakeProvider,
        };
        pub use nodes::{
            CaptureMode, ClockEdge, ClockSource, DeferredDslFileSource, DsLogicU3Pro16,
            DsLogicU3Pro16Source, DslCaptureReader, DslChunkedCaptureReader,
            DslFileCaptureDataSource, DslFileSource, LinkSpeed, LogicAnalyzer,
            LogicAnalyzerError, LogicAnalyzerInfo, LogicAnalyzerResult, LogicAnalyzerSource,
            LogicCaptureConfig, LogicChunk, LogicEncoding, LogicEncodingRequest, LogicTrigger,
            LogicTriggerStage, RusbTransport, SigrokCaptureReader, SigrokChunkedCaptureReader,
            SigrokFileCaptureDataSource, SigrokFileSource, TriggerCondition, TriggerLogic,
            UsbTransport, open_dsl_chunked_capture, open_dsl_chunked_capture_with_progress,
            open_sigrok_chunked_capture,
        };
        pub use nodes::sinks::{
            BinaryFileWriter, CsvValueFormat, CsvWordWriter, TextFileWriter, WriteWidth,
        };
    }
}
