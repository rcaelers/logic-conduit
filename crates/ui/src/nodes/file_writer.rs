//! `File Writer` node (§4.8).

use super::{COLOR_OUTPUT, Text, Words};
use egui::Color32;
use node_graph::{BoolValue, EnumValue, InputDef, NodeDef, OutputDef, PanelSection, PropDef};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileWriterState {
    pub write_width: EnumValue,
    pub index_csv: BoolValue,
}

pub struct FileWriter;
impl NodeDef for FileWriter {
    type State = FileWriterState;

    fn name() -> &'static str {
        "File Writer"
    }
    fn category() -> &'static str {
        "Output"
    }
    fn color() -> Color32 {
        COLOR_OUTPUT
    }

    fn inputs() -> Vec<InputDef<Self::State>> {
        vec![
            InputDef::new::<Words>("Data"),
            InputDef::new::<Text>("Filename"),
        ]
    }

    fn outputs() -> Vec<OutputDef<Self::State>> {
        vec![]
    }

    fn state() -> Self::State {
        FileWriterState {
            write_width: EnumValue::new(0, &["U8 (low byte)", "U16 LE", "U32 LE"]),
            index_csv: BoolValue::new(true),
        }
    }

    fn panel() -> Vec<PanelSection<Self::State>> {
        vec![PanelSection::new(
            "Options",
            vec![
                PropDef::control("write_width", "Write", |state| &mut state.write_width),
                PropDef::control("index_csv", "Index CSV", |state| &mut state.index_csv),
            ],
        )]
    }
}
