//! Platform-neutral trigger configuration for logic-analyzer sources.

use signal_processing::TriggerCountMode;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TriggerCondition {
    Ignore,
    Low,
    High,
    Rising,
    Falling,
    Either,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TriggerLogic {
    And,
    Or,
}

/// One stage of a portable logic trigger. Two planes accommodate analyzers
/// with parallel trigger match units; one-plane drivers reject plane1.
#[derive(Debug, Clone)]
pub struct LogicTriggerStage {
    pub plane0: [TriggerCondition; 16],
    pub plane1: [TriggerCondition; 16],
    pub logic: TriggerLogic,
    pub inverted: bool,
    pub count_mode: TriggerCountMode,
    pub count: u32,
}

impl Default for LogicTriggerStage {
    fn default() -> Self {
        Self {
            plane0: [TriggerCondition::Ignore; 16],
            plane1: [TriggerCondition::Ignore; 16],
            logic: TriggerLogic::And,
            inverted: false,
            count_mode: TriggerCountMode::Occurrences,
            count: 0,
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct LogicTrigger {
    pub stages: Vec<LogicTriggerStage>,
    pub serial: bool,
}
