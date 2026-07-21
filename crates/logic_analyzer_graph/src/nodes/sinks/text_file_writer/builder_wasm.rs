//! Browser runtime builder for `Text File Writer`.

use serde_json::Value;

use logic_analyzer_processing::nodes::sinks::DiscardTextWriter;
use node_graph::Socket;
use signal_processing::{ProcessNode, TextSample};

use crate::{CompileCtx, PortKind, ResolvedInputs, RuntimeBuilder};

pub(crate) struct TextFileWriterBuilder;

impl RuntimeBuilder for TextFileWriterBuilder {
    fn is_sink(&self) -> bool {
        true
    }

    fn accepted_kinds(&self, _socket: &Socket, _state: &Value) -> Vec<PortKind> {
        vec![PortKind::of::<TextSample>()]
    }

    fn offered_kinds(&self, _socket: &Socket, _state: &Value) -> Vec<PortKind> {
        Vec::new()
    }

    fn input_port(&self, socket: &Socket, _: usize, _: &Value, _: PortKind) -> Option<String> {
        match socket.def_index {
            0 => Some("lines".to_owned()),
            1 => Some("filename".to_owned()),
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
        Ok(Box::new(DiscardTextWriter::new(name)))
    }
}
