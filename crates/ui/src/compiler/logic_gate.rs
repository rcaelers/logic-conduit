//! `Logic Gate` builder.

use dsl::runtime::ProcessNode;
use dsl::{GateOp, LogicGate, Sample};
use node_graph::Socket;
use serde_json::Value;

use super::{CompileCtx, PortKind, ResolvedInputs, RuntimeBuilder, parse_state};
use crate::nodes;

pub(super) struct LogicGateBuilder;

impl RuntimeBuilder for LogicGateBuilder {
    fn accepted_kinds(&self, _socket: &Socket, _state: &Value) -> Vec<PortKind> {
        vec![PortKind::of::<Sample>()]
    }
    fn offered_kinds(&self, _socket: &Socket, _state: &Value) -> Vec<PortKind> {
        vec![PortKind::of::<Sample>()]
    }
    fn input_port(
        &self,
        _socket: &Socket,
        member_index: usize,
        _state: &Value,
        _kind: PortKind,
    ) -> Option<String> {
        Some(format!("in{member_index}"))
    }
    fn output_port(&self, _socket: &Socket, _state: &Value, _kind: PortKind) -> Option<String> {
        Some("out".into())
    }
    fn build(
        &self,
        name: &str,
        state: &Value,
        resolved: &ResolvedInputs,
        _ctx: &mut CompileCtx,
    ) -> Result<Box<dyn ProcessNode>, String> {
        let state: nodes::LogicGateState = parse_state(state)?;
        let inputs = resolved.member_count(0);
        if inputs == 0 {
            return Err("no inputs connected".into());
        }
        let op = match state.op.selected() {
            "NOT" => GateOp::Not,
            "NAND" => GateOp::Nand,
            "OR" => GateOp::Or,
            "NOR" => GateOp::Nor,
            "XOR" => GateOp::Xor,
            "XNOR" => GateOp::Xnor,
            _ => GateOp::And,
        };
        if op == GateOp::Not && inputs != 1 {
            return Err("NOT takes exactly one input".into());
        }
        Ok(Box::new(LogicGate::new(op, inputs).with_name(name)))
    }
}
