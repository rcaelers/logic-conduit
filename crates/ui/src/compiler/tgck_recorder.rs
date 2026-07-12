//! `TGCK Recorder` builder — pure edge/word correlation, no file I/O (see
//! `dsl::TgckRecorder`'s doc comment). Its `Rows`/`Filename` outputs need a
//! `Text File Writer` downstream to actually persist anything; available on
//! every target.

use dsl::runtime::ProcessNode;
use dsl::{Sample, TextSample, Word};
use node_graph::Socket;
use serde_json::Value;

use super::{CompileCtx, PortKind, ResolvedInputs, RuntimeBuilder};

pub(super) struct TgckRecorderBuilder;

impl RuntimeBuilder for TgckRecorderBuilder {
    fn accepted_kinds(&self, socket: &Socket, _state: &Value) -> Vec<PortKind> {
        match socket.def_index {
            0 => vec![PortKind::of::<Word>()],
            1 => vec![PortKind::of::<Sample>()],
            2 => vec![PortKind::of::<TextSample>()],
            _ => vec![],
        }
    }
    fn offered_kinds(&self, _socket: &Socket, _state: &Value) -> Vec<PortKind> {
        vec![PortKind::of::<TextSample>()]
    }
    fn input_port(&self, socket: &Socket, _: usize, _: &Value, _: PortKind) -> Option<String> {
        match socket.def_index {
            0 => Some("words".into()),
            1 => Some("tgck".into()),
            2 => Some("filename".into()),
            _ => None,
        }
    }
    fn output_port(&self, socket: &Socket, _state: &Value, kind: PortKind) -> Option<String> {
        if kind != PortKind::of::<TextSample>() {
            return None;
        }
        match socket.def_index {
            0 => Some("rows".into()),
            1 => Some("filename".into()),
            _ => None,
        }
    }
    fn build(
        &self,
        name: &str,
        _state: &Value,
        _resolved: &ResolvedInputs,
        _ctx: &mut CompileCtx,
    ) -> Result<Box<dyn ProcessNode>, String> {
        Ok(Box::new(dsl::TgckRecorder::new().with_name(name)))
    }
}
