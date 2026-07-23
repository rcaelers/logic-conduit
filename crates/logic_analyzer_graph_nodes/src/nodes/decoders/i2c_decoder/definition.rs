//! `I2C Decoder` graph-node definition — demo placeholder. No matching runtime builder
//! exists (nothing implements the decode), so the node is editable but not
//! runnable.

use egui::Color32;

use node_graph::{InputDef, NodeDef, OutputDef};

use crate::nodes::registry::{COLOR_DECODERS, Signal, Words};

pub(crate) struct I2cDecoder;
impl NodeDef for I2cDecoder {
    type State = ();

    fn name() -> &'static str {
        "I2C Decoder"
    }
    fn category() -> &'static str {
        "Decoders"
    }
    fn color() -> Color32 {
        COLOR_DECODERS
    }

    fn inputs() -> Vec<InputDef<Self::State>> {
        vec![
            InputDef::new::<Signal>("SCL"),
            InputDef::new::<Signal>("SDA"),
        ]
    }

    fn outputs() -> Vec<OutputDef<Self::State>> {
        vec![OutputDef::new::<Words>("Words")]
    }

    fn state() -> Self::State {}
}
