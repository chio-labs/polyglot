//! Column-alias lists on table functions and base tables name the relation's columns.

use polyglot_sql::{validate_with_schema, DialectType, SchemaValidationOptions, ValidationSchema};

fn schema() -> ValidationSchema {
    serde_json::from_value(serde_json::json!({
        "tables": [{"name": "orders", "columns": [
            {"name": "id", "type": "INTEGER"},
            {"name": "items", "type": "VARCHAR"}
        ]}],
        "strict": true
    }))
    .unwrap()
}

fn options() -> SchemaValidationOptions {
    SchemaValidationOptions {
        semantic: true,
        check_types: true,
        check_references: true,
        ..Default::default()
    }
}

fn error_codes(sql: &str, dialect: DialectType) -> Vec<String> {
    validate_with_schema(sql, dialect, &schema(), &options())
        .errors
        .into_iter()
        .filter(|error| error.severity == polyglot_sql::ValidationSeverity::Error)
        .map(|error| format!("{}: {}", error.code, error.message))
        .collect()
}

#[test]
fn column_alias_lists_name_relation_columns() {
    let cases: [(&str, bool); 9] = [
        ("SELECT g.n FROM generate_series(1, 3) AS g(n)", true),
        ("SELECT n FROM generate_series(1, 3) AS g(n)", true),
        ("SELECT o.a, o.b FROM orders AS o(a, b)", true),
        ("SELECT a, b FROM orders AS o(a, b)", true),
        ("SELECT o.a, o.items FROM orders AS o(a)", true),
        ("SELECT o.id FROM orders AS o(a, b)", false),
        ("SELECT o.c FROM orders AS o(a, b)", false),
        ("SELECT o.a FROM orders AS o(a, b, c)", false),
        ("SELECT o.id FROM orders AS o", true),
    ];
    for dialect in [DialectType::DuckDB, DialectType::Snowflake] {
        for (sql, valid) in cases {
            let errors: Vec<String> = error_codes(sql, dialect);
            assert_eq!(errors.is_empty(), valid, "{dialect:?} {sql}: {errors:?}");
        }
    }
}
