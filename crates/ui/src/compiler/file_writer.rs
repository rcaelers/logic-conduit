//! `File Writer` builder — writes decoded words to a binary file. Native-only:
//! no filesystem in the browser.

use super::{CompileCtx, PortKind, ResolvedInputs, RuntimeBuilder, parse_state};
use crate::nodes;
use dsl::runtime::ProcessNode;
use dsl::{BinaryFileWriter, TextSample, Word, WriteWidth};
use node_graph::Socket;
use serde_json::Value;

pub(super) struct FileWriterBuilder;

impl RuntimeBuilder for FileWriterBuilder {
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
    fn build(
        &self,
        name: &str,
        state: &Value,
        _resolved: &ResolvedInputs,
        _ctx: &mut CompileCtx,
    ) -> Result<Box<dyn ProcessNode>, String> {
        let state: nodes::FileWriterState = parse_state(state)?;
        let width = match state.write_width.selected() {
            "U16 LE" => WriteWidth::U16Le,
            "U32 LE" => WriteWidth::U32Le,
            _ => WriteWidth::U8,
        };
        Ok(Box::new(
            BinaryFileWriter::new()
                .with_width(width)
                .with_index_csv(state.index_csv.value)
                .with_name(name),
        ))
    }
}
