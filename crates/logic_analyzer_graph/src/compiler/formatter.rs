//! `String Formatter` builder.

use serde_json::Value;

use node_graph::Socket;
use signal_processing::{
    ConfigValue, NodeConfig, NumberSample, ProcessNode, TextFormatter, TextSample,
};

use super::graph::{CompileCtx, ResolvedInputs, RuntimeBuilder, parse_state};
use super::port_kind::PortKind;
use crate::nodes;

pub(super) struct FormatterBuilder;

impl RuntimeBuilder for FormatterBuilder {
    fn accepted_kinds(&self, _socket: &Socket, _state: &Value) -> Vec<PortKind> {
        vec![PortKind::of::<NumberSample>()]
    }
    fn offered_kinds(&self, _socket: &Socket, _state: &Value) -> Vec<PortKind> {
        vec![PortKind::of::<TextSample>()]
    }
    fn input_port(
        &self,
        _socket: &Socket,
        member_index: usize,
        _: &Value,
        _: PortKind,
    ) -> Option<String> {
        // First value keeps the historic port name.
        Some(if member_index == 0 {
            "value".into()
        } else {
            format!("value{member_index}")
        })
    }
    fn output_port(&self, _socket: &Socket, _state: &Value, _kind: PortKind) -> Option<String> {
        Some("text".into())
    }
    fn build(
        &self,
        name: &str,
        state: &Value,
        resolved: &ResolvedInputs,
        _ctx: &mut CompileCtx,
    ) -> Result<Box<dyn ProcessNode>, String> {
        let state: nodes::StringFormatterState = parse_state(state)?;
        let values = resolved.member_count(0).max(1);
        Ok(Box::new(
            TextFormatter::with_num_values(state.template.value.clone(), values).with_name(name),
        ))
    }

    fn hot_config(&self, state: &Value) -> Option<NodeConfig> {
        let state: nodes::StringFormatterState = parse_state(state).ok()?;
        let mut config = NodeConfig::new();
        config.insert(
            "template".into(),
            ConfigValue::Text(state.template.value.clone()),
        );
        Some(config)
    }
}
