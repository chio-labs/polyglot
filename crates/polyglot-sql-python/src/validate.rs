use crate::helpers::{decode_complexity_guard, resolve_dialect};
use crate::types::{validation_result_from_core, ValidationResult};
use polyglot_sql::ValidationOptions;
use pyo3::prelude::*;
use pyo3::types::{PyBool, PyDict, PyInt};
use pythonize::depythonize;

#[pyfunction(signature = (sql, dialect = "generic", *, strict_syntax = false, semantic = false, complexity_guard = None))]
pub fn validate(
    py: Python<'_>,
    sql: &str,
    dialect: &str,
    strict_syntax: bool,
    semantic: bool,
    complexity_guard: Option<&Bound<'_, PyDict>>,
) -> PyResult<ValidationResult> {
    let dialect = resolve_dialect(dialect)?;
    let options = ValidationOptions {
        complexity_guard: decode_complexity_guard(complexity_guard)?,
        strict_syntax,
        semantic,
    };

    let result = py.detach(|| polyglot_sql::validate_with_dialect(sql, &dialect, &options));

    Ok(validation_result_from_core(result))
}

#[pyfunction(signature = (sql, schema, dialect = "generic", *, check_types = false, check_references = false, strict = None, semantic = false, strict_syntax = false, complexity_guard = None, function_catalog = None))]
#[allow(clippy::too_many_arguments)]
pub fn validate_with_schema(
    py: Python<'_>,
    sql: &str,
    schema: &Bound<'_, PyAny>,
    dialect: &str,
    check_types: bool,
    check_references: bool,
    strict: Option<bool>,
    semantic: bool,
    strict_syntax: bool,
    complexity_guard: Option<&Bound<'_, PyDict>>,
    function_catalog: Option<&Bound<'_, PyDict>>,
) -> PyResult<ValidationResult> {
    let dialect = resolve_dialect(dialect)?.dialect_type();
    let schema: polyglot_sql::ValidationSchema = depythonize(schema).map_err(|err| {
        pyo3::exceptions::PyValueError::new_err(format!(
            "Invalid schema object (expected ValidationSchema shape): {err}"
        ))
    })?;
    let function_catalog = function_catalog.map(|value| {
        let spec: polyglot_sql::function_catalog::FunctionCatalogSpec = depythonize(value)
            .map_err(|err| pyo3::exceptions::PyValueError::new_err(format!("Invalid function catalog: {err}")))?;
        // Python bool is an int subclass. Check the original values as well
        // so depythonize's integer coercion cannot accept bool arities.
        if let Some(functions) = value.get_item("functions")? {
            for function in functions.try_iter()? {
                if let Ok(function) = function?.cast::<PyDict>() {
                    if let Some(signatures) = function.get_item("signatures")? {
                        for signature in signatures.try_iter()? {
                            if let Ok(signature) = signature?.cast::<PyDict>() {
                                for key in ["minArity", "maxArity"] {
                                    if let Some(arity) = signature.get_item(key)? {
                                        if !arity.is_none() && (arity.is_instance_of::<PyBool>() || !arity.is_instance_of::<PyInt>()) {
                                            return Err(pyo3::exceptions::PyValueError::new_err("Function arities must be nonnegative integers (or None for maxArity)"));
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
        spec.build(dialect)
            .map(|catalog| std::sync::Arc::new(catalog) as std::sync::Arc<dyn polyglot_sql::function_catalog::FunctionCatalog>)
            .map_err(pyo3::exceptions::PyValueError::new_err)
    }).transpose()?;
    let options = polyglot_sql::SchemaValidationOptions {
        complexity_guard: decode_complexity_guard(complexity_guard)?,
        check_types,
        check_references,
        strict,
        semantic,
        strict_syntax,
        function_catalog,
        ..Default::default()
    };
    let result = py.detach(|| polyglot_sql::validate_with_schema(sql, dialect, &schema, &options));
    Ok(validation_result_from_core(result))
}
