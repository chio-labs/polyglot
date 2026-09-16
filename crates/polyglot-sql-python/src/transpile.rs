use crate::errors::map_transpile_error;
use crate::helpers::{
    normalize_error_level, normalize_unsupported_level, resolve_dialect, run_detached,
};
use polyglot_sql::dialects::{Dialect, TranspileOptions};
use polyglot_sql::ComplexityGuardOptions;
use pyo3::prelude::*;
use pyo3::types::{PyBool, PyDict, PyInt};

#[pyfunction(signature = (sql, read = None, write = None, *, identity = true, error_level = None, unsupported_level = None, pretty = false, max_unsupported = None, complexity_guard = None))]
#[allow(clippy::too_many_arguments)]
pub fn transpile(
    py: Python<'_>,
    sql: &str,
    read: Option<&str>,
    write: Option<&str>,
    identity: bool,
    error_level: Option<&str>,
    unsupported_level: Option<&str>,
    pretty: bool,
    max_unsupported: Option<usize>,
    complexity_guard: Option<&Bound<'_, PyDict>>,
) -> PyResult<Vec<String>> {
    // Reject Python coercions before conversion: bool is an int subclass, and
    // non-finite floats become JSON null (which would disable a guard).
    let complexity_guard: Option<ComplexityGuardOptions> = complexity_guard
        .map(|value| {
            for (_, limit) in value.iter() {
                if !limit.is_none()
                    && (limit.is_instance_of::<PyBool>() || !limit.is_instance_of::<PyInt>())
                {
                    return Err(pyo3::exceptions::PyTypeError::new_err(
                        "Complexity guard limits must be nonnegative integers or None",
                    ));
                }
            }
            // The shared serde contract still handles defaults and integer ranges.
            let value: serde_json::Value = pythonize::depythonize(value).map_err(|error| {
                pyo3::exceptions::PyValueError::new_err(format!(
                    "Invalid complexity guard options: {error}"
                ))
            })?;
            serde_json::from_value(value).map_err(|error| {
                pyo3::exceptions::PyValueError::new_err(format!(
                    "Invalid complexity guard options: {error}"
                ))
            })
        })
        .transpose()?;
    let _ = normalize_error_level(error_level)?;
    let unsupported_level = normalize_unsupported_level(unsupported_level)?;
    let read = read.unwrap_or("generic");
    resolve_dialect(read)?;
    let write = if identity {
        write.unwrap_or(read)
    } else {
        write.unwrap_or("generic")
    };
    resolve_dialect(write)?;

    let sql_owned = sql.to_owned();
    let read_owned = read.to_owned();
    let write_owned = write.to_owned();
    run_detached(py, move || {
        let read_dialect = Dialect::get_by_name(&read_owned)
            .expect("dialect existence checked before entering stack");
        let write_dialect = Dialect::get_by_name(&write_owned)
            .expect("dialect existence checked before entering stack");
        let mut opts = if pretty {
            TranspileOptions::pretty()
        } else {
            TranspileOptions::default()
        };
        if let Some(level) = unsupported_level {
            opts.unsupported_level = level;
        }
        if let Some(max) = max_unsupported {
            opts.max_unsupported = max;
        }
        if let Some(guard) = complexity_guard {
            opts.complexity_guard = guard;
        }
        read_dialect.transpile_with(&sql_owned, &write_dialect, opts)
    })?
    .map_err(map_transpile_error)
}
