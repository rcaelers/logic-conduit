//! `DSL File Source` node — reads channels from a `.dsl` capture file.

use super::{COLOR_SOURCES, Signal};
use egui::Color32;
use node_graph::{FileValue, InputDef, IntValue, NodeDef, OutputDef, Socket};
use serde::{Deserialize, Serialize};

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
            InputDef::control::<node_graph::FileSocket>("File", |state| &mut state.file),
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
                "_captures/wipneus5.dsl",
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
