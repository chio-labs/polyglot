//! DuckDB output writes every lambda as `LAMBDA x : expr`. DuckDB 2.0 rejects the arrow form
//! (`x -> expr`) by default and DuckDB 2.1 removes it, while the colon form works from 1.3.
use polyglot_sql::{transpile, DialectType};

fn to_duckdb(sql: &str, read: DialectType) -> String {
    transpile(sql, read, DialectType::DuckDB).unwrap().remove(0)
}

#[test]
fn snowflake_filter_lambda_is_written_with_a_colon_for_duckdb() {
    assert_eq!(
        to_duckdb(
            "SELECT FILTER(arr, x -> x > 1) FROM t",
            DialectType::Snowflake
        ),
        "SELECT LIST_FILTER(arr, LAMBDA x : x > 1) FROM t"
    );
}

#[test]
fn databricks_filter_lambda_is_written_with_a_colon_for_duckdb() {
    assert_eq!(
        to_duckdb(
            "SELECT FILTER(arr, x -> x > 1) FROM t",
            DialectType::Databricks
        ),
        "SELECT LIST_FILTER(arr, LAMBDA x : x > 1) FROM t"
    );
}

#[test]
fn multi_parameter_lambda_lists_parameters_without_parentheses() {
    assert_eq!(
        to_duckdb(
            "SELECT LIST_REDUCE(arr, (acc, x) -> acc + x) FROM t",
            DialectType::DuckDB
        ),
        "SELECT LIST_REDUCE(arr, LAMBDA acc, x : acc + x) FROM t"
    );
}

#[test]
fn duckdb_arrow_lambda_is_rewritten_and_colon_lambda_round_trips() {
    assert_eq!(
        to_duckdb(
            "SELECT LIST_TRANSFORM(arr, x -> x * 2) FROM t",
            DialectType::DuckDB
        ),
        "SELECT LIST_TRANSFORM(arr, LAMBDA x : x * 2) FROM t"
    );
    assert_eq!(
        to_duckdb(
            "SELECT LIST_TRANSFORM(arr, lambda x: x * 2) FROM t",
            DialectType::DuckDB
        ),
        "SELECT LIST_TRANSFORM(arr, LAMBDA x : x * 2) FROM t"
    );
}

#[test]
fn nested_lambdas_are_both_written_with_a_colon() {
    assert_eq!(
        to_duckdb(
            "SELECT LIST_TRANSFORM(arr, x -> LIST_FILTER(x, y -> y > 0)) FROM t",
            DialectType::DuckDB
        ),
        "SELECT LIST_TRANSFORM(arr, LAMBDA x : LIST_FILTER(x, LAMBDA y : y > 0)) FROM t"
    );
}

#[test]
fn other_targets_keep_the_arrow_form() {
    for target in [
        DialectType::Snowflake,
        DialectType::Databricks,
        DialectType::Spark,
    ] {
        let out = transpile(
            "SELECT FILTER(arr, x -> x > 1) FROM t",
            DialectType::Snowflake,
            target,
        )
        .unwrap()
        .remove(0);
        assert!(out.contains("x -> x > 1"), "target {target:?}: {out}");
    }
}
