//! Runtime builder for `Binary Decoder`.

use serde_json::Value;

use logic_analyzer_processing::nodes::decoders::{
    ParallelDecoder, ParallelInputStrategy, StrobeMode,
};
use logic_analyzer_processing::types::{CsPolarity, Endianness};
use logic_analyzer_viewer::SamplingEdge;
use node_graph::Socket;
use signal_processing::{ProcessNode, Sample, SampleBlock, Word};

use crate::decoder_table::DecoderTableColumnPresentation;
use crate::{
    CompileCtx, PortKind, ResolvedInputs, RuntimeBuilder, SamplingOverlayDescriptor,
    SamplingQualifierDescriptor, parse_state,
};

pub(crate) struct BinaryDecoderBuilder;

impl BinaryDecoderBuilder {
    fn parsed(state: &Value) -> Result<super::definition::BinaryDecoderState, String> {
        parse_state(state)
    }
    fn cs_polarity(state: &super::definition::BinaryDecoderState) -> CsPolarity {
        match state.cs_polarity.selected() {
            "Active low" => CsPolarity::ActiveLow,
            "Active high" => CsPolarity::ActiveHigh,
            _ => CsPolarity::Disabled,
        }
    }
}

impl RuntimeBuilder for BinaryDecoderBuilder {
    fn decoder_table_column(
        &self,
        socket: &Socket,
        _state: &Value,
    ) -> Option<DecoderTableColumnPresentation> {
        super::presentation::binary_table_column(socket.def_index)
    }

    fn sampling_overlay(&self, state: &Value) -> Option<SamplingOverlayDescriptor> {
        let state = Self::parsed(state).ok()?;
        let edge = match state.sample_on.selected() {
            "Rising (SDR)" => SamplingEdge::Rising,
            "Falling (SDR)" => SamplingEdge::Falling,
            "Both (DDR)" => SamplingEdge::Both,
            _ => return None,
        };
        Some(SamplingOverlayDescriptor {
            clock_input: 0,
            sampled_input_groups: vec![1],
            edge,
            qualifiers: {
                let mut qualifiers = Vec::new();
                match Self::cs_polarity(&state) {
                    CsPolarity::ActiveLow => qualifiers.push(SamplingQualifierDescriptor {
                        input: 2,
                        active_level: false,
                        runtime_fallback: false,
                    }),
                    CsPolarity::ActiveHigh => qualifiers.push(SamplingQualifierDescriptor {
                        input: 2,
                        active_level: true,
                        runtime_fallback: false,
                    }),
                    CsPolarity::Disabled => {}
                }
                qualifiers.push(SamplingQualifierDescriptor {
                    input: 3,
                    active_level: true,
                    runtime_fallback: true,
                });
                qualifiers
            },
        })
    }

    fn word_display_format(&self, _socket: &Socket, state: &Value) -> Option<String> {
        Self::parsed(state)
            .ok()
            .map(|state| state.display_format.selected().to_string())
    }
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
        ctx: &mut CompileCtx,
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
        let mut decoder = ParallelDecoder::new(data_bits, strobe_mode, Self::cs_polarity(&state))
            .with_name(name)
            .with_input_strategy(match state.input_strategy.selected() {
                "Packed stream" => ParallelInputStrategy::PackedStream,
                "Indexed" => ParallelInputStrategy::Indexed,
                _ => ParallelInputStrategy::Auto,
            });
        if let Some(activity) = ctx.sampling_activity(name, 3) {
            decoder = decoder.with_enable_activity(activity);
        }
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

#[cfg(test)]
mod tests {
    use node_graph::NodeDef;

    use super::super::definition::BinaryDecoder;
    use super::*;

    #[test]
    fn sampling_overlay_follows_edge_mode_and_ignores_level_modes() {
        let builder = BinaryDecoderBuilder;
        let mut state = BinaryDecoder::state();
        for (mode, expected) in [
            ("Rising (SDR)", Some(SamplingEdge::Rising)),
            ("Falling (SDR)", Some(SamplingEdge::Falling)),
            ("Both (DDR)", Some(SamplingEdge::Both)),
            ("High level", None),
            ("Low level", None),
        ] {
            state.sample_on.select(mode);
            let descriptor = builder.sampling_overlay(&serde_json::to_value(&state).unwrap());
            assert_eq!(descriptor.map(|descriptor| descriptor.edge), expected);
        }
    }
}
