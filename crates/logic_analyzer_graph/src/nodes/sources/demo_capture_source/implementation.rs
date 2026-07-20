use serde_json::Value;

use super::definition::DemoCaptureSourceState;
use crate::LiveCaptureEdit;

pub(crate) fn apply_live_capture_edit(
    state: &Value,
    edit: &LiveCaptureEdit,
) -> Result<Value, String> {
    let mut state = serde_json::from_value::<DemoCaptureSourceState>(state.clone())
        .map_err(|error| format!("invalid demo capture state: {error}"))?;
    match edit {
        LiveCaptureEdit::SetSimpleTrigger {
            channel_id,
            condition,
        } => {
            let channel = channel_id
                .as_str()
                .strip_prefix("demo:")
                .and_then(|channel| channel.parse::<usize>().ok())
                .ok_or_else(|| format!("unknown demo capture channel {channel_id}"))?;
            state.set_trigger_condition(channel, *condition)?;
        }
        LiveCaptureEdit::SetTriggerProgram { program } => {
            state.set_trigger_program(program.clone())?;
        }
    }
    serde_json::to_value(state).map_err(|error| error.to_string())
}
