//! `CSV Writer` builder — writes decoded words to a CSV file. Native-only:
//! no filesystem in the browser.

use serde_json::Value;

use signal_processing::{CsvValueFormat, CsvWordWriter, ProcessNode, TextSample, Word};
use node_graph::Socket;

use super::{CompileCtx, PortKind, ResolvedInputs, RuntimeBuilder, parse_state};
use crate::nodes;

pub(super) struct CsvWriterBuilder;

impl RuntimeBuilder for CsvWriterBuilder {
    fn is_sink(&self) -> bool {
        true
    }
    fn accepted_kinds(&self, socket: &Socket, _state: &Value) -> Vec<PortKind> {
        match socket.def_index {
            0 => vec![PortKind::of::<Word>()],
            1 => vec![PortKind::of::<TextSample>()],
            _ => vec![],
        }
    }
    fn offered_kinds(&self, _socket: &Socket, _state: &Value) -> Vec<PortKind> {
        vec![]
    }
    fn input_port(&self, socket: &Socket, _: usize, _: &Value, _: PortKind) -> Option<String> {
        match socket.def_index {
            0 => Some("data".into()),
            1 => Some("filename".into()),
            _ => None,
        }
    }
    fn output_port(&self, _: &Socket, _: &Value, _: PortKind) -> Option<String> {
        None
    }
    fn input_required(&self, socket: &Socket, state: &Value) -> bool {
        match socket.def_index {
            // The Filename input can stay unconnected when the node's own
            // static filename (save-dialog prop) is set.
            1 => parse_state::<nodes::CsvWriterState>(state)
                .map(|state| state.filename.value.trim().is_empty())
                .unwrap_or(true),
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
        let state: nodes::CsvWriterState = parse_state(state)?;
        let format = match state.value_format.selected() {
            "Hex" => CsvValueFormat::Hex {
                width: state.hex_digits.value.clamp(1, 16) as usize,
            },
            _ => CsvValueFormat::Decimal,
        };
        let header = state.header.value.trim();
        let mut writer = CsvWordWriter::new()
            .with_value_format(format)
            .with_header((!header.is_empty()).then(|| header.to_string()))
            .with_name(name);
        // Static fallback only when nothing is wired into Filename — a
        // connected stream always wins.
        let static_filename = state.filename.value.trim();
        if resolved.kind(1).is_none() && !static_filename.is_empty() {
            writer = writer.with_filename(static_filename);
        }
        Ok(Box::new(writer))
    }
}
