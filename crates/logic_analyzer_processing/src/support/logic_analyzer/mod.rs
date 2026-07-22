//! Shared logic-analyzer driver and source-adaptation support.

mod implementation;
mod trigger;

pub use implementation::{
    CaptureMode, ClockEdge, ClockSource, LogicAnalyzerError, LogicAnalyzerResult,
    LogicCaptureConfig, LogicEncodingRequest,
};
pub(crate) use implementation::{
    LogicAnalyzer, LogicAnalyzerInfo, LogicAnalyzerSource, LogicChunk, LogicEncoding,
};
pub use trigger::{LogicTrigger, LogicTriggerStage, TriggerCondition, TriggerLogic};
