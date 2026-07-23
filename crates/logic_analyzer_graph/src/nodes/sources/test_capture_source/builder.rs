//! Runtime builder for the in-memory mixed-protocol test capture.

use serde_json::Value;

use logic_analyzer_processing::nodes::sources::synthetic_capture_source::SyntheticCaptureSource;
use node_graph::Socket;
use signal_processing::{ProcessNode, Sample, SampleBlock};

use crate::{
    CapturePresentation, CapturePresentationSignal, CompileCtx, PortKind, ResolvedInputs,
    RuntimeBuilder,
};

#[derive(Default)]
pub(crate) struct TestCaptureSourceBuilder;

impl RuntimeBuilder for TestCaptureSourceBuilder {
    fn is_source(&self) -> bool {
        true
    }

    fn accepted_kinds(&self, _socket: &Socket, _state: &Value) -> Vec<PortKind> {
        vec![]
    }

    fn offered_kinds(&self, _socket: &Socket, _state: &Value) -> Vec<PortKind> {
        vec![PortKind::of::<SampleBlock>(), PortKind::of::<Sample>()]
    }

    fn input_port(
        &self,
        _socket: &Socket,
        _member_index: usize,
        _state: &Value,
        _kind: PortKind,
    ) -> Option<String> {
        None
    }

    fn output_port(&self, socket: &Socket, _state: &Value, kind: PortKind) -> Option<String> {
        if kind == PortKind::of::<SampleBlock>() {
            Some(format!("block{}", socket.def_index))
        } else if kind == PortKind::of::<Sample>() {
            Some(format!("ch{}", socket.def_index))
        } else {
            None
        }
    }

    fn viewer_channel_origin(&self, socket: &Socket, _state: &Value) -> Option<usize> {
        Some(socket.def_index)
    }

    fn capture_presentation(&self, _state: &Value) -> Result<Option<CapturePresentation>, String> {
        let channels = SyntheticCaptureSource::preview_channels();
        let signals = (0..=8)
            .chain(std::iter::once(10))
            .map(|index| {
                let samples = &channels[index];
                CapturePresentationSignal {
                    index,
                    name: format!("Ch {index}"),
                    initial: samples.first().is_some_and(|sample| sample.value),
                    transitions: samples
                        .iter()
                        .skip(1)
                        .map(|sample| (sample.start_time_ns as f64 / 1_000.0, sample.value))
                        .collect(),
                }
            })
            .collect::<Vec<_>>();
        let duration_us = signals
            .iter()
            .flat_map(|signal| signal.transitions.last().map(|(time, _)| *time))
            .fold(1.0_f64, f64::max);
        Ok(Some(CapturePresentation::InMemory {
            signals,
            duration_us,
        }))
    }

    fn input_required(&self, _socket: &Socket, _state: &Value) -> bool {
        false
    }

    fn build(
        &self,
        name: &str,
        _state: &Value,
        _resolved: &ResolvedInputs,
        _ctx: &mut CompileCtx,
    ) -> Result<Box<dyn ProcessNode>, String> {
        Ok(Box::new(SyntheticCaptureSource::new().with_name(name)))
    }
}
