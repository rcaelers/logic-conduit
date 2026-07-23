use std::path::Path;

use pyo3::exceptions::{PyNotImplementedError, PyValueError};
use pyo3::prelude::*;
use pyo3::types::{
    PyAny, PyBool, PyDict, PyDictMethods, PyFloat, PyInt, PyList, PyModule, PyString,
};

const OUTPUT_ANN: i32 = 0;
const OUTPUT_PYTHON: i32 = 1;
const OUTPUT_BINARY: i32 = 2;
const OUTPUT_LOGIC: i32 = 3;
const OUTPUT_META: i32 = 4;
const SRD_CONF_SAMPLERATE: i32 = 10_000;

#[derive(Clone, Debug, PartialEq)]
enum ScalarValue {
    Bool(bool),
    Integer(i64),
    Float(f64),
    String(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct DecoderChannel {
    id: String,
    name: String,
    description: String,
}

#[derive(Clone, Debug, PartialEq)]
struct DecoderOption {
    id: String,
    description: String,
    default: ScalarValue,
    values: Vec<ScalarValue>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct AnnotationClass {
    id: String,
    description: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct AnnotationRow {
    id: String,
    description: String,
    classes: Vec<usize>,
}

#[derive(Clone, Debug, PartialEq)]
struct DecoderDescriptor {
    api_version: i64,
    id: String,
    name: String,
    long_name: String,
    description: String,
    license: String,
    inputs: Vec<String>,
    outputs: Vec<String>,
    tags: Vec<String>,
    channels: Vec<DecoderChannel>,
    optional_channels: Vec<DecoderChannel>,
    options: Vec<DecoderOption>,
    annotations: Vec<AnnotationClass>,
    annotation_rows: Vec<AnnotationRow>,
    binary: Vec<AnnotationClass>,
}

#[pyclass(subclass, name = "Decoder", module = "sigrokdecode")]
#[derive(Default)]
struct HostDecoder {
    next_output_id: usize,
}

#[pymethods]
impl HostDecoder {
    #[new]
    fn new() -> Self {
        Self::default()
    }

    #[pyo3(signature = (output_type, proto_id=None, meta=None))]
    fn register(
        &mut self,
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
        let _ = proto_id;

        let output_id = self.next_output_id;
        self.next_output_id += 1;
        Ok(output_id)
    }

    #[pyo3(signature = (conditions=None))]
    fn wait(&self, conditions: Option<&Bound<'_, PyAny>>) -> PyResult<()> {
        let _ = conditions;
        Err(PyNotImplementedError::new_err(
            "the feasibility harness does not yet schedule wait conditions",
        ))
    }

    fn put(
        &self,
        start_sample: u64,
        end_sample: u64,
        output_id: usize,
        data: &Bound<'_, PyAny>,
    ) -> PyResult<()> {
        let _ = (start_sample, end_sample, output_id, data);
        Err(PyNotImplementedError::new_err(
            "the feasibility harness does not yet collect decoder output",
        ))
    }

    fn has_channel(&self, channel_index: usize) -> bool {
        let _ = channel_index;
        false
    }
}

fn install_sigrokdecode_module(py: Python<'_>) -> PyResult<()> {
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

fn import_decoder<'py>(
    py: Python<'py>,
    decoder_root: &Path,
    id: &str,
) -> PyResult<Bound<'py, PyAny>> {
    let sys = PyModule::import(py, "sys")?;
    let path: Bound<'_, PyList> = sys.getattr("path")?.cast_into()?;
    let decoder_root = decoder_root.to_str().ok_or_else(|| {
        PyValueError::new_err("the Sigrok decoder search path is not valid UTF-8")
    })?;
    path.call_method1("insert", (0, decoder_root))?;

    let modules: Bound<'_, PyDict> = sys.getattr("modules")?.cast_into()?;
    modules.del_item(id).ok();
    modules.del_item(format!("{id}.pd")).ok();

    PyModule::import(py, id)?.getattr("Decoder")
}

fn discover_decoder(decoder_class: &Bound<'_, PyAny>) -> PyResult<DecoderDescriptor> {
    let api_version = decoder_class.getattr("api_version")?.extract()?;
    if api_version != 3 {
        return Err(PyValueError::new_err(format!(
            "unsupported Sigrok decoder API version {api_version}"
        )));
    }

    Ok(DecoderDescriptor {
        api_version,
        id: string_attr(decoder_class, "id")?,
        name: string_attr(decoder_class, "name")?,
        long_name: string_attr(decoder_class, "longname")?,
        description: string_attr(decoder_class, "desc")?,
        license: string_attr(decoder_class, "license")?,
        inputs: string_sequence(&decoder_class.getattr("inputs")?)?,
        outputs: string_sequence(&decoder_class.getattr("outputs")?)?,
        tags: string_sequence(&decoder_class.getattr("tags")?)?,
        channels: channels(&decoder_class.getattr("channels")?)?,
        optional_channels: channels(&decoder_class.getattr("optional_channels")?)?,
        options: options(&decoder_class.getattr("options")?)?,
        annotations: annotation_classes(&decoder_class.getattr("annotations")?)?,
        annotation_rows: annotation_rows(&decoder_class.getattr("annotation_rows")?)?,
        binary: annotation_classes(&decoder_class.getattr("binary")?)?,
    })
}

fn start_decoder<'py>(
    py: Python<'py>,
    decoder_class: &Bound<'py, PyAny>,
    descriptor: &DecoderDescriptor,
) -> PyResult<Bound<'py, PyAny>> {
    let decoder = decoder_class.call0()?;
    let configured_options = PyDict::new(py);
    for option in &descriptor.options {
        set_scalar(&configured_options, &option.id, &option.default)?;
    }
    decoder.setattr("options", configured_options)?;
    decoder.setattr("samplenum", 0)?;
    decoder.setattr("matched", py.None())?;
    decoder.call_method1("metadata", (SRD_CONF_SAMPLERATE, 1_000_000_u64))?;
    decoder.call_method0("start")?;
    Ok(decoder)
}

fn string_attr(object: &Bound<'_, PyAny>, name: &str) -> PyResult<String> {
    object.getattr(name)?.extract()
}

fn string_sequence(value: &Bound<'_, PyAny>) -> PyResult<Vec<String>> {
    value
        .try_iter()?
        .map(|item| item.and_then(|item| item.extract()))
        .collect()
}

fn channels(value: &Bound<'_, PyAny>) -> PyResult<Vec<DecoderChannel>> {
    value
        .try_iter()?
        .map(|item| {
            let item = item?;
            let item = item.cast::<PyDict>()?;
            Ok(DecoderChannel {
                id: required_dict_item(item, "id")?.extract()?,
                name: required_dict_item(item, "name")?.extract()?,
                description: required_dict_item(item, "desc")?.extract()?,
            })
        })
        .collect()
}

fn options(value: &Bound<'_, PyAny>) -> PyResult<Vec<DecoderOption>> {
    value
        .try_iter()?
        .map(|item| {
            let item = item?;
            let item = item.cast::<PyDict>()?;
            let values = item
                .get_item("values")?
                .map(|values| scalar_sequence(&values))
                .transpose()?
                .unwrap_or_default();
            Ok(DecoderOption {
                id: required_dict_item(item, "id")?.extract()?,
                description: required_dict_item(item, "desc")?.extract()?,
                default: scalar_value(&required_dict_item(item, "default")?)?,
                values,
            })
        })
        .collect()
}

fn annotation_classes(value: &Bound<'_, PyAny>) -> PyResult<Vec<AnnotationClass>> {
    value
        .try_iter()?
        .map(|item| {
            let (id, description): (String, String) = item?.extract()?;
            Ok(AnnotationClass { id, description })
        })
        .collect()
}

fn annotation_rows(value: &Bound<'_, PyAny>) -> PyResult<Vec<AnnotationRow>> {
    value
        .try_iter()?
        .map(|item| {
            let (id, description, classes): (String, String, Vec<usize>) = item?.extract()?;
            Ok(AnnotationRow {
                id,
                description,
                classes,
            })
        })
        .collect()
}

fn scalar_sequence(value: &Bound<'_, PyAny>) -> PyResult<Vec<ScalarValue>> {
    value
        .try_iter()?
        .map(|item| item.and_then(|item| scalar_value(&item)))
        .collect()
}

fn scalar_value(value: &Bound<'_, PyAny>) -> PyResult<ScalarValue> {
    if value.is_instance_of::<PyBool>() {
        Ok(ScalarValue::Bool(value.extract()?))
    } else if value.is_instance_of::<PyInt>() {
        Ok(ScalarValue::Integer(value.extract()?))
    } else if value.is_instance_of::<PyFloat>() {
        Ok(ScalarValue::Float(value.extract()?))
    } else if value.is_instance_of::<PyString>() {
        Ok(ScalarValue::String(value.extract()?))
    } else {
        Err(PyValueError::new_err(format!(
            "unsupported decoder option value type {}",
            value.get_type().name()?
        )))
    }
}

fn set_scalar(dict: &Bound<'_, PyDict>, key: &str, value: &ScalarValue) -> PyResult<()> {
    match value {
        ScalarValue::Bool(value) => dict.set_item(key, *value),
        ScalarValue::Integer(value) => dict.set_item(key, *value),
        ScalarValue::Float(value) => dict.set_item(key, *value),
        ScalarValue::String(value) => dict.set_item(key, value),
    }
}

fn required_dict_item<'py>(dict: &Bound<'py, PyDict>, key: &str) -> PyResult<Bound<'py, PyAny>> {
    dict.get_item(key)?
        .ok_or_else(|| PyValueError::new_err(format!("missing required decoder field '{key}'")))
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::sync::{Mutex, OnceLock};

    use super::*;

    #[test]
    fn standard_spi_decoder_can_be_discovered_and_started_without_libsigrokdecode() {
        let Some(decoder_root) = local_decoder_root() else {
            eprintln!(
                "skipping Sigrok SPI feasibility test: set SIGROK_DECODERS_DIR to a decoder tree"
            );
            return;
        };
        let _guard = python_test_lock().lock().unwrap();

        Python::initialize();
        Python::attach(|py| {
            install_sigrokdecode_module(py)?;
            let decoder_class = import_decoder(py, &decoder_root, "spi")?;
            let descriptor = discover_decoder(&decoder_class)?;

            assert_eq!(descriptor.api_version, 3);
            assert_eq!(descriptor.id, "spi");
            assert_eq!(descriptor.name, "SPI");
            assert_eq!(descriptor.long_name, "Serial Peripheral Interface");
            assert_eq!(
                descriptor.description,
                "Full-duplex, synchronous, serial bus."
            );
            assert_eq!(descriptor.license, "gplv2+");
            assert_eq!(descriptor.inputs, ["logic"]);
            assert_eq!(descriptor.outputs, ["spi"]);
            assert_eq!(descriptor.tags, ["Embedded/industrial"]);
            assert_eq!(descriptor.channels.len(), 1);
            assert_eq!(descriptor.channels[0].id, "clk");
            assert_eq!(descriptor.optional_channels.len(), 3);
            assert_eq!(descriptor.optional_channels[2].id, "cs");
            assert_eq!(descriptor.options.len(), 5);
            assert_eq!(descriptor.options[4].id, "wordsize");
            assert_eq!(descriptor.options[4].default, ScalarValue::Integer(8));
            assert!(descriptor.options[4].values.is_empty());
            assert_eq!(descriptor.annotations.len(), 7);
            assert_eq!(descriptor.annotation_rows.len(), 7);
            assert_eq!(descriptor.annotation_rows[0].classes, [2]);
            assert_eq!(descriptor.binary.len(), 2);

            let decoder = start_decoder(py, &decoder_class, &descriptor)?;
            assert_eq!(decoder.getattr("samplerate")?.extract::<u64>()?, 1_000_000);
            assert_eq!(decoder.getattr("out_python")?.extract::<usize>()?, 0);
            assert_eq!(decoder.getattr("out_ann")?.extract::<usize>()?, 1);
            assert_eq!(decoder.getattr("out_binary")?.extract::<usize>()?, 2);
            assert_eq!(decoder.getattr("out_bitrate")?.extract::<usize>()?, 3);
            assert_eq!(decoder.getattr("bw")?.extract::<usize>()?, 1);
            PyResult::Ok(())
        })
        .unwrap();
    }

    fn local_decoder_root() -> Option<PathBuf> {
        std::env::var_os("SIGROK_DECODERS_DIR")
            .map(PathBuf::from)
            .or_else(|| {
                Some(
                    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                        .join("../../../dslogic/libsigrokdecode/decoders"),
                )
            })
            .filter(|path| path.join("spi/pd.py").is_file())
    }

    fn python_test_lock() -> &'static Mutex<()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
    }
}
