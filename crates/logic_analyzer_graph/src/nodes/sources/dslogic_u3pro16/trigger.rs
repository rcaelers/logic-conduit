use std::collections::BTreeMap;

use logic_analyzer_processing::{LogicTrigger, LogicTriggerStage, TriggerCondition, TriggerLogic};
use signal_processing::{
    CaptureChannelId, SimpleTriggerCondition, TriggerCountCapabilities, TriggerCountMode,
    TriggerEditorSchema, TriggerIdentifier, TriggerLogicOperator, TriggerPredicate, TriggerProgram,
};

use super::definition::{U3PRO16_CHANNELS, U3Pro16State};
use crate::{SimpleTriggerChannel, TriggerConfigurationFeature};

const SCHEMA_ID: &str = "dsl.dslogic-u3pro16.trigger";

pub(crate) fn schema() -> TriggerEditorSchema {
    TriggerEditorSchema::new(
        TriggerIdentifier::new(SCHEMA_ID).expect("static trigger schema ID is valid"),
        1,
        16,
        U3PRO16_CHANNELS,
        vec![TriggerLogicOperator::And, TriggerLogicOperator::Or],
    )
    .expect("U3Pro16 trigger schema is valid")
    .with_digital_conditions(vec![
        SimpleTriggerCondition::Low,
        SimpleTriggerCondition::High,
        SimpleTriggerCondition::Rising,
        SimpleTriggerCondition::Falling,
        SimpleTriggerCondition::Either,
    ])
    .expect("U3Pro16 trigger conditions are valid")
    .with_stage_inversion(true)
    .with_count(
        TriggerCountCapabilities::new(
            vec![TriggerCountMode::Occurrences, TriggerCountMode::Consecutive],
            1,
            i32::MAX as u64,
            1,
        )
        .expect("U3Pro16 trigger count capabilities are valid"),
    )
}

pub(crate) fn physical_channel_id(channel: usize) -> CaptureChannelId {
    CaptureChannelId::new(format!("u3pro16:input:{channel}"))
}

fn enabled_channel_ids(state: &U3Pro16State) -> Vec<CaptureChannelId> {
    state
        .channels
        .enabled
        .iter()
        .copied()
        .enumerate()
        .filter(|(_, enabled)| *enabled)
        .map(|(channel, _)| physical_channel_id(channel))
        .collect()
}

pub(crate) fn program_from_conditions(
    conditions: &[SimpleTriggerCondition],
    enabled: &[bool],
) -> Result<Option<TriggerProgram>, String> {
    schema().simple_program(
        conditions
            .iter()
            .copied()
            .enumerate()
            .filter(|(channel, _)| enabled.get(*channel).copied().unwrap_or(false))
            .map(|(channel, condition)| (physical_channel_id(channel), condition)),
    )
}

pub(crate) fn conditions(state: &U3Pro16State) -> Result<Vec<SimpleTriggerCondition>, String> {
    validate_program(state, state.trigger_program())?;
    let mut conditions = BTreeMap::new();
    if let Some(stage) = state
        .trigger_program()
        .and_then(|program| program.stages.first())
    {
        for predicate in &stage.predicates {
            let TriggerPredicate::Digital { channel, condition } = predicate else {
                unreachable!("validated U3Pro16 schemas contain only digital predicates");
            };
            conditions.insert(channel.clone(), *condition);
        }
    }
    Ok((0..U3PRO16_CHANNELS)
        .map(|channel| {
            conditions
                .get(&physical_channel_id(channel))
                .copied()
                .unwrap_or(SimpleTriggerCondition::Ignore)
        })
        .collect())
}

pub(crate) fn validate_program(
    state: &U3Pro16State,
    program: Option<&TriggerProgram>,
) -> Result<(), String> {
    if let Some(program) = program {
        schema()
            .validate_program(program, &enabled_channel_ids(state))
            .map_err(|error| error.to_string())?;
    }
    Ok(())
}

pub(crate) fn set_condition(
    state: &U3Pro16State,
    physical_channel: usize,
    condition: SimpleTriggerCondition,
) -> Result<Option<TriggerProgram>, String> {
    if !state
        .channels
        .enabled
        .get(physical_channel)
        .copied()
        .unwrap_or(false)
    {
        return Err(format!("U3Pro16 input {physical_channel} is not enabled"));
    }
    let channels = enabled_channel_ids(state);
    schema()
        .with_simple_condition(
            state.trigger_program(),
            &channels,
            &physical_channel_id(physical_channel),
            condition,
        )
        .map_err(|error| error.to_string())
}

pub(crate) fn retain_enabled_conditions(
    state: &U3Pro16State,
) -> Result<Option<TriggerProgram>, String> {
    let all_channels: Vec<_> = (0..U3PRO16_CHANNELS).map(physical_channel_id).collect();
    let Some(mut program) = state.trigger_program().cloned() else {
        return Ok(None);
    };
    schema()
        .validate_program(&program, &all_channels)
        .map_err(|error| error.to_string())?;
    for stage in &mut program.stages {
        stage.predicates.retain(|predicate| {
            let TriggerPredicate::Digital { channel, .. } = predicate else {
                unreachable!("validated U3Pro16 schemas contain only digital predicates");
            };
            channel
                .as_str()
                .strip_prefix("u3pro16:input:")
                .and_then(|channel| channel.parse::<usize>().ok())
                .and_then(|channel| state.channels.enabled.get(channel))
                .copied()
                .unwrap_or(false)
        });
    }
    program.stages.retain(|stage| !stage.predicates.is_empty());
    if program.stages.is_empty() {
        return Ok(None);
    }
    validate_program(state, Some(&program))?;
    Ok(Some(program))
}

pub(crate) fn lower_program(state: &U3Pro16State) -> Result<LogicTrigger, String> {
    validate_program(state, state.trigger_program())?;
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

pub(crate) fn configuration(state: &U3Pro16State) -> Result<TriggerConfigurationFeature, String> {
    let conditions = conditions(state)?;
    let channels = state
        .channels
        .enabled
        .iter()
        .copied()
        .enumerate()
        .filter(|(_, enabled)| *enabled)
        .enumerate()
        .map(
            |(viewer_channel, (physical_channel, _))| SimpleTriggerChannel {
                channel_id: physical_channel_id(physical_channel),
                viewer_channel,
                name: format!("Ch {physical_channel}"),
                enabled: true,
                condition: conditions[physical_channel],
            },
        )
        .collect();
    TriggerConfigurationFeature::new(schema(), state.trigger_program().cloned(), channels)
}

#[cfg(test)]
mod tests {
    use signal_processing::{
        TriggerCount, TriggerCountMode, TriggerLogicOperator, TriggerPredicate, TriggerProgram,
        TriggerStage,
    };

    use super::*;

    fn advanced_program() -> TriggerProgram {
        TriggerProgram::new(
            schema().id().clone(),
            schema().revision(),
            vec![
                TriggerStage {
                    predicates: vec![
                        TriggerPredicate::Digital {
                            channel: physical_channel_id(3),
                            condition: SimpleTriggerCondition::Rising,
                        },
                        TriggerPredicate::Digital {
                            channel: physical_channel_id(7),
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
                        channel: physical_channel_id(1),
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
    fn schema_and_lowering_match_the_documented_hardware_subset() {
        let schema = schema();
        assert_eq!(schema.maximum_stages(), 16);
        assert_eq!(schema.maximum_predicates_per_stage(), 16);
        assert_eq!(
            schema.logic_operators(),
            [TriggerLogicOperator::And, TriggerLogicOperator::Or]
        );
        assert!(schema.supports_stage_inversion());
        assert_eq!(
            schema.count_capabilities().unwrap().modes(),
            [TriggerCountMode::Occurrences, TriggerCountMode::Consecutive]
        );

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

    #[test]
    fn owner_rejects_duplicate_stage_channels_and_removes_disabled_inputs() {
        let mut state = U3Pro16State::default();
        let mut duplicate = advanced_program();
        duplicate.stages[0].predicates[1] = duplicate.stages[0].predicates[0].clone();
        assert!(
            state
                .set_trigger_program(Some(duplicate))
                .unwrap_err()
                .contains("more than once")
        );

        state.set_trigger_program(Some(advanced_program())).unwrap();
        state.channels.enabled[3] = false;
        let retained = retain_enabled_conditions(&state).unwrap().unwrap();
        assert_eq!(retained.stages.len(), 2);
        assert_eq!(retained.stages[0].predicates.len(), 1);
        assert_eq!(retained.stages[1].predicates.len(), 1);
    }
}
