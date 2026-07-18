mod builder;
mod definition;
mod live_capture;

pub(crate) use builder::DsLogicU3Pro16Builder;
pub use definition::{DsLogicU3Pro16, U3Pro16State};

use serde_json::Value;

use logic_analyzer_processing::{
    CaptureMode, ClockEdge, ClockSource, LogicCaptureConfig, LogicEncodingRequest, LogicTrigger,
    LogicTriggerStage, TriggerCondition,
};
use signal_processing::SimpleTriggerCondition;

use crate::compiler::{LiveCaptureEdit, parse_state};

use definition::U3PRO16_CHANNELS;

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
            if *enabled { mask | (1_u64 << index) } else { mask }
        })
}

fn lower_trigger(state: &U3Pro16State) -> LogicTrigger {
    let mut stage = LogicTriggerStage::default();
    let mut active = false;
    for physical_channel in 0..U3PRO16_CHANNELS {
        let enabled = state
            .channels
            .enabled
            .get(physical_channel)
            .copied()
            .unwrap_or(false);
        let condition = enabled
            .then(|| state.trigger_conditions().get(physical_channel).copied())
            .flatten()
            .unwrap_or(SimpleTriggerCondition::Ignore);
        stage.plane0[physical_channel] = match condition {
            SimpleTriggerCondition::Ignore => TriggerCondition::Ignore,
            SimpleTriggerCondition::Low => TriggerCondition::Low,
            SimpleTriggerCondition::High => TriggerCondition::High,
            SimpleTriggerCondition::Rising => TriggerCondition::Rising,
            SimpleTriggerCondition::Falling => TriggerCondition::Falling,
            SimpleTriggerCondition::Either => TriggerCondition::Either,
        };
        active |= condition != SimpleTriggerCondition::Ignore;
    }
    LogicTrigger {
        stages: active.then_some(stage).into_iter().collect(),
        serial: false,
    }
}

fn capture_config(state: &U3Pro16State) -> Result<LogicCaptureConfig, String> {
    let sample_rate_hz = selected_sample_rate_hz(state)?;
    let duration_ms = u64::try_from(state.duration_ms.value.max(1)).unwrap_or(1);
    Ok(LogicCaptureConfig {
        mode: if state.mode.selected() == "Stream" {
            CaptureMode::Streaming
        } else {
            CaptureMode::Finite
        },
        sample_rate_hz,
        input_mask: physical_input_mask(state),
        sample_limit: sample_rate_hz.saturating_mul(duration_ms).div_ceil(1_000),
        trigger_percent: 50,
        threshold_volts: Some(state.threshold.value),
        trigger: lower_trigger(state),
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

fn apply_live_capture_edit(state: &Value, edit: &LiveCaptureEdit) -> Result<Value, String> {
    let mut state = parse_state::<U3Pro16State>(state)?;
    let LiveCaptureEdit::SetSimpleTrigger {
        channel_id,
        condition,
    } = edit;
    let physical_channel = channel_id
        .as_str()
        .strip_prefix("u3pro16:input:")
        .and_then(|channel| channel.parse::<usize>().ok())
        .ok_or_else(|| format!("unknown U3Pro16 input {channel_id}"))?;
    state.set_trigger_condition(physical_channel, *condition)?;
    serde_json::to_value(state).map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    use logic_analyzer_processing::{
        CaptureMode, ClockEdge, ClockSource, LogicEncodingRequest, TriggerCondition,
    };
    use signal_processing::SimpleTriggerCondition;

    use super::{U3Pro16State, capture_config};

    #[test]
    fn buffered_state_lowers_channels_depth_trigger_timebase_and_encoding() {
        let mut state = U3Pro16State::default();
        state.mode.select("Buffer");
        state.sample_rate.select("100 MHz");
        state.duration_ms.value = 10;
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
        assert_eq!(
            config.trigger.stages[0].plane0[1],
            TriggerCondition::Ignore
        );
    }
}
