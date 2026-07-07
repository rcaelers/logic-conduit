//! `Text File Writer` builder — writes text lines (e.g. `TGCK Recorder`'s
//! CSV rows) to a file. Native-only: no filesystem in the browser.

use super::{CompileCtx, PortKind, ResolvedInputs, RuntimeBuilder};
use dsl::TextFileWriter;
use dsl::runtime::ProcessNode;
use node_graph::Socket;
use serde_json::Value;

pub(super) struct TextFileWriterBuilder;

impl RuntimeBuilder for TextFileWriterBuilder {
    fn is_sink(&self) -> bool {
        true
    }
    fn accepted_kinds(&self, _socket: &Socket, _state: &Value) -> Vec<PortKind> {
        vec![PortKind::Text]
    }
    fn offered_kinds(&self, _socket: &Socket, _state: &Value) -> Vec<PortKind> {
        vec![]
    }
    fn input_port(&self, socket: &Socket, _: usize, _: &Value, _: PortKind) -> Option<String> {
        match socket.def_index {
            0 => Some("lines".into()),
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
        _state: &Value,
        _resolved: &ResolvedInputs,
        _ctx: &mut CompileCtx,
    ) -> Result<Box<dyn ProcessNode>, String> {
        Ok(Box::new(TextFileWriter::new().with_name(name)))
    }
}
