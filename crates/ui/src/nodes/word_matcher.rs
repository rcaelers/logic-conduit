//! `Word Matcher` node (§4.2).

use super::{COLOR_LOGIC, Signal, Trigger, Words};
use egui::Color32;
use node_graph::{
    BoolValue, EnumValue, InputDef, NodeBadge, NodeDef, OutputDef, PanelSection, PropDef, Socket,
    StringValue,
};
use serde::{Deserialize, Serialize};

pub const MATCH_OPS: &[&str] = &["==", "≠", "<", "≤", ">", "≥"];

pub fn default_match_op() -> EnumValue {
    EnumValue::new(0, MATCH_OPS)
}

fn parse_hex(text: &str) -> Option<u64> {
    let trimmed = text.trim();
    let digits = trimmed
        .strip_prefix("0x")
        .or_else(|| trimmed.strip_prefix("0X"))
        .unwrap_or(trimmed);
    u64::from_str_radix(digits, 16).ok()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WordMatcherState {
    pub pattern: StringValue,
    pub mask: StringValue,
    /// Comparison of the masked word against the masked pattern.
    #[serde(default = "default_match_op")]
    pub op: EnumValue,
    pub pulse_output: BoolValue,
}

pub struct WordMatcher;
impl NodeDef for WordMatcher {
    type State = WordMatcherState;

    fn name() -> &'static str {
        "Word Matcher"
    }
    fn category() -> &'static str {
        "Logic"
    }
    fn color() -> Color32 {
        COLOR_LOGIC
    }

    fn inputs() -> Vec<InputDef<Self::State>> {
        vec![InputDef::new::<Words>("Words")]
    }

    fn outputs() -> Vec<OutputDef<Self::State>> {
        vec![
            OutputDef::new::<Trigger>("Match"),
            OutputDef::new::<Signal>("Matched"),
        ]
    }

    fn state() -> Self::State {
        WordMatcherState {
            pattern: StringValue::new("0x000000"),
            mask: StringValue::new("0xFFFFFF"),
            op: default_match_op(),
            pulse_output: BoolValue::new(false),
        }
    }

    fn props() -> Vec<PropDef<Self::State>> {
        vec![PropDef::control("pattern", "Pattern", |state| {
            &mut state.pattern
        })]
    }

    fn panel() -> Vec<PanelSection<Self::State>> {
        vec![PanelSection::new(
            "Options",
            vec![
                PropDef::control("op", "Compare", |state| &mut state.op),
                PropDef::control("mask", "Mask", |state| &mut state.mask),
                PropDef::control("pulse_output", "Pulse output", |state| {
                    &mut state.pulse_output
                }),
            ],
        )]
    }

    fn on_update(state: &mut Self::State, _inputs: &mut [Socket], outputs: &mut [Socket]) {
        if let Some(matched) = outputs.get_mut(1) {
            matched.visible = state.pulse_output.value;
        }
    }

    fn badge(state: &Self::State) -> Option<NodeBadge> {
        if parse_hex(&state.pattern.value).is_none() {
            return Some(NodeBadge::error("Invalid hex pattern"));
        }
        if parse_hex(&state.mask.value).is_none() {
            return Some(NodeBadge::error("Invalid hex mask"));
        }
        None
    }
}
