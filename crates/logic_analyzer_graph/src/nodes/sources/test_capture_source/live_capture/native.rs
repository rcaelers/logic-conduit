//! Native deterministic acquisition provider adapter for tests.

use std::sync::Arc;

use serde_json::Value;

use logic_analyzer_processing::nodes::sources::{
    DeterministicFakeConfig, DeterministicFakeProvider, DeterministicTrigger,
    DeterministicTriggerCount, DeterministicTriggerCountMode, DeterministicTriggerLogic,
    DeterministicTriggerPredicate, DeterministicTriggerStage,
};
use signal_processing::{
    AcquisitionContext, AcquisitionResult, CaptureAnalysisChannel, CaptureAnalysisSource,
    CaptureChannelId, CaptureCommandCapabilities, CaptureDataDelivery, CaptureFraction,
    CapturePolicy, CapturePolicyCapabilities, CapturePolicyContext, CaptureProviderCapabilities,
    CaptureSessionPlan, CaptureSettingCombination, CaptureStartMode, CaptureStoreCursor,
    CompletionPolicy, CompletionPolicyKind, PreparedAcquisition, ProcessNode, RecordingStart,
    RetentionPolicy, RetentionPolicyKind, TriggerCountMode, TriggerLogicOperator, TriggerPlacement,
    TriggerPlacementCapability, TriggerPredicate, TriggerProgram, TriggerTimeoutAction,
};

use super::super::definition::TestCaptureSourceState;
use crate::{CaptureGraphSourceFactory, LiveCaptureFeature, SimpleTriggerChannel};

const CHUNK_SAMPLES: u64 = 4_096;
const CHUNK_COUNT: usize = 64;
const SAMPLE_RATE_HZ: f64 = 1_000_000.0;

struct DemoCaptureGraphSourceFactory {
    channels: Arc<[CaptureChannelId]>,
}

impl CaptureGraphSourceFactory for DemoCaptureGraphSourceFactory {
    fn create(&self, cursor: Box<dyn CaptureStoreCursor>) -> Result<Box<dyn ProcessNode>, String> {
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

struct TestLiveCaptureFeature {
    channels: Arc<[CaptureChannelId]>,
    channel_names: Arc<[String]>,
    simple_trigger_channels: Arc<[SimpleTriggerChannel]>,
    trigger_program: Option<TriggerProgram>,
    capabilities: CaptureProviderCapabilities,
    session_plan: CaptureSessionPlan,
    config: DeterministicFakeConfig,
}

impl LiveCaptureFeature for TestLiveCaptureFeature {
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

    fn trigger_program(&self) -> Option<&TriggerProgram> {
        self.trigger_program.as_ref()
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

impl TestLiveCaptureFeature {
    fn prepare_mode(
        self: Box<Self>,
        mut context: AcquisitionContext,
        mode: CaptureStartMode,
    ) -> AcquisitionResult<Box<dyn PreparedAcquisition>> {
        let (config, plan) = if mode == CaptureStartMode::CaptureNow {
            (
                self.config.without_trigger(),
                self.session_plan.capture_now(),
            )
        } else {
            (self.config, self.session_plan)
        };
        context.publish_plan(plan)?;
        DeterministicFakeProvider::new(config).prepare(context)
    }
}

fn lower_trigger(program: Option<&TriggerProgram>) -> Result<Option<DeterministicTrigger>, String> {
    super::super::trigger::validate_program(program)?;
    program
        .map(|program| {
            let stages = program
                .stages
                .iter()
                .map(|stage| {
                    let predicates = stage
                        .predicates
                        .iter()
                        .map(|predicate| {
                            let TriggerPredicate::Digital { channel, condition } = predicate else {
                                unreachable!(
                                    "validated demo schemas contain only digital predicates"
                                );
                            };
                            let channel = channel
                                .as_str()
                                .strip_prefix("demo:")
                                .and_then(|channel| channel.parse::<usize>().ok())
                                .ok_or_else(|| format!("unknown test capture channel {channel}"))?;
                            Ok(DeterministicTriggerPredicate {
                                channel,
                                condition: *condition,
                            })
                        })
                        .collect::<Result<Vec<_>, String>>()?;
                    let logic = match stage.logic {
                        TriggerLogicOperator::And => DeterministicTriggerLogic::And,
                        TriggerLogicOperator::Or => DeterministicTriggerLogic::Or,
                        TriggerLogicOperator::Xor => DeterministicTriggerLogic::Xor,
                        TriggerLogicOperator::Nand => DeterministicTriggerLogic::Nand,
                        TriggerLogicOperator::Nor => DeterministicTriggerLogic::Nor,
                    };
                    let count = stage.count.map(|count| DeterministicTriggerCount {
                        mode: match count.mode {
                            TriggerCountMode::Occurrences => {
                                DeterministicTriggerCountMode::Occurrences
                            }
                            TriggerCountMode::Consecutive => {
                                DeterministicTriggerCountMode::Consecutive
                            }
                        },
                        value: count.value,
                    });
                    Ok(DeterministicTriggerStage {
                        predicates,
                        logic,
                        inverted: stage.inverted,
                        count,
                    })
                })
                .collect::<Result<Vec<_>, String>>()?;
            Ok(DeterministicTrigger { stages })
        })
        .transpose()
}

pub(crate) fn feature(state: &Value) -> Result<Option<Box<dyn LiveCaptureFeature>>, String> {
    let state = serde_json::from_value::<TestCaptureSourceState>(state.clone())
        .map_err(|error| format!("invalid test capture state: {error}"))?;
    let channels: Arc<[CaptureChannelId]> = super::super::trigger::channel_ids().into();
    let channel_names: Arc<[String]> = (0..11)
        .map(|channel| format!("D{channel}"))
        .collect::<Vec<_>>()
        .into();
    let trigger_conditions = super::super::live_builder::conditions(state.trigger_program())?;
    let config = DeterministicFakeConfig::new(
        Arc::clone(&channels),
        vec![CHUNK_SAMPLES; CHUNK_COUNT],
        0x5a17_d3a0,
    )
    .map_err(|error| error.to_string())?
    .with_trigger(lower_trigger(state.trigger_program())?)
    .map_err(|error| error.to_string())?;
    let simple_trigger_channels: Arc<[SimpleTriggerChannel]> = channels
        .iter()
        .cloned()
        .zip(channel_names.iter().cloned())
        .zip(trigger_conditions.iter().copied())
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
    .with_policy(policy_capabilities)
    .with_trigger_schema(super::super::trigger::schema());
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
    let session_plan = CaptureSessionPlan {
        sample_rate_hz: SAMPLE_RATE_HZ as u64,
        channel_count: channels.len(),
        capture_window_samples: Some(config.total_samples()),
        policy,
    };
    Ok(Some(Box::new(TestLiveCaptureFeature {
        channels,
        channel_names,
        simple_trigger_channels,
        trigger_program: state.trigger_program().cloned(),
        capabilities,
        session_plan,
        config,
    })))
}

#[cfg(test)]
mod tests {
    use signal_processing::{
        SimpleTriggerCondition, TriggerCount, TriggerCountMode, TriggerLogicOperator,
        TriggerPredicate, TriggerProgram, TriggerStage,
    };

    use super::*;

    fn advanced_program() -> TriggerProgram {
        let schema = super::super::super::trigger::schema();
        TriggerProgram::new(
            schema.id().clone(),
            schema.revision(),
            vec![
                TriggerStage {
                    predicates: vec![TriggerPredicate::Digital {
                        channel: CaptureChannelId::new("demo:0"),
                        condition: SimpleTriggerCondition::High,
                    }],
                    logic: TriggerLogicOperator::And,
                    inverted: false,
                    count: Some(TriggerCount {
                        mode: TriggerCountMode::Occurrences,
                        value: 2,
                    }),
                },
                TriggerStage {
                    predicates: vec![TriggerPredicate::Digital {
                        channel: CaptureChannelId::new("demo:0"),
                        condition: SimpleTriggerCondition::Falling,
                    }],
                    logic: TriggerLogicOperator::Or,
                    inverted: true,
                    count: Some(TriggerCount {
                        mode: TriggerCountMode::Consecutive,
                        value: 1,
                    }),
                },
            ],
        )
    }

    #[test]
    fn advanced_program_lowers_and_executes_identically_after_state_json_reload() {
        let mut state = TestCaptureSourceState::default();
        state.set_trigger_program(Some(advanced_program())).unwrap();
        let trigger = lower_trigger(state.trigger_program()).unwrap();
        let before = DeterministicFakeConfig::new(
            super::super::super::trigger::channel_ids(),
            vec![3, 5],
            0x5a17_d3a0,
        )
        .unwrap()
        .with_trigger(trigger)
        .unwrap()
        .first_trigger_sample();

        let restored: TestCaptureSourceState =
            serde_json::from_value(serde_json::to_value(state).unwrap()).unwrap();
        let after = DeterministicFakeConfig::new(
            super::super::super::trigger::channel_ids(),
            vec![3, 5],
            0x5a17_d3a0,
        )
        .unwrap()
        .with_trigger(lower_trigger(restored.trigger_program()).unwrap())
        .unwrap()
        .first_trigger_sample();

        assert_eq!(before, Some(5));
        assert_eq!(after, before);
    }

    #[test]
    fn demo_schema_advertises_every_lowered_digital_operation() {
        let schema = super::super::super::trigger::schema();
        assert_eq!(schema.maximum_stages(), 4);
        assert_eq!(schema.maximum_predicates_per_stage(), 11);
        assert_eq!(
            schema.logic_operators(),
            [
                TriggerLogicOperator::And,
                TriggerLogicOperator::Or,
                TriggerLogicOperator::Xor,
                TriggerLogicOperator::Nand,
                TriggerLogicOperator::Nor,
            ]
        );
        assert!(schema.supports_stage_inversion());
        assert_eq!(
            schema.count_capabilities().unwrap().modes(),
            [TriggerCountMode::Occurrences, TriggerCountMode::Consecutive]
        );
    }
}
