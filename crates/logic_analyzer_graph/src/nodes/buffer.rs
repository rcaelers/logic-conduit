//! `Buffer` node (`docs/PIPELINE_DESIGN.md`, flow control) — an explicit,
//! user-placed decoupling point. Wires to/from anything (`AnySocket`); the
//! payload kind it actually carries is picked explicitly via `kind`, not
//! inferred — the compiler's kind negotiation has no way to express
//! "whatever kind my input resolved to" for a genuinely generic passthrough
//! (see `crates/logic_analyzer_graph/src/compiler/buffer.rs`).

use egui::Color32;
use node_graph::{
    AnySocket, EnumValue, InputDef, IntValue, NodeDef, OutputDef, PanelSection, PropDef,
};
use serde::{Deserialize, Serialize};

use super::COLOR_LOGIC;

/// Which built-in payload kind flows through a given `Buffer` instance —
/// order matches the dropdown and `crates/logic_analyzer_graph/src/compiler/buffer.rs`'s
/// `selected_kind()`.
pub const KIND_LABELS: &[&str] = &["Signal", "Block", "Word", "Number", "Text", "Trigger"];

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BufferState {
    pub kind: EnumValue,
    pub capacity: IntValue,
}

pub struct Buffer;
impl NodeDef for Buffer {
    type State = BufferState;

    fn name() -> &'static str {
        "Buffer"
    }
    fn category() -> &'static str {
        "Logic"
    }
    fn color() -> Color32 {
        COLOR_LOGIC
    }

    fn inputs() -> Vec<InputDef<Self::State>> {
        vec![InputDef::new::<AnySocket>("In")]
    }

    fn outputs() -> Vec<OutputDef<Self::State>> {
        // Stays visually `Any`-styled regardless of the resolved kind —
        // only inputs get visual type resolution today
        // (`docs/NODE_GRAPH_DESIGN.md`: "the only polymorphic output is the
        // reroute node's `Any`"). Cosmetic only.
        vec![OutputDef::new::<AnySocket>("Out")]
    }

    fn state() -> Self::State {
        BufferState {
            kind: EnumValue::new(0, KIND_LABELS),
            capacity: IntValue::new(1_000, 1, i32::MAX),
        }
    }

    fn panel() -> Vec<PanelSection<Self::State>> {
        vec![PanelSection::new(
            "Options",
            vec![
                PropDef::control("kind", "Payload", |state| &mut state.kind),
                PropDef::control("capacity", "Capacity", |state| &mut state.capacity),
            ],
        )]
    }
}
