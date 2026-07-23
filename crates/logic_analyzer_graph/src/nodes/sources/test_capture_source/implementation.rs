//! Test-capture state editing.

use serde_json::Value;

use logic_analyzer_graph_api::node_support::LiveCaptureEdit;

use super::definition::TestCaptureSourceState;

pub(crate) fn apply_live_capture_edit(
    state: &Value,
    edit: &LiveCaptureEdit,
) -> Result<Value, String> {
    let mut state = serde_json::from_value::<TestCaptureSourceState>(state.clone())
        .map_err(|error| format!("invalid test capture state: {error}"))?;
    match edit {
        LiveCaptureEdit::SetSimpleTrigger {
            channel_id,
            condition,
        } => {
            let channel = channel_id
                .as_str()
                .strip_prefix("demo:")
                .and_then(|channel| channel.parse::<usize>().ok())
                .ok_or_else(|| format!("unknown test capture channel {channel_id}"))?;
            state.set_trigger_condition(channel, *condition)?;
        }
        LiveCaptureEdit::SetTriggerProgram { program } => {
            state.set_trigger_program(program.clone())?;
        }
    }
    serde_json::to_value(state).map_err(|error| error.to_string())
}
