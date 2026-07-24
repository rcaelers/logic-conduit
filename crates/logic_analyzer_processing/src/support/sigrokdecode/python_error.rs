use pyo3::prelude::*;
use pyo3::types::PyModule;

pub(crate) fn format_python_error(error: PyErr) -> String {
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
