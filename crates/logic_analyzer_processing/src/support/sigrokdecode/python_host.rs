use std::sync::Arc;

use pyo3::exceptions::{PyEOFError, PyRuntimeError, PyValueError};
use pyo3::prelude::*;
use pyo3::types::{PyAny, PyDict, PyDictMethods, PyModule};

use super::bridge::{DecoderBridge, DecoderOutput, OutputRegistration, matched_parts};
use super::conditions::{PinCondition, WaitCondition, WaitRequest, WaitTerm};
use super::scheduler::SchedulerStatus;

pub(crate) const OUTPUT_ANN: i32 = 0;
pub(crate) const OUTPUT_PYTHON: i32 = 1;
pub(crate) const OUTPUT_BINARY: i32 = 2;
pub(crate) const OUTPUT_LOGIC: i32 = 3;
pub(crate) const OUTPUT_META: i32 = 4;
pub(crate) const SRD_CONF_SAMPLERATE: i32 = 10_000;

#[pyclass(subclass, name = "Decoder", module = "sigrokdecode")]
#[derive(Default)]
pub(crate) struct HostDecoder {
    bridge: Option<Arc<DecoderBridge>>,
}

impl HostDecoder {
    pub(crate) fn attach(&mut self, bridge: Arc<DecoderBridge>) {
        self.bridge = Some(bridge);
    }

    fn bridge(&self) -> PyResult<Arc<DecoderBridge>> {
        self.bridge
            .clone()
            .ok_or_else(|| PyRuntimeError::new_err("decoder host is not attached"))
    }
}

#[pymethods]
impl HostDecoder {
    #[new]
    fn new() -> Self {
        Self::default()
    }

    #[pyo3(signature = (output_type, proto_id=None, meta=None))]
    fn register(
        &self,
        output_type: i32,
        proto_id: Option<&str>,
        meta: Option<&Bound<'_, PyAny>>,
    ) -> PyResult<usize> {
        if !(OUTPUT_ANN..=OUTPUT_META).contains(&output_type) {
            return Err(PyValueError::new_err(format!(
                "unsupported Sigrok output type {output_type}"
            )));
        }
        if output_type != OUTPUT_META && meta.is_some() {
            return Err(PyValueError::new_err(
                "metadata descriptors are valid only for OUTPUT_META",
            ));
        }
        Ok(self.bridge()?.register(OutputRegistration {
            output_type,
            protocol_id: proto_id.map(str::to_owned),
        }))
    }

    #[pyo3(signature = (conditions=None))]
    fn wait<'py>(
        slf: &Bound<'py, Self>,
        py: Python<'py>,
        conditions: Option<&Bound<'py, PyAny>>,
    ) -> PyResult<Vec<u8>> {
        let request = parse_wait_request(conditions)?;
        let bridge = slf.borrow().bridge()?;
        let status = py.detach(|| bridge.wait(request));
        match status.map_err(|error| PyRuntimeError::new_err(error.to_string()))? {
            SchedulerStatus::Matched(result) => {
                let (sample, pins, matched) = matched_parts(result);
                slf.as_any().setattr("samplenum", sample)?;
                slf.as_any().setattr("matched", matched)?;
                Ok(pins)
            }
            SchedulerStatus::EndOfStream => Err(PyEOFError::new_err("end of sample input")),
            SchedulerStatus::Cancelled => Err(PyEOFError::new_err("decoder cancelled")),
            SchedulerStatus::Waiting => Err(PyRuntimeError::new_err(
                "decoder wait returned without a match",
            )),
        }
    }

    fn put(
        &self,
        py: Python<'_>,
        start_sample: u64,
        end_sample: u64,
        output_id: usize,
        data: &Bound<'_, PyAny>,
    ) -> PyResult<()> {
        self.bridge()?
            .put(DecoderOutput {
                start_sample,
                end_sample,
                output_id,
                data: data.clone().unbind(),
            })
            .map_err(|error| PyRuntimeError::new_err(error.to_string()))?;
        let _ = py;
        Ok(())
    }

    fn has_channel(&self, channel_index: usize) -> PyResult<bool> {
        Ok(self.bridge()?.has_channel(channel_index))
    }
}

pub(crate) fn install_sigrokdecode_module(py: Python<'_>) -> PyResult<()> {
    let module = PyModule::new(py, "sigrokdecode")?;
    module.add_class::<HostDecoder>()?;
    module.add("OUTPUT_ANN", OUTPUT_ANN)?;
    module.add("OUTPUT_PYTHON", OUTPUT_PYTHON)?;
    module.add("OUTPUT_BINARY", OUTPUT_BINARY)?;
    module.add("OUTPUT_LOGIC", OUTPUT_LOGIC)?;
    module.add("OUTPUT_META", OUTPUT_META)?;
    module.add("SRD_CONF_SAMPLERATE", SRD_CONF_SAMPLERATE)?;

    let sys = PyModule::import(py, "sys")?;
    let modules: Bound<'_, PyDict> = sys.getattr("modules")?.cast_into()?;
    modules.set_item("sigrokdecode", module)
}

fn parse_wait_request(conditions: Option<&Bound<'_, PyAny>>) -> PyResult<WaitRequest> {
    let Some(conditions) = conditions else {
        return Ok(WaitRequest::Next);
    };
    if conditions.is_none() {
        return Ok(WaitRequest::Next);
    }
    if let Ok(condition) = conditions.cast::<PyDict>() {
        return Ok(WaitRequest::Conditions(vec![parse_condition(condition)?]));
    }
    let alternatives = conditions
        .try_iter()?
        .map(|condition| {
            let condition = condition?;
            parse_condition(condition.cast::<PyDict>()?)
        })
        .collect::<PyResult<Vec<_>>>()?;
    Ok(WaitRequest::Conditions(alternatives))
}

fn parse_condition(condition: &Bound<'_, PyDict>) -> PyResult<WaitCondition> {
    let mut terms = Vec::with_capacity(condition.len());
    for (key, value) in condition.iter() {
        if key.extract::<String>().is_ok_and(|key| key == "skip") {
            terms.push(WaitTerm::Skip(value.extract()?));
            continue;
        }
        let channel = key.extract::<usize>().map_err(|_| {
            PyValueError::new_err("wait condition keys must be channel indexes or 'skip'")
        })?;
        let code: String = value.extract()?;
        let pin_condition = match code.as_str() {
            "h" => PinCondition::High,
            "l" => PinCondition::Low,
            "r" => PinCondition::Rising,
            "f" => PinCondition::Falling,
            "e" => PinCondition::EitherEdge,
            "n" => PinCondition::NoEdge,
            _ => {
                return Err(PyValueError::new_err(format!(
                    "unsupported wait condition '{code}'"
                )));
            }
        };
        terms.push(WaitTerm::pin(channel, pin_condition));
    }
    Ok(WaitCondition::new(terms))
}
