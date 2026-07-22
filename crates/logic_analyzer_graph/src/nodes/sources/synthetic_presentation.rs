//! Shared browser presentation for concrete synthetic source stand-ins.

use logic_analyzer_processing::nodes::sources::synthetic_capture_source::SyntheticCaptureSource;

use crate::{CapturePresentation, CapturePresentationSignal};

pub(crate) fn capture_presentation(
    channel_names: impl IntoIterator<Item = String>,
) -> CapturePresentation {
    let channel_names = channel_names.into_iter().collect::<Vec<_>>();
    let channels = SyntheticCaptureSource::preview_channels_with_count(channel_names.len());
    let signals = channel_names
        .into_iter()
        .enumerate()
        .map(|(index, name)| {
            let samples = &channels[index];
            CapturePresentationSignal {
                index,
                name,
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
        .filter_map(|signal| signal.transitions.last().map(|(time, _)| *time))
        .fold(1.0_f64, f64::max);
    CapturePresentation::InMemory {
        signals,
        duration_us,
    }
}
