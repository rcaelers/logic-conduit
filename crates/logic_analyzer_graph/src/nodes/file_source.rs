//! `DSL File Source` node — reads channels from a `.dsl` capture file.

use egui::Color32;
use node_graph::{FileValue, InputDef, IntValue, NodeDef, OutputDef, Socket};
use serde::{Deserialize, Serialize};

use super::{COLOR_SOURCES, Signal, TextOpenPath};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DslFileSourceState {
    pub file: FileValue,
    pub channels: IntValue,
}

pub struct DslFileSource;
impl NodeDef for DslFileSource {
    type State = DslFileSourceState;

    fn name() -> &'static str {
        "DSL File Source"
    }
    fn category() -> &'static str {
        "Sources"
    }
    fn color() -> Color32 {
        COLOR_SOURCES
    }

    fn inputs() -> Vec<InputDef<Self::State>> {
        vec![
            // A `Text` wire supplies the filename at run start; while
            // unconnected the socket shows an open-file picker instead.
            InputDef::control::<TextOpenPath>("File", |state| &mut state.file),
            InputDef::control::<node_graph::IntSocket>("Channels", |state| &mut state.channels),
        ]
    }

    fn outputs() -> Vec<OutputDef<Self::State>> {
        (0..32_usize)
            .map(|i| OutputDef::new::<Signal>(format!("Ch {i}")))
            .collect()
    }

    fn state() -> Self::State {
        DslFileSourceState {
            file: FileValue::with_filter(
                "",
                "Select DSLogic capture",
                "DSLogic captures",
                &["dsl"],
            ),
            channels: IntValue::new(11, 1, 32),
        }
    }

    fn on_update(state: &mut Self::State, _inputs: &mut [Socket], outputs: &mut [Socket]) {
        let channels = (state.channels.value as usize).clamp(1, 32);
        for (index, output) in outputs.iter_mut().enumerate() {
            output.visible = index < channels;
        }
    }
}
