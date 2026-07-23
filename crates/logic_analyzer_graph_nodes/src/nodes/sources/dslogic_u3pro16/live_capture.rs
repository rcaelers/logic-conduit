use std::sync::Arc;

use serde_json::Value;

use logic_analyzer_graph_api::node::{CaptureGraphSourceFactory, LiveCaptureFeature};
use logic_analyzer_graph_api::node_support::{SimpleTriggerChannel, parse_state};
use logic_analyzer_processing::nodes::sources::dslogic_u3pro16::DsLogicU3Pro16Capture;
use signal_processing::{
    AcquisitionContext, AcquisitionError, AcquisitionResult, CaptureAnalysisChannel,
    CaptureAnalysisSource, CaptureChannelId, CaptureCommandCapabilities, CaptureFraction,
    CapturePolicyCapabilities, CapturePolicyContext, CaptureProviderCapabilities,
    CaptureSessionPlan, CaptureStartMode, CaptureStoreCursor, CompletionPolicyKind,
    PreparedAcquisition, ProcessNode, RecordingStart, RetentionPolicyKind,
    TriggerPlacementCapability, TriggerProgram, TriggerTimeoutAction,
};

use super::definition::U3Pro16State;
use super::implementation::{capture_config, requested_capture_policy};

struct U3Pro16GraphSourceFactory {
    channels: Arc<[CaptureAnalysisChannel]>,
    sample_rate_hz: f64,
}

impl CaptureGraphSourceFactory for U3Pro16GraphSourceFactory {
    fn create(&self, cursor: Box<dyn CaptureStoreCursor>) -> Result<Box<dyn ProcessNode>, String> {
        CaptureAnalysisSource::new(
            "u3pro16-captured-analysis",
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
    trigger_program: Option<TriggerProgram>,
    analysis_channels: Arc<[CaptureAnalysisChannel]>,
    capabilities: CaptureProviderCapabilities,
    session_plan: CaptureSessionPlan,
    capture: DsLogicU3Pro16Capture,
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

    fn trigger_program(&self) -> Option<&TriggerProgram> {
        self.trigger_program.as_ref()
    }

    fn session_plan(&self) -> Option<&CaptureSessionPlan> {
        Some(&self.session_plan)
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
        self.prepare_mode(context, CaptureStartMode::SavedPolicy)
    }

    fn prepare_with_mode(
        self: Box<Self>,
        context: AcquisitionContext,
        mode: CaptureStartMode,
    ) -> AcquisitionResult<Box<dyn PreparedAcquisition>> {
        self.prepare_mode(context, mode)
    }
}

impl U3Pro16LiveCaptureFeature {
    fn prepare_mode(
        self: Box<Self>,
        mut context: AcquisitionContext,
        mode: CaptureStartMode,
    ) -> AcquisitionResult<Box<dyn PreparedAcquisition>> {
        let plan = if mode == CaptureStartMode::CaptureNow {
            if !self.capabilities.commands().capture_now {
                return Err(AcquisitionError::UnsupportedOperation("capture now".into()));
            }
            self.session_plan.clone().capture_now()
        } else {
            self.session_plan.clone()
        };
        context.publish_plan(plan)?;
        let capture = if mode == CaptureStartMode::CaptureNow {
            self.capture.without_trigger()
        } else {
            self.capture
        };
        capture.prepare(context)
    }
}

pub(crate) fn feature(state: &Value) -> Result<Option<Box<dyn LiveCaptureFeature>>, String> {
    let state = parse_state::<U3Pro16State>(state)?;
    let config = capture_config(&state)?;
    let trigger_conditions = super::trigger::conditions(&state)?;
    let mut channels = Vec::new();
    let mut channel_names = Vec::new();
    let mut simple_trigger_channels = Vec::new();
    let mut analysis_channels = Vec::new();
    for (physical_channel, enabled) in state.channels.enabled.iter().copied().enumerate() {
        if !enabled {
            continue;
        }
        let channel_id = super::trigger::physical_channel_id(physical_channel);
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
            condition: trigger_conditions[physical_channel],
        });
    }
    let channels: Arc<[CaptureChannelId]> = channels.into();
    let capture = DsLogicU3Pro16Capture::new(config.clone(), Arc::clone(&channels))
        .map_err(|error| error.to_string())?;
    let delivery = capture.data_delivery();
    let actual_samples = capture.capture_window_samples();
    let policy_capabilities = CapturePolicyCapabilities::new(
        Arc::from([RecordingStart::Immediate, RecordingStart::Trigger]),
        Arc::from([
            RetentionPolicyKind::Everything,
            RetentionPolicyKind::RecentDuration,
            RetentionPolicyKind::RecentBytes,
        ]),
        Arc::from([CompletionPolicyKind::SamplesAfterOrigin]),
        TriggerPlacementCapability::SelectableFraction {
            minimum: CaptureFraction::from_percent(0).expect("zero percentage is valid"),
            maximum: CaptureFraction::from_percent(100).expect("full percentage is valid"),
            step: CaptureFraction::from_percent(1).expect("one percentage is valid"),
            sample_alignment: 64,
        },
        Arc::from([
            TriggerTimeoutAction::ContinueWaiting,
            TriggerTimeoutAction::Stop,
        ]),
    )
    .map_err(|error| error.to_string())?;
    let capabilities =
        CaptureProviderCapabilities::single(delivery, Arc::clone(&channels), config.sample_rate_hz)
            .with_commands(CaptureCommandCapabilities::new(true, false, false, true))
            .with_policy(policy_capabilities)
            .with_trigger_schema(super::trigger::schema());
    let requested_policy = requested_capture_policy(&state)?;
    let mut policy = capabilities
        .policy()
        .negotiate(
            &requested_policy,
            CapturePolicyContext {
                sample_rate_hz: config.sample_rate_hz,
                capture_window_samples: Some(actual_samples),
                has_trigger_program: !config.trigger.stages.is_empty(),
            },
        )
        .map_err(|error| error.to_string())?;
    let effective_before = match policy.effective.trigger_placement {
        Some(signal_processing::TriggerPlacement::SamplesBefore(samples)) => samples,
        Some(signal_processing::TriggerPlacement::Fraction(fraction)) => {
            fraction.samples_of(actual_samples)
        }
        Some(signal_processing::TriggerPlacement::DurationBefore(duration)) => u64::try_from(
            duration
                .as_nanos()
                .saturating_mul(u128::from(config.sample_rate_hz))
                .div_ceil(1_000_000_000),
        )
        .unwrap_or(actual_samples),
        None => 0,
    };
    policy.effective.completion = signal_processing::CompletionPolicy::SamplesAfterOrigin(
        actual_samples.saturating_sub(effective_before).max(1),
    );
    let session_plan = CaptureSessionPlan {
        sample_rate_hz: config.sample_rate_hz,
        channel_count: channels.len(),
        capture_window_samples: Some(actual_samples),
        policy,
    };
    Ok(Some(Box::new(U3Pro16LiveCaptureFeature {
        channel_names: channel_names.into(),
        analysis_channels: analysis_channels.into(),
        sample_rate_hz: config.sample_rate_hz as f64,
        simple_trigger_channels: simple_trigger_channels.into(),
        trigger_program: state.trigger_program().cloned(),
        channels,
        capabilities,
        session_plan,
        capture,
    })))
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use signal_processing::{
        CaptureCursorItem, CaptureStoreCursor, CaptureStoreResult, CompletionPolicy, ProcessNode,
        RecordingStart, RetentionPolicy, TriggerPlacement,
    };

    use super::super::definition::CaptureDurationValue;
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

    #[test]
    fn requested_policy_and_aligned_effective_values_are_part_of_the_session_plan() {
        let mut state = U3Pro16State::default();
        state.mode.select("Buffer");
        state.sample_rate.select("100 MHz");
        state.duration = CaptureDurationValue::from_milliseconds(10);
        state.channels.enabled.fill(false);
        state.channels.enabled[0] = true;
        state
            .set_trigger_condition(0, signal_processing::SimpleTriggerCondition::Rising)
            .unwrap();
        state.trigger_position_percent.value = 37;
        state.retention.select("Recent duration");
        state.retention_duration_ms.value = 250;

        let feature = feature(&serde_json::to_value(state).unwrap())
            .unwrap()
            .unwrap();
        let plan = feature.session_plan().unwrap();

        assert_eq!(plan.policy.requested.start, RecordingStart::Trigger);
        assert_eq!(
            plan.policy.requested.trigger_placement,
            Some(TriggerPlacement::Fraction(
                signal_processing::CaptureFraction::from_percent(37).unwrap()
            ))
        );
        assert_eq!(
            plan.policy.requested.retention_after_origin,
            RetentionPolicy::RecentDuration(Duration::from_millis(250))
        );
        let TriggerPlacement::SamplesBefore(before) =
            plan.policy.effective.trigger_placement.unwrap()
        else {
            panic!("effective placement must be sample aligned");
        };
        assert_eq!(before % 64, 0);
        let CompletionPolicy::SamplesAfterOrigin(after) = plan.policy.effective.completion else {
            panic!("effective completion must be a finite sample count");
        };
        assert_eq!(before + after, 1_000_448);
        assert_eq!(plan.capture_window_samples, Some(1_000_448));
    }
}
