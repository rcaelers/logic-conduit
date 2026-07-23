use node_graph::NodeDef;
use signal_processing::SimpleTriggerCondition;

pub fn test_capture_source_name() -> &'static str {
    super::sources::TestCaptureSource::name()
}

pub fn test_live_capture_source_name() -> &'static str {
    super::sources::TestLiveCaptureSource::name()
}

pub fn set_test_capture_trigger_condition(
    state: &mut serde_json::Value,
    channel: usize,
    condition: SimpleTriggerCondition,
) -> Result<(), String> {
    let mut typed: super::sources::TestCaptureSourceState =
        serde_json::from_value(state.clone()).map_err(|error| error.to_string())?;
    typed.set_trigger_condition(channel, condition)?;
    *state = serde_json::to_value(typed).map_err(|error| error.to_string())?;
    Ok(())
}

pub fn dslogic_u3pro16_name() -> &'static str {
    super::sources::DsLogicU3Pro16::name()
}

pub fn configure_u3pro16_test_capture(
    state: &mut serde_json::Value,
    mode: &str,
    sample_rate: &str,
    duration_ms: u64,
    enabled_channels: &[usize],
) -> Result<(), String> {
    let mut typed: super::sources::U3Pro16State =
        serde_json::from_value(state.clone()).map_err(|error| error.to_string())?;
    typed.configure_test_capture(mode, sample_rate, duration_ms, enabled_channels);
    *state = serde_json::to_value(typed).map_err(|error| error.to_string())?;
    Ok(())
}
