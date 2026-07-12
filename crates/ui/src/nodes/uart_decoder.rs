//! `UART Decoder` node (single line).

use super::{COLOR_DECODERS, Signal, Trigger, Words};
use egui::Color32;
use node_graph::{
    BoolValue, EnumValue, InputDef, IntValue, NodeDef, OutputDef, PanelSection, PropDef, Socket,
};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UartDecoderState {
    #[serde(default = "default_display_format")]
    pub display_format: EnumValue,
    /// A common baud-rate preset, or `Custom` to use `baud_rate` below.
    #[serde(default = "default_baud_preset")]
    pub baud_preset: EnumValue,
    /// Custom baud rate retained for saved-graph compatibility.
    pub baud_rate: IntValue,
    pub data_bits: IntValue,
    pub parity: EnumValue,
    pub check_parity: BoolValue,
    pub stop_bits: EnumValue,
    pub bit_order: EnumValue,
    pub invert: BoolValue,
    pub error_output: BoolValue,
}

pub const DISPLAY_FORMATS: &[&str] = &["Hex", "Binary", "Octal", "Decimal", "ASCII", "Hex + ASCII"];
pub fn default_display_format() -> EnumValue {
    EnumValue::new(0, DISPLAY_FORMATS)
}

const BAUD_PRESETS: &[&str] = &[
    "300",
    "1,200",
    "2,400",
    "4,800",
    "9,600",
    "19,200",
    "38,400",
    "57,600",
    "115,200",
    "230,400",
    "460,800",
    "921,600",
    "1,000,000",
    "Custom",
];

pub fn default_baud_preset() -> EnumValue {
    // Old saved graphs have only `baud_rate`; selecting Custom preserves
    // that value when they are deserialized.
    EnumValue::new(13, BAUD_PRESETS)
}

pub fn selected_baud_rate(state: &UartDecoderState) -> i32 {
    match state.baud_preset.selected() {
        "300" => 300,
        "1,200" => 1_200,
        "2,400" => 2_400,
        "4,800" => 4_800,
        "9,600" => 9_600,
        "19,200" => 19_200,
        "38,400" => 38_400,
        "57,600" => 57_600,
        "115,200" => 115_200,
        "230,400" => 230_400,
        "460,800" => 460_800,
        "921,600" => 921_600,
        "1,000,000" => 1_000_000,
        _ => state.baud_rate.value,
    }
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
        vec![InputDef::new::<Signal>("RX/TX")]
    }

    fn outputs() -> Vec<OutputDef<Self::State>> {
        vec![
            OutputDef::new::<Words>("Data"),
            OutputDef::new::<Trigger>("Error"),
            OutputDef::new::<Words>("Bits"),
            OutputDef::new::<Words>("Data"),
        ]
    }

    fn state() -> Self::State {
        UartDecoderState {
            display_format: default_display_format(),
            baud_preset: EnumValue::new(12, BAUD_PRESETS),
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
                PropDef::control("display_format", "Data display", |state| {
                    &mut state.display_format
                }),
                PropDef::control("baud_preset", "Baud rate", |state| &mut state.baud_preset),
                PropDef::control("baud_rate", "Custom baud rate", |state| {
                    &mut state.baud_rate
                }),
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
        // Runtime port 0 is the legacy `words` stream. Keep it alive for
        // older graphs that are already wired to it, but remove it from the
        // node and View panels; new graphs use the framed Data output.
        if let Some(words) = outputs.get_mut(0) {
            words.visible = false;
        }
        if let Some(error) = outputs.get_mut(1) {
            error.visible = state.error_output.value;
        }
    }
}
