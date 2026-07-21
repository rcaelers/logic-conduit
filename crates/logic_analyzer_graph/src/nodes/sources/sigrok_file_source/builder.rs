//! Runtime builder for `Sigrok File Source`.

use serde_json::Value;

use logic_analyzer_processing::{SigrokFileSource, SyntheticCaptureSource};
use node_graph::Socket;
use signal_processing::{
    CaptureIndexBuildProgress, CaptureIndexFactory, IndexSampler, ProcessNode, Sample, SampleBlock,
};

use crate::{
    CapturePresentation, CapturePresentationSignal, CompileCtx, PortKind, ResolvedInputs,
    RuntimeBuilder, parse_state,
};

struct SigrokCaptureIndexFactory {
    path: std::path::PathBuf,
}

impl CaptureIndexFactory for SigrokCaptureIndexFactory {
    fn display_name(&self) -> String {
        self.path.display().to_string()
    }

    fn open(
        self: Box<Self>,
        progress: &mut dyn FnMut(CaptureIndexBuildProgress),
    ) -> signal_processing::Result<Box<dyn signal_processing::CaptureIndex + Send>> {
        let source = logic_analyzer_processing::SigrokFileCaptureDataSource::open(&self.path)
            .map_err(|error| signal_processing::Error::ParseError(error.to_string()))?;
        IndexSampler::open_data_source_with_progress(source, |value| {
            progress(CaptureIndexBuildProgress {
                completed: value.completed_roots,
                total: value.total_roots,
            });
        })
        .map(|index| Box::new(index) as Box<dyn signal_processing::CaptureIndex + Send>)
    }
}

pub(crate) struct SigrokFileSourceBuilder;

impl RuntimeBuilder for SigrokFileSourceBuilder {
    fn is_source(&self) -> bool {
        true
    }
    fn accepted_kinds(&self, _socket: &Socket, _state: &Value) -> Vec<PortKind> {
        vec![]
    }
    fn offered_kinds(&self, _socket: &Socket, state: &Value) -> Vec<PortKind> {
        let demo_data = parse_state::<super::definition::SigrokFileSourceState>(state)
            .map(|state| state.demo_data)
            .unwrap_or(false);
        if demo_data {
            vec![PortKind::of::<SampleBlock>(), PortKind::of::<Sample>()]
        } else {
            vec![PortKind::of::<Sample>()]
        }
    }
    fn input_port(&self, _socket: &Socket, _: usize, _: &Value, _: PortKind) -> Option<String> {
        None
    }
    fn output_port(&self, socket: &Socket, state: &Value, kind: PortKind) -> Option<String> {
        let demo_data = parse_state::<super::definition::SigrokFileSourceState>(state)
            .map(|state| state.demo_data)
            .unwrap_or(false);
        if demo_data && kind == PortKind::of::<SampleBlock>() {
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
        if state.demo_data {
            let channels = SyntheticCaptureSource::preview_channels_with_count(
                state.channels.value.clamp(1, 32) as usize,
            );
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
        let path = std::path::PathBuf::from(state.file.value);
        if path.as_os_str().is_empty() {
            return Ok(None);
        }
        Ok(Some(CapturePresentation::Indexed {
            identity: path.clone(),
            factory: Box::new(SigrokCaptureIndexFactory { path }),
        }))
    }
    fn input_required(&self, socket: &Socket, state: &Value) -> bool {
        socket.def_index == 0
            && parse_state::<super::definition::SigrokFileSourceState>(state)
                .map(|state| !state.demo_data && state.file.value.trim().is_empty())
                .unwrap_or(true)
    }
    fn build(
        &self,
        name: &str,
        state: &Value,
        _resolved: &ResolvedInputs,
        _ctx: &mut CompileCtx,
    ) -> Result<Box<dyn ProcessNode>, String> {
        let state: super::definition::SigrokFileSourceState = parse_state(state)?;
        if state.demo_data {
            return Ok(Box::new(
                SyntheticCaptureSource::new()
                    .with_channel_count(state.channels.value.clamp(1, 32) as usize)
                    .with_name(name),
            ));
        }
        SigrokFileSource::new(&state.file.value, state.channels.value.clamp(1, 32) as u8)
            .map(|source| Box::new(source.with_name(name)) as Box<dyn ProcessNode>)
            .map_err(|error| format!("cannot open '{}': {error}", state.file.value))
    }
}
