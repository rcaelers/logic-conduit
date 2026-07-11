//! `TGCK Recorder` node.

use super::{COLOR_OUTPUT, Signal, Text, Words};
use egui::Color32;
use node_graph::{InputDef, NodeDef, OutputDef};

pub struct TgckRecorder;
impl NodeDef for TgckRecorder {
    type State = ();

    fn name() -> &'static str {
        "TGCK Recorder"
    }
    fn category() -> &'static str {
        "Output"
    }
    fn color() -> Color32 {
        COLOR_OUTPUT
    }

    fn inputs() -> Vec<InputDef<Self::State>> {
        vec![
            InputDef::new::<Words>("Words"),
            InputDef::new::<Signal>("TGCK"),
            InputDef::new::<Text>("Filename"),
        ]
    }

    fn outputs() -> Vec<OutputDef<Self::State>> {
        // No file I/O here — `Rows` (CSV lines) and `Filename` (the derived
        // `_tgck.csv` path) feed a `Text File Writer` sink.
        vec![
            OutputDef::new::<Text>("Rows"),
            OutputDef::new::<Text>("Filename"),
        ]
    }

    fn state() -> Self::State {}
}
