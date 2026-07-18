use signal_processing::{
    CaptureChannelId, SimpleTriggerCondition, TriggerEditorSchema, TriggerIdentifier,
    TriggerLogicOperator, TriggerProgram, TriggerProgramForm,
};

use crate::compiler::{SimpleTriggerChannel, TriggerConfigurationFeature};

use super::definition::U3PRO16_CHANNELS;
use super::U3Pro16State;

const SCHEMA_ID: &str = "dsl.dslogic-u3pro16.trigger";

pub(super) fn schema() -> TriggerEditorSchema {
    TriggerEditorSchema::new(
        TriggerIdentifier::new(SCHEMA_ID).expect("static trigger schema ID is valid"),
        1,
        1,
        U3PRO16_CHANNELS,
        vec![TriggerLogicOperator::And],
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
}

pub(super) fn physical_channel_id(channel: usize) -> CaptureChannelId {
    CaptureChannelId::new(format!("u3pro16:input:{channel}"))
}

pub(super) fn enabled_channel_ids(state: &U3Pro16State) -> Vec<CaptureChannelId> {
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

pub(super) fn program_from_conditions(
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

pub(super) fn conditions(
    state: &U3Pro16State,
) -> Result<Vec<SimpleTriggerCondition>, String> {
    let channels = enabled_channel_ids(state);
    let form = schema()
        .program_form(state.trigger_program(), &channels)
        .map_err(|error| error.to_string())?;
    let conditions = match form {
        TriggerProgramForm::FreeRun => Default::default(),
        TriggerProgramForm::CommonDigital(conditions) => conditions,
        TriggerProgramForm::Advanced => {
            return Err("U3Pro16 advanced-trigger execution is not available".into());
        }
    };
    Ok((0..U3PRO16_CHANNELS)
        .map(|channel| {
            conditions
                .get(&physical_channel_id(channel))
                .copied()
                .unwrap_or(SimpleTriggerCondition::Ignore)
        })
        .collect())
}

pub(super) fn validate_program(
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

pub(super) fn set_condition(
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

pub(super) fn retain_enabled_conditions(
    state: &U3Pro16State,
) -> Result<Option<TriggerProgram>, String> {
    let all_channels: Vec<_> = (0..U3PRO16_CHANNELS).map(physical_channel_id).collect();
    let form = schema()
        .program_form(state.trigger_program(), &all_channels)
        .map_err(|error| error.to_string())?;
    let TriggerProgramForm::CommonDigital(mut conditions) = form else {
        return Ok(state.trigger_program().cloned());
    };
    conditions.retain(|channel, _| {
        channel
            .as_str()
            .strip_prefix("u3pro16:input:")
            .and_then(|channel| channel.parse::<usize>().ok())
            .and_then(|channel| state.channels.enabled.get(channel))
            .copied()
            .unwrap_or(false)
    });
    schema().simple_program(conditions)
}

pub(super) fn configuration(
    state: &U3Pro16State,
) -> Result<TriggerConfigurationFeature, String> {
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
