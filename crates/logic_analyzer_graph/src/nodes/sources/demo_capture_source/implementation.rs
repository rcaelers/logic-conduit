use serde_json::Value;

use super::definition::{DemoCaptureSource, DemoCaptureSourceState};
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

/// One raw capture row that can be shown independently of a pipeline run.
pub struct CapturePreviewSignal {
    pub index: usize,
    pub name: String,
    pub initial: bool,
    pub transitions: Vec<(f64, bool)>,
}

/// Returns the generated capture represented by `node` as the ten active raw
/// channels used by the demo graph. This is concrete source behavior; callers
/// only receive the generic preview contract.
pub(crate) fn capture_preview(node: &node_graph::Node) -> Option<Vec<CapturePreviewSignal>> {
    use node_graph::NodeDef;

    (node.def_name() == DemoCaptureSource::name()).then(|| {
        let channels = logic_analyzer_processing::DemoCaptureSource::preview_channels();
        (0..=8)
            .chain(std::iter::once(10))
            .map(|index| {
                let samples = &channels[index];
                CapturePreviewSignal {
                    index,
                    name: format!("Ch {index}"),
                    initial: samples.first().is_some_and(|sample| sample.value),
                    transitions: samples
                        .iter()
                        .skip(1)
                        .map(|sample| (sample.start_time_ns as f64 / 1_000.0, sample.value))
                        .collect(),
                }
            })
            .collect()
    })
}
