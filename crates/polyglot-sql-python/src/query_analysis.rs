use crate::errors::map_transpile_error;
use crate::helpers::{decode_complexity_guard, resolve_dialect, to_python_object};
use pyo3::prelude::*;
use pyo3::types::{PyAny, PyDict};
use pythonize::depythonize;

#[pyfunction(signature = (sql, options = None, dialect = "generic", *, complexity_guard = None))]
pub fn analyze_query(
    py: Python<'_>,
    sql: &str,
    options: Option<&Bound<'_, PyAny>>,
    dialect: &str,
    complexity_guard: Option<&Bound<'_, PyDict>>,
) -> PyResult<Py<PyAny>> {
    let dialect_impl = resolve_dialect(dialect)?;
    let guard = decode_complexity_guard(complexity_guard)?;
    let mut options = match options {
        Some(options) => {
            let dict = options.cast::<PyDict>().map_err(|err| {
                pyo3::exceptions::PyValueError::new_err(format!(
                    "Invalid analyze_query options object: {err}"
                ))
            })?;
            if let Some(value) = dict.get_item("complexityGuard")? {
                if !value.is_none() {
                    decode_complexity_guard(Some(value.cast::<PyDict>()?))?;
                    if guard.is_some() {
                        return Err(pyo3::exceptions::PyValueError::new_err(
                            "Specify complexity_guard either as a keyword or in options, not both",
                        ));
                    }
                }
            }
            depythonize::<polyglot_sql::AnalyzeQueryOptions>(options).map_err(|err| {
                pyo3::exceptions::PyValueError::new_err(format!(
                    "Invalid analyze_query options object: {err}"
                ))
            })?
        }
        None => polyglot_sql::AnalyzeQueryOptions::default(),
    };

    if guard.is_some() {
        options.complexity_guard = guard;
    }

    if dialect != "generic" && options.dialect == polyglot_sql::DialectType::Generic {
        options.dialect = dialect_impl.dialect_type();
    }

    let analysis =
        py.detach(|| polyglot_sql::analyze_query(sql, options).map_err(map_transpile_error))?;
    to_python_object(py, &analysis)
}
