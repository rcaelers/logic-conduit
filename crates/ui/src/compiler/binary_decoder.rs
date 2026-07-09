//! `Binary Decoder` builder — parallel-bus word assembly from raw blocks.

use super::{CompileCtx, PortKind, ResolvedInputs, RuntimeBuilder, parse_state};
use crate::nodes;
use dsl::nodes::decoders::Endianness;
use dsl::runtime::ProcessNode;
use dsl::{CsPolarity, Sample, SampleBlock, StrobeMode, Word};
use node_graph::Socket;
use serde_json::Value;

pub(super) struct BinaryDecoderBuilder;

impl BinaryDecoderBuilder {
    fn parsed(state: &Value) -> Result<nodes::BinaryDecoderState, String> {
        parse_state(state)
    }
    fn cs_polarity(state: &nodes::BinaryDecoderState) -> CsPolarity {
        match state.cs_polarity.selected() {
            "Active low" => CsPolarity::ActiveLow,
            "Active high" => CsPolarity::ActiveHigh,
            _ => CsPolarity::Disabled,
        }
    }
}

impl RuntimeBuilder for BinaryDecoderBuilder {
    fn accepted_kinds(&self, socket: &Socket, _state: &Value) -> Vec<PortKind> {
        match socket.def_index {
            3 => vec![PortKind::of::<Sample>()], // Enable is a level stream
            _ => vec![PortKind::of::<SampleBlock>()], // Clock, D group, CS read blocks
        }
    }
    fn offered_kinds(&self, _socket: &Socket, _state: &Value) -> Vec<PortKind> {
        vec![PortKind::of::<Word>()]
    }
    fn input_port(
        &self,
        socket: &Socket,
        member_index: usize,
        _state: &Value,
        _kind: PortKind,
    ) -> Option<String> {
        match socket.def_index {
            0 => Some("strobe".into()),
            1 => Some(format!("d{member_index}")),
            2 => Some("cs".into()),
            3 => Some("enable_signal".into()),
            _ => None,
        }
    }
    fn output_port(&self, _socket: &Socket, _state: &Value, kind: PortKind) -> Option<String> {
        (kind == PortKind::of::<Word>()).then(|| "words".into())
    }
    fn input_required(&self, socket: &Socket, state: &Value) -> bool {
        match socket.def_index {
            2 => Self::parsed(state)
                .map(|s| Self::cs_polarity(&s) != CsPolarity::Disabled)
                .unwrap_or(false),
            3 => false, // unconnected Enable = always enabled
            _ => true,
        }
    }
    fn build(
        &self,
        name: &str,
        state: &Value,
        resolved: &ResolvedInputs,
        _ctx: &mut CompileCtx,
    ) -> Result<Box<dyn ProcessNode>, String> {
        let state = Self::parsed(state)?;
        let data_bits = resolved.member_count(1);
        if data_bits == 0 {
            return Err("no data channels connected".into());
        }
        let strobe_mode = match state.sample_on.selected() {
            "Falling (SDR)" => StrobeMode::FallingEdge,
            "Both (DDR)" => StrobeMode::AnyEdge,
            "High level" => StrobeMode::HighLevel,
            "Low level" => StrobeMode::LowLevel,
            _ => StrobeMode::RisingEdge,
        };
        let mut decoder =
            dsl::ParallelDecoder::new(data_bits, strobe_mode, Self::cs_polarity(&state))
                .with_name(name);
        let cycles = state.word_size.value.clamp(1, 8) as usize;
        if cycles > 1 {
            let endianness = if state.endianness.selected() == "Big" {
                Endianness::Big
            } else {
                Endianness::Little
            };
            decoder = decoder.with_word_assembly(cycles, endianness);
        }
        Ok(Box::new(decoder))
    }
}
