use std::sync::Arc;

use serde_json::Value;

use logic_analyzer_processing::{
    AcquisitionContext, AcquisitionResult, CaptureAnalysisChannel, CaptureAnalysisSource,
    DeterministicFakeConfig, DeterministicFakeProvider, PreparedAcquisition,
};
use signal_processing::{
    CaptureCapacityRequest, CaptureChannelId, CaptureCommandCapabilities, CaptureDataDelivery,
    CaptureFraction, CapturePolicy, CapturePolicyCapabilities, CapturePolicyContext,
    CaptureProviderCapabilities, CaptureSessionPlan, CaptureSettingCombination, CaptureStartMode,
    CaptureStoreCursor, CompletionPolicy, CompletionPolicyKind, ProcessNode, RecordingStart,
    RetentionPolicy, RetentionPolicyKind, TriggerPlacement, TriggerPlacementCapability,
    TriggerTimeoutAction, estimate_capture_capacity,
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
    session_plan: CaptureSessionPlan,
    config: DeterministicFakeConfig,
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

    fn session_plan(&self) -> Option<&CaptureSessionPlan> {
        Some(&self.session_plan)
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

impl DemoLiveCaptureFeature {
    fn prepare_mode(
        self: Box<Self>,
        mut context: AcquisitionContext,
        mode: CaptureStartMode,
    ) -> AcquisitionResult<Box<dyn PreparedAcquisition>> {
        let (config, plan) = if mode == CaptureStartMode::CaptureNow {
            (self.config.without_trigger(), self.session_plan.capture_now())
        } else {
            (self.config, self.session_plan)
        };
        context.publish_plan(plan)?;
        DeterministicFakeProvider::new(config).prepare(context)
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
    let has_trigger_program = config.has_trigger();
    let trigger_sample = config.first_trigger_sample().unwrap_or(0);
    let policy_capabilities = CapturePolicyCapabilities::new(
        Arc::from([RecordingStart::Immediate, RecordingStart::Trigger]),
        Arc::from([
            RetentionPolicyKind::Everything,
            RetentionPolicyKind::RecentDuration,
            RetentionPolicyKind::RecentBytes,
        ]),
        Arc::from([
            CompletionPolicyKind::UntilStopped,
            CompletionPolicyKind::SamplesAfterOrigin,
        ]),
        TriggerPlacementCapability::Fixed(TriggerPlacement::Fraction(
            CaptureFraction::from_percent(0).expect("zero percentage is valid"),
        )),
        Arc::from([
            TriggerTimeoutAction::ContinueWaiting,
            TriggerTimeoutAction::Stop,
        ]),
    )
    .map_err(|error| error.to_string())?;
    let capabilities = CaptureProviderCapabilities::new(
        CaptureDataDelivery::DuringAcquisition,
        setting_matrix,
        false,
    )
    .map_err(|error| error.to_string())?
    .with_commands(CaptureCommandCapabilities::new(true, true, true, true))
    .with_policy(policy_capabilities);
    let requested_policy = CapturePolicy {
        start: if has_trigger_program {
            RecordingStart::Trigger
        } else {
            RecordingStart::Immediate
        },
        trigger_placement: has_trigger_program.then(|| {
            TriggerPlacement::Fraction(
                CaptureFraction::from_percent(0).expect("zero percentage is valid"),
            )
        }),
        retention_before_origin: RetentionPolicy::Everything,
        retention_after_origin: RetentionPolicy::Everything,
        completion: CompletionPolicy::SamplesAfterOrigin(
            config.total_samples().saturating_sub(trigger_sample).max(1),
        ),
        trigger_timeout: None,
    };
    let mut policy = capabilities
        .policy()
        .negotiate(
            &requested_policy,
            CapturePolicyContext {
                sample_rate_hz: SAMPLE_RATE_HZ as u64,
                capture_window_samples: Some(config.total_samples()),
                has_trigger_program,
            },
        )
        .map_err(|error| error.to_string())?;
    if has_trigger_program {
        policy.effective.trigger_placement = Some(TriggerPlacement::SamplesBefore(trigger_sample));
    }
    let capacity = estimate_capture_capacity(
        CaptureCapacityRequest {
            sample_rate_hz: SAMPLE_RATE_HZ as u64,
            channel_count: channels.len(),
            capture_window_samples: Some(config.total_samples()),
            storage_budget_bytes: None,
            available_storage_bytes: None,
        },
        &requested_policy,
    )
    .map_err(|error| error.to_string())?;
    let session_plan = CaptureSessionPlan {
        sample_rate_hz: SAMPLE_RATE_HZ as u64,
        channel_count: channels.len(),
        policy,
        capacity,
    };
    Ok(Some(Box::new(DemoLiveCaptureFeature {
        channels,
        channel_names,
        simple_trigger_channels,
        capabilities,
        session_plan,
        config,
    })))
}
