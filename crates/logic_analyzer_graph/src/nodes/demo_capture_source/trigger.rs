use signal_processing::{
    CaptureChannelId, SimpleTriggerCondition, TriggerCountCapabilities, TriggerCountMode,
    TriggerEditorSchema, TriggerIdentifier, TriggerLogicOperator, TriggerPredicate, TriggerProgram,
};

use super::DemoCaptureSourceState;
use super::definition::DEMO_CAPTURE_CHANNELS;
use crate::{SimpleTriggerChannel, TriggerConfigurationFeature};

const SCHEMA_ID: &str = "dsl.demo-capture.trigger";

pub(crate) fn schema() -> TriggerEditorSchema {
    TriggerEditorSchema::new(
        TriggerIdentifier::new(SCHEMA_ID).expect("static trigger schema ID is valid"),
        1,
        4,
        DEMO_CAPTURE_CHANNELS,
        vec![
            TriggerLogicOperator::And,
            TriggerLogicOperator::Or,
            TriggerLogicOperator::Xor,
            TriggerLogicOperator::Nand,
            TriggerLogicOperator::Nor,
        ],
    )
    .expect("demo trigger schema is valid")
    .with_digital_conditions(vec![
        SimpleTriggerCondition::Low,
        SimpleTriggerCondition::High,
        SimpleTriggerCondition::Rising,
        SimpleTriggerCondition::Falling,
        SimpleTriggerCondition::Either,
    ])
    .expect("demo trigger conditions are valid")
    .with_stage_inversion(true)
    .with_count(
        TriggerCountCapabilities::new(
            vec![TriggerCountMode::Occurrences, TriggerCountMode::Consecutive],
            1,
            1_000_000,
            1,
        )
        .expect("demo trigger count capabilities are valid"),
    )
}

pub(crate) fn channel_ids() -> Vec<CaptureChannelId> {
    (0..DEMO_CAPTURE_CHANNELS)
        .map(|channel| CaptureChannelId::new(format!("demo:{channel}")))
        .collect()
}

pub(crate) fn program_from_conditions(
    conditions: &[SimpleTriggerCondition],
) -> Result<Option<TriggerProgram>, String> {
    schema().simple_program(channel_ids().into_iter().zip(conditions.iter().copied()))
}

pub(crate) fn conditions(
    program: Option<&TriggerProgram>,
) -> Result<Vec<SimpleTriggerCondition>, String> {
    let channel_ids = channel_ids();
    validate_program(program)?;
    let mut conditions = std::collections::BTreeMap::new();
    if let Some(stage) = program.and_then(|program| program.stages.first()) {
        for predicate in &stage.predicates {
            let TriggerPredicate::Digital { channel, condition } = predicate else {
                unreachable!("validated demo schemas contain only digital predicates");
            };
            conditions.insert(channel.clone(), *condition);
        }
    }
    Ok(channel_ids
        .iter()
        .map(|channel| {
            conditions
                .get(channel)
                .copied()
                .unwrap_or(SimpleTriggerCondition::Ignore)
        })
        .collect())
}

pub(crate) fn validate_program(program: Option<&TriggerProgram>) -> Result<(), String> {
    if let Some(program) = program {
        schema()
            .validate_program(program, &channel_ids())
            .map_err(|error| error.to_string())?;
    }
    Ok(())
}

pub(crate) fn set_condition(
    program: Option<&TriggerProgram>,
    channel: usize,
    condition: SimpleTriggerCondition,
) -> Result<Option<TriggerProgram>, String> {
    let channels = channel_ids();
    let channel = channels.get(channel).ok_or_else(|| {
        format!("demo capture channel {channel} is outside 0..{DEMO_CAPTURE_CHANNELS}")
    })?;
    schema()
        .with_simple_condition(program, &channels, channel, condition)
        .map_err(|error| error.to_string())
}

pub(crate) fn configuration(
    state: &DemoCaptureSourceState,
) -> Result<TriggerConfigurationFeature, String> {
    let conditions = conditions(state.trigger_program())?;
    let channels = channel_ids()
        .into_iter()
        .zip(conditions)
        .enumerate()
        .map(
            |(viewer_channel, (channel_id, condition))| SimpleTriggerChannel {
                channel_id,
                viewer_channel,
                name: format!("D{viewer_channel}"),
                enabled: true,
                condition,
            },
        )
        .collect();
    TriggerConfigurationFeature::new(schema(), state.trigger_program().cloned(), channels)
}
