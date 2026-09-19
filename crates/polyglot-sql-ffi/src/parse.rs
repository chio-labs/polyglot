use crate::helpers::{dialect_by_name, err_result, ok_json_result, panic_result, required_arg};
use crate::types::{PolyglotResult, STATUS_PARSE_ERROR, STATUS_SERIALIZATION_ERROR};
use polyglot_sql::ParseOptions;
use std::os::raw::c_char;

/// Parse SQL into an AST JSON string.
#[no_mangle]
pub extern "C" fn polyglot_parse(sql: *const c_char, dialect: *const c_char) -> PolyglotResult {
    parse(sql, dialect, None, ParseKind::Statements)
}

/// Parse a single SQL statement into a JSON AST object.
#[no_mangle]
pub extern "C" fn polyglot_parse_one(sql: *const c_char, dialect: *const c_char) -> PolyglotResult {
    parse(sql, dialect, None, ParseKind::One)
}

/// Parse a standalone SQL data type into a JSON DataType object.
#[no_mangle]
pub extern "C" fn polyglot_parse_data_type(
    sql: *const c_char,
    dialect: *const c_char,
) -> PolyglotResult {
    parse(sql, dialect, None, ParseKind::DataType)
}

/// Parse SQL using ParseOptions JSON, e.g. {"complexityGuard":{"maxFunctionCallDepth":128}}.
/// All arguments must be non-NULL UTF-8 strings. Free with polyglot_free_result.
#[no_mangle]
pub extern "C" fn polyglot_parse_with_options(
    sql: *const c_char,
    dialect: *const c_char,
    options_json: *const c_char,
) -> PolyglotResult {
    parse(sql, dialect, Some(options_json), ParseKind::Statements)
}

/// Parse exactly one statement using ParseOptions JSON. Pass "{}" for defaults.
/// All arguments must be non-NULL UTF-8 strings. Free with polyglot_free_result.
#[no_mangle]
pub extern "C" fn polyglot_parse_one_with_options(
    sql: *const c_char,
    dialect: *const c_char,
    options_json: *const c_char,
) -> PolyglotResult {
    parse(sql, dialect, Some(options_json), ParseKind::One)
}

/// Parse a standalone data type using ParseOptions JSON. Pass "{}" for defaults.
/// All arguments must be non-NULL UTF-8 strings. Free with polyglot_free_result.
#[no_mangle]
pub extern "C" fn polyglot_parse_data_type_with_options(
    sql: *const c_char,
    dialect: *const c_char,
    options_json: *const c_char,
) -> PolyglotResult {
    parse(sql, dialect, Some(options_json), ParseKind::DataType)
}

enum ParseKind {
    Statements,
    One,
    DataType,
}

fn parse(
    sql: *const c_char,
    dialect: *const c_char,
    options: Option<*const c_char>,
    kind: ParseKind,
) -> PolyglotResult {
    match std::panic::catch_unwind(|| parse_impl(sql, dialect, options, kind)) {
        Ok(result) => result,
        Err(panic) => panic_result(panic),
    }
}

fn parse_impl(
    sql: *const c_char,
    dialect: *const c_char,
    options: Option<*const c_char>,
    kind: ParseKind,
) -> PolyglotResult {
    let sql = match unsafe { required_arg(sql, "sql") } {
        Ok(value) => value,
        Err(result) => return result,
    };
    let dialect_name = match unsafe { required_arg(dialect, "dialect") } {
        Ok(value) => value,
        Err(result) => return result,
    };

    let dialect = match dialect_by_name(&dialect_name) {
        Ok(dialect) => dialect,
        Err(result) => return result,
    };

    let options = match options {
        Some(options) => {
            let json = match unsafe { required_arg(options, "options_json") } {
                Ok(value) => value,
                Err(result) => return result,
            };
            match serde_json::from_str::<ParseOptions>(&json) {
                Ok(value) => value,
                Err(error) => {
                    return err_result(
                        STATUS_SERIALIZATION_ERROR,
                        format!("Invalid parse options JSON: {error}"),
                    )
                }
            }
        }
        None => ParseOptions::default(),
    };

    if matches!(kind, ParseKind::DataType) {
        return match dialect.parse_data_type_with_options(&sql, &options) {
            Ok(data_type) => ok_json_result(&data_type),
            Err(error) => err_result(STATUS_PARSE_ERROR, error.to_string()),
        };
    }

    match dialect.parse_with_options(&sql, &options) {
        Ok(mut expressions) => {
            if matches!(kind, ParseKind::Statements) {
                return ok_json_result(&expressions);
            }
            if expressions.len() != 1 {
                return err_result(
                    STATUS_PARSE_ERROR,
                    format!("Expected 1 statement, found {}", expressions.len()),
                );
            }
            ok_json_result(&expressions.remove(0))
        }
        Err(error) => err_result(STATUS_PARSE_ERROR, error.to_string()),
    }
}
