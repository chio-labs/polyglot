//! Snowflake lambdas with typed parameters parse and keep their types in Snowflake output.

use polyglot_sql::{transpile, validate, DialectType};

fn snowflake(sql: &str) -> String {
    transpile(sql, DialectType::Snowflake, DialectType::Snowflake)
        .unwrap_or_else(|error| panic!("Snowflake parse failed for {sql:?}: {error}"))
        .remove(0)
}

#[test]
fn parenthesized_typed_lambda_parameters_parse_and_round_trip() {
    let cases: [(&str, &str); 4] = [
        (
            "SELECT REDUCE(v:items, NULL::DOUBLE, (total DOUBLE, item VARIANT) -> COALESCE(total, 0) + TRY_TO_DOUBLE(item:amount::VARCHAR)) AS total FROM t",
            "(total DOUBLE, item VARIANT) -> COALESCE(total, 0) + TRY_TO_DOUBLE(CAST(GET_PATH(item, 'amount') AS VARCHAR))",
        ),
        (
            "SELECT FILTER(xs, (x INT) -> x > 1) FROM t",
            "FILTER(xs, (x INT) -> x > 1)",
        ),
        (
            "SELECT TRANSFORM(xs, x INT -> x + 1) FROM t",
            "TRANSFORM(xs, x -> CAST(x AS INT) + 1)",
        ),
        (
            "SELECT REDUCE(xs, 0, (acc, x) -> acc + x) FROM t",
            "REDUCE(xs, 0, (acc, x) -> acc + x)",
        ),
    ];
    for (sql, expected_fragment) in cases {
        assert!(validate(sql, DialectType::Snowflake).valid, "{sql}");
        let generated: String = snowflake(sql);
        assert!(
            generated.contains(expected_fragment),
            "{sql}\n=> {generated}"
        );
    }
}
