//! Stars expand over derived tables in query analysis.

use polyglot_sql::{
    analyze_query, analyze_query_for_project_projections, AnalyzeQueryOptions, DialectType,
    QueryAnalysis, ValidationSchema,
};

fn options(dialect: DialectType) -> AnalyzeQueryOptions {
    AnalyzeQueryOptions {
        complexity_guard: None,
        dialect,
        schema: Some(ValidationSchema {
            tables: vec![],
            strict: None,
        }),
    }
}

fn names(analysis: QueryAnalysis) -> Vec<String> {
    analysis
        .projections
        .into_iter()
        .map(|projection| projection.name.unwrap_or_default())
        .collect()
}

const CASES: [(DialectType, &str, &[&str]); 4] = [
    (
        DialectType::DuckDB,
        "SELECT * FROM (SELECT 1 AS order_id, 'a' AS status)",
        &["order_id", "status"],
    ),
    (
        DialectType::DuckDB,
        "SELECT * FROM (SELECT 1 AS order_id UNION ALL SELECT 2) AS s",
        &["order_id"],
    ),
    (
        DialectType::DuckDB,
        "SELECT * FROM (SELECT 1 AS order_id, 'a' AS status) AS s(a, b)",
        &["a", "b"],
    ),
    (
        DialectType::DuckDB,
        "WITH t AS (SELECT 1 AS order_id) SELECT s.* FROM (SELECT * FROM t) AS s",
        &["order_id"],
    ),
];

#[test]
fn project_projection_analysis_expands_stars_over_derived_tables() {
    for (dialect, sql, expected) in CASES {
        let analysis = analyze_query_for_project_projections(sql, options(dialect)).unwrap();
        assert_eq!(names(analysis), expected, "{dialect:?} {sql}");
    }
}

#[test]
fn default_analysis_expands_stars_over_derived_tables() {
    for (dialect, sql, expected) in CASES {
        let analysis = analyze_query(sql, options(dialect)).unwrap();
        assert_eq!(names(analysis), expected, "{dialect:?} {sql}");
    }
}
