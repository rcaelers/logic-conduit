//! `Binary Decoder` node.

use super::{COLOR_DECODERS, Signal, Words};
use egui::Color32;
use node_graph::{EnumValue, InputDef, IntValue, NodeDef, OutputDef, PanelSection, PropDef};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BinaryDecoderState {
    #[serde(default = "super::uart_decoder::default_display_format")]
    pub display_format: EnumValue,
    pub sample_on: EnumValue,
    #[serde(default = "default_input_strategy")]
    pub input_strategy: EnumValue,
    pub word_size: IntValue,
    pub endianness: EnumValue,
    pub cs_polarity: EnumValue,
}

pub fn default_input_strategy() -> EnumValue {
    EnumValue::new(0, &["Auto", "Packed stream", "Indexed"])
}

pub struct BinaryDecoder;
impl NodeDef for BinaryDecoder {
    type State = BinaryDecoderState;

    fn name() -> &'static str {
        "Binary Decoder"
    }
    fn category() -> &'static str {
        "Decoders"
    }
    fn color() -> Color32 {
        COLOR_DECODERS
    }

    fn inputs() -> Vec<InputDef<Self::State>> {
        vec![
            InputDef::new::<Signal>("Clock"),
            InputDef::new::<Signal>("D").variadic(32),
            InputDef::new::<Signal>("CS"),
            InputDef::new::<Signal>("Enable"),
        ]
    }

    fn outputs() -> Vec<OutputDef<Self::State>> {
        vec![OutputDef::new::<Words>("Words")]
    }

    fn state() -> Self::State {
        BinaryDecoderState {
            display_format: super::uart_decoder::default_display_format(),
            sample_on: EnumValue::new(
                0,
                &[
                    "Rising (SDR)",
                    "Falling (SDR)",
                    "Both (DDR)",
                    "High level",
                    "Low level",
                ],
            ),
            input_strategy: default_input_strategy(),
            word_size: IntValue::new(1, 1, 8),
            endianness: EnumValue::new(0, &["Little", "Big"]),
            cs_polarity: EnumValue::new(0, &["Disabled", "Active low", "Active high"]),
        }
    }

    fn panel() -> Vec<PanelSection<Self::State>> {
        vec![PanelSection::new(
            "Options",
            vec![
                PropDef::control("display_format", "Data display", |state| {
                    &mut state.display_format
                }),
                PropDef::control("sample_on", "Sample on", |state| &mut state.sample_on),
                PropDef::control("input_strategy", "Input strategy", |state| {
                    &mut state.input_strategy
                }),
                PropDef::control("word_size", "Word size (cycles)", |state| {
                    &mut state.word_size
                }),
                PropDef::control("endianness", "Endianness", |state| &mut state.endianness),
                PropDef::control("cs_polarity", "CS polarity", |state| &mut state.cs_polarity),
            ],
        )]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn older_state_without_input_strategy_defaults_to_auto() {
        let mut value = serde_json::to_value(BinaryDecoder::state()).unwrap();
        value.as_object_mut().unwrap().remove("input_strategy");

        let state: BinaryDecoderState = serde_json::from_value(value).unwrap();
        assert_eq!(state.input_strategy.selected(), "Auto");
    }
}
