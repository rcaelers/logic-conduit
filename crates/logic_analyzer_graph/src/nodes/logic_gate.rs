//! `Logic Gate` node.

use egui::Color32;
use serde::{Deserialize, Serialize};

use node_graph::{EnumValue, InputDef, NodeBadge, NodeDef, OutputDef, PropDef, Socket};

use super::registry::{COLOR_LOGIC, Signal};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LogicGateState {
    pub op: EnumValue,
    /// Set by `on_update` when extra inputs are connected to a NOT gate.
    #[serde(skip)]
    pub note: Option<String>,
}

pub struct LogicGate;
impl NodeDef for LogicGate {
    type State = LogicGateState;

    fn name() -> &'static str {
        "Logic Gate"
    }
    fn category() -> &'static str {
        "Logic"
    }
    fn color() -> Color32 {
        COLOR_LOGIC
    }

    fn inputs() -> Vec<InputDef<Self::State>> {
        vec![InputDef::new::<Signal>("In").variadic(8)]
    }

    fn outputs() -> Vec<OutputDef<Self::State>> {
        vec![OutputDef::new::<Signal>("Out")]
    }

    fn state() -> Self::State {
        LogicGateState {
            op: EnumValue::new(1, &["NOT", "AND", "NAND", "OR", "NOR", "XOR", "XNOR"]),
            note: None,
        }
    }

    fn props() -> Vec<PropDef<Self::State>> {
        vec![PropDef::control("op", "Op", |state| &mut state.op)]
    }

    fn on_update(state: &mut Self::State, inputs: &mut [Socket], _outputs: &mut [Socket]) {
        let is_not = state.op.selected() == "NOT";
        let members = inputs.iter().filter(|s| s.is_variadic_member()).count();
        // NOT is single-input: once one member is connected, stop offering
        // the placeholder.
        for socket in inputs.iter_mut() {
            if socket.is_variadic_placeholder() {
                socket.visible = !(is_not && members >= 1);
            }
        }
        state.note = (is_not && members > 1)
            .then(|| "NOT uses input 1 only; disconnect the others".to_owned());
    }

    fn badge(state: &Self::State) -> Option<NodeBadge> {
        state.note.as_ref().map(NodeBadge::warning)
    }
}
