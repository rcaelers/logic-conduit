use std::sync::Arc;

use serde_json::Value;

use logic_analyzer_processing::{
    AcquisitionContext, AcquisitionResult, CaptureAnalysisChannel, CaptureAnalysisSource,
    DeterministicFakeConfig, DeterministicFakeProvider, PreparedAcquisition,
};
use signal_processing::{
    CaptureChannelId, CaptureDataDelivery, CaptureProviderCapabilities, CaptureSettingCombination,
    CaptureStoreCursor, ProcessNode,
};

use crate::compiler::{CaptureGraphSourceFactory, LiveCaptureFeature, SimpleTriggerChannel};
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
    simple_trigger_channels: Arc<[SimpleTriggerChannel]>,
    capabilities: CaptureProviderCapabilities,
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

    fn capabilities(&self) -> &CaptureProviderCapabilities {
        &self.capabilities
    }

    fn simple_trigger_channels(&self) -> &[SimpleTriggerChannel] {
        &self.simple_trigger_channels
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
    let state = serde_json::from_value::<DemoCaptureSourceState>(state.clone())
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
    .map_err(|error| error.to_string())?
    .with_simple_trigger(
        state
            .trigger_conditions()
            .iter()
            .copied()
            .map(Some)
            .collect::<Vec<_>>(),
    )
    .map_err(|error| error.to_string())?;
    let simple_trigger_channels: Arc<[SimpleTriggerChannel]> = channels
        .iter()
        .cloned()
        .zip(channel_names.iter().cloned())
        .zip(state.trigger_conditions().iter().copied())
        .enumerate()
        .map(
            |(viewer_channel, ((channel_id, name), condition))| SimpleTriggerChannel {
                channel_id,
                viewer_channel,
                name,
                enabled: true,
                condition,
            },
        )
        .collect::<Vec<_>>()
        .into();
    let setting_matrix = vec![
        CaptureSettingCombination::new(Arc::clone(&channels), Arc::from([1_000_000_u64]))
            .map_err(|error| error.to_string())?,
        CaptureSettingCombination::new(
            channels[..4].to_vec(),
            Arc::from([5_000_000_u64, 10_000_000]),
        )
        .map_err(|error| error.to_string())?,
    ];
    let capabilities = CaptureProviderCapabilities::new(
        CaptureDataDelivery::DuringAcquisition,
        setting_matrix,
        false,
    )
    .map_err(|error| error.to_string())?;
    Ok(Some(Box::new(DemoLiveCaptureFeature {
        channels,
        channel_names,
        simple_trigger_channels,
        capabilities,
        provider: DeterministicFakeProvider::new(config),
    })))
}
