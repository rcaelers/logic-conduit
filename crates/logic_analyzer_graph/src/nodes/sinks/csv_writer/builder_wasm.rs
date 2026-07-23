//! Browser runtime builder for `CSV Writer`.

use serde_json::Value;

use logic_analyzer_processing::nodes::sinks::discard_writer::DiscardWordWriter;
use node_graph::Socket;
use signal_processing::{ProcessNode, TextSample, Word};

use crate::{NodeBuildContext, PortKind, ResolvedInputs, RuntimeBuilder, parse_state};

#[derive(Default)]
pub(crate) struct CsvWriterBuilder;

impl RuntimeBuilder for CsvWriterBuilder {
    fn is_sink(&self) -> bool {
        true
    }

    fn accepted_kinds(&self, socket: &Socket, _state: &Value) -> Vec<PortKind> {
        match socket.def_index {
            0 => vec![PortKind::of::<Word>()],
            1 => vec![PortKind::of::<TextSample>()],
            _ => Vec::new(),
        }
    }

    fn offered_kinds(&self, _socket: &Socket, _state: &Value) -> Vec<PortKind> {
        Vec::new()
    }

    fn input_port(&self, socket: &Socket, _: usize, _: &Value, _: PortKind) -> Option<String> {
        match socket.def_index {
            0 => Some("data".to_owned()),
            1 => Some("filename".to_owned()),
            _ => None,
        }
    }

    fn output_port(&self, _: &Socket, _: &Value, _: PortKind) -> Option<String> {
        None
    }

    fn input_required(&self, socket: &Socket, state: &Value) -> bool {
        match socket.def_index {
            1 => parse_state::<super::definition::CsvWriterState>(state)
                .map(|state| state.filename.value.trim().is_empty())
                .unwrap_or(true),
            _ => true,
        }
    }

    fn build(
        &self,
        name: &str,
        _state: &Value,
        _resolved: &ResolvedInputs,
        _ctx: &mut dyn NodeBuildContext,
    ) -> Result<Box<dyn ProcessNode>, String> {
        Ok(Box::new(DiscardWordWriter::new(name)))
    }
}
