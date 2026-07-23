//! `Text File Writer` graph-node definition — generic text-line sink, e.g. for `TGCK
//! Recorder`'s CSV output. Native-only: no filesystem in the browser.

use egui::Color32;

use node_graph::{InputDef, NodeDef, OutputDef};

use crate::nodes::registry::{COLOR_OUTPUT, Text};

pub(crate) struct TextFileWriter;
impl NodeDef for TextFileWriter {
    type State = ();

    fn name() -> &'static str {
        "Text File Writer"
    }
    fn category() -> &'static str {
        "Output"
    }
    fn color() -> Color32 {
        COLOR_OUTPUT
    }

    fn inputs() -> Vec<InputDef<Self::State>> {
        vec![
            InputDef::new::<Text>("Lines"),
            InputDef::new::<Text>("Filename"),
        ]
    }

    fn outputs() -> Vec<OutputDef<Self::State>> {
        vec![]
    }

    fn state() -> Self::State {}
}
