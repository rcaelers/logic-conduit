//! Browser runtime builder for `Sigrok File Source`.

use serde_json::Value;

use logic_analyzer_processing::nodes::sources::synthetic_capture_source::SyntheticCaptureSource;
use node_graph::Socket;
use signal_processing::{ProcessNode, Sample, SampleBlock};

use crate::{
    CapturePresentation, CapturePresentationSignal, CompileCtx, PortKind, ResolvedInputs,
    RuntimeBuilder, parse_state,
};

#[derive(Default)]
pub(crate) struct SigrokFileSourceBuilder;

impl RuntimeBuilder for SigrokFileSourceBuilder {
    fn is_source(&self) -> bool {
        true
    }

    fn accepted_kinds(&self, _socket: &Socket, _state: &Value) -> Vec<PortKind> {
        Vec::new()
    }

    fn offered_kinds(&self, _socket: &Socket, _state: &Value) -> Vec<PortKind> {
        vec![PortKind::of::<SampleBlock>(), PortKind::of::<Sample>()]
    }

    fn input_port(&self, _socket: &Socket, _: usize, _: &Value, _: PortKind) -> Option<String> {
        None
    }

    fn output_port(&self, socket: &Socket, _state: &Value, kind: PortKind) -> Option<String> {
        if kind == PortKind::of::<SampleBlock>() {
            Some(format!("block{}", socket.def_index))
        } else {
            (kind == PortKind::of::<Sample>()).then(|| format!("ch{}", socket.def_index))
        }
    }

    fn viewer_channel_origin(&self, socket: &Socket, _state: &Value) -> Option<usize> {
        Some(socket.def_index)
    }

    fn capture_presentation(&self, state: &Value) -> Result<Option<CapturePresentation>, String> {
        let state: super::definition::SigrokFileSourceState = parse_state(state)?;
        let channels = state.channels.value.clamp(1, 32) as usize;
        if state.demo_data {
            let channels = SyntheticCaptureSource::preview_channels_with_count(channels);
            let signals = channels
                .iter()
                .enumerate()
                .filter(|(index, _)| *index != 9)
                .map(|(index, samples)| CapturePresentationSignal {
                    index,
                    name: format!("Ch {index}"),
                    initial: samples.first().is_some_and(|sample| sample.value),
                    transitions: samples
                        .iter()
                        .skip(1)
                        .map(|sample| (sample.start_time_ns as f64 / 1_000.0, sample.value))
                        .collect(),
                })
                .collect::<Vec<_>>();
            let duration_us = signals
                .iter()
                .filter_map(|signal| signal.transitions.last().map(|(time, _)| *time))
                .fold(1.0_f64, f64::max);
            return Ok(Some(CapturePresentation::InMemory {
                signals,
                duration_us,
            }));
        }
        Ok(Some(
            super::super::synthetic_presentation::capture_presentation(
                (0..channels).map(|channel| format!("Ch {channel}")),
            ),
        ))
    }

    fn input_required(&self, _socket: &Socket, _state: &Value) -> bool {
        false
    }

    fn build(
        &self,
        name: &str,
        state: &Value,
        _resolved: &ResolvedInputs,
        _ctx: &mut CompileCtx,
    ) -> Result<Box<dyn ProcessNode>, String> {
        let state: super::definition::SigrokFileSourceState = parse_state(state)?;
        Ok(Box::new(
            SyntheticCaptureSource::new()
                .with_channel_count(state.channels.value.clamp(1, 32) as usize)
                .with_name(name),
        ))
    }
}
