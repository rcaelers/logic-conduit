//! `SPI Decoder` node (§4.1).

use super::{COLOR_DECODERS, Signal, Words};
use egui::Color32;
use node_graph::{
    BoolValue, EnumValue, InputDef, IntValue, NodeDef, OutputDef, PanelSection, PropDef, Socket,
};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SpiDecoderState {
    pub word_size: IntValue,
    pub cpol: EnumValue,
    pub cpha: EnumValue,
    pub bit_order: EnumValue,
    pub cs_polarity: EnumValue,
    pub has_miso: BoolValue,
}

pub struct SpiDecoder;
impl NodeDef for SpiDecoder {
    type State = SpiDecoderState;

    fn name() -> &'static str {
        "SPI Decoder"
    }
    fn category() -> &'static str {
        "Decoders"
    }
    fn color() -> Color32 {
        COLOR_DECODERS
    }

    fn inputs() -> Vec<InputDef<Self::State>> {
        vec![
            InputDef::new::<Signal>("CLK"),
            InputDef::new::<Signal>("MOSI"),
            InputDef::new::<Signal>("MISO"),
            InputDef::new::<Signal>("CS#"),
        ]
    }

    fn outputs() -> Vec<OutputDef<Self::State>> {
        vec![
            OutputDef::new::<Words>("MOSI Words"),
            OutputDef::new::<Words>("MISO Words"),
        ]
    }

    fn state() -> Self::State {
        SpiDecoderState {
            word_size: IntValue::new(8, 1, 64),
            cpol: EnumValue::new(0, &["0", "1"]),
            cpha: EnumValue::new(0, &["0", "1"]),
            bit_order: EnumValue::new(0, &["MSB first", "LSB first"]),
            cs_polarity: EnumValue::new(0, &["Active low", "Active high", "Disabled"]),
            has_miso: BoolValue::new(true),
        }
    }

    fn props() -> Vec<PropDef<Self::State>> {
        vec![PropDef::control("word_size", "Word size", |state| {
            &mut state.word_size
        })]
    }

    fn panel() -> Vec<PanelSection<Self::State>> {
        vec![PanelSection::new(
            "Options",
            vec![
                PropDef::control("cpol", "CPOL", |state| &mut state.cpol),
                PropDef::control("cpha", "CPHA", |state| &mut state.cpha),
                PropDef::control("bit_order", "Bit order", |state| &mut state.bit_order),
                PropDef::control("cs_polarity", "CS# polarity", |state| {
                    &mut state.cs_polarity
                }),
                PropDef::control("has_miso", "Has MISO", |state| &mut state.has_miso),
            ],
        )]
    }

    fn on_update(state: &mut Self::State, inputs: &mut [Socket], outputs: &mut [Socket]) {
        if let Some(miso) = inputs.get_mut(2) {
            miso.visible = state.has_miso.value;
        }
        if let Some(miso_words) = outputs.get_mut(1) {
            miso_words.visible = state.has_miso.value;
        }
    }
}
