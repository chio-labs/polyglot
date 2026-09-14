use super::*;
use crate::function_catalog::{FunctionNameCase, FunctionSignature, HashMapFunctionCatalog};
use std::sync::Arc;

#[test]
fn test_schema_validation_options_json_names() {
    for json in [
        r#"{"check_types":true,"check_references":true,"strict_syntax":true,"strict":false}"#,
        r#"{"checkTypes":true,"checkReferences":true,"strictSyntax":true,"strict":false}"#,
    ] {
        let options: SchemaValidationOptions = serde_json::from_str(json).unwrap();
        assert!(options.check_types && options.check_references && options.strict_syntax);
        assert_eq!(options.strict, Some(false));
    }
    assert!(serde_json::from_str::<SchemaValidationOptions>(r#"{"checkType":true}"#).is_err());
}

#[test]
fn test_schema_validation_lexical_scopes() {
    let schema = base_schema();
    for check_references in [false, true] {
        let options = SchemaValidationOptions {
            check_references,
            ..Default::default()
        };
        for sql in [
            "WITH a AS (SELECT id AS k FROM users), b AS (SELECT k FROM a) SELECT k FROM b",
            "WITH a(k) AS (SELECT id FROM users), b AS (SELECT k FROM a) SELECT b.k FROM b",
            "SELECT u.id FROM users u WHERE EXISTS (SELECT 1 FROM orders o WHERE o.user_id = u.id)",
            "SELECT u.id FROM users u WHERE EXISTS (SELECT 1 FROM orders o WHERE o.user_id = age)",
            "SELECT u.id FROM users u WHERE EXISTS (SELECT 1 FROM orders o WHERE EXISTS (SELECT 1 WHERE u.id = o.user_id))",
            "SELECT u.id FROM users u WHERE EXISTS (SELECT u.id FROM orders u)",
            "WITH q AS (SELECT id FROM users) SELECT q.id FROM q WHERE EXISTS (WITH q AS (SELECT total FROM orders) SELECT total FROM q)",
            "WITH unused AS (SELECT id FROM orders) SELECT id FROM users",
            "SELECT q.id FROM (SELECT id FROM users) q",
            "SELECT u.id FROM users u WHERE EXISTS (SELECT u.id UNION ALL SELECT u.id)",
            "SELECT u.id FROM users u JOIN orders o ON EXISTS (SELECT 1 WHERE o.user_id = u.id)",
        ] {
            let result = validate_with_schema(sql, DialectType::Snowflake, &schema, &options);
            assert!(result.valid, "{sql}: {:?}", result.errors);
        }
        for (sql, code) in [
            ("SELECT id FROM q WHERE EXISTS (WITH q AS (SELECT id FROM users) SELECT id FROM q)", "E200"),
            ("WITH q AS (SELECT id FROM users) SELECT q.id FROM users", "E222"),
            ("WITH q AS (SELECT id FROM users) SELECT age FROM q", "E201"),
            ("SELECT u.id FROM users u WHERE EXISTS (SELECT u.age FROM orders u)", "E201"),
            ("SELECT u.id FROM users u JOIN (SELECT u.id) q ON TRUE", "E222"),
            ("SELECT u.id FROM users u WHERE EXISTS (SELECT 1 FROM orders o WHERE u.missing = o.id)", "E201"),
            ("WITH q AS (SELECT id FROM users) SELECT id FROM public.q", "E200"),
        ] {
            let result = validate_with_schema(sql, DialectType::Snowflake, &schema, &options);
            assert!(!result.valid && result.errors.iter().any(|error| error.code == code), "{sql}: {:?}", result.errors);
        }
    }
}

#[test]
fn test_schema_validation_issue_441_nested_scopes() {
    let schema = ValidationSchema {
        tables: ["t1", "t2"]
            .into_iter()
            .map(|name| SchemaTable {
                name: name.to_string(),
                schema: None,
                columns: vec![SchemaColumn {
                    name: "id".to_string(),
                    data_type: "NUMBER".to_string(),
                    nullable: None,
                    primary_key: false,
                    unique: false,
                    references: None,
                }],
                aliases: vec![],
                primary_key: vec![],
                unique_keys: vec![],
                foreign_keys: vec![],
            })
            .collect(),
        strict: Some(true),
    };
    let options = SchemaValidationOptions {
        check_types: false,
        check_references: true,
        strict: Some(true),
        semantic: false,
        strict_syntax: false,
        ..Default::default()
    };

    for (case, sql) in [
        (
            "qualified_cte_column_is_not_ambiguous",
            "WITH a AS (SELECT id FROM t1), b AS (SELECT id FROM t2) \
             SELECT a.id FROM a JOIN b ON a.id = b.id",
        ),
        (
            "correlated_subquery_resolves_outer_alias",
            "SELECT outer_table.id FROM t1 outer_table \
             WHERE NOT EXISTS ( \
               SELECT 1 FROM t2 inner_table \
               WHERE inner_table.id = outer_table.id \
             )",
        ),
    ] {
        let result = validate_with_schema(sql, DialectType::Snowflake, &schema, &options);
        assert!(result.valid, "{case}: {:#?}", result.errors);
        assert!(result.errors.is_empty(), "{case}: {:#?}", result.errors);
    }
}

#[test]
fn test_schema_validation_issue_442_prior_cte_outputs() {
    let schema = ValidationSchema {
        tables: vec![SchemaTable {
            name: "t1".to_string(),
            schema: None,
            columns: ["id", "value"]
                .into_iter()
                .map(|name| SchemaColumn {
                    name: name.to_string(),
                    data_type: "NUMBER".to_string(),
                    nullable: None,
                    primary_key: false,
                    unique: false,
                    references: None,
                })
                .collect(),
            aliases: vec![],
            primary_key: vec![],
            unique_keys: vec![],
            foreign_keys: vec![],
        }],
        strict: Some(true),
    };
    let options = SchemaValidationOptions {
        check_types: false,
        check_references: true,
        strict: Some(true),
        semantic: false,
        strict_syntax: false,
        ..Default::default()
    };

    for (case, sql) in [
        (
            "subsequent_cte_resolves_prior_cte_projection",
            "WITH derived AS (SELECT value AS derived_value FROM t1), \
             next AS (SELECT derived_value FROM derived) \
             SELECT derived_value FROM next",
        ),
        (
            "window_order_by_resolves_prior_cte_projection",
            "WITH scored AS (SELECT id, value AS info_score FROM t1), \
             ranked AS ( \
               SELECT id, ROW_NUMBER() OVER ( \
                 PARTITION BY id ORDER BY info_score DESC \
               ) AS rn \
               FROM scored \
             ) \
             SELECT id FROM ranked",
        ),
    ] {
        let result = validate_with_schema(sql, DialectType::Snowflake, &schema, &options);
        assert!(result.valid, "{case}: {:#?}", result.errors);
        assert!(result.errors.is_empty(), "{case}: {:#?}", result.errors);
    }
}

#[test]
fn test_schema_validation_open_sources() {
    for columns in [serde_json::json!([]), serde_json::json!([{"name": "*"}])] {
        let mut schema = base_schema();
        schema.tables[0].columns = serde_json::from_value(columns).unwrap();
        for sql in [
            "SELECT missing FROM users",
            "SELECT u.missing FROM users u",
            "SELECT payload.field FROM users",
            "SELECT o.id FROM orders o WHERE EXISTS (SELECT 1 FROM users u WHERE u.missing = o.id)",
            "SELECT missing FROM users u JOIN orders o ON TRUE",
            "SELECT missing FROM orders o JOIN users u ON TRUE",
            "WITH q AS (SELECT * FROM users) SELECT missing FROM q",
            "SELECT q.missing FROM (SELECT * FROM users) q",
        ] {
            let result = validate_with_schema(
                sql,
                DialectType::Snowflake,
                &schema,
                &SchemaValidationOptions {
                    check_references: true,
                    ..Default::default()
                },
            );
            assert!(result.valid, "{sql}: {:?}", result.errors);
        }
        let result = validate_with_schema(
            "SELECT o.missing FROM users u JOIN orders o ON TRUE",
            DialectType::Snowflake,
            &schema,
            &SchemaValidationOptions::default(),
        );
        assert!(!result.valid);
    }
}

#[test]
fn test_schema_reference_spans_and_ambiguity_options() {
    let schema = base_schema();
    for (sql, code, token) in [
        (
            "SELECT u.id FROM users u WHERE u.missing = TRUE",
            "E201",
            "missing",
        ),
        ("SELECT x.id FROM users", "E222", "x"),
        ("SELECT * FROM absent", "E200", "absent"),
        (
            "SELECT '😀', u.\"míssing\" FROM users u",
            "E201",
            "\"míssing\"",
        ),
        (
            "SELECT id FROM users u JOIN orders o ON u.id = o.user_id",
            "E221",
            "id",
        ),
    ] {
        let result = validate_with_schema(
            sql,
            DialectType::Snowflake,
            &schema,
            &SchemaValidationOptions {
                check_references: true,
                ..Default::default()
            },
        );
        let error = result
            .errors
            .iter()
            .find(|error| error.code == code)
            .unwrap_or_else(|| panic!("{sql}: {:?}", result.errors));
        let start = sql[..sql.find(token).unwrap()].chars().count();
        assert_eq!(error.start, Some(start), "{sql}");
        assert_eq!(error.end, Some(start + token.chars().count()), "{sql}");
        assert_eq!(error.line, Some(1));
        assert!(error.column.is_some());
    }
    let sql = "SELECT missing, missing FROM users";
    let result = validate_with_schema(
        sql,
        DialectType::Snowflake,
        &schema,
        &SchemaValidationOptions::default(),
    );
    let starts: Vec<_> = result
        .errors
        .iter()
        .filter(|e| e.code == "E201")
        .map(|e| e.start)
        .collect();
    assert_eq!(starts, vec![Some(7), Some(16)]);
    let sql = "SELECT id FROM users u JOIN orders o ON u.id = o.user_id";
    assert!(
        validate_with_schema(
            sql,
            DialectType::Snowflake,
            &schema,
            &SchemaValidationOptions::default()
        )
        .valid
    );
    let result = validate_with_schema(
        sql,
        DialectType::Snowflake,
        &schema,
        &SchemaValidationOptions {
            check_references: true,
            strict: Some(false),
            ..Default::default()
        },
    );
    assert!(result.valid);
    assert!(result
        .errors
        .iter()
        .any(|e| e.code == "W222" && e.start == Some(7)));
    let error = reference_diagnostic("Unknown column".into(), "E201", true, None);
    assert_eq!(
        (error.start, error.end, error.line, error.column),
        (None, None, None, None)
    );
}

#[test]
fn test_canonical_type_family_aliases() {
    assert_eq!(canonical_type_family("INT4"), TypeFamily::Integer);
    for name in ["HUGEINT", "INT128", "LARGEINT", "Nullable(Int128)"] {
        assert_eq!(canonical_type_family(name), TypeFamily::Integer);
    }
    assert_eq!(data_type_family(&DataType::Int128), TypeFamily::Integer);
    for name in [
        "UTINYINT",
        "UINT8",
        "USMALLINT",
        "UINT16",
        "UINTEGER",
        "UINT32",
        "UBIGINT",
        "UINT64",
        "UHUGEINT",
        "UINT128",
    ] {
        assert_eq!(canonical_type_family(name), TypeFamily::Integer);
        let dt = crate::parse_data_type(name, DialectType::DuckDB).unwrap();
        assert_eq!(data_type_family(&dt), TypeFamily::Integer);
    }
    assert_eq!(
        canonical_type_family("double precision"),
        TypeFamily::Numeric
    );
    assert_eq!(canonical_type_family("VARCHAR(255)"), TypeFamily::String);
    assert_eq!(
        canonical_type_family("timestamp with time zone"),
        TypeFamily::Timestamp
    );
    assert_eq!(canonical_type_family("JSONB"), TypeFamily::Json);
    assert_eq!(canonical_type_family("UUID"), TypeFamily::Uuid);
}

#[test]
fn test_canonical_type_family_wrappers_and_collections() {
    assert_eq!(
        canonical_type_family("Nullable(Int64)"),
        TypeFamily::Integer
    );
    assert_eq!(
        canonical_type_family("LowCardinality(String)"),
        TypeFamily::String
    );
    assert_eq!(canonical_type_family("Array(String)"), TypeFamily::Array);
    assert_eq!(canonical_type_family("list(varchar)"), TypeFamily::Array);
    assert_eq!(canonical_type_family("Map(String, Int64)"), TypeFamily::Map);
    assert_eq!(canonical_type_family("STRUCT<a INT>"), TypeFamily::Struct);
    assert_eq!(canonical_type_family(""), TypeFamily::Unknown);
}

fn base_schema() -> ValidationSchema {
    ValidationSchema {
        tables: vec![
            SchemaTable {
                name: "users".to_string(),
                schema: None,
                columns: vec![
                    SchemaColumn {
                        name: "id".to_string(),
                        data_type: "integer".to_string(),
                        nullable: None,
                        primary_key: false,
                        unique: false,
                        references: None,
                    },
                    SchemaColumn {
                        name: "name".to_string(),
                        data_type: "varchar".to_string(),
                        nullable: None,
                        primary_key: false,
                        unique: false,
                        references: None,
                    },
                    SchemaColumn {
                        name: "email".to_string(),
                        data_type: "varchar".to_string(),
                        nullable: None,
                        primary_key: false,
                        unique: false,
                        references: None,
                    },
                    SchemaColumn {
                        name: "age".to_string(),
                        data_type: "integer".to_string(),
                        nullable: None,
                        primary_key: false,
                        unique: false,
                        references: None,
                    },
                ],
                aliases: vec![],
                primary_key: vec![],
                unique_keys: vec![],
                foreign_keys: vec![],
            },
            SchemaTable {
                name: "orders".to_string(),
                schema: None,
                columns: vec![
                    SchemaColumn {
                        name: "id".to_string(),
                        data_type: "integer".to_string(),
                        nullable: None,
                        primary_key: false,
                        unique: false,
                        references: None,
                    },
                    SchemaColumn {
                        name: "user_id".to_string(),
                        data_type: "integer".to_string(),
                        nullable: None,
                        primary_key: false,
                        unique: false,
                        references: None,
                    },
                    SchemaColumn {
                        name: "total".to_string(),
                        data_type: "decimal".to_string(),
                        nullable: None,
                        primary_key: false,
                        unique: false,
                        references: None,
                    },
                ],
                aliases: vec![],
                primary_key: vec![],
                unique_keys: vec![],
                foreign_keys: vec![],
            },
        ],
        strict: Some(true),
    }
}

fn attach_column_fk(
    schema: &mut ValidationSchema,
    table_name: &str,
    column_name: &str,
    target_table: &str,
    target_column: &str,
) {
    if let Some(table) = schema.tables.iter_mut().find(|t| t.name == table_name) {
        if let Some(column) = table.columns.iter_mut().find(|c| c.name == column_name) {
            column.references = Some(SchemaColumnReference {
                table: target_table.to_string(),
                column: target_column.to_string(),
                schema: None,
            });
        }
    }
}

fn mark_primary_key(schema: &mut ValidationSchema, table_name: &str, column_name: &str) {
    if let Some(table) = schema.tables.iter_mut().find(|t| t.name == table_name) {
        table.primary_key = vec![column_name.to_string()];
        if let Some(column) = table.columns.iter_mut().find(|c| c.name == column_name) {
            column.primary_key = true;
        }
    }
}

fn test_function_catalog() -> Arc<HashMapFunctionCatalog> {
    let mut catalog = HashMapFunctionCatalog::default();
    catalog.register(
        DialectType::Generic,
        "abs",
        vec![FunctionSignature::exact(1)],
    );
    catalog.register(
        DialectType::Generic,
        "coalesce",
        vec![FunctionSignature::variadic(1)],
    );
    catalog.register(
        DialectType::Generic,
        "foo",
        vec![FunctionSignature::exact(1)],
    );
    Arc::new(catalog)
}

#[test]
fn test_validate_with_schema_known_table_column() {
    let schema = base_schema();
    let opts = SchemaValidationOptions::default();
    let result = validate_with_schema(
        "SELECT id, name FROM users",
        DialectType::Generic,
        &schema,
        &opts,
    );
    assert!(result.valid);
    assert!(result.errors.is_empty());
}

#[test]
fn test_validate_with_schema_unknown_table() {
    let schema = base_schema();
    let opts = SchemaValidationOptions::default();
    let result = validate_with_schema(
        "SELECT * FROM nonexistent",
        DialectType::Generic,
        &schema,
        &opts,
    );
    assert!(!result.valid);
    assert!(result
        .errors
        .iter()
        .any(|e| e.code == validation_codes::E_UNKNOWN_TABLE && e.message.contains("nonexistent")));
}

#[test]
fn test_validate_with_schema_unknown_column() {
    let schema = base_schema();
    let opts = SchemaValidationOptions::default();
    let result = validate_with_schema(
        "SELECT unknown_col FROM users",
        DialectType::Generic,
        &schema,
        &opts,
    );
    assert!(!result.valid);
    assert!(
        result
            .errors
            .iter()
            .any(|e| e.code == validation_codes::E_UNKNOWN_COLUMN
                && e.message.contains("unknown_col"))
    );
}

#[test]
fn test_validate_with_schema_partial_schema_stays_strict() {
    let mut schema = base_schema();
    let users = schema
        .tables
        .iter_mut()
        .find(|table| table.name == "users")
        .expect("users table");
    users.columns.retain(|column| column.name == "id");

    let opts = SchemaValidationOptions::default();
    let result = validate_with_schema(
        "SELECT id, name FROM users",
        DialectType::Generic,
        &schema,
        &opts,
    );
    assert!(!result.valid);
    assert!(result.errors.iter().any(|error| {
        error.code == validation_codes::E_UNKNOWN_COLUMN && error.message.contains("name")
    }));
}

#[test]
fn test_validate_with_schema_function_catalog_unknown_function() {
    let schema = base_schema();
    let opts = SchemaValidationOptions {
        check_types: true,
        function_catalog: Some(test_function_catalog()),
        ..Default::default()
    };
    let result = validate_with_schema(
        "SELECT made_up_fn(id) FROM users",
        DialectType::Generic,
        &schema,
        &opts,
    );
    assert!(!result.valid);
    assert!(result
        .errors
        .iter()
        .any(|e| e.code == validation_codes::E_UNKNOWN_FUNCTION));
}

#[test]
fn test_validate_with_schema_function_catalog_invalid_arity() {
    let schema = base_schema();
    let opts = SchemaValidationOptions {
        check_types: true,
        function_catalog: Some(test_function_catalog()),
        ..Default::default()
    };
    let result = validate_with_schema(
        "SELECT FOO(id, age) FROM users",
        DialectType::Generic,
        &schema,
        &opts,
    );
    assert!(!result.valid);
    assert!(result
        .errors
        .iter()
        .any(|e| e.code == validation_codes::E_INVALID_FUNCTION_ARITY));
}

#[test]
fn test_validate_with_schema_function_catalog_valid_variadic() {
    let schema = base_schema();
    let opts = SchemaValidationOptions {
        check_types: true,
        function_catalog: Some(test_function_catalog()),
        ..Default::default()
    };
    let result = validate_with_schema(
        "SELECT COALESCE(name, email, 'fallback') FROM users",
        DialectType::Generic,
        &schema,
        &opts,
    );
    assert!(result.valid, "{:?}", result.errors);
}

#[test]
fn test_validate_with_schema_function_catalog_dialect_case_sensitive() {
    let schema = base_schema();
    let mut catalog = HashMapFunctionCatalog::default();
    catalog.set_dialect_name_case(DialectType::Generic, FunctionNameCase::Sensitive);
    catalog.register(
        DialectType::Generic,
        "Foo",
        vec![FunctionSignature::exact(1)],
    );

    let opts = SchemaValidationOptions {
        check_types: true,
        function_catalog: Some(Arc::new(catalog)),
        ..Default::default()
    };

    let valid = validate_with_schema(
        "SELECT Foo(id) FROM users",
        DialectType::Generic,
        &schema,
        &opts,
    );
    assert!(valid.valid, "{:?}", valid.errors);

    let invalid = validate_with_schema(
        "SELECT FOO(id) FROM users",
        DialectType::Generic,
        &schema,
        &opts,
    );
    assert!(!invalid.valid);
    assert!(invalid
        .errors
        .iter()
        .any(|e| e.code == validation_codes::E_UNKNOWN_FUNCTION));
}

#[test]
fn test_validate_with_schema_function_catalog_function_case_override() {
    let schema = base_schema();
    let mut catalog = HashMapFunctionCatalog::default();
    catalog.set_dialect_name_case(DialectType::Generic, FunctionNameCase::Insensitive);
    catalog.register(
        DialectType::Generic,
        "Bar",
        vec![FunctionSignature::exact(1)],
    );
    catalog.set_function_name_case(DialectType::Generic, "bar", FunctionNameCase::Sensitive);

    let opts = SchemaValidationOptions {
        check_types: true,
        function_catalog: Some(Arc::new(catalog)),
        ..Default::default()
    };

    let valid = validate_with_schema(
        "SELECT Bar(id) FROM users",
        DialectType::Generic,
        &schema,
        &opts,
    );
    assert!(valid.valid, "{:?}", valid.errors);

    let invalid = validate_with_schema(
        "SELECT BAR(id) FROM users",
        DialectType::Generic,
        &schema,
        &opts,
    );
    assert!(!invalid.valid);
    assert!(invalid
        .errors
        .iter()
        .any(|e| e.code == validation_codes::E_UNKNOWN_FUNCTION));
}

#[cfg(any(
    feature = "function-catalog-clickhouse",
    feature = "function-catalog-all-dialects"
))]
#[test]
fn test_validate_with_schema_uses_embedded_function_catalog_when_unset() {
    let schema = base_schema();
    let opts = SchemaValidationOptions {
        check_types: true,
        ..Default::default()
    };
    let result = validate_with_schema(
        "SELECT made_up_fn(id) FROM users",
        DialectType::ClickHouse,
        &schema,
        &opts,
    );
    assert!(!result.valid);
    assert!(result
        .errors
        .iter()
        .any(|e| e.code == validation_codes::E_UNKNOWN_FUNCTION));
}

#[cfg(any(
    feature = "function-catalog-duckdb",
    feature = "function-catalog-all-dialects"
))]
#[test]
fn test_validate_with_schema_uses_embedded_duckdb_function_catalog_when_unset() {
    let schema = base_schema();
    let opts = SchemaValidationOptions {
        check_types: true,
        ..Default::default()
    };
    let result = validate_with_schema(
        "SELECT made_up_fn(id) FROM users",
        DialectType::DuckDB,
        &schema,
        &opts,
    );
    assert!(!result.valid);
    assert!(result
        .errors
        .iter()
        .any(|e| e.code == validation_codes::E_UNKNOWN_FUNCTION));
}

#[test]
fn test_validate_with_schema_cte_projected_alias_column() {
    let schema = base_schema();
    let opts = SchemaValidationOptions::default();
    let result = validate_with_schema(
        "WITH my_cte AS (SELECT id AS emp_id FROM users) SELECT emp_id FROM my_cte",
        DialectType::ClickHouse,
        &schema,
        &opts,
    );
    assert!(result.valid, "{:?}", result.errors);
    assert!(result.errors.is_empty(), "{:?}", result.errors);
}

#[test]
fn test_validate_with_schema_cte_projected_alias_column_non_strict() {
    let schema = base_schema();
    let opts = SchemaValidationOptions {
        strict: Some(false),
        check_types: true,
        check_references: true,
        ..Default::default()
    };
    let result = validate_with_schema(
        "WITH my_cte AS (SELECT id AS emp_id FROM users) SELECT emp_id FROM my_cte",
        DialectType::ClickHouse,
        &schema,
        &opts,
    );
    assert!(result.valid, "{:?}", result.errors);
    assert!(result.errors.is_empty(), "{:?}", result.errors);
}

#[test]
fn test_validate_with_schema_unknown_cte_projected_alias_column() {
    let schema = base_schema();
    let opts = SchemaValidationOptions::default();
    let result = validate_with_schema(
        "WITH my_cte AS (SELECT id AS emp_id FROM users) SELECT missing_col FROM my_cte",
        DialectType::ClickHouse,
        &schema,
        &opts,
    );
    assert!(!result.valid);
    assert!(result.errors.iter().any(|e| {
        e.code == validation_codes::E_UNKNOWN_COLUMN && e.message.contains("missing_col")
    }));
}

#[test]
fn test_validate_with_schema_join_columns() {
    let schema = base_schema();
    let opts = SchemaValidationOptions::default();
    let result = validate_with_schema(
        "SELECT users.id, orders.total FROM users JOIN orders ON users.id = orders.user_id",
        DialectType::Generic,
        &schema,
        &opts,
    );
    assert!(result.valid);
}

#[test]
fn test_validate_with_schema_non_strict_is_warning() {
    let schema = base_schema();
    let opts = SchemaValidationOptions {
        strict: Some(false),
        ..Default::default()
    };
    let result = validate_with_schema(
        "SELECT unknown FROM users",
        DialectType::Generic,
        &schema,
        &opts,
    );
    assert!(result.valid);
    assert!(result
        .errors
        .iter()
        .all(|e| e.severity == crate::ValidationSeverity::Warning));
}

#[test]
fn test_semantic_warning_uses_column_source_span() {
    let sql = "SELECT customer_id, SUM(amount) FROM orders";
    let result = crate::validate_with_options(
        sql,
        DialectType::Snowflake,
        &crate::ValidationOptions {
            semantic: true,
            ..Default::default()
        },
    );
    let warning = result
        .errors
        .iter()
        .find(|error| error.code == validation_codes::W_AGGREGATE_WITHOUT_GROUP_BY)
        .unwrap();
    assert_eq!((warning.start, warning.end), (Some(7), Some(18)));
    assert_eq!((warning.line, warning.column), (Some(1), Some(19)));
}

#[test]
fn test_validate_with_schema_semantic_warnings() {
    let schema = base_schema();
    let opts = SchemaValidationOptions {
        semantic: true,
        ..Default::default()
    };
    let result = validate_with_schema(
        "SELECT * FROM users LIMIT 10",
        DialectType::Generic,
        &schema,
        &opts,
    );
    assert!(result.valid);
    assert!(result
        .errors
        .iter()
        .any(|e| e.code == validation_codes::W_SELECT_STAR));
    assert!(result
        .errors
        .iter()
        .any(|e| e.code == validation_codes::W_LIMIT_WITHOUT_ORDER_BY));
}

#[test]
fn test_basic_and_schema_validation_share_semantic_diagnostics() {
    let sql = "SELECT * FROM users LIMIT 10";
    let basic = crate::validate_with_options(
        sql,
        DialectType::Generic,
        &crate::ValidationOptions {
            semantic: true,
            ..Default::default()
        },
    );
    let schema = validate_with_schema(
        sql,
        DialectType::Generic,
        &base_schema(),
        &SchemaValidationOptions {
            semantic: true,
            ..Default::default()
        },
    );

    let diagnostics = |result: &ValidationResult| {
        result
            .errors
            .iter()
            .filter(|error| error.code.starts_with('W'))
            .map(|error| {
                (
                    error.code.clone(),
                    error.message.clone(),
                    error.line,
                    error.column,
                    error.start,
                    error.end,
                )
            })
            .collect::<Vec<_>>()
    };
    assert_eq!(diagnostics(&basic), diagnostics(&schema));
}

#[test]
fn test_validate_with_schema_reference_check_valid_column_fk() {
    let mut schema = base_schema();
    mark_primary_key(&mut schema, "users", "id");
    attach_column_fk(&mut schema, "orders", "user_id", "users", "id");

    let opts = SchemaValidationOptions {
        check_references: true,
        ..Default::default()
    };
    let result = validate_with_schema("SELECT 1", DialectType::Generic, &schema, &opts);
    assert!(result.valid, "errors: {:?}", result.errors);
    assert!(result.errors.is_empty());
}

#[test]
fn test_validate_with_schema_reference_check_unknown_target_table() {
    let mut schema = base_schema();
    mark_primary_key(&mut schema, "users", "id");
    attach_column_fk(&mut schema, "orders", "user_id", "missing_users", "id");

    let opts = SchemaValidationOptions {
        check_references: true,
        ..Default::default()
    };
    let result = validate_with_schema("SELECT 1", DialectType::Generic, &schema, &opts);
    assert!(!result.valid);
    assert!(result
        .errors
        .iter()
        .any(|e| e.code == validation_codes::E_INVALID_FOREIGN_KEY_REFERENCE));
}

#[test]
fn test_validate_with_schema_reference_check_unknown_target_column() {
    let mut schema = base_schema();
    mark_primary_key(&mut schema, "users", "id");
    attach_column_fk(&mut schema, "orders", "user_id", "users", "missing_id");

    let opts = SchemaValidationOptions {
        check_references: true,
        ..Default::default()
    };
    let result = validate_with_schema("SELECT 1", DialectType::Generic, &schema, &opts);
    assert!(!result.valid);
    assert!(result
        .errors
        .iter()
        .any(|e| e.code == validation_codes::E_INVALID_FOREIGN_KEY_REFERENCE));
}

#[test]
fn test_validate_with_schema_reference_check_type_mismatch() {
    let mut schema = base_schema();
    mark_primary_key(&mut schema, "users", "id");
    if let Some(orders) = schema.tables.iter_mut().find(|t| t.name == "orders") {
        if let Some(user_id) = orders.columns.iter_mut().find(|c| c.name == "user_id") {
            user_id.data_type = "varchar".to_string();
        }
    }
    attach_column_fk(&mut schema, "orders", "user_id", "users", "id");

    let opts = SchemaValidationOptions {
        check_references: true,
        ..Default::default()
    };
    let result = validate_with_schema("SELECT 1", DialectType::Generic, &schema, &opts);
    assert!(!result.valid);
    assert!(result
        .errors
        .iter()
        .any(|e| e.code == validation_codes::E_INVALID_FOREIGN_KEY_REFERENCE));
}

#[test]
fn test_validate_with_schema_reference_check_non_strict_warning() {
    let mut schema = base_schema();
    attach_column_fk(&mut schema, "orders", "user_id", "missing_users", "id");

    let opts = SchemaValidationOptions {
        check_references: true,
        strict: Some(false),
        ..Default::default()
    };
    let result = validate_with_schema("SELECT 1", DialectType::Generic, &schema, &opts);
    assert!(result.valid);
    assert!(result
        .errors
        .iter()
        .any(|e| e.code == validation_codes::W_WEAK_REFERENCE_INTEGRITY));
}

#[test]
fn test_validate_with_schema_reference_check_ambiguous_unqualified_column() {
    let mut schema = base_schema();
    mark_primary_key(&mut schema, "users", "id");
    attach_column_fk(&mut schema, "orders", "user_id", "users", "id");

    let opts = SchemaValidationOptions {
        check_references: true,
        ..Default::default()
    };
    let result = validate_with_schema(
        "SELECT id FROM users JOIN orders ON users.id = orders.user_id",
        DialectType::Generic,
        &schema,
        &opts,
    );
    assert!(!result.valid);
    assert!(result
        .errors
        .iter()
        .any(|e| e.code == validation_codes::E_AMBIGUOUS_COLUMN_REFERENCE));
}

#[test]
fn test_validate_with_schema_reference_check_ambiguous_unqualified_column_non_strict() {
    let mut schema = base_schema();
    mark_primary_key(&mut schema, "users", "id");
    attach_column_fk(&mut schema, "orders", "user_id", "users", "id");

    let opts = SchemaValidationOptions {
        check_references: true,
        strict: Some(false),
        ..Default::default()
    };
    let result = validate_with_schema(
        "SELECT id FROM users JOIN orders ON users.id = orders.user_id",
        DialectType::Generic,
        &schema,
        &opts,
    );
    assert!(result.valid);
    assert!(result
        .errors
        .iter()
        .any(|e| e.code == validation_codes::W_WEAK_REFERENCE_INTEGRITY));
}

#[test]
fn test_validate_with_schema_reference_check_cartesian_join_warning() {
    let schema = base_schema();
    let opts = SchemaValidationOptions {
        check_references: true,
        ..Default::default()
    };
    let result = validate_with_schema(
        "SELECT users.id FROM users CROSS JOIN orders",
        DialectType::Generic,
        &schema,
        &opts,
    );
    assert!(result.valid);
    assert!(result
        .errors
        .iter()
        .any(|e| e.code == validation_codes::W_CARTESIAN_JOIN));
}

#[test]
fn test_validate_with_schema_reference_check_join_not_using_declared_fk_warning() {
    let mut schema = base_schema();
    mark_primary_key(&mut schema, "users", "id");
    attach_column_fk(&mut schema, "orders", "user_id", "users", "id");

    let opts = SchemaValidationOptions {
        check_references: true,
        ..Default::default()
    };
    let result = validate_with_schema(
        "SELECT users.id FROM users JOIN orders ON users.age = orders.total",
        DialectType::Generic,
        &schema,
        &opts,
    );
    assert!(result.valid);
    assert!(result
        .errors
        .iter()
        .any(|e| e.code == validation_codes::W_JOIN_NOT_USING_DECLARED_REFERENCE));
}

#[test]
fn test_validate_with_schema_reference_check_join_using_declared_fk_no_warning() {
    let mut schema = base_schema();
    mark_primary_key(&mut schema, "users", "id");
    attach_column_fk(&mut schema, "orders", "user_id", "users", "id");

    let opts = SchemaValidationOptions {
        check_references: true,
        ..Default::default()
    };
    let result = validate_with_schema(
        "SELECT users.id FROM users JOIN orders ON users.id = orders.user_id",
        DialectType::Generic,
        &schema,
        &opts,
    );
    assert!(result.valid);
    assert!(!result
        .errors
        .iter()
        .any(|e| e.code == validation_codes::W_JOIN_NOT_USING_DECLARED_REFERENCE));
}

#[test]
fn test_validate_with_schema_type_check_comparison_mismatch() {
    let schema = base_schema();
    let opts = SchemaValidationOptions {
        check_types: true,
        ..Default::default()
    };
    let result = validate_with_schema(
        "SELECT id FROM users WHERE age = 'abc'",
        DialectType::Generic,
        &schema,
        &opts,
    );
    assert!(!result.valid);
    assert!(result
        .errors
        .iter()
        .any(|e| e.code == validation_codes::E_INCOMPATIBLE_COMPARISON_TYPES));
}

#[test]
fn test_validate_with_schema_type_check_arithmetic_mismatch() {
    let schema = base_schema();
    let opts = SchemaValidationOptions {
        check_types: true,
        ..Default::default()
    };
    let result = validate_with_schema(
        "SELECT age + name FROM users",
        DialectType::Generic,
        &schema,
        &opts,
    );
    assert!(!result.valid);
    assert!(result
        .errors
        .iter()
        .any(|e| e.code == validation_codes::E_INVALID_ARITHMETIC_TYPE));
}

#[test]
fn test_validate_with_schema_type_check_function_argument_mismatch() {
    let schema = base_schema();
    let opts = SchemaValidationOptions {
        check_types: true,
        ..Default::default()
    };
    let result = validate_with_schema(
        "SELECT ABS(name) FROM users",
        DialectType::Generic,
        &schema,
        &opts,
    );
    assert!(!result.valid);
    assert!(result
        .errors
        .iter()
        .any(|e| e.code == validation_codes::E_INVALID_FUNCTION_ARGUMENT_TYPE));
}

#[test]
fn test_validate_with_schema_type_check_predicate_mismatch() {
    let schema = base_schema();
    let opts = SchemaValidationOptions {
        check_types: true,
        ..Default::default()
    };
    let result = validate_with_schema(
        "SELECT id FROM users WHERE age + 1",
        DialectType::Generic,
        &schema,
        &opts,
    );
    assert!(!result.valid);
    assert!(result
        .errors
        .iter()
        .any(|e| e.code == validation_codes::E_INVALID_PREDICATE_TYPE));
}

#[test]
fn test_validate_with_schema_type_check_non_strict_warnings() {
    let schema = base_schema();
    let opts = SchemaValidationOptions {
        check_types: true,
        strict: Some(false),
        ..Default::default()
    };
    let result = validate_with_schema(
        "SELECT ABS(name) FROM users",
        DialectType::Generic,
        &schema,
        &opts,
    );
    assert!(result.valid);
    assert!(result
        .errors
        .iter()
        .any(|e| e.code == validation_codes::W_FUNCTION_ARGUMENT_COERCION));
}

#[test]
fn test_validate_with_schema_type_check_setop_arity_mismatch() {
    let schema = base_schema();
    let opts = SchemaValidationOptions {
        check_types: true,
        ..Default::default()
    };
    let result = validate_with_schema(
        "SELECT id FROM users UNION SELECT id, total FROM orders",
        DialectType::Generic,
        &schema,
        &opts,
    );
    assert!(!result.valid);
    assert!(result
        .errors
        .iter()
        .any(|e| e.code == validation_codes::E_SETOP_ARITY_MISMATCH));
}

#[test]
fn test_validate_with_schema_type_check_setop_type_mismatch() {
    let schema = base_schema();
    let opts = SchemaValidationOptions {
        check_types: true,
        ..Default::default()
    };
    let result = validate_with_schema(
        "SELECT age FROM users UNION SELECT name FROM users",
        DialectType::Generic,
        &schema,
        &opts,
    );
    assert!(!result.valid);
    assert!(result
        .errors
        .iter()
        .any(|e| e.code == validation_codes::E_SETOP_TYPE_MISMATCH));
}

#[test]
fn test_validate_with_schema_type_check_insert_values_assignment_mismatch() {
    let schema = base_schema();
    let opts = SchemaValidationOptions {
        check_types: true,
        ..Default::default()
    };
    let result = validate_with_schema(
        "INSERT INTO users (age) VALUES ('abc')",
        DialectType::Generic,
        &schema,
        &opts,
    );
    assert!(!result.valid);
    assert!(result
        .errors
        .iter()
        .any(|e| e.code == validation_codes::E_INVALID_ASSIGNMENT_TYPE));
}

#[test]
fn test_validate_with_schema_type_check_insert_query_assignment_mismatch() {
    let schema = base_schema();
    let opts = SchemaValidationOptions {
        check_types: true,
        ..Default::default()
    };
    let result = validate_with_schema(
        "INSERT INTO users (age) SELECT name FROM users",
        DialectType::Generic,
        &schema,
        &opts,
    );
    assert!(!result.valid);
    assert!(result
        .errors
        .iter()
        .any(|e| e.code == validation_codes::E_INVALID_ASSIGNMENT_TYPE));
}

#[test]
fn test_validate_with_schema_type_check_update_assignment_mismatch() {
    let schema = base_schema();
    let opts = SchemaValidationOptions {
        check_types: true,
        ..Default::default()
    };
    let result = validate_with_schema(
        "UPDATE users SET age = name",
        DialectType::Generic,
        &schema,
        &opts,
    );
    assert!(!result.valid);
    assert!(result
        .errors
        .iter()
        .any(|e| e.code == validation_codes::E_INVALID_ASSIGNMENT_TYPE));
}

#[test]
fn test_validate_with_schema_type_check_update_non_strict_warning() {
    let schema = base_schema();
    let opts = SchemaValidationOptions {
        check_types: true,
        strict: Some(false),
        ..Default::default()
    };
    let result = validate_with_schema(
        "UPDATE users SET age = name",
        DialectType::Generic,
        &schema,
        &opts,
    );
    assert!(result.valid);
    assert!(result
        .errors
        .iter()
        .any(|e| e.code == validation_codes::W_IMPLICIT_CAST_ASSIGNMENT));
}

#[test]
fn test_validate_with_schema_unresolved_table_alias_in_join_on() {
    let schema = base_schema();
    let opts = SchemaValidationOptions::default();
    let result = validate_with_schema(
        "SELECT * FROM users u LEFT JOIN orders o ON u.id = q.user_id",
        DialectType::Generic,
        &schema,
        &opts,
    );
    assert!(!result.valid);
    assert!(result.errors.iter().any(|e| {
        e.code == validation_codes::E_UNRESOLVED_REFERENCE && e.message.contains("q")
    }));
}

#[test]
fn test_validate_with_schema_unresolved_table_alias_in_join_on_non_strict() {
    let schema = base_schema();
    let opts = SchemaValidationOptions {
        strict: Some(false),
        ..Default::default()
    };
    let result = validate_with_schema(
        "SELECT * FROM users u LEFT JOIN orders o ON u.id = q.user_id",
        DialectType::Generic,
        &schema,
        &opts,
    );
    // Non-strict: valid but with a warning
    assert!(result.valid);
    assert!(result.errors.iter().any(|e| {
        e.code == validation_codes::E_UNRESOLVED_REFERENCE
            && e.message.contains("q")
            && e.severity == crate::ValidationSeverity::Warning
    }));
}

#[test]
fn test_validate_with_schema_valid_aliases_in_join_on() {
    let schema = base_schema();
    let opts = SchemaValidationOptions::default();
    let result = validate_with_schema(
        "SELECT * FROM users u LEFT JOIN orders o ON u.id = o.user_id",
        DialectType::Generic,
        &schema,
        &opts,
    );
    assert!(result.valid, "{:?}", result.errors);
}

#[test]
fn test_validate_with_schema_accepts_struct_field_access_issue_408() {
    let schema: ValidationSchema = serde_json::from_value(serde_json::json!({
        "tables": [{
            "name": "source_table",
            "columns": [
                {"name": "nested_items", "type": "STRUCT(field_value VARCHAR)[]"},
                {"name": "composite_value", "type": "STRUCT(field_value VARCHAR, label VARCHAR)"}
            ]
        }]
    }))
    .expect("schema");
    let options = SchemaValidationOptions::default();

    for sql in [
        "SELECT composite_value.field_value AS output_value FROM source_table",
        "SELECT item.field_value AS output_value FROM source_table s \
         CROSS JOIN UNNEST(s.nested_items) AS expanded(item)",
    ] {
        let result = validate_with_schema(sql, DialectType::DuckDB, &schema, &options);
        assert!(
            result.valid,
            "validation failed for {sql:?}: {:?}",
            result.errors
        );
    }

    let negative = validate_with_schema(
        "SELECT missing.field_value FROM source_table",
        DialectType::DuckDB,
        &schema,
        &options,
    );
    assert!(!negative.valid);
    assert!(negative.errors.iter().any(|error| {
        error.code == validation_codes::E_UNRESOLVED_REFERENCE && error.message.contains("missing")
    }));
}
