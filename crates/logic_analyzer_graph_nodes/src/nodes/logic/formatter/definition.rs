//! `String Formatter` graph-node definition.

use egui::Color32;
use serde::{Deserialize, Serialize};

use node_graph::{InputDef, NodeDef, OutputDef, PropDef, StringValue};

use crate::nodes::registry::{COLOR_LOGIC, Number, Text};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct StringFormatterState {
    pub(crate) template: StringValue,
}

pub(crate) struct StringFormatter;
impl NodeDef for StringFormatter {
    type State = StringFormatterState;

    fn name() -> &'static str {
        "String Formatter"
    }
    fn category() -> &'static str {
        "Logic"
    }
    fn color() -> Color32 {
        COLOR_LOGIC
    }

    fn inputs() -> Vec<InputDef<Self::State>> {
        // Additional values appear in the template as {1}, {2}, … ({0} and
        // the legacy {n} are the first input).
        vec![InputDef::new::<Number>("Value").variadic(4)]
    }

    fn outputs() -> Vec<OutputDef<Self::State>> {
        vec![OutputDef::new::<Text>("Text")]
    }

    fn state() -> Self::State {
        StringFormatterState {
            template: StringValue::new("output/capture_{n:04}.bin"),
        }
    }

    fn props() -> Vec<PropDef<Self::State>> {
        vec![PropDef::control("template", "Template", |state| {
            &mut state.template
        })]
    }
}
