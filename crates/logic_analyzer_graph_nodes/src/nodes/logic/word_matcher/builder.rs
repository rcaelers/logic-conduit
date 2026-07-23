//! Runtime builder for `Word Matcher` — fires a trigger when a decoded word matches a
//! pattern/mask. Works on any decoder's `Word` output, no decoder-specific
//! knowledge needed (kind negotiation, `docs/APP_DESIGN.md`).

use serde_json::Value;

use logic_analyzer_graph_api::node::RuntimeBuilder;
use logic_analyzer_graph_api::node_support::{
    NodeBuildContext, PortKind, ResolvedInputs, parse_state,
};
use logic_analyzer_processing::nodes::logic::word_matcher::{MatchOp, TriggerAt, WordMatcher};
use node_graph::Socket;
use signal_processing::{ConfigValue, NodeConfig, ProcessNode, Sample, Trigger, Word};

use super::definition::parse_hex;

#[derive(Default)]
pub(crate) struct WordMatcherBuilder;

impl WordMatcherBuilder {
    /// UI op glyph → runtime `MatchOp` and its config wire name.
    fn match_op(selected: &str) -> (MatchOp, &'static str) {
        match selected {
            "≠" => (MatchOp::Ne, "ne"),
            "<" => (MatchOp::Lt, "lt"),
            "≤" => (MatchOp::Le, "le"),
            ">" => (MatchOp::Gt, "gt"),
            "≥" => (MatchOp::Ge, "ge"),
            _ => (MatchOp::Eq, "eq"),
        }
    }

    /// UI "Trigger at" selection → runtime `TriggerAt` and its wire name.
    fn trigger_at(selected: &str) -> (TriggerAt, &'static str) {
        match selected {
            "Word start" => (TriggerAt::Start, "start"),
            _ => (TriggerAt::End, "end"),
        }
    }
}

impl RuntimeBuilder for WordMatcherBuilder {
    fn accepted_kinds(&self, _socket: &Socket, _state: &Value) -> Vec<PortKind> {
        vec![PortKind::of::<Word>()]
    }
    fn offered_kinds(&self, socket: &Socket, _state: &Value) -> Vec<PortKind> {
        match socket.def_index {
            0 => vec![PortKind::of::<Trigger>()],
            1 => vec![PortKind::of::<Sample>()],
            _ => vec![],
        }
    }
    fn input_port(&self, socket: &Socket, _: usize, _: &Value, _: PortKind) -> Option<String> {
        (socket.def_index == 0).then(|| "words".into())
    }
    fn output_port(&self, socket: &Socket, _state: &Value, _kind: PortKind) -> Option<String> {
        match socket.def_index {
            0 => Some("trigger".into()),
            1 => Some("matched".into()),
            _ => None,
        }
    }
    fn build(
        &self,
        name: &str,
        state: &Value,
        _resolved: &ResolvedInputs,
        _ctx: &mut dyn NodeBuildContext,
    ) -> Result<Box<dyn ProcessNode>, String> {
        let state: super::definition::WordMatcherState = parse_state(state)?;
        let pattern = parse_hex(&state.pattern.value)?;
        let mask = parse_hex(&state.mask.value)?;
        let (op, _) = Self::match_op(state.op.selected());
        let (trigger_at, _) = Self::trigger_at(state.trigger_at.selected());
        Ok(Box::new(
            WordMatcher::new(pattern, mask)
                .with_op(op)
                .with_trigger_at(trigger_at)
                .with_name(name),
        ))
    }

    fn hot_config(&self, state: &Value) -> Option<NodeConfig> {
        let state: super::definition::WordMatcherState = parse_state(state).ok()?;
        let mut config = NodeConfig::new();
        config.insert(
            "pattern".into(),
            ConfigValue::U64(parse_hex(&state.pattern.value).ok()?),
        );
        config.insert(
            "mask".into(),
            ConfigValue::U64(parse_hex(&state.mask.value).ok()?),
        );
        let (_, op_name) = Self::match_op(state.op.selected());
        config.insert("op".into(), ConfigValue::Text(op_name.into()));
        let (_, trigger_at_name) = Self::trigger_at(state.trigger_at.selected());
        config.insert(
            "trigger_at".into(),
            ConfigValue::Text(trigger_at_name.into()),
        );
        // The pulse-output toggle only affects UI socket visibility.
        Some(config)
    }
}
