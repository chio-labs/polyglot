//! Portable signature families; dialect overloads remain explicit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArgumentType {
    Any,
    Numeric,
    Integer,
    String,
    Sequence,
    Temporal,
    Boolean,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReturnType {
    Argument(usize),
    Average,
    Numeric,
    Integer,
    String,
    Boolean,
    Timestamp,
}

#[derive(Debug, Clone, Copy)]
pub struct TypeSignature {
    pub arguments: &'static [ArgumentType],
    pub returns: ReturnType,
}

/// Type metadata is intentionally partial: absence means unchecked, not invalid.
pub fn type_signature(dialect: &str, name: &str) -> Option<TypeSignature> {
    use ArgumentType::*;
    if !matches!(dialect, "duckdb" | "postgres" | "snowflake" | "bigquery") {
        return None;
    }
    let (arguments, returns): (&'static [ArgumentType], ReturnType) = match name {
        "sum" if dialect == "duckdb" => (&[Any], ReturnType::Numeric),
        "sum" => (&[Numeric], ReturnType::Argument(0)),
        // DuckDB also implements temporal AVG overloads. Preserve their return type.
        "avg" if dialect == "duckdb" => (&[Any], ReturnType::Average),
        "avg" | "stddev" | "stddev_pop" | "stddev_samp" | "variance" | "var_pop" | "var_samp" => {
            (&[Numeric], ReturnType::Numeric)
        }
        "min" | "max" | "first_value" | "last_value" => (&[Any], ReturnType::Argument(0)),
        "count" | "row_number" | "rank" | "dense_rank" => (&[Any], ReturnType::Integer),
        "ntile" => (&[Integer], ReturnType::Integer),
        "lag" | "lead" => (&[Any, Integer, Any], ReturnType::Argument(0)),
        "abs" | "round" | "floor" | "ceil" | "ceiling" => {
            (&[Numeric, Integer], ReturnType::Argument(0))
        }
        "sqrt" | "power" | "pow" | "mod" => (&[Numeric, Numeric], ReturnType::Numeric),
        "upper" | "lower" | "trim" | "ltrim" | "rtrim" => (&[String, String], ReturnType::String),
        "length" | "char_length" => (&[Sequence], ReturnType::Integer),
        "substring" | "substr" => (&[String, Integer, Integer], ReturnType::String),
        "left" | "right" => (&[String, Integer], ReturnType::String),
        "replace" => (&[String, String, String], ReturnType::String),
        "split_part" => (&[String, String, Integer], ReturnType::String),
        "regexp_matches" | "regexp_like" | "regexp_full_match" => {
            (&[String, String, String], ReturnType::Boolean)
        }
        "regexp_replace" => (&[String, String, String, String], ReturnType::String),
        "regexp_extract" => (&[String, String, Any, String], ReturnType::String),
        "concat" => (&[Any], ReturnType::String),
        "string_agg" | "listagg" => (&[Any, String], ReturnType::String),
        "date_trunc" if dialect == "bigquery" => (&[Temporal, Any], ReturnType::Argument(0)),
        "date_trunc" if dialect == "snowflake" => (&[String, Temporal], ReturnType::Argument(1)),
        "date_trunc" => (&[String, Temporal], ReturnType::Timestamp),
        "extract" | "date_part" => (&[String, Temporal], ReturnType::Integer),
        "date_diff" if dialect == "bigquery" => (&[Temporal, Temporal, Any], ReturnType::Integer),
        "date_diff" | "datediff" => (&[String, Temporal, Temporal], ReturnType::Integer),
        "dateadd" => (&[String, Integer, Temporal], ReturnType::Argument(2)),
        "strftime" => (&[Temporal, String], ReturnType::String),
        "to_char" => (&[Any, String], ReturnType::String),
        "coalesce" | "nullif" | "ifnull" | "nvl" | "greatest" | "least" => {
            (&[Any], ReturnType::Argument(0))
        }
        "if" | "iff" => (&[Boolean, Any, Any], ReturnType::Argument(1)),
        _ => return None,
    };
    Some(TypeSignature { arguments, returns })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signatures_do_not_guess_unknown_dialects_or_functions() {
        assert!(type_signature("unknown", "sum").is_none());
        assert!(type_signature("duckdb", "order_total").is_none());
        assert_eq!(
            type_signature("duckdb", "lag").unwrap().arguments.get(1),
            Some(&ArgumentType::Integer)
        );
        assert_eq!(
            type_signature("duckdb", "split_part").unwrap().returns,
            ReturnType::String
        );
        assert_eq!(
            type_signature("bigquery", "date_diff").unwrap().arguments[0],
            ArgumentType::Temporal
        );
        assert_eq!(
            type_signature("duckdb", "date_diff").unwrap().arguments[0],
            ArgumentType::String
        );
    }
}
