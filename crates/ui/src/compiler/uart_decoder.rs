//! `UART Decoder` builder.

use dsl::nodes::decoders::{BitOrder, UartParity, UartStopBits};
use dsl::runtime::ProcessNode;
use dsl::{Sample, Trigger, Word};
use node_graph::Socket;
use serde_json::Value;

use super::{CompileCtx, PortKind, ResolvedInputs, RuntimeBuilder, parse_state};
use crate::nodes;

pub(super) struct UartDecoderBuilder;

impl RuntimeBuilder for UartDecoderBuilder {
    fn word_display_format(&self, socket: &Socket, state: &Value) -> Option<String> {
        if socket.def_index == 3 {
            parse_state::<nodes::UartDecoderState>(state)
                .ok()
                .map(|state| state.display_format.selected().to_string())
        } else {
            None
        }
    }
    fn accepted_kinds(&self, _socket: &Socket, _state: &Value) -> Vec<PortKind> {
        vec![PortKind::of::<Sample>()]
    }
    fn offered_kinds(&self, socket: &Socket, _state: &Value) -> Vec<PortKind> {
        match socket.def_index {
            0 | 2 | 3 => vec![PortKind::of::<Word>()],
            1 => vec![PortKind::of::<Trigger>()],
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
            2 => Some("bits".into()),
            3 => Some("frame".into()),
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
            nodes::selected_baud_rate(&state).max(1) as u64,
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
