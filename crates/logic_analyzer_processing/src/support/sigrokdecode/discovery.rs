use std::path::{Path, PathBuf};
use std::sync::Arc;

use pyo3::exceptions::PyValueError;
use pyo3::prelude::*;
use pyo3::types::{
    PyAny, PyBool, PyDict, PyDictMethods, PyFloat, PyInt, PyList, PyModule, PyString,
};

use super::bridge::DecoderBridge;
use super::python_host::{
    HostDecoder, OUTPUT_ANN, OUTPUT_BINARY, OUTPUT_LOGIC, OUTPUT_META, OUTPUT_PYTHON,
    SRD_CONF_SAMPLERATE, install_sigrokdecode_module,
};
use super::scheduler::InitialPin;

#[derive(Clone, Debug, PartialEq)]
pub enum SigrokScalarValue {
    Bool(bool),
    Integer(i64),
    Float(f64),
    String(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SigrokDecoderChannelDescriptor {
    pub id: String,
    pub name: String,
    pub description: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct SigrokDecoderOptionDescriptor {
    pub id: String,
    pub description: String,
    pub default: SigrokScalarValue,
    pub values: Vec<SigrokScalarValue>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SigrokAnnotationClassDescriptor {
    pub id: String,
    pub description: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SigrokAnnotationRowDescriptor {
    pub id: String,
    pub description: String,
    pub classes: Vec<usize>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SigrokOutputKind {
    Annotation,
    Binary,
    GeneratedLogic,
    Metadata,
    ProtocolPacket,
}

#[derive(Clone, Debug, PartialEq)]
pub struct SigrokDecoderDescriptor {
    pub api_version: i64,
    pub id: String,
    pub name: String,
    pub long_name: String,
    pub description: String,
    pub license: String,
    pub inputs: Vec<String>,
    pub outputs: Vec<String>,
    pub tags: Vec<String>,
    pub channels: Vec<SigrokDecoderChannelDescriptor>,
    pub optional_channels: Vec<SigrokDecoderChannelDescriptor>,
    pub options: Vec<SigrokDecoderOptionDescriptor>,
    pub annotations: Vec<SigrokAnnotationClassDescriptor>,
    pub annotation_rows: Vec<SigrokAnnotationRowDescriptor>,
    pub binary: Vec<SigrokAnnotationClassDescriptor>,
    pub logic_output_channels: Vec<SigrokDecoderChannelDescriptor>,
    pub registered_outputs: Vec<SigrokOutputKind>,
    pub package_fingerprint: String,
}

pub fn discover_sigrok_decoder(
    decoder_root: impl Into<PathBuf>,
    id: &str,
) -> Result<SigrokDecoderDescriptor, String> {
    let decoder_root = decoder_root.into();
    Python::initialize();
    Python::attach(|py| {
        install_sigrokdecode_module(py)?;
        let decoder_class = import_decoder(py, &decoder_root, id)?;
        let mut descriptor = descriptor_from_class(&decoder_class)?;
        let (_decoder, bridge) = start_decoder(py, &decoder_class, &descriptor)?;
        descriptor.registered_outputs = bridge
            .registrations()
            .into_iter()
            .filter_map(|registration| output_kind(registration.output_type))
            .collect();
        descriptor.package_fingerprint =
            package_fingerprint(&decoder_root.join(id)).map_err(PyValueError::new_err)?;
        PyResult::Ok(descriptor)
    })
    .map_err(|error| format!("could not discover Sigrok decoder '{id}': {error}"))
}

fn import_decoder<'py>(
    py: Python<'py>,
    decoder_root: &Path,
    id: &str,
) -> PyResult<Bound<'py, PyAny>> {
    let sys = PyModule::import(py, "sys")?;
    sys.setattr("dont_write_bytecode", true)?;
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

fn descriptor_from_class(decoder_class: &Bound<'_, PyAny>) -> PyResult<SigrokDecoderDescriptor> {
    let api_version = decoder_class.getattr("api_version")?.extract()?;
    if api_version != 3 {
        return Err(PyValueError::new_err(format!(
            "unsupported Sigrok decoder API version {api_version}"
        )));
    }

    Ok(SigrokDecoderDescriptor {
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
        logic_output_channels: decoder_class
            .getattr("logic_output_channels")
            .ok()
            .map(|value| channels(&value))
            .transpose()?
            .unwrap_or_default(),
        registered_outputs: Vec::new(),
        package_fingerprint: String::new(),
    })
}

fn start_decoder<'py>(
    py: Python<'py>,
    decoder_class: &Bound<'py, PyAny>,
    descriptor: &SigrokDecoderDescriptor,
) -> PyResult<(Bound<'py, PyAny>, Arc<DecoderBridge>)> {
    let decoder = decoder_class.call0()?;
    let channel_count = descriptor.channels.len() + descriptor.optional_channels.len();
    let (bridge, _outputs) = DecoderBridge::new(vec![Some(InitialPin::Low); channel_count], 16)
        .map_err(|error| PyValueError::new_err(error.to_string()))?;
    decoder
        .cast::<HostDecoder>()?
        .borrow_mut()
        .attach(bridge.clone());
    let configured_options = PyDict::new(py);
    for option in &descriptor.options {
        set_scalar(&configured_options, &option.id, &option.default)?;
    }
    decoder.setattr("options", configured_options)?;
    decoder.setattr("samplenum", 0)?;
    decoder.setattr("matched", py.None())?;
    decoder.call_method1("metadata", (SRD_CONF_SAMPLERATE, 1_000_000_u64))?;
    decoder.call_method0("start")?;
    Ok((decoder, bridge))
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

fn channels(value: &Bound<'_, PyAny>) -> PyResult<Vec<SigrokDecoderChannelDescriptor>> {
    value
        .try_iter()?
        .map(|item| {
            let item = item?;
            let item = item.cast::<PyDict>()?;
            Ok(SigrokDecoderChannelDescriptor {
                id: required_dict_item(item, "id")?.extract()?,
                name: required_dict_item(item, "name")?.extract()?,
                description: required_dict_item(item, "desc")?.extract()?,
            })
        })
        .collect()
}

fn options(value: &Bound<'_, PyAny>) -> PyResult<Vec<SigrokDecoderOptionDescriptor>> {
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
            Ok(SigrokDecoderOptionDescriptor {
                id: required_dict_item(item, "id")?.extract()?,
                description: required_dict_item(item, "desc")?.extract()?,
                default: scalar_value(&required_dict_item(item, "default")?)?,
                values,
            })
        })
        .collect()
}

fn annotation_classes(value: &Bound<'_, PyAny>) -> PyResult<Vec<SigrokAnnotationClassDescriptor>> {
    value
        .try_iter()?
        .map(|item| {
            let (id, description): (String, String) = item?.extract()?;
            Ok(SigrokAnnotationClassDescriptor { id, description })
        })
        .collect()
}

fn annotation_rows(value: &Bound<'_, PyAny>) -> PyResult<Vec<SigrokAnnotationRowDescriptor>> {
    value
        .try_iter()?
        .map(|item| {
            let (id, description, classes): (String, String, Vec<usize>) = item?.extract()?;
            Ok(SigrokAnnotationRowDescriptor {
                id,
                description,
                classes,
            })
        })
        .collect()
}

fn scalar_sequence(value: &Bound<'_, PyAny>) -> PyResult<Vec<SigrokScalarValue>> {
    value
        .try_iter()?
        .map(|item| item.and_then(|item| scalar_value(&item)))
        .collect()
}

fn scalar_value(value: &Bound<'_, PyAny>) -> PyResult<SigrokScalarValue> {
    if value.is_instance_of::<PyBool>() {
        Ok(SigrokScalarValue::Bool(value.extract()?))
    } else if value.is_instance_of::<PyInt>() {
        Ok(SigrokScalarValue::Integer(value.extract()?))
    } else if value.is_instance_of::<PyFloat>() {
        Ok(SigrokScalarValue::Float(value.extract()?))
    } else if value.is_instance_of::<PyString>() {
        Ok(SigrokScalarValue::String(value.extract()?))
    } else {
        Err(PyValueError::new_err(format!(
            "unsupported decoder option value type {}",
            value.get_type().name()?
        )))
    }
}

fn set_scalar(dict: &Bound<'_, PyDict>, key: &str, value: &SigrokScalarValue) -> PyResult<()> {
    match value {
        SigrokScalarValue::Bool(value) => dict.set_item(key, *value),
        SigrokScalarValue::Integer(value) => dict.set_item(key, *value),
        SigrokScalarValue::Float(value) => dict.set_item(key, *value),
        SigrokScalarValue::String(value) => dict.set_item(key, value),
    }
}

fn required_dict_item<'py>(dict: &Bound<'py, PyDict>, key: &str) -> PyResult<Bound<'py, PyAny>> {
    dict.get_item(key)?
        .ok_or_else(|| PyValueError::new_err(format!("missing required decoder field '{key}'")))
}

fn output_kind(output_type: i32) -> Option<SigrokOutputKind> {
    match output_type {
        OUTPUT_ANN => Some(SigrokOutputKind::Annotation),
        OUTPUT_BINARY => Some(SigrokOutputKind::Binary),
        OUTPUT_LOGIC => Some(SigrokOutputKind::GeneratedLogic),
        OUTPUT_META => Some(SigrokOutputKind::Metadata),
        OUTPUT_PYTHON => Some(SigrokOutputKind::ProtocolPacket),
        _ => None,
    }
}

fn package_fingerprint(package: &Path) -> Result<String, String> {
    fn collect_files(directory: &Path, files: &mut Vec<PathBuf>) -> Result<(), String> {
        for entry in std::fs::read_dir(directory)
            .map_err(|error| format!("could not read {}: {error}", directory.display()))?
        {
            let path = entry
                .map_err(|error| format!("could not read {}: {error}", directory.display()))?
                .path();
            if path.is_dir() {
                if path.file_name().is_some_and(|name| name == "__pycache__") {
                    continue;
                }
                collect_files(&path, files)?;
            } else if path.is_file() {
                if matches!(
                    path.extension().and_then(|extension| extension.to_str()),
                    Some("pyc" | "pyo")
                ) {
                    continue;
                }
                files.push(path);
            }
        }
        Ok(())
    }

    let mut files = Vec::new();
    collect_files(package, &mut files)?;
    files.sort();
    let mut hasher = blake3::Hasher::new();
    for path in files {
        let relative = path.strip_prefix(package).unwrap_or(&path);
        hasher.update(relative.to_string_lossy().as_bytes());
        hasher.update(
            &std::fs::read(&path)
                .map_err(|error| format!("could not read {}: {error}", path.display()))?,
        );
    }
    Ok(hasher.finalize().to_hex().to_string())
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
            let descriptor = descriptor_from_class(&decoder_class)?;

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
            assert_eq!(descriptor.options[4].default, SigrokScalarValue::Integer(8));
            assert!(descriptor.options[4].values.is_empty());
            assert_eq!(descriptor.annotations.len(), 7);
            assert_eq!(descriptor.annotation_rows.len(), 7);
            assert_eq!(descriptor.annotation_rows[0].classes, [2]);
            assert_eq!(descriptor.binary.len(), 2);

            let (decoder, _) = start_decoder(py, &decoder_class, &descriptor)?;
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
