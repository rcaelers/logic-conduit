//! Native lowering from portable trigger metadata to the hardware contract.

use logic_analyzer_processing::nodes::sources::{
    LogicTrigger, LogicTriggerStage, TriggerCondition, TriggerLogic,
};
use signal_processing::{SimpleTriggerCondition, TriggerLogicOperator, TriggerPredicate};

use super::definition::U3Pro16State;
use super::trigger;

pub(crate) fn lower_program(state: &U3Pro16State) -> Result<LogicTrigger, String> {
    trigger::validate_program(state, state.trigger_program())?;
    let Some(program) = state.trigger_program() else {
        return Ok(LogicTrigger::default());
    };
    let stages = program
        .stages
        .iter()
        .map(|stage| {
            let mut lowered = LogicTriggerStage::default();
            for predicate in &stage.predicates {
                let TriggerPredicate::Digital { channel, condition } = predicate else {
                    unreachable!("validated U3Pro16 schemas contain only digital predicates");
                };
                let physical_channel = channel
                    .as_str()
                    .strip_prefix("u3pro16:input:")
                    .and_then(|channel| channel.parse::<usize>().ok())
                    .ok_or_else(|| format!("unknown U3Pro16 input {channel}"))?;
                lowered.plane0[physical_channel] = match condition {
                    SimpleTriggerCondition::Ignore => TriggerCondition::Ignore,
                    SimpleTriggerCondition::Low => TriggerCondition::Low,
                    SimpleTriggerCondition::High => TriggerCondition::High,
                    SimpleTriggerCondition::Rising => TriggerCondition::Rising,
                    SimpleTriggerCondition::Falling => TriggerCondition::Falling,
                    SimpleTriggerCondition::Either => TriggerCondition::Either,
                };
            }
            lowered.logic = match stage.logic {
                TriggerLogicOperator::And => TriggerLogic::And,
                TriggerLogicOperator::Or => TriggerLogic::Or,
                _ => unreachable!("validated U3Pro16 schemas contain only AND/OR logic"),
            };
            lowered.inverted = stage.inverted;
            if let Some(count) = stage.count {
                lowered.count = u32::try_from(count.value)
                    .map_err(|_| "U3Pro16 trigger count exceeds the hardware range")?;
                lowered.count_mode = count.mode;
            }
            Ok(lowered)
        })
        .collect::<Result<Vec<_>, String>>()?;
    Ok(LogicTrigger {
        stages,
        serial: false,
    })
}

#[cfg(test)]
mod tests {
    use logic_analyzer_processing::nodes::sources::{TriggerCondition, TriggerLogic};
    use signal_processing::{
        SimpleTriggerCondition, TriggerCount, TriggerCountMode, TriggerLogicOperator,
        TriggerPredicate, TriggerProgram, TriggerStage,
    };

    use super::super::definition::U3PRO16_CHANNELS;
    use super::*;

    fn advanced_program() -> TriggerProgram {
        TriggerProgram::new(
            trigger::schema().id().clone(),
            trigger::schema().revision(),
            vec![
                TriggerStage {
                    predicates: vec![
                        TriggerPredicate::Digital {
                            channel: trigger::physical_channel_id(3),
                            condition: SimpleTriggerCondition::Rising,
                        },
                        TriggerPredicate::Digital {
                            channel: trigger::physical_channel_id(7),
                            condition: SimpleTriggerCondition::High,
                        },
                    ],
                    logic: TriggerLogicOperator::And,
                    inverted: true,
                    count: Some(TriggerCount {
                        mode: TriggerCountMode::Occurrences,
                        value: 3,
                    }),
                },
                TriggerStage {
                    predicates: vec![TriggerPredicate::Digital {
                        channel: trigger::physical_channel_id(1),
                        condition: SimpleTriggerCondition::Falling,
                    }],
                    logic: TriggerLogicOperator::Or,
                    inverted: false,
                    count: Some(TriggerCount {
                        mode: TriggerCountMode::Consecutive,
                        value: 5,
                    }),
                },
            ],
        )
    }

    #[test]
    fn lowering_matches_the_documented_hardware_subset() {
        let mut state = U3Pro16State::default();
        state.set_trigger_program(Some(advanced_program())).unwrap();
        let lowered = lower_program(&state).unwrap();

        assert_eq!(lowered.stages.len(), 2);
        assert_eq!(lowered.stages[0].plane0[3], TriggerCondition::Rising);
        assert_eq!(lowered.stages[0].plane0[7], TriggerCondition::High);
        assert_eq!(lowered.stages[0].logic, TriggerLogic::And);
        assert!(lowered.stages[0].inverted);
        assert_eq!(lowered.stages[0].count_mode, TriggerCountMode::Occurrences);
        assert_eq!(lowered.stages[0].count, 3);
        assert_eq!(lowered.stages[1].plane0[1], TriggerCondition::Falling);
        assert_eq!(lowered.stages[1].logic, TriggerLogic::Or);
        assert_eq!(lowered.stages[1].count_mode, TriggerCountMode::Consecutive);
        assert_eq!(lowered.stages[1].count, 5);
        assert!(
            lowered
                .stages
                .iter()
                .all(|stage| stage.plane1 == [TriggerCondition::Ignore; U3PRO16_CHANNELS])
        );
    }
}
