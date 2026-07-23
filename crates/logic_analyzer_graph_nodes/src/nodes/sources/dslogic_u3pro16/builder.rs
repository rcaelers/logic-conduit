//! Native runtime builder for the DSLogic U3Pro16 graph source.

use serde_json::Value;

use logic_analyzer_graph_api::node::{LiveCaptureFeature, RuntimeBuilder};
use logic_analyzer_graph_api::node_support::{
    CapturePresentation, LiveCaptureEdit, NodeBuildContext, PortKind, ResolvedInputs,
    TriggerConfigurationFeature, parse_state,
};
use logic_analyzer_processing::nodes::sources::dslogic_u3pro16::DsLogicU3Pro16Source;
use node_graph::Socket;
use signal_processing::{DerivedDataRetention, ProcessNode, Sample, SampleBlock};

use super::definition::U3Pro16State;

#[derive(Default)]
pub(crate) struct DsLogicU3Pro16Builder;

impl RuntimeBuilder for DsLogicU3Pro16Builder {
    fn is_source(&self) -> bool {
        true
    }

    fn derived_data_retention(&self, _state: &Value) -> DerivedDataRetention {
        DerivedDataRetention::Unlimited
    }

    fn accepted_kinds(&self, _socket: &Socket, _state: &Value) -> Vec<PortKind> {
        Vec::new()
    }

    fn offered_kinds(&self, _socket: &Socket, _state: &Value) -> Vec<PortKind> {
        vec![PortKind::of::<SampleBlock>(), PortKind::of::<Sample>()]
    }

    fn input_port(
        &self,
        _socket: &Socket,
        _member: usize,
        _state: &Value,
        _kind: PortKind,
    ) -> Option<String> {
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
        Some(format!("ch{logical_channel}"))
    }

    fn viewer_channel_origin(&self, socket: &Socket, state: &Value) -> Option<usize> {
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
        Some(
            state.channels.enabled[..socket.def_index]
                .iter()
                .filter(|enabled| **enabled)
                .count(),
        )
    }

    fn capture_presentation(&self, state: &Value) -> Result<Option<CapturePresentation>, String> {
        let state: U3Pro16State = parse_state(state)?;
        let channels = state
            .channels
            .enabled
            .iter()
            .enumerate()
            .filter(|(_, enabled)| **enabled)
            .enumerate()
            .map(|(viewer_channel, (physical_channel, _))| {
                (viewer_channel, format!("Ch {physical_channel}"))
            })
            .collect();
        Ok(Some(CapturePresentation::Channels(channels)))
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
        let state: U3Pro16State = parse_state(state)?;
        super::trigger::configuration(&state).map(Some)
    }

    fn apply_live_capture_edit(
        &self,
        state: &Value,
        edit: &LiveCaptureEdit,
    ) -> Result<Option<Value>, String> {
        super::implementation::apply_live_capture_edit(state, edit).map(Some)
    }

    fn build(
        &self,
        name: &str,
        state: &Value,
        _resolved: &ResolvedInputs,
        _ctx: &mut dyn NodeBuildContext,
    ) -> Result<Box<dyn ProcessNode>, String> {
        let state: U3Pro16State = parse_state(state)?;
        let config = super::implementation::capture_config(&state)?;
        let source = DsLogicU3Pro16Source::open_first(config)
            .map_err(|error| error.to_string())?
            .with_name(name);
        Ok(Box::new(source))
    }
}
