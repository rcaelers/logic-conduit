//! `UART Decoder` node (§4.13, single line).

use super::{COLOR_DECODERS, Signal, Trigger, Words};
use egui::Color32;
use node_graph::{
    BoolValue, EnumValue, InputDef, IntValue, NodeDef, OutputDef, PanelSection, PropDef, Socket,
};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UartDecoderState {
    pub baud_rate: IntValue,
    pub data_bits: IntValue,
    pub parity: EnumValue,
    pub check_parity: BoolValue,
    pub stop_bits: EnumValue,
    pub bit_order: EnumValue,
    pub invert: BoolValue,
    pub error_output: BoolValue,
}

pub struct UartDecoder;
impl NodeDef for UartDecoder {
    type State = UartDecoderState;

    fn name() -> &'static str {
        "UART Decoder"
    }
    fn category() -> &'static str {
        "Decoders"
    }
    fn color() -> Color32 {
        COLOR_DECODERS
    }

    fn inputs() -> Vec<InputDef<Self::State>> {
        vec![
            InputDef::new::<Signal>("RX/TX"),
            InputDef::control::<node_graph::IntSocket>("Baud Rate", |state| &mut state.baud_rate),
        ]
    }

    fn outputs() -> Vec<OutputDef<Self::State>> {
        vec![
            OutputDef::new::<Words>("Words"),
            OutputDef::new::<Trigger>("Error"),
        ]
    }

    fn state() -> Self::State {
        UartDecoderState {
            baud_rate: IntValue::new(1_000_000, 300, 100_000_000),
            data_bits: IntValue::new(8, 5, 9),
            parity: EnumValue::new(0, &["None", "Odd", "Even", "Mark", "Space"]),
            check_parity: BoolValue::new(false),
            stop_bits: EnumValue::new(2, &["0", "0.5", "1", "1.5", "2"]),
            bit_order: EnumValue::new(0, &["LSB first", "MSB first"]),
            invert: BoolValue::new(false),
            error_output: BoolValue::new(false),
        }
    }

    fn panel() -> Vec<PanelSection<Self::State>> {
        vec![PanelSection::new(
            "Options",
            vec![
                PropDef::control("data_bits", "Data bits", |state| &mut state.data_bits),
                PropDef::control("parity", "Parity", |state| &mut state.parity),
                PropDef::control("check_parity", "Check parity", |state| {
                    &mut state.check_parity
                }),
                PropDef::control("stop_bits", "Stop bits", |state| &mut state.stop_bits),
                PropDef::control("bit_order", "Bit order", |state| &mut state.bit_order),
                PropDef::control("invert", "Invert signal", |state| &mut state.invert),
                PropDef::control("error_output", "Error output", |state| {
                    &mut state.error_output
                }),
            ],
        )]
    }

    fn on_update(state: &mut Self::State, _inputs: &mut [Socket], outputs: &mut [Socket]) {
        if let Some(error) = outputs.get_mut(1) {
            error.visible = state.error_output.value;
        }
    }
}
