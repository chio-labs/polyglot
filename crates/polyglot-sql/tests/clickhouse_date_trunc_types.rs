//! ClickHouse DATE_TRUNC types follow its unit and value, not its string unit argument.

use polyglot_sql::{
    analyze_query_for_project_projections, AnalyzeQueryOptions, DialectType, ValidationSchema,
};

fn projected_type(sql: &str) -> Option<String> {
    let options = AnalyzeQueryOptions {
        complexity_guard: None,
        dialect: DialectType::ClickHouse,
        schema: Some(ValidationSchema {
            tables: vec![],
            strict: None,
        }),
    };
    analyze_query_for_project_projections(sql, options)
        .unwrap()
        .projections
        .remove(0)
        .type_hint
}

#[test]
fn date_trunc_types_follow_unit_and_value() {
    let cases: [(&str, Option<&str>); 7] = [
        (
            "SELECT DATE_TRUNC('hour', CAST('2026-01-01 10:11:00' AS DateTime)) AS h",
            Some("DateTime"),
        ),
        (
            "SELECT dateTrunc('second', CAST('2026-01-01 10:11:00' AS DateTime64(6))) AS h",
            Some("DateTime64(6)"),
        ),
        (
            "SELECT dateTrunc('day', CAST('2026-01-01' AS Date)) AS h",
            Some("DateTime"),
        ),
        (
            "SELECT dateTrunc('month', CAST('2026-01-01 10:11:00' AS DateTime)) AS h",
            Some("DATE"),
        ),
        (
            "SELECT dateTrunc('year', CAST('2026-01-01 10:11:00' AS DateTime64(3))) AS h",
            Some("Date32"),
        ),
        (
            "SELECT dateTrunc('hour', CAST(NULL AS Nullable(DateTime))) AS h",
            Some("Nullable(DateTime)"),
        ),
        ("SELECT dateTrunc('hour', 'not a date') AS h", None),
    ];
    for (sql, expected) in cases {
        assert_eq!(projected_type(sql).as_deref(), expected, "{sql}");
    }
}
