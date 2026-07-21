//! Test-only UART signal-source graph-node definition.

use egui::Color32;
use serde::{Deserialize, Serialize};

use node_graph::{InputDef, IntValue, NodeDef, OutputDef, StringValue};

use crate::nodes::registry::{COLOR_SOURCES, Signal};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TestUartSourceState {
    pub message: StringValue,
    pub baud_rate: IntValue,
}

pub struct TestUartSource;
impl NodeDef for TestUartSource {
    type State = TestUartSourceState;

    fn name() -> &'static str {
        "Test UART Source"
    }
    fn category() -> &'static str {
        "Sources"
    }
    fn color() -> Color32 {
        COLOR_SOURCES
    }

    fn inputs() -> Vec<InputDef<Self::State>> {
        vec![
            InputDef::control::<node_graph::StrSocket>("Message", |state| &mut state.message),
            InputDef::control::<node_graph::IntSocket>("Baud Rate", |state| &mut state.baud_rate),
        ]
    }

    fn outputs() -> Vec<OutputDef<Self::State>> {
        vec![OutputDef::new::<Signal>("RX").view_selectable(false)]
    }

    fn state() -> Self::State {
        TestUartSourceState {
            message: StringValue::new("HELLO\n"),
            baud_rate: IntValue::new(115_200, 300, 100_000_000),
        }
    }
}
