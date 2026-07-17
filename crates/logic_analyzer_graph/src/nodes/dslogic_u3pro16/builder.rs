//! Native runtime builder for the DSLogic U3Pro16 graph source.

use serde_json::Value;

use logic_analyzer_processing::{
    CaptureMode, ClockEdge, ClockSource, DsLogicU3Pro16, LogicCaptureConfig,
    LogicEncodingRequest, LogicTrigger,
};
use node_graph::Socket;
use signal_processing::{ProcessNode, Sample, SampleBlock, ViewerRetention};

use crate::compiler::{CompileCtx, PortKind, ResolvedInputs, RuntimeBuilder, parse_state};
use crate::nodes::U3Pro16State;

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

    fn build(
        &self,
        name: &str,
        state: &Value,
        _resolved: &ResolvedInputs,
        _ctx: &mut CompileCtx,
    ) -> Result<Box<dyn ProcessNode>, String> {
        let state: U3Pro16State = parse_state(state)?;
        let sample_rate_hz = state
            .sample_rate
            .selected()
            .strip_suffix(" GHz")
            .and_then(|value| value.parse::<u64>().ok())
            .map(|value| value * 1_000_000_000)
            .or_else(|| {
                state
                    .sample_rate
                    .selected()
                    .strip_suffix(" MHz")
                    .and_then(|value| value.parse::<u64>().ok())
                    .map(|value| value * 1_000_000)
            })
            .ok_or_else(|| "invalid U3Pro16 sample rate".to_string())?;
        let input_mask = state
            .channels
            .enabled
            .iter()
            .enumerate()
            .fold(0_u64, |mask, (index, enabled)| {
                if *enabled { mask | (1_u64 << index) } else { mask }
            });
        let duration_ms = u64::try_from(state.duration_ms.value.max(1)).unwrap_or(1);
        let config = LogicCaptureConfig {
            mode: if state.mode.selected() == "Stream" {
                CaptureMode::Streaming
            } else {
                CaptureMode::Finite
            },
            sample_rate_hz,
            input_mask,
            sample_limit: sample_rate_hz.saturating_mul(duration_ms).div_ceil(1_000),
            trigger_percent: 50,
            threshold_volts: Some(state.threshold.value),
            trigger: LogicTrigger::default(),
            encoding: if state.rle.value {
                LogicEncodingRequest::RunLength
            } else {
                LogicEncodingRequest::Raw
            },
            clock: if state.ext_clock.value {
                ClockSource::External {
                    edge: if state.clock_edge.selected() == "Falling" {
                        ClockEdge::Falling
                    } else {
                        ClockEdge::Rising
                    },
                }
            } else {
                ClockSource::Internal
            },
            input_filter: state.filter.value,
        };
        let source = DsLogicU3Pro16::open_first()
            .map_err(|error| error.to_string())?
            .into_source(config)
            .map_err(|error| error.to_string())?
            .with_name(name);
        Ok(Box::new(source))
    }
}
