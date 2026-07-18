use std::sync::Arc;

use serde_json::Value;

use logic_analyzer_processing::{
    AcquisitionContext, AcquisitionResult, CaptureAnalysisChannel, CaptureAnalysisSource,
    DeterministicFakeConfig, DeterministicFakeProvider, PreparedAcquisition,
};
use signal_processing::{CaptureChannelId, CaptureStoreCursor, ProcessNode};

use crate::compiler::{CaptureGraphSourceFactory, LiveCaptureFeature};
use crate::nodes::DemoCaptureSourceState;

const CHUNK_SAMPLES: u64 = 4_096;
const CHUNK_COUNT: usize = 64;
const SAMPLE_RATE_HZ: f64 = 1_000_000.0;

struct DemoCaptureGraphSourceFactory {
    channels: Arc<[CaptureChannelId]>,
}

impl CaptureGraphSourceFactory for DemoCaptureGraphSourceFactory {
    fn create(
        &self,
        cursor: Box<dyn CaptureStoreCursor>,
    ) -> Result<Box<dyn ProcessNode>, String> {
        let channels = self
            .channels
            .iter()
            .cloned()
            .enumerate()
            .map(|(index, channel)| {
                CaptureAnalysisChannel::separate(
                    channel,
                    format!("ch{index}"),
                    format!("block{index}"),
                )
            })
            .collect();
        CaptureAnalysisSource::new("demo-capture-analysis", cursor, SAMPLE_RATE_HZ, channels)
            .map(|source| Box::new(source) as Box<dyn ProcessNode>)
    }
}

struct DemoLiveCaptureFeature {
    channels: Arc<[CaptureChannelId]>,
    channel_names: Arc<[String]>,
    provider: DeterministicFakeProvider,
}

impl LiveCaptureFeature for DemoLiveCaptureFeature {
    fn channels(&self) -> &[CaptureChannelId] {
        &self.channels
    }

    fn channel_names(&self) -> &[String] {
        &self.channel_names
    }

    fn sample_rate_hz(&self) -> f64 {
        SAMPLE_RATE_HZ
    }

    fn graph_source_factory(&self) -> Arc<dyn CaptureGraphSourceFactory> {
        Arc::new(DemoCaptureGraphSourceFactory {
            channels: Arc::clone(&self.channels),
        })
    }

    fn prepare(
        self: Box<Self>,
        context: AcquisitionContext,
    ) -> AcquisitionResult<Box<dyn PreparedAcquisition>> {
        self.provider.prepare(context)
    }
}

pub(super) fn feature(
    state: &Value,
) -> Result<Option<Box<dyn LiveCaptureFeature>>, String> {
    serde_json::from_value::<DemoCaptureSourceState>(state.clone())
        .map_err(|error| format!("invalid demo capture state: {error}"))?;
    let channels: Arc<[CaptureChannelId]> = (0..11)
        .map(|channel| CaptureChannelId::new(format!("demo:{channel}")))
        .collect::<Vec<_>>()
        .into();
    let channel_names: Arc<[String]> = (0..11)
        .map(|channel| format!("D{channel}"))
        .collect::<Vec<_>>()
        .into();
    let config = DeterministicFakeConfig::new(
        Arc::clone(&channels),
        vec![CHUNK_SAMPLES; CHUNK_COUNT],
        0x5a17_d3a0,
    )
    .map_err(|error| error.to_string())?;
    Ok(Some(Box::new(DemoLiveCaptureFeature {
        channels,
        channel_names,
        provider: DeterministicFakeProvider::new(config),
    })))
}
