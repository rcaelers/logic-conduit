//! `SPI Decoder` builder.

use serde_json::Value;

use node_graph::Socket;
use signal_processing::nodes::decoders::BitOrder;
use signal_processing::{CsPolarity, ProcessNode, Sample, SpiDecoder, SpiMode, Word};

use super::graph::{CompileCtx, ResolvedInputs, RuntimeBuilder, parse_state};
use super::port_kind::PortKind;
use crate::nodes;

pub(super) struct SpiDecoderBuilder;

impl SpiDecoderBuilder {
    fn parsed(state: &Value) -> Result<nodes::SpiDecoderState, String> {
        parse_state(state)
    }
    fn cs_polarity(state: &nodes::SpiDecoderState) -> CsPolarity {
        match state.cs_polarity.selected() {
            "Active high" => CsPolarity::ActiveHigh,
            "Disabled" => CsPolarity::Disabled,
            _ => CsPolarity::ActiveLow,
        }
    }
}

impl RuntimeBuilder for SpiDecoderBuilder {
    fn accepted_kinds(&self, _socket: &Socket, _state: &Value) -> Vec<PortKind> {
        vec![PortKind::of::<Sample>()]
    }
    fn offered_kinds(&self, _socket: &Socket, _state: &Value) -> Vec<PortKind> {
        vec![PortKind::of::<Word>()]
    }
    fn input_port(&self, socket: &Socket, _: usize, _: &Value, _: PortKind) -> Option<String> {
        match socket.def_index {
            0 => Some("clk".into()),
            1 => Some("mosi".into()),
            2 => Some("miso".into()),
            3 => Some("cs".into()),
            _ => None,
        }
    }
    fn output_port(&self, socket: &Socket, _state: &Value, kind: PortKind) -> Option<String> {
        (kind == PortKind::of::<Word>()).then(|| {
            match socket.def_index {
                0 => "mosi_words",
                _ => "miso_words",
            }
            .into()
        })
    }
    fn input_required(&self, socket: &Socket, state: &Value) -> bool {
        let Ok(state) = Self::parsed(state) else {
            return true;
        };
        match socket.def_index {
            2 => state.has_miso.value,
            3 => Self::cs_polarity(&state) != CsPolarity::Disabled,
            _ => true,
        }
    }
    fn build(
        &self,
        name: &str,
        state: &Value,
        _resolved: &ResolvedInputs,
        _ctx: &mut CompileCtx,
    ) -> Result<Box<dyn ProcessNode>, String> {
        let state = Self::parsed(state)?;
        let mode = match (state.cpol.selected(), state.cpha.selected()) {
            ("0", "0") => SpiMode::Mode0,
            ("0", "1") => SpiMode::Mode1,
            ("1", "0") => SpiMode::Mode2,
            ("1", "1") => SpiMode::Mode3,
            _ => return Err("invalid CPOL/CPHA".into()),
        };
        let bit_order = if state.bit_order.selected() == "LSB first" {
            BitOrder::LsbFirst
        } else {
            BitOrder::MsbFirst
        };
        let decoder = SpiDecoder::with_cs_polarity(
            mode,
            state.word_size.value.clamp(1, 64) as usize,
            true,
            state.has_miso.value,
            Self::cs_polarity(&state),
        )
        .with_bit_order(bit_order)
        .with_name(name);
        Ok(Box::new(decoder))
    }
}
