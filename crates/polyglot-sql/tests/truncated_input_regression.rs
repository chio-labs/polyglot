//! Parsing any prefix of a valid statement returns a result or an error; it never panics.

use std::panic::{catch_unwind, AssertUnwindSafe};

use polyglot_sql::{parse, DialectType};

const DIALECTS: [DialectType; 9] = [
    DialectType::Generic,
    DialectType::TSQL,
    DialectType::Snowflake,
    DialectType::DuckDB,
    DialectType::BigQuery,
    DialectType::PostgreSQL,
    DialectType::ClickHouse,
    DialectType::SQLite,
    DialectType::Databricks,
];

const STATEMENTS: [&str; 6] = [
    "SELECT IF(region > 1, region, 0) AS r FROM sales",
    "SELECT o.id, CASE WHEN o.amount > 10 THEN 'big' ELSE 'small' END AS size FROM orders AS o",
    "WITH t AS (SELECT a, b FROM x WHERE a IN (1, 2)) SELECT t.a, SUM(t.b) OVER (PARTITION BY t.a ORDER BY t.b) FROM t",
    "SELECT CAST(o.amount AS DECIMAL(10, 2)), COALESCE(o.note, '') FROM orders AS o LEFT JOIN items AS i ON i.order_id = o.id",
    "SELECT a FROM t UNION ALL SELECT b FROM u ORDER BY 1 LIMIT 5",
    "SELECT x.n FROM (SELECT 1 AS n) AS x GROUP BY x.n HAVING COUNT(*) > 1",
];

#[test]
fn every_prefix_of_a_statement_parses_or_errors_without_panicking() {
    let mut panics: Vec<String> = Vec::new();
    for dialect in DIALECTS {
        for statement in STATEMENTS {
            for (end, _) in statement.char_indices().skip(1) {
                let prefix: &str = &statement[..end];
                let outcome = catch_unwind(AssertUnwindSafe(|| parse(prefix, dialect)));
                if outcome.is_err() {
                    panics.push(format!("{dialect:?}: {prefix:?}"));
                }
            }
        }
    }
    assert!(
        panics.is_empty(),
        "parser panicked on:\n{}",
        panics.join("\n")
    );
}

#[test]
fn tsql_if_with_an_unclosed_condition_is_a_parse_error() {
    assert!(parse("SELECT IF(region > 1, re", DialectType::TSQL).is_err());
}
