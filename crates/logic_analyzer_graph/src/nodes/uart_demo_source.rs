//! `UART Demo Source` node — generates a fixed UART byte sequence in-memory.

use egui::Color32;
use node_graph::{InputDef, IntValue, NodeDef, OutputDef, StringValue};
use serde::{Deserialize, Serialize};

use super::{COLOR_SOURCES, Signal};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UartDemoSourceState {
    pub message: StringValue,
    pub baud_rate: IntValue,
}

pub struct UartDemoSource;
impl NodeDef for UartDemoSource {
    type State = UartDemoSourceState;

    fn name() -> &'static str {
        "UART Demo Source"
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
        vec![OutputDef::new::<Signal>("RX")]
    }

    fn state() -> Self::State {
        UartDemoSourceState {
            message: StringValue::new("HELLO\n"),
            baud_rate: IntValue::new(115_200, 300, 100_000_000),
        }
    }
}
