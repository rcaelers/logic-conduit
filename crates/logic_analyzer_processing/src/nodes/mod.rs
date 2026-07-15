//! Concrete processing nodes used by logic-analyzer graphs.

pub mod decoders;
pub mod logic;
pub mod sinks;
mod uart_demo_source;

pub use uart_demo_source::UartDemoSource;

std::cfg_select! {
    target_arch = "wasm32" => {}
    _ => {
        mod dsl_file;
        mod dslogic_u3pro16;
        mod logic_analyzer;
        mod sigrok_file;

        pub use dsl_file::{
            DeferredDslFileSource, DslCaptureReader, DslChunkedCaptureReader,
            DslFileCaptureDataSource, DslFileSource, open_dsl_chunked_capture,
            open_dsl_chunked_capture_with_progress,
        };
        pub use dslogic_u3pro16::{DsLogicU3Pro16, LinkSpeed, RusbTransport, UsbTransport};
        pub use logic_analyzer::{
            CaptureMode, ClockEdge, ClockSource, DsLogicU3Pro16Source, LogicAnalyzer,
            LogicAnalyzerError, LogicAnalyzerInfo, LogicAnalyzerResult, LogicAnalyzerSource,
            LogicCaptureConfig, LogicChunk, LogicEncoding, LogicEncodingRequest, LogicTrigger,
            LogicTriggerStage, TriggerCondition, TriggerLogic,
        };
        pub use sigrok_file::{
            SigrokCaptureReader, SigrokChunkedCaptureReader, SigrokFileCaptureDataSource,
            SigrokFileSource, open_sigrok_chunked_capture,
        };
    }
}
