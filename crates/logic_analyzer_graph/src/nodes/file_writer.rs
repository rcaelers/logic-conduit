//! `File Writer` node.

use egui::Color32;
use serde::{Deserialize, Serialize};

use node_graph::{
    BoolValue, EnumValue, FileValue, InputDef, NodeDef, OutputDef, PanelSection, PropDef,
};

use super::registry::{COLOR_OUTPUT, TextSavePath, Words};

/// Empty save-dialog picker, shown inline on the `Filename` socket while it
/// is unconnected; a connected filename stream always wins.
pub fn default_writer_filename() -> FileValue {
    FileValue::new_save("", "Save capture as")
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileWriterState {
    /// Static fallback for an unconnected `Filename` input.
    #[serde(default = "default_writer_filename")]
    pub filename: FileValue,
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
            // While unconnected, the socket shows a save-file picker in the
            // node body — the static path the writer falls back to.
            InputDef::control::<TextSavePath>("Filename", |state| &mut state.filename),
        ]
    }

    fn outputs() -> Vec<OutputDef<Self::State>> {
        vec![]
    }

    fn state() -> Self::State {
        FileWriterState {
            filename: default_writer_filename(),
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
