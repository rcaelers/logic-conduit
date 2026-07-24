use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::thread::{self, JoinHandle};

use crossbeam_channel::{Receiver, RecvTimeoutError, TryRecvError};
use pyo3::exceptions::PyEOFError;
use pyo3::prelude::*;
use pyo3::types::{PyAny, PyDict, PyDictMethods, PyList, PyModule};
use thiserror::Error;

use super::bridge::{BridgeError, DecoderBridge, DecoderOutput, OutputRegistration};
use super::python_host::{HostDecoder, SRD_CONF_SAMPLERATE, install_sigrokdecode_module};
use super::scheduler::{InitialPin, LogicChunk};

#[derive(Clone, Debug)]
pub(crate) enum OptionValue {
    Bool(bool),
    Integer(i64),
    Float(f64),
    String(String),
}

#[derive(Clone, Debug)]
pub(crate) struct WorkerConfig {
    pub(crate) decoder_root: PathBuf,
    pub(crate) decoder_id: String,
    pub(crate) sample_rate: u64,
    pub(crate) channels: Vec<Option<InitialPin>>,
    pub(crate) options: BTreeMap<String, OptionValue>,
    pub(crate) queue_capacity: usize,
}

#[derive(Debug, Error)]
pub(crate) enum WorkerError {
    #[error(transparent)]
    Bridge(#[from] BridgeError),
    #[error("failed to start Sigrok decoder worker: {0}")]
    Spawn(std::io::Error),
    #[error("Sigrok decoder worker panicked")]
    Panic,
    #[error("Sigrok decoder failed:\n{0}")]
    Python(String),
}

pub(crate) struct DecoderWorker {
    bridge: Arc<DecoderBridge>,
    outputs: Receiver<DecoderOutput>,
    thread: Option<JoinHandle<Result<(), WorkerError>>>,
}

impl DecoderWorker {
    pub(crate) fn spawn(config: WorkerConfig) -> Result<Self, WorkerError> {
        let (bridge, outputs) = DecoderBridge::new(config.channels.clone(), config.queue_capacity)?;
        let thread_bridge = Arc::clone(&bridge);
        let thread = thread::Builder::new()
            .name(format!("sigrok-{}", config.decoder_id))
            .spawn(move || run_decoder(config, thread_bridge))
            .map_err(WorkerError::Spawn)?;
        Ok(Self {
            bridge,
            outputs,
            thread: Some(thread),
        })
    }

    pub(crate) fn push_chunk(&self, chunk: LogicChunk) -> Result<(), WorkerError> {
        self.bridge.push_chunk(chunk).map_err(Into::into)
    }

    pub(crate) fn finish(&self) -> Result<(), WorkerError> {
        self.bridge.finish().map_err(Into::into)
    }

    pub(crate) fn cancel(&self) {
        self.bridge.cancel();
    }

    pub(crate) fn try_output(&self) -> Result<Option<DecoderOutput>, WorkerError> {
        match self.outputs.try_recv() {
            Ok(output) => Ok(Some(output)),
            Err(TryRecvError::Empty) => Ok(None),
            Err(TryRecvError::Disconnected) => Ok(None),
        }
    }

    pub(crate) fn registrations(&self) -> Vec<OutputRegistration> {
        self.bridge.registrations()
    }

    pub(crate) fn is_finished(&self) -> bool {
        self.thread
            .as_ref()
            .is_none_or(std::thread::JoinHandle::is_finished)
    }

    pub(crate) fn receive_output(
        &self,
        timeout: std::time::Duration,
    ) -> Result<Option<DecoderOutput>, WorkerError> {
        match self.outputs.recv_timeout(timeout) {
            Ok(output) => Ok(Some(output)),
            Err(RecvTimeoutError::Timeout | RecvTimeoutError::Disconnected) => Ok(None),
        }
    }

    pub(crate) fn join(&mut self) -> Result<(), WorkerError> {
        self.join_inner()
    }

    fn join_inner(&mut self) -> Result<(), WorkerError> {
        let Some(thread) = self.thread.take() else {
            return Ok(());
        };
        thread.join().map_err(|_| WorkerError::Panic)?
    }
}

impl Drop for DecoderWorker {
    fn drop(&mut self) {
        if self.thread.is_some() {
            self.cancel();
            let _ = self.join_inner();
        }
    }
}

fn run_decoder(config: WorkerConfig, bridge: Arc<DecoderBridge>) -> Result<(), WorkerError> {
    Python::initialize();
    Python::attach(|py| {
        install_sigrokdecode_module(py)?;
        let decoder_class = import_decoder(py, &config.decoder_root, &config.decoder_id)?;
        let decoder = decoder_class.call0()?;
        decoder.cast::<HostDecoder>()?.borrow_mut().attach(bridge);

        let options = PyDict::new(py);
        for (name, value) in &config.options {
            match value {
                OptionValue::Bool(value) => options.set_item(name, *value)?,
                OptionValue::Integer(value) => options.set_item(name, *value)?,
                OptionValue::Float(value) => options.set_item(name, *value)?,
                OptionValue::String(value) => options.set_item(name, value)?,
            }
        }
        decoder.setattr("options", options)?;
        decoder.setattr("samplenum", 0)?;
        decoder.setattr("matched", py.None())?;
        decoder.call_method1("metadata", (SRD_CONF_SAMPLERATE, config.sample_rate))?;
        decoder.call_method0("start")?;
        match decoder.call_method0("decode") {
            Ok(_) => Ok(()),
            Err(error) if error.is_instance_of::<PyEOFError>(py) => Ok(()),
            Err(error) => Err(error),
        }
    })
    .map_err(|error| WorkerError::Python(format_python_error(error)))
}

fn import_decoder<'py>(
    py: Python<'py>,
    decoder_root: &Path,
    decoder_id: &str,
) -> PyResult<Bound<'py, PyAny>> {
    let decoder_root = decoder_root
        .to_str()
        .ok_or_else(|| pyo3::exceptions::PyValueError::new_err("decoder path is not UTF-8"))?;
    let sys = PyModule::import(py, "sys")?;
    let path: Bound<'_, PyList> = sys.getattr("path")?.cast_into()?;
    path.call_method1("insert", (0, decoder_root))?;
    let modules: Bound<'_, PyDict> = sys.getattr("modules")?.cast_into()?;
    modules.del_item(decoder_id).ok();
    modules.del_item(format!("{decoder_id}.pd")).ok();
    PyModule::import(py, decoder_id)?.getattr("Decoder")
}

fn format_python_error(error: PyErr) -> String {
    Python::attach(|py| {
        let formatted = (|| -> PyResult<String> {
            let traceback = PyModule::import(py, "traceback")?;
            let lines = traceback
                .call_method1(
                    "format_exception",
                    (error.get_type(py), error.value(py), error.traceback(py)),
                )?
                .extract::<Vec<String>>()?;
            Ok(lines.concat())
        })();
        formatted.unwrap_or_else(|_| error.to_string())
    })
}
