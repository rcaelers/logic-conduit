//! `Demo Capture Source` graph-node definition.

use egui::Color32;
use serde::{Deserialize, Serialize};

use node_graph::{InputDef, NodeDef, OutputDef};

use crate::nodes::registry::{COLOR_SOURCES, Signal};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DemoCaptureSourceState {}

pub struct DemoCaptureSource;

impl NodeDef for DemoCaptureSource {
    type State = DemoCaptureSourceState;

    fn name() -> &'static str {
        "Demo Capture Source"
    }

    fn category() -> &'static str {
        "Sources"
    }

    fn color() -> Color32 {
        COLOR_SOURCES
    }

    fn inputs() -> Vec<InputDef<Self::State>> {
        vec![]
    }

    fn outputs() -> Vec<OutputDef<Self::State>> {
        (0..11)
            .map(|channel| OutputDef::new::<Signal>(format!("Ch {channel}")))
            .collect()
    }

    fn state() -> Self::State {
        DemoCaptureSourceState::default()
    }
}
