//! `Viewer` node.

use egui::Color32;
use serde::{Deserialize, Serialize};

use node_graph::{InputDef, NodeDef, OutputDef, PanelSection, PropDef, StringValue};

use super::{COLOR_OUTPUT, Signal, Trigger, Words};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ViewerState {
    pub label: StringValue,
}

pub struct Viewer;
impl NodeDef for Viewer {
    type State = ViewerState;

    fn name() -> &'static str {
        "Viewer"
    }
    fn category() -> &'static str {
        "Output"
    }
    fn color() -> Color32 {
        COLOR_OUTPUT
    }

    fn inputs() -> Vec<InputDef<Self::State>> {
        // A lane renders whatever it is fed: raw/derived signals as
        // waveforms, words as annotation boxes, triggers as markers.
        vec![
            InputDef::new::<Signal>("In")
                .accepts::<Words>()
                .accepts::<Trigger>()
                .variadic(16),
        ]
    }

    fn outputs() -> Vec<OutputDef<Self::State>> {
        vec![]
    }

    fn state() -> Self::State {
        ViewerState {
            label: StringValue::new(""),
        }
    }

    fn panel() -> Vec<PanelSection<Self::State>> {
        vec![PanelSection::new(
            "Options",
            vec![PropDef::control("label", "Label", |state| &mut state.label)],
        )]
    }
}
