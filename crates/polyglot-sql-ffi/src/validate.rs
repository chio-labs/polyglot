use crate::helpers::{
    dialect_by_name, err_validation_result, panic_validation_result, required_arg_validation,
    validation_result_from_core,
};
use crate::types::{PolyglotValidationResult, STATUS_INVALID_ARGUMENT, STATUS_SERIALIZATION_ERROR};
use polyglot_sql::ValidationOptions;
use std::os::raw::c_char;

/// Validate SQL against a ValidationSchema JSON object using the shared Rust engine.
///
/// All arguments must be non-NULL, UTF-8, NUL-terminated strings. Pass "{}" for
/// default options. SchemaValidationOptions accepts snake_case option names
/// (check_types, check_references, strict, semantic, strict_syntax), with
/// camelCase aliases for the compound names. Unknown options are rejected.
/// Free the returned payload with polyglot_free_validation_result.
#[no_mangle]
pub extern "C" fn polyglot_validate_with_schema(
    sql: *const c_char,
    schema_json: *const c_char,
    dialect: *const c_char,
    options_json: *const c_char,
) -> PolyglotValidationResult {
    match std::panic::catch_unwind(|| {
        validate_with_schema_impl(sql, schema_json, dialect, options_json)
    }) {
        Ok(result) => result,
        Err(panic) => panic_validation_result(panic),
    }
}

fn validate_with_schema_impl(
    sql: *const c_char,
    schema_json: *const c_char,
    dialect: *const c_char,
    options_json: *const c_char,
) -> PolyglotValidationResult {
    let (sql, dialect) = match validation_input(sql, dialect) {
        Ok(input) => input,
        Err(result) => return result,
    };
    let schema_json = match unsafe { required_arg_validation(schema_json, "schema_json") } {
        Ok(value) => value,
        Err(result) => return result,
    };
    let options_json = match unsafe { required_arg_validation(options_json, "options_json") } {
        Ok(value) => value,
        Err(result) => return result,
    };
    let schema: polyglot_sql::ValidationSchema = match serde_json::from_str(&schema_json) {
        Ok(value) => value,
        Err(error) => {
            return err_validation_result(
                STATUS_SERIALIZATION_ERROR,
                format!("Invalid validation schema JSON: {error}"),
            )
        }
    };
    let options: polyglot_sql::SchemaValidationOptions = match serde_json::from_str(&options_json) {
        Ok(value) => value,
        Err(error) => {
            return err_validation_result(
                STATUS_SERIALIZATION_ERROR,
                format!("Invalid schema validation options JSON: {error}"),
            )
        }
    };
    validation_result_from_core(polyglot_sql::validate_with_schema(
        &sql,
        dialect.dialect_type(),
        &schema,
        &options,
    ))
}

/// Validate SQL syntax for a dialect.
#[no_mangle]
pub extern "C" fn polyglot_validate(
    sql: *const c_char,
    dialect: *const c_char,
) -> PolyglotValidationResult {
    match std::panic::catch_unwind(|| validate_impl(sql, dialect, None)) {
        Ok(result) => result,
        Err(panic) => panic_validation_result(panic),
    }
}

/// Validate SQL syntax and optional semantic warnings for a dialect.
///
/// `options_json` must be a JSON object compatible with `ValidationOptions`, e.g.
/// `{"strictSyntax": true, "semantic": true}`.
#[no_mangle]
pub extern "C" fn polyglot_validate_with_options(
    sql: *const c_char,
    dialect: *const c_char,
    options_json: *const c_char,
) -> PolyglotValidationResult {
    match std::panic::catch_unwind(|| validate_with_options_impl(sql, dialect, options_json)) {
        Ok(result) => result,
        Err(panic) => panic_validation_result(panic),
    }
}

fn validate_with_options_impl(
    sql: *const c_char,
    dialect: *const c_char,
    options_json: *const c_char,
) -> PolyglotValidationResult {
    let options_json = match unsafe { required_arg_validation(options_json, "options_json") } {
        Ok(value) => value,
        Err(result) => return result,
    };
    let options: ValidationOptions = match serde_json::from_str(&options_json) {
        Ok(value) => value,
        Err(error) => {
            return err_validation_result(
                STATUS_SERIALIZATION_ERROR,
                format!("Invalid validation options JSON: {error}"),
            );
        }
    };

    validate_impl(sql, dialect, Some(&options))
}

fn validate_impl(
    sql: *const c_char,
    dialect: *const c_char,
    options: Option<&ValidationOptions>,
) -> PolyglotValidationResult {
    let (sql, dialect) = match validation_input(sql, dialect) {
        Ok(input) => input,
        Err(result) => return result,
    };

    let default_options = ValidationOptions::default();
    let result =
        polyglot_sql::validate_with_dialect(&sql, &dialect, options.unwrap_or(&default_options));

    validation_result_from_core(result)
}

fn validation_input(
    sql: *const c_char,
    dialect: *const c_char,
) -> Result<(String, polyglot_sql::dialects::Dialect), PolyglotValidationResult> {
    let sql = unsafe { required_arg_validation(sql, "sql") }?;
    let dialect_name = unsafe { required_arg_validation(dialect, "dialect") }?;
    let dialect = dialect_by_name(&dialect_name).map_err(|_| {
        err_validation_result(
            STATUS_INVALID_ARGUMENT,
            format!("Unknown dialect: {dialect_name}"),
        )
    })?;
    Ok((sql, dialect))
}
