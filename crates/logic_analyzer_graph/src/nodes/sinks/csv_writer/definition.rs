//! `CSV Writer` graph-node definition — decoded words to CSV rows. Generic replacement for
//! ad hoc "dump this decoder to CSV" sinks; pairs with `Viewer` for the
//! console-printer role a one-off example sink used to fill.

use egui::Color32;
use serde::{Deserialize, Serialize};

use node_graph::{
    EnumValue, FileValue, InputDef, IntValue, NodeDef, OutputDef, PanelSection, PropDef,
    StringValue,
};

use crate::nodes::registry::{COLOR_OUTPUT, TextSavePath, Words};

/// Empty save-dialog picker, shown inline on the `Filename` socket while it
/// is unconnected; a connected filename stream always wins.
fn default_csv_writer_filename() -> FileValue {
    FileValue::new_save("", "Save CSV as")
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct CsvWriterState {
    /// Static fallback for an unconnected `Filename` input.
    #[serde(default = "default_csv_writer_filename")]
    pub(crate) filename: FileValue,
    pub(crate) header: StringValue,
    pub(crate) value_format: EnumValue,
    pub(crate) hex_digits: IntValue,
}

pub(crate) struct CsvWriter;
impl NodeDef for CsvWriter {
    type State = CsvWriterState;

    fn name() -> &'static str {
        "CSV Writer"
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
        CsvWriterState {
            filename: default_csv_writer_filename(),
            header: StringValue::new("id,time_ns,value"),
            value_format: EnumValue::new(0, &["Decimal", "Hex"]),
            hex_digits: IntValue::new(6, 1, 16),
        }
    }

    fn panel() -> Vec<PanelSection<Self::State>> {
        vec![PanelSection::new(
            "Options",
            vec![
                PropDef::control("header", "Header (blank = none)", |state| &mut state.header),
                PropDef::control("value_format", "Value format", |state| {
                    &mut state.value_format
                }),
                PropDef::control("hex_digits", "Hex digits", |state| &mut state.hex_digits),
            ],
        )]
    }
}
