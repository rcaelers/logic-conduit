//! `UART Decoder` builder.

use super::{CompileCtx, PortKind, ResolvedInputs, RuntimeBuilder, parse_state};
use crate::nodes;
use dsl::nodes::decoders::{BitOrder, UartParity, UartStopBits};
use dsl::runtime::ProcessNode;
use node_graph::Socket;
use serde_json::Value;

pub(super) struct UartDecoderBuilder;

impl RuntimeBuilder for UartDecoderBuilder {
    fn accepted_kinds(&self, _socket: &Socket, _state: &Value) -> Vec<PortKind> {
        vec![PortKind::SampleEdge]
    }
    fn offered_kinds(&self, socket: &Socket, _state: &Value) -> Vec<PortKind> {
        match socket.def_index {
            0 => vec![PortKind::ParallelWords],
            1 => vec![PortKind::Trigger],
            _ => vec![],
        }
    }
    fn input_port(&self, socket: &Socket, _: usize, _: &Value, _: PortKind) -> Option<String> {
        (socket.def_index == 0).then(|| "rx".into())
    }
    fn output_port(&self, socket: &Socket, _state: &Value, _kind: PortKind) -> Option<String> {
        match socket.def_index {
            0 => Some("words".into()),
            1 => Some("error".into()),
            _ => None,
        }
    }
    fn input_required(&self, socket: &Socket, _state: &Value) -> bool {
        socket.def_index == 0
    }
    fn build(
        &self,
        name: &str,
        state: &Value,
        _resolved: &ResolvedInputs,
        _ctx: &mut CompileCtx,
    ) -> Result<Box<dyn ProcessNode>, String> {
        let state: nodes::UartDecoderState = parse_state(state)?;
        let parity = match state.parity.selected() {
            "Odd" => UartParity::Odd,
            "Even" => UartParity::Even,
            "Mark" => UartParity::Mark,
            "Space" => UartParity::Space,
            _ => UartParity::None,
        };
        let stop_bits = match state.stop_bits.selected() {
            "0" => UartStopBits::S0,
            "0.5" => UartStopBits::S0_5,
            "1.5" => UartStopBits::S1_5,
            "2" => UartStopBits::S2,
            _ => UartStopBits::S1,
        };
        let bit_order = if state.bit_order.selected() == "MSB first" {
            BitOrder::MsbFirst
        } else {
            BitOrder::LsbFirst
        };
        let decoder = dsl::nodes::decoders::UartDecoder::new(
            state.baud_rate.value.max(1) as u64,
            state.data_bits.value.clamp(5, 9) as usize,
        )
        .with_parity(parity, state.check_parity.value)
        .with_stop_bits(stop_bits)
        .with_bit_order(bit_order)
        .with_invert(state.invert.value)
        .with_name(name);
        Ok(Box::new(decoder))
    }
}
