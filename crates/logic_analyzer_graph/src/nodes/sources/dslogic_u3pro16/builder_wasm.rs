//! Browser runtime builder for the DSLogic U3Pro16 graph source.

use serde_json::Value;

use logic_analyzer_processing::SyntheticCaptureSource;
use node_graph::Socket;
use signal_processing::{ProcessNode, Sample, SampleBlock, ViewerRetention};

use super::definition::U3Pro16State;
use crate::{
    CapturePresentation, CompileCtx, PortKind, ResolvedInputs, RuntimeBuilder,
    TriggerConfigurationFeature, parse_state,
};

pub(crate) struct DsLogicU3Pro16Builder;

impl RuntimeBuilder for DsLogicU3Pro16Builder {
    fn is_source(&self) -> bool {
        true
    }

    fn viewer_retention(&self, _state: &Value) -> ViewerRetention {
        ViewerRetention::Unlimited
    }

    fn accepted_kinds(&self, _socket: &Socket, _state: &Value) -> Vec<PortKind> {
        Vec::new()
    }

    fn offered_kinds(&self, _socket: &Socket, _state: &Value) -> Vec<PortKind> {
        vec![PortKind::of::<SampleBlock>(), PortKind::of::<Sample>()]
    }

    fn input_port(&self, _: &Socket, _: usize, _: &Value, _: PortKind) -> Option<String> {
        None
    }

    fn output_port(&self, socket: &Socket, state: &Value, kind: PortKind) -> Option<String> {
        if kind != PortKind::of::<Sample>() && kind != PortKind::of::<SampleBlock>() {
            return None;
        }
        let state: U3Pro16State = parse_state(state).ok()?;
        if !state
            .channels
            .enabled
            .get(socket.def_index)
            .copied()
            .unwrap_or(false)
        {
            return None;
        }
        let logical_channel = state.channels.enabled[..socket.def_index]
            .iter()
            .filter(|enabled| **enabled)
            .count();
        if kind == PortKind::of::<SampleBlock>() {
            Some(format!("block{logical_channel}"))
        } else {
            Some(format!("ch{logical_channel}"))
        }
    }

    fn viewer_channel_origin(&self, socket: &Socket, state: &Value) -> Option<usize> {
        let state: U3Pro16State = parse_state(state).ok()?;
        state
            .channels
            .enabled
            .get(socket.def_index)
            .copied()
            .unwrap_or(false)
            .then(|| {
                state.channels.enabled[..socket.def_index]
                    .iter()
                    .filter(|enabled| **enabled)
                    .count()
            })
    }

    fn capture_presentation(&self, state: &Value) -> Result<Option<CapturePresentation>, String> {
        let state: U3Pro16State = parse_state(state)?;
        let names = state
            .channels
            .enabled
            .iter()
            .enumerate()
            .filter(|(_, enabled)| **enabled)
            .map(|(channel, _)| format!("Ch {channel}"));
        Ok(Some(
            super::super::synthetic_presentation::capture_presentation(names),
        ))
    }

    fn trigger_configuration(
        &self,
        state: &Value,
    ) -> Result<Option<TriggerConfigurationFeature>, String> {
        let state: U3Pro16State = parse_state(state)?;
        super::trigger::configuration(&state).map(Some)
    }

    fn build(
        &self,
        name: &str,
        state: &Value,
        _resolved: &ResolvedInputs,
        _ctx: &mut CompileCtx,
    ) -> Result<Box<dyn ProcessNode>, String> {
        let state: U3Pro16State = parse_state(state)?;
        let channels = state
            .channels
            .enabled
            .iter()
            .filter(|enabled| **enabled)
            .count()
            .max(1);
        Ok(Box::new(
            SyntheticCaptureSource::new()
                .with_channel_count(channels)
                .with_name(name),
        ))
    }
}
