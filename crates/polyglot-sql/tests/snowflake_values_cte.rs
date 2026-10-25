//! Snowflake rejects a CTE whose body is a bare VALUES list; VALUES in FROM stays valid.

use polyglot_sql::{parse, validate, DialectType};

#[test]
fn snowflake_bare_values_cte_body_is_a_parse_error() {
    let error = parse(
        "WITH v AS (VALUES (1), (2)) SELECT v.column1 FROM v",
        DialectType::Snowflake,
    )
    .unwrap_err()
    .to_string();
    assert!(
        error.contains("bare VALUES list as the body of CTE 'v'"),
        "{error}"
    );
    assert!(
        !validate(
            "WITH v AS (VALUES (1), (2)) SELECT v.column1 FROM v",
            DialectType::Snowflake
        )
        .valid
    );
}

#[test]
fn values_in_from_and_other_dialects_stay_valid() {
    let cases: [(&str, DialectType); 5] = [
        (
            "WITH v AS (SELECT t.c FROM (VALUES (1), (2)) AS t(c)) SELECT v.c FROM v",
            DialectType::Snowflake,
        ),
        (
            "WITH v AS (SELECT * FROM VALUES (1), (2)) SELECT v.column1 FROM v",
            DialectType::Snowflake,
        ),
        (
            "SELECT t.c FROM (VALUES (1), (2)) AS t(c)",
            DialectType::Snowflake,
        ),
        (
            "WITH v AS (VALUES (1), (2)) SELECT * FROM v",
            DialectType::DuckDB,
        ),
        (
            "WITH v AS (VALUES (1), (2)) SELECT * FROM v",
            DialectType::PostgreSQL,
        ),
    ];
    for (sql, dialect) in cases {
        assert!(validate(sql, dialect).valid, "{dialect:?} {sql}");
    }
}
