//! DSLogic U3Pro16 source node, USB driver, and acquisition profiles.

mod buffered;
mod common;
mod implementation;
mod source;
mod streaming;

pub use buffered::DsLogicU3Pro16BufferedProvider;
pub use implementation::{DsLogicCapturePlan, LinkSpeed};
pub use source::DsLogicU3Pro16Source;
pub use streaming::DsLogicU3Pro16StreamingProvider;

pub use crate::support::logic_analyzer::{
    CaptureMode, ClockEdge, ClockSource, LogicAnalyzerError, LogicAnalyzerResult,
    LogicCaptureConfig, LogicEncodingRequest, LogicTrigger, LogicTriggerStage, TriggerCondition,
    TriggerLogic,
};
