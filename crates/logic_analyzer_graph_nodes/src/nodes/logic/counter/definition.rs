//! `Counter` graph-node definition.

use egui::Color32;
use serde::{Deserialize, Serialize};

use node_graph::{InputDef, IntValue, NodeDef, OutputDef, PanelSection, PropDef};

use crate::nodes::registry::{COLOR_LOGIC, Number, Trigger};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct CounterState {
    pub(crate) start: IntValue,
    pub(crate) step: IntValue,
}

pub(crate) struct Counter;
impl NodeDef for Counter {
    type State = CounterState;

    fn name() -> &'static str {
        "Counter"
    }
    fn category() -> &'static str {
        "Logic"
    }
    fn color() -> Color32 {
        COLOR_LOGIC
    }

    fn inputs() -> Vec<InputDef<Self::State>> {
        vec![InputDef::new::<Trigger>("Trigger")]
    }

    fn outputs() -> Vec<OutputDef<Self::State>> {
        vec![OutputDef::new::<Number>("Count")]
    }

    fn state() -> Self::State {
        CounterState {
            start: IntValue::plain(0),
            step: IntValue::plain(1),
        }
    }

    fn panel() -> Vec<PanelSection<Self::State>> {
        vec![PanelSection::new(
            "Options",
            vec![
                PropDef::control("start", "Start", |state| &mut state.start),
                PropDef::control("step", "Step", |state| &mut state.step),
            ],
        )]
    }
}
