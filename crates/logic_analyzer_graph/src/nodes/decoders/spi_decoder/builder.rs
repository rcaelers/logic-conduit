//! Runtime builder for `SPI Decoder`.

use serde_json::Value;

use logic_analyzer_processing::nodes::decoders::spi_decoder::{SpiDecoder, SpiMode};
use logic_analyzer_processing::types::{BitOrder, CsPolarity};
use logic_analyzer_viewer::{SamplingEdge, ViewerOutputPresentation};
use node_graph::Socket;
use signal_processing::{ProcessNode, Sample, Word};

use crate::decoder_table::DecoderTableColumnPresentation;
use crate::{
    CompileCtx, PortKind, ResolvedInputs, RuntimeBuilder, SamplingOverlayDescriptor,
    SamplingQualifierDescriptor, parse_state,
};

pub(crate) struct SpiDecoderBuilder;

impl SpiDecoderBuilder {
    fn parsed(state: &Value) -> Result<super::definition::SpiDecoderState, String> {
        parse_state(state)
    }
    fn cs_polarity(state: &super::definition::SpiDecoderState) -> CsPolarity {
        match state.cs_polarity.selected() {
            "Active high" => CsPolarity::ActiveHigh,
            "Disabled" => CsPolarity::Disabled,
            _ => CsPolarity::ActiveLow,
        }
    }
}

impl RuntimeBuilder for SpiDecoderBuilder {
    fn viewer_output_presentation(
        &self,
        socket: &Socket,
        _state: &Value,
    ) -> Option<ViewerOutputPresentation> {
        super::presentation::spi_output_presentation(socket.def_index)
    }

    fn decoder_table_column(
        &self,
        socket: &Socket,
        _state: &Value,
    ) -> Option<DecoderTableColumnPresentation> {
        super::presentation::spi_table_column(socket.def_index)
    }

    fn word_display_format(&self, socket: &Socket, state: &Value) -> Option<String> {
        if !matches!(socket.def_index, 3 | 5) {
            return None;
        }
        Self::parsed(state)
            .ok()
            .map(|state| state.display_format.selected().to_string())
    }

    fn sampling_overlay(&self, state: &Value) -> Option<SamplingOverlayDescriptor> {
        let state = Self::parsed(state).ok()?;
        let edge = if state.cpol.selected() == state.cpha.selected() {
            SamplingEdge::Rising
        } else {
            SamplingEdge::Falling
        };
        let mut sampled_input_groups = vec![1];
        if state.has_miso.value {
            sampled_input_groups.push(2);
        }
        Some(SamplingOverlayDescriptor {
            clock_input: 0,
            sampled_input_groups,
            edge,
            qualifiers: match Self::cs_polarity(&state) {
                CsPolarity::ActiveLow => vec![SamplingQualifierDescriptor {
                    input: 3,
                    active_level: false,
                    runtime_fallback: true,
                }],
                CsPolarity::ActiveHigh => vec![SamplingQualifierDescriptor {
                    input: 3,
                    active_level: true,
                    runtime_fallback: true,
                }],
                CsPolarity::Disabled => Vec::new(),
            },
        })
    }

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
        if kind != PortKind::of::<Word>() {
            return None;
        }
        match socket.def_index {
            0 => Some("mosi_words".into()),
            1 => Some("miso_words".into()),
            2 => Some("mosi_bits".into()),
            3 => Some("mosi_data".into()),
            4 => Some("miso_bits".into()),
            5 => Some("miso_data".into()),
            _ => None,
        }
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
        ctx: &mut CompileCtx,
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
        let mut decoder = SpiDecoder::with_cs_polarity(
            mode,
            state.word_size.value.clamp(1, 64) as usize,
            true,
            state.has_miso.value,
            Self::cs_polarity(&state),
        )
        .with_bit_order(bit_order)
        .with_name(name);
        if let Some(activity) = ctx.sampling_activity(name, 3) {
            decoder = decoder.with_cs_activity(activity);
        }
        Ok(Box::new(decoder))
    }
}

#[cfg(test)]
mod tests {
    use node_graph::NodeDef;

    use super::super::definition::SpiDecoder;
    use super::*;

    #[test]
    fn sampling_overlay_uses_spi_sampling_edge() {
        let builder = SpiDecoderBuilder;
        let mut state = SpiDecoder::state();
        for (cpol, cpha, expected) in [
            ("0", "0", SamplingEdge::Rising),
            ("0", "1", SamplingEdge::Falling),
            ("1", "0", SamplingEdge::Falling),
            ("1", "1", SamplingEdge::Rising),
        ] {
            state.cpol.select(cpol);
            state.cpha.select(cpha);
            let descriptor = builder
                .sampling_overlay(&serde_json::to_value(&state).unwrap())
                .unwrap();
            assert_eq!(descriptor.edge, expected);
        }
    }
}
