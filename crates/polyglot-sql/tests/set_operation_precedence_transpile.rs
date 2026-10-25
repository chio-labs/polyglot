//! Transpiled set-operation chains keep Snowflake's grouping in targets with other precedence.

use polyglot_sql::{transpile, DialectType};

fn snowflake_to(sql: &str, target: DialectType) -> String {
    transpile(sql, DialectType::Snowflake, target)
        .unwrap_or_else(|error| panic!("transpile failed for {sql:?}: {error}"))
        .remove(0)
}

#[test]
fn right_nested_intersect_is_grouped_where_operators_evaluate_left_to_right() {
    let cases: [(&str, DialectType, &str); 7] = [
        (
            "SELECT 1 UNION SELECT 2 INTERSECT SELECT 2",
            DialectType::SQLite,
            "SELECT 1 UNION SELECT * FROM (SELECT 2 INTERSECT SELECT 2)",
        ),
        (
            "SELECT 1 EXCEPT SELECT 1 INTERSECT SELECT 2",
            DialectType::SQLite,
            "SELECT 1 EXCEPT SELECT * FROM (SELECT 1 INTERSECT SELECT 2)",
        ),
        (
            "SELECT 2 INTERSECT SELECT 2 UNION SELECT 1",
            DialectType::SQLite,
            "SELECT 2 INTERSECT SELECT 2 UNION SELECT 1",
        ),
        (
            "SELECT 1 UNION SELECT 2 INTERSECT SELECT 2",
            DialectType::Oracle,
            "SELECT 1 UNION (SELECT 2 INTERSECT SELECT 2)",
        ),
        (
            "SELECT 1 UNION SELECT 2 INTERSECT SELECT 2",
            DialectType::BigQuery,
            "SELECT 1 UNION DISTINCT (SELECT 2 INTERSECT DISTINCT SELECT 2)",
        ),
        (
            "SELECT 1 UNION SELECT 2 INTERSECT SELECT 2",
            DialectType::DuckDB,
            "SELECT 1 UNION SELECT 2 INTERSECT SELECT 2",
        ),
        (
            "SELECT 1 UNION SELECT 2 INTERSECT SELECT 2",
            DialectType::Snowflake,
            "SELECT 1 UNION SELECT 2 INTERSECT SELECT 2",
        ),
    ];
    for (sql, target, expected) in cases {
        assert_eq!(snowflake_to(sql, target), expected, "{sql} -> {target:?}");
    }
}
