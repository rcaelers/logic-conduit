//! `Word Matcher` builder — fires a trigger when a decoded word matches a
//! pattern/mask. Works on any decoder's `Word` output, no decoder-specific
//! knowledge needed (§5.4).

use super::{CompileCtx, PortKind, ResolvedInputs, RuntimeBuilder, parse_hex, parse_state};
use crate::nodes;
use dsl::runtime::{ConfigValue, NodeConfig, ProcessNode};
use dsl::{MatchOp, Sample, Trigger, Word, WordMatcher};
use node_graph::Socket;
use serde_json::Value;

pub(super) struct WordMatcherBuilder;

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
        _ctx: &mut CompileCtx,
    ) -> Result<Box<dyn ProcessNode>, String> {
        let state: nodes::WordMatcherState = parse_state(state)?;
        let pattern = parse_hex(&state.pattern.value)?;
        let mask = parse_hex(&state.mask.value)?;
        let (op, _) = Self::match_op(state.op.selected());
        Ok(Box::new(
            WordMatcher::new(pattern, mask).with_op(op).with_name(name),
        ))
    }

    fn hot_config(&self, state: &Value) -> Option<NodeConfig> {
        let state: nodes::WordMatcherState = parse_state(state).ok()?;
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
        // The pulse-output toggle only affects UI socket visibility.
        Some(config)
    }
}
