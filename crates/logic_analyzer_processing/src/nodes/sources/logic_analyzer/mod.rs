//! Vendor-neutral logic-analyzer source-node adapter and configuration.

mod implementation;
mod trigger;

pub use implementation::{
    CaptureMode, ClockEdge, ClockSource, LogicAnalyzer, LogicAnalyzerError, LogicAnalyzerInfo,
    LogicAnalyzerResult, LogicAnalyzerSource, LogicCaptureConfig, LogicChunk, LogicEncoding,
    LogicEncodingRequest,
};
pub use trigger::{LogicTrigger, LogicTriggerStage, TriggerCondition, TriggerLogic};
