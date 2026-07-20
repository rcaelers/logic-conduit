//! `Sigrok File Source` graph-node definition.

use egui::Color32;
use serde::{Deserialize, Serialize};

use node_graph::{FileValue, InputDef, IntValue, NodeDef, OutputDef, Socket};

use crate::nodes::registry::{COLOR_SOURCES, Signal, TextOpenPath};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SigrokFileSourceState {
    pub file: FileValue,
    pub channels: IntValue,
}

pub struct SigrokFileSource;

impl NodeDef for SigrokFileSource {
    type State = SigrokFileSourceState;
    fn name() -> &'static str {
        "Sigrok File Source"
    }
    fn category() -> &'static str {
        "Sources"
    }
    fn color() -> Color32 {
        COLOR_SOURCES
    }
    fn inputs() -> Vec<InputDef<Self::State>> {
        vec![
            InputDef::control::<TextOpenPath>("File", |state| &mut state.file),
            InputDef::control::<node_graph::IntSocket>("Channels", |state| &mut state.channels),
        ]
    }
    fn outputs() -> Vec<OutputDef<Self::State>> {
        (0..32)
            .map(|channel| OutputDef::new::<Signal>(format!("Ch {channel}")).view_selectable(false))
            .collect()
    }
    fn state() -> Self::State {
        SigrokFileSourceState {
            file: FileValue::with_filter("", "Select sigrok capture", "Sigrok captures", &["sr"]),
            channels: IntValue::new(8, 1, 32),
        }
    }
    fn on_update(state: &mut Self::State, _inputs: &mut [Socket], outputs: &mut [Socket]) {
        let channels = (state.channels.value as usize).clamp(1, 32);
        for (index, output) in outputs.iter_mut().enumerate() {
            output.visible = index < channels;
        }
    }
}
