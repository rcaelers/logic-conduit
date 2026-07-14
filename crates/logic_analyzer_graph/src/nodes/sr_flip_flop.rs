//! `SR Flip-Flop` node.

use egui::Color32;
use serde::{Deserialize, Serialize};

use node_graph::{BoolValue, InputDef, NodeDef, OutputDef, PanelSection, PropDef};

use super::registry::{COLOR_LOGIC, Signal, Trigger};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SrFlipFlopState {
    pub initial: BoolValue,
}

pub struct SrFlipFlop;
impl NodeDef for SrFlipFlop {
    type State = SrFlipFlopState;

    fn name() -> &'static str {
        "SR Flip-Flop"
    }
    fn category() -> &'static str {
        "Logic"
    }
    fn color() -> Color32 {
        COLOR_LOGIC
    }

    fn inputs() -> Vec<InputDef<Self::State>> {
        vec![
            InputDef::new::<Trigger>("Set"),
            InputDef::new::<Trigger>("Reset"),
        ]
    }

    fn outputs() -> Vec<OutputDef<Self::State>> {
        vec![OutputDef::new::<Signal>("Q")]
    }

    fn state() -> Self::State {
        SrFlipFlopState {
            initial: BoolValue::new(false),
        }
    }

    fn panel() -> Vec<PanelSection<Self::State>> {
        vec![PanelSection::new(
            "Options",
            vec![PropDef::control("initial", "Initial state", |state| {
                &mut state.initial
            })],
        )]
    }
}
