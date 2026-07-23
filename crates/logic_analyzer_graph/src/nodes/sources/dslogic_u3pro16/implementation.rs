use std::time::Duration;

use serde_json::Value;

use logic_analyzer_processing::support::logic_analyzer::{
    CaptureMode, ClockEdge, ClockSource, LogicCaptureConfig, LogicEncodingRequest, LogicTrigger,
};
use signal_processing::{
    CaptureFraction, CapturePolicy, CompletionPolicy, RecordingStart, RetentionPolicy,
    TriggerPlacement, TriggerTimeout, TriggerTimeoutAction,
};

use super::definition::{U3Pro16State, capture_duration_limit_ns, channel_rate_validation_error};
use super::trigger_lowering;
use crate::{LiveCaptureEdit, parse_state};

fn selected_sample_rate_hz(state: &U3Pro16State) -> Result<u64, String> {
    state
        .sample_rate
        .selected()
        .strip_suffix(" GHz")
        .and_then(|value| value.parse::<u64>().ok())
        .map(|value| value * 1_000_000_000)
        .or_else(|| {
            state
                .sample_rate
                .selected()
                .strip_suffix(" MHz")
                .and_then(|value| value.parse::<u64>().ok())
                .map(|value| value * 1_000_000)
        })
        .ok_or_else(|| "invalid U3Pro16 sample rate".to_owned())
}

fn physical_input_mask(state: &U3Pro16State) -> u64 {
    state
        .channels
        .enabled
        .iter()
        .enumerate()
        .fold(0_u64, |mask, (index, enabled)| {
            if *enabled {
                mask | (1_u64 << index)
            } else {
                mask
            }
        })
}

pub(crate) fn capture_config(state: &U3Pro16State) -> Result<LogicCaptureConfig, String> {
    if let Some(error) = channel_rate_validation_error(state) {
        return Err(error);
    }
    let sample_rate_hz = selected_sample_rate_hz(state)?;
    let enabled_channels = state.channels.enabled_count();
    let duration_ns = state.duration.nanoseconds().min(capture_duration_limit_ns(
        state.mode.selected(),
        sample_rate_hz,
        enabled_channels,
    ));
    let sample_limit = (u128::from(sample_rate_hz) * u128::from(duration_ns))
        .div_ceil(1_000_000_000)
        .min(u128::from(u64::MAX)) as u64;
    Ok(LogicCaptureConfig {
        mode: if state.mode.selected() == "Stream" {
            CaptureMode::Streaming
        } else {
            CaptureMode::Finite
        },
        sample_rate_hz,
        input_mask: physical_input_mask(state),
        sample_limit,
        trigger_percent: u8::try_from(state.trigger_position_percent.value.clamp(0, 100))
            .unwrap_or(50),
        threshold_volts: Some(state.threshold.value),
        trigger: if state.recording_start.selected() == "Trigger" {
            trigger_lowering::lower_program(state)?
        } else {
            LogicTrigger::default()
        },
        encoding: if state.rle.value {
            LogicEncodingRequest::RunLength
        } else {
            LogicEncodingRequest::Raw
        },
        clock: if state.ext_clock.value {
            ClockSource::External {
                edge: if state.clock_edge.selected() == "Falling" {
                    ClockEdge::Falling
                } else {
                    ClockEdge::Rising
                },
            }
        } else {
            ClockSource::Internal
        },
        input_filter: state.filter.value,
    })
}

fn retention_policy(state: &U3Pro16State) -> RetentionPolicy {
    match state.retention.selected() {
        "Recent duration" => RetentionPolicy::RecentDuration(Duration::from_millis(
            u64::try_from(state.retention_duration_ms.value.max(1)).unwrap_or(1),
        )),
        "Recent bytes" => RetentionPolicy::RecentBytes(
            u64::try_from(state.retention_megabytes.value.max(1))
                .unwrap_or(1)
                .saturating_mul(1024 * 1024),
        ),
        _ => RetentionPolicy::Everything,
    }
}

pub(crate) fn requested_capture_policy(state: &U3Pro16State) -> Result<CapturePolicy, String> {
    let config = capture_config(state)?;
    let start = if state.recording_start.selected() == "Trigger" {
        RecordingStart::Trigger
    } else {
        RecordingStart::Immediate
    };
    if start == RecordingStart::Trigger && config.trigger.stages.is_empty() {
        return Err("triggered recording requires at least one enabled trigger condition".into());
    }
    let before_samples = if start == RecordingStart::Trigger {
        config
            .sample_limit
            .saturating_mul(u64::from(config.trigger_percent))
            / 100
    } else {
        0
    };
    let trigger_timeout = match state.trigger_timeout_action.selected() {
        "Continue waiting" => Some(TriggerTimeout {
            after: Duration::from_millis(
                u64::try_from(state.trigger_timeout_ms.value.max(1)).unwrap_or(1),
            ),
            action: TriggerTimeoutAction::ContinueWaiting,
        }),
        "Stop" => Some(TriggerTimeout {
            after: Duration::from_millis(
                u64::try_from(state.trigger_timeout_ms.value.max(1)).unwrap_or(1),
            ),
            action: TriggerTimeoutAction::Stop,
        }),
        _ => None,
    };
    Ok(CapturePolicy {
        start,
        trigger_placement: (start == RecordingStart::Trigger).then(|| {
            TriggerPlacement::Fraction(
                CaptureFraction::from_percent(config.trigger_percent)
                    .expect("clamped trigger percentage is valid"),
            )
        }),
        retention_before_origin: RetentionPolicy::Everything,
        retention_after_origin: retention_policy(state),
        completion: CompletionPolicy::SamplesAfterOrigin(
            config.sample_limit.saturating_sub(before_samples).max(1),
        ),
        trigger_timeout,
    })
}

pub(crate) fn apply_live_capture_edit(
    state: &Value,
    edit: &LiveCaptureEdit,
) -> Result<Value, String> {
    let mut state = parse_state::<U3Pro16State>(state)?;
    match edit {
        LiveCaptureEdit::SetSimpleTrigger {
            channel_id,
            condition,
        } => {
            let physical_channel = channel_id
                .as_str()
                .strip_prefix("u3pro16:input:")
                .and_then(|channel| channel.parse::<usize>().ok())
                .ok_or_else(|| format!("unknown U3Pro16 input {channel_id}"))?;
            state.set_trigger_condition(physical_channel, *condition)?;
        }
        LiveCaptureEdit::SetTriggerProgram { program } => {
            state.set_trigger_program(program.clone())?;
        }
    }
    serde_json::to_value(state).map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    use logic_analyzer_processing::support::logic_analyzer::{
        CaptureMode, ClockEdge, ClockSource, LogicEncodingRequest, TriggerCondition,
    };
    use signal_processing::SimpleTriggerCondition;

    use super::super::definition::CaptureDurationValue;
    use super::{U3Pro16State, capture_config};

    #[test]
    fn buffered_state_lowers_channels_depth_trigger_timebase_and_encoding() {
        let mut state = U3Pro16State::default();
        state.mode.select("Buffer");
        state.sample_rate.select("100 MHz");
        state.duration = CaptureDurationValue::from_milliseconds(10);
        state.channels.enabled.fill(false);
        state.channels.enabled[0] = true;
        state.channels.enabled[2] = true;
        state
            .set_trigger_condition(2, SimpleTriggerCondition::Falling)
            .unwrap();
        state.ext_clock.value = true;
        state.clock_edge.select("Falling");
        state.rle.value = true;
        state.filter.value = true;
        state.threshold.value = 1.25;

        let config = capture_config(&state).unwrap();

        assert_eq!(config.mode, CaptureMode::Finite);
        assert_eq!(config.sample_rate_hz, 100_000_000);
        assert_eq!(config.input_mask, 0b0101);
        assert_eq!(config.sample_limit, 1_000_000);
        assert_eq!(config.trigger_percent, 50);
        assert_eq!(config.threshold_volts, Some(1.25));
        assert_eq!(config.encoding, LogicEncodingRequest::RunLength);
        assert_eq!(
            config.clock,
            ClockSource::External {
                edge: ClockEdge::Falling
            }
        );
        assert!(config.input_filter);
        assert_eq!(config.trigger.stages.len(), 1);
        assert_eq!(
            config.trigger.stages[0].plane0[2],
            TriggerCondition::Falling
        );
        assert_eq!(config.trigger.stages[0].plane0[1], TriggerCondition::Ignore);
    }

    #[test]
    fn microsecond_capture_duration_lowers_to_samples() {
        let mut state = U3Pro16State::default();
        state.sample_rate.select("100 MHz");
        state.duration = CaptureDurationValue::from_nanoseconds(10_000);

        let config = capture_config(&state).unwrap();

        assert_eq!(config.sample_limit, 1_000);
    }

    #[test]
    fn streaming_capture_is_capped_at_the_dsview_sample_depth() {
        let mut state = U3Pro16State::default();
        state.sample_rate.select("1 MHz");
        state.duration = CaptureDurationValue::from_nanoseconds(u64::MAX);

        let config = capture_config(&state).unwrap();

        assert_eq!(config.sample_limit, 1_u64 << 34);
    }

    #[test]
    fn capture_config_rejects_too_many_channels_for_stream_rate() {
        let mut state = U3Pro16State::default();
        state.sample_rate.select("1 GHz");

        let error = capture_config(&state).unwrap_err();

        assert!(error.contains("Too many channels"));
        assert!(error.contains("Ch 0–2"));
    }
}
