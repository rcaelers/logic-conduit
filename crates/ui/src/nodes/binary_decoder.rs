//! `Binary Decoder` node (§4.5).

use super::{COLOR_DECODERS, Signal, Words};
use egui::Color32;
use node_graph::{EnumValue, InputDef, IntValue, NodeDef, OutputDef, PanelSection, PropDef};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BinaryDecoderState {
    pub sample_on: EnumValue,
    pub word_size: IntValue,
    pub endianness: EnumValue,
    pub cs_polarity: EnumValue,
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
            word_size: IntValue::new(1, 1, 8),
            endianness: EnumValue::new(0, &["Little", "Big"]),
            cs_polarity: EnumValue::new(0, &["Disabled", "Active low", "Active high"]),
        }
    }

    fn panel() -> Vec<PanelSection<Self::State>> {
        vec![PanelSection::new(
            "Options",
            vec![
                PropDef::control("sample_on", "Sample on", |state| &mut state.sample_on),
                PropDef::control("word_size", "Word size (cycles)", |state| {
                    &mut state.word_size
                }),
                PropDef::control("endianness", "Endianness", |state| &mut state.endianness),
                PropDef::control("cs_polarity", "CS polarity", |state| &mut state.cs_polarity),
            ],
        )]
    }
}
