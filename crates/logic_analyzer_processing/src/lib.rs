//! Concrete, UI-independent logic-analyzer processing nodes.

pub mod live_capture;
pub mod nodes;

pub use live_capture::{
    AcquisitionContext, AcquisitionError, AcquisitionOutcome, AcquisitionResult,
    CaptureAnalysisChannel, CaptureAnalysisSource, LogicCaptureEvent, PreparedAcquisition,
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
pub use nodes::sources::{DemoCaptureSource, UartDemoSource};

std::cfg_select! {
    target_arch = "wasm32" => {}
    _ => {
        mod capture_export;

        pub use capture_export::{
            CaptureExportError, CaptureExportFormatDescriptor, CaptureExportObserver,
            CaptureExportProgress, CaptureExportReport, CaptureExportRequest,
            CaptureExportWarning, DerivedExportSupport, IgnoreCaptureExportProgress,
            RawCaptureExportFormat, TriggerMetadataSupport, export_finalized_capture,
        };
        pub use live_capture::{
            BufferedFakeConfig, BufferedFakeController, BufferedFakeProvider,
            DeterministicFakeConfig, DeterministicFakeController, DeterministicFakeProvider,
            DeterministicTrigger, DeterministicTriggerCount, DeterministicTriggerCountMode,
            DeterministicTriggerLogic, DeterministicTriggerPredicate, DeterministicTriggerStage,
            DsLogicU3Pro16BufferedProvider, DsLogicU3Pro16StreamingProvider,
        };
        pub use nodes::sources::{
            CaptureMode, ClockEdge, ClockSource, DeferredDslFileSource, DsLogicCapturePlan,
            DsLogicTriggerHeader, DsLogicU3Pro16, DsLogicU3Pro16Source, DslCaptureReader,
            DslChunkedCaptureReader,
            DslFileCaptureDataSource, DslFileSource, LinkSpeed, LogicAnalyzer,
            LogicAnalyzerError, LogicAnalyzerInfo, LogicAnalyzerResult, LogicAnalyzerSource,
            LogicCaptureConfig, LogicChunk, LogicEncoding, LogicEncodingRequest, LogicTrigger,
            LogicTriggerStage, RusbTransport, SigrokCaptureReader, SigrokChunkedCaptureReader,
            SigrokFileCaptureDataSource, SigrokFileSource, TriggerCondition, TriggerLogic,
            UsbError, UsbTransport, open_dsl_chunked_capture, open_dsl_chunked_capture_with_progress,
            open_sigrok_chunked_capture, u3pro16_buffered_plan, u3pro16_streaming_plan,
        };
        pub use nodes::sinks::{
            BinaryFileWriter, CsvValueFormat, CsvWordWriter, TextFileWriter, WriteWidth,
        };
    }
}
