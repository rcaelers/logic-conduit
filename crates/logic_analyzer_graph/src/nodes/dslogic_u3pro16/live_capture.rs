use std::sync::Arc;

use serde_json::Value;

use logic_analyzer_processing::{
    AcquisitionContext, AcquisitionResult, CaptureAnalysisChannel, CaptureAnalysisSource,
    DsLogicU3Pro16BufferedProvider, LogicCaptureConfig, PreparedAcquisition,
    u3pro16_buffered_plan,
};
use signal_processing::{
    CaptureChannelId, CaptureDataDelivery, CaptureProviderCapabilities, CaptureStoreCursor,
    ProcessNode,
};

use crate::compiler::{
    CaptureGraphSourceFactory, LiveCaptureFeature, SimpleTriggerChannel, parse_state,
};

use super::{U3Pro16State, capture_config};

struct U3Pro16GraphSourceFactory {
    channels: Arc<[CaptureAnalysisChannel]>,
    sample_rate_hz: f64,
}

impl CaptureGraphSourceFactory for U3Pro16GraphSourceFactory {
    fn create(
        &self,
        cursor: Box<dyn CaptureStoreCursor>,
    ) -> Result<Box<dyn ProcessNode>, String> {
        CaptureAnalysisSource::new(
            "u3pro16-buffered-analysis",
            cursor,
            self.sample_rate_hz,
            self.channels.to_vec(),
        )
        .map(|source| Box::new(source) as Box<dyn ProcessNode>)
    }
}

struct U3Pro16LiveCaptureFeature {
    channels: Arc<[CaptureChannelId]>,
    channel_names: Arc<[String]>,
    sample_rate_hz: f64,
    simple_trigger_channels: Arc<[SimpleTriggerChannel]>,
    analysis_channels: Arc<[CaptureAnalysisChannel]>,
    capabilities: CaptureProviderCapabilities,
    config: LogicCaptureConfig,
}

impl LiveCaptureFeature for U3Pro16LiveCaptureFeature {
    fn channels(&self) -> &[CaptureChannelId] {
        &self.channels
    }

    fn channel_names(&self) -> &[String] {
        &self.channel_names
    }

    fn sample_rate_hz(&self) -> f64 {
        self.sample_rate_hz
    }

    fn capabilities(&self) -> &CaptureProviderCapabilities {
        &self.capabilities
    }

    fn simple_trigger_channels(&self) -> &[SimpleTriggerChannel] {
        &self.simple_trigger_channels
    }

    fn graph_source_factory(&self) -> Arc<dyn CaptureGraphSourceFactory> {
        Arc::new(U3Pro16GraphSourceFactory {
            channels: Arc::clone(&self.analysis_channels),
            sample_rate_hz: self.sample_rate_hz,
        })
    }

    fn prepare(
        self: Box<Self>,
        context: AcquisitionContext,
    ) -> AcquisitionResult<Box<dyn PreparedAcquisition>> {
        DsLogicU3Pro16BufferedProvider::open_first(self.config, self.channels)?.prepare(context)
    }
}

pub(super) fn feature(
    state: &Value,
) -> Result<Option<Box<dyn LiveCaptureFeature>>, String> {
    let state = parse_state::<U3Pro16State>(state)?;
    if state.mode.selected() != "Buffer" {
        return Ok(None);
    }
    let config = capture_config(&state)?;
    u3pro16_buffered_plan(&config).map_err(|error| error.to_string())?;
    let mut channels = Vec::new();
    let mut channel_names = Vec::new();
    let mut simple_trigger_channels = Vec::new();
    let mut analysis_channels = Vec::new();
    for (physical_channel, enabled) in state.channels.enabled.iter().copied().enumerate() {
        if !enabled {
            continue;
        }
        let channel_id = CaptureChannelId::new(format!("u3pro16:input:{physical_channel}"));
        let name = format!("Ch {physical_channel}");
        let viewer_channel = channels.len();
        channels.push(channel_id.clone());
        channel_names.push(name.clone());
        analysis_channels.push(CaptureAnalysisChannel::polymorphic(
            channel_id.clone(),
            format!("ch{physical_channel}"),
        ));
        simple_trigger_channels.push(SimpleTriggerChannel {
            channel_id,
            viewer_channel,
            name,
            enabled: true,
            condition: state.trigger_conditions()[physical_channel],
        });
    }
    let channels: Arc<[CaptureChannelId]> = channels.into();
    let capabilities = CaptureProviderCapabilities::single(
        CaptureDataDelivery::BufferedUpload,
        Arc::clone(&channels),
        config.sample_rate_hz,
    );
    Ok(Some(Box::new(U3Pro16LiveCaptureFeature {
        channel_names: channel_names.into(),
        analysis_channels: analysis_channels.into(),
        sample_rate_hz: config.sample_rate_hz as f64,
        simple_trigger_channels: simple_trigger_channels.into(),
        channels,
        capabilities,
        config,
    })))
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use signal_processing::{
        CaptureCursorItem, CaptureStoreCursor, CaptureStoreResult, ProcessNode,
    };

    use super::{U3Pro16State, feature};

    struct EndCursor;

    impl CaptureStoreCursor for EndCursor {
        fn next(&mut self) -> CaptureStoreResult<CaptureCursorItem> {
            Ok(CaptureCursorItem::End)
        }

        fn wait_next(&mut self, _timeout: Duration) -> CaptureStoreResult<CaptureCursorItem> {
            self.next()
        }

        fn next_sequence(&self) -> u64 {
            0
        }
    }

    #[test]
    fn replay_source_preserves_non_contiguous_physical_output_ports() {
        let mut state = U3Pro16State::default();
        state.mode.select("Buffer");
        state.channels.enabled.fill(false);
        for channel in [0, 2, 9] {
            state.channels.enabled[channel] = true;
        }
        let feature = feature(&serde_json::to_value(state).unwrap())
            .unwrap()
            .unwrap();
        let source = feature
            .graph_source_factory()
            .create(Box::new(EndCursor))
            .unwrap();

        assert_eq!(
            ProcessNode::output_schema(source.as_ref())
                .into_iter()
                .map(|port| port.name)
                .collect::<Vec<_>>(),
            ["ch0", "ch2", "ch9"]
        );
    }
}
