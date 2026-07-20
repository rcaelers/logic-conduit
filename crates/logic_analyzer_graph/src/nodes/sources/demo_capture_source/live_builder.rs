//! Native deterministic live-capture builder used for capture development and tests.

use serde_json::Value;

use node_graph::Socket;
use signal_processing::{ProcessNode, SimpleTriggerCondition, TriggerPredicate, TriggerProgram};

use super::builder::DemoCaptureSourceBuilder;
use crate::{
    CapturePresentation, CompileCtx, LiveCaptureEdit, LiveCaptureFeature, PortKind, ResolvedInputs,
    RuntimeBuilder, SimpleTriggerChannel, TriggerConfigurationFeature,
};

pub(crate) struct DemoLiveCaptureSourceBuilder;

pub(crate) fn conditions(
    program: Option<&TriggerProgram>,
) -> Result<Vec<SimpleTriggerCondition>, String> {
    let channel_ids = super::trigger::channel_ids();
    super::trigger::validate_program(program)?;
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

fn configuration(
    state: &super::definition::DemoCaptureSourceState,
) -> Result<TriggerConfigurationFeature, String> {
    let conditions = conditions(state.trigger_program())?;
    let channels = super::trigger::channel_ids()
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
    TriggerConfigurationFeature::new(
        super::trigger::schema(),
        state.trigger_program().cloned(),
        channels,
    )
}

impl RuntimeBuilder for DemoLiveCaptureSourceBuilder {
    fn is_source(&self) -> bool {
        true
    }

    fn accepted_kinds(&self, socket: &Socket, state: &Value) -> Vec<PortKind> {
        DemoCaptureSourceBuilder.accepted_kinds(socket, state)
    }

    fn offered_kinds(&self, socket: &Socket, state: &Value) -> Vec<PortKind> {
        DemoCaptureSourceBuilder.offered_kinds(socket, state)
    }

    fn input_port(
        &self,
        socket: &Socket,
        member_index: usize,
        state: &Value,
        kind: PortKind,
    ) -> Option<String> {
        DemoCaptureSourceBuilder.input_port(socket, member_index, state, kind)
    }

    fn output_port(&self, socket: &Socket, state: &Value, kind: PortKind) -> Option<String> {
        DemoCaptureSourceBuilder.output_port(socket, state, kind)
    }

    fn viewer_channel_origin(&self, socket: &Socket, state: &Value) -> Option<usize> {
        DemoCaptureSourceBuilder.viewer_channel_origin(socket, state)
    }

    fn capture_presentation(&self, state: &Value) -> Result<Option<CapturePresentation>, String> {
        DemoCaptureSourceBuilder.capture_presentation(state)
    }

    fn live_capture_feature(
        &self,
        state: &Value,
    ) -> Result<Option<Box<dyn LiveCaptureFeature>>, String> {
        super::live_capture::feature(state)
    }

    fn trigger_configuration(
        &self,
        state: &Value,
    ) -> Result<Option<TriggerConfigurationFeature>, String> {
        let state =
            serde_json::from_value::<super::definition::DemoCaptureSourceState>(state.clone())
                .map_err(|error| format!("invalid demo capture state: {error}"))?;
        configuration(&state).map(Some)
    }

    fn apply_live_capture_edit(
        &self,
        state: &Value,
        edit: &LiveCaptureEdit,
    ) -> Result<Option<Value>, String> {
        super::implementation::apply_live_capture_edit(state, edit).map(Some)
    }

    fn input_required(&self, socket: &Socket, state: &Value) -> bool {
        DemoCaptureSourceBuilder.input_required(socket, state)
    }

    fn build(
        &self,
        name: &str,
        state: &Value,
        resolved: &ResolvedInputs,
        ctx: &mut CompileCtx,
    ) -> Result<Box<dyn ProcessNode>, String> {
        DemoCaptureSourceBuilder.build(name, state, resolved, ctx)
    }
}
