use polyglot_sql::lineage::{get_source_tables, lineage};
use polyglot_sql::traversal::get_all_tables;
use polyglot_sql::{
    analyze_query, generate, parse, AnalyzeQueryOptions, DialectType, Expression, TransformKind,
};

fn first_projection(sql: &str) -> polyglot_sql::ProjectionFact {
    let analysis = analyze_query(
        sql,
        AnalyzeQueryOptions {
            dialect: DialectType::DuckDB,
            ..Default::default()
        },
    )
    .unwrap_or_else(|error| panic!("analyze_query failed for {sql:?}: {error}"));

    analysis
        .projections
        .into_iter()
        .next()
        .expect("expected one projection")
}

fn parse_one_statement(sql: &str, dialect: DialectType) -> Expression {
    let mut expressions = parse(sql, dialect).expect("statement should parse");
    assert_eq!(expressions.len(), 1);
    expressions.remove(0)
}

fn assert_postgres_command(sql: &str, expected: &str) {
    let expr = parse_one_statement(sql, DialectType::PostgreSQL);
    let Expression::Command(command) = &expr else {
        panic!("expected command expression, got {}", expr.variant_name());
    };

    assert_eq!(command.this, expected);
    assert_eq!(
        generate(&expr, DialectType::PostgreSQL).expect("command should generate"),
        expected
    );
}

#[test]
fn analyze_query_reports_top_level_transform_function() {
    let projection = first_projection("SELECT DATE_TRUNC('month', created_at) AS m FROM orders");

    let transform = projection
        .transform_function
        .expect("DATE_TRUNC should be reported");
    assert_eq!(projection.transform_kind, TransformKind::Expression);
    assert_eq!(transform.name, "DATE_TRUNC");
    assert_eq!(transform.literal_args, vec!["month"]);
    assert_eq!(transform.column_args.len(), 1);
    assert_eq!(transform.column_args[0].table.as_deref(), Some("orders"));
    assert_eq!(transform.column_args[0].column, "created_at");
}

#[test]
fn analyze_query_reports_transform_function_wrapped_in_coalesce() {
    let projection = first_projection(
        "SELECT COALESCE(DATE_TRUNC('month', created_at), DATE '1970-01-01') AS m FROM orders",
    );

    let transform = projection
        .transform_function
        .expect("nested DATE_TRUNC should be reported");
    assert_eq!(projection.transform_kind, TransformKind::Expression);
    assert_eq!(transform.name, "DATE_TRUNC");
    assert_eq!(transform.literal_args, vec!["month"]);
    assert_eq!(transform.column_args.len(), 1);
    assert_eq!(transform.column_args[0].table.as_deref(), Some("orders"));
    assert_eq!(transform.column_args[0].column, "created_at");
}

#[test]
fn analyze_query_reports_transform_function_wrapped_in_cast() {
    let projection =
        first_projection("SELECT CAST(DATE_TRUNC('day', created_at) AS DATE) AS d FROM orders");

    let transform = projection
        .transform_function
        .expect("nested DATE_TRUNC should be reported");
    assert_eq!(projection.transform_kind, TransformKind::Cast);
    assert_eq!(projection.cast_type.as_deref(), Some("DATE"));
    assert_eq!(transform.name, "DATE_TRUNC");
    assert_eq!(transform.literal_args, vec!["day"]);
    assert_eq!(transform.column_args.len(), 1);
    assert_eq!(transform.column_args[0].table.as_deref(), Some("orders"));
    assert_eq!(transform.column_args[0].column, "created_at");
}

#[test]
fn analyze_query_omits_ambiguous_nested_transform_functions() {
    let projection = first_projection(
        "SELECT COALESCE(DATE_TRUNC('month', created_at), DATE_TRUNC('day', updated_at)) AS m FROM orders",
    );

    assert!(
        projection.transform_function.is_none(),
        "multiple transform function candidates should remain ambiguous"
    );
}

#[test]
fn postgres_prepare_is_structured_and_traversable() {
    let expr = parse_one_statement(
        "PREPARE leak AS SELECT id FROM sensitive_table WHERE id = $1",
        DialectType::PostgreSQL,
    );

    let Expression::Prepare(prepare) = &expr else {
        panic!("expected prepare expression, got {}", expr.variant_name());
    };
    assert_eq!(prepare.name.name, "leak");
    assert!(prepare.parameter_types.is_empty());
    assert!(matches!(prepare.statement, Expression::Select(_)));

    let tables = get_all_tables(&expr);
    assert!(tables.iter().any(|table| match table {
        Expression::Table(table) => table.name.name == "sensitive_table",
        _ => false,
    }));

    let node = lineage("id", &expr, Some(DialectType::PostgreSQL), false)
        .expect("lineage should analyze prepared statement body");
    let source_tables = get_source_tables(&node);
    assert!(source_tables.contains("sensitive_table"));
}

#[test]
fn postgres_prepare_with_parameter_types_roundtrips() {
    let expr = parse_one_statement(
        r#"PREPARE leak (int) AS SELECT * FROM "Employee" WHERE "EmployeeId" = $1"#,
        DialectType::PostgreSQL,
    );

    let Expression::Prepare(prepare) = &expr else {
        panic!("expected prepare expression, got {}", expr.variant_name());
    };
    assert_eq!(prepare.name.name, "leak");
    assert_eq!(prepare.parameter_types.len(), 1);

    let sql = generate(&expr, DialectType::PostgreSQL).expect("prepare should generate");
    assert!(sql.starts_with("PREPARE leak (INT) AS SELECT"));
    assert!(sql.contains(r#""Employee""#));
}

#[test]
fn postgres_execute_prepared_statement_with_arguments_roundtrips() {
    let expr = parse_one_statement("EXECUTE leak(1)", DialectType::PostgreSQL);

    let Expression::Execute(execute) = &expr else {
        panic!("expected execute expression, got {}", expr.variant_name());
    };
    assert!(execute.prepared);
    assert_eq!(execute.arguments.len(), 1);
    assert!(execute.parameters.is_empty());

    let sql = generate(&expr, DialectType::PostgreSQL).expect("execute should generate");
    assert_eq!(sql, "EXECUTE leak(1)");
}

#[test]
fn generic_prepare_and_execute_parse_without_command_fallback() {
    let prepare = parse_one_statement(
        "PREPARE leak AS SELECT id FROM sensitive_table WHERE id = $1",
        DialectType::Generic,
    );
    assert!(matches!(prepare, Expression::Prepare(_)));

    let execute = parse_one_statement("EXECUTE leak(1)", DialectType::Generic);
    assert!(matches!(execute, Expression::Execute(_)));
}

#[test]
fn postgres_create_replication_slot_parses_as_command() {
    assert_postgres_command(
        r#"CREATE_REPLICATION_SLOT "sdp" LOGICAL pgoutput (SNAPSHOT 'nothing')"#,
        r#"CREATE_REPLICATION_SLOT "sdp" LOGICAL pgoutput(SNAPSHOT 'nothing')"#,
    );
}

#[test]
fn postgres_replication_protocol_commands_parse_as_commands() {
    for (sql, expected) in [
        (
            "BASE_BACKUP (LABEL 'polyglot')",
            "BASE_BACKUP(LABEL 'polyglot')",
        ),
        ("DROP_REPLICATION_SLOT sdp", "DROP_REPLICATION_SLOT sdp"),
        ("IDENTIFY_SYSTEM", "IDENTIFY_SYSTEM"),
        ("READ_REPLICATION_SLOT sdp", "READ_REPLICATION_SLOT sdp"),
        (
            "START_REPLICATION SLOT sdp LOGICAL 0/0",
            "START_REPLICATION SLOT sdp LOGICAL 0/0",
        ),
        ("TIMELINE_HISTORY 1", "TIMELINE_HISTORY 1"),
    ] {
        assert_postgres_command(sql, expected);
    }
}

#[test]
fn base_tables_include_every_parenthesized_join_member() {
    for (sql, expected) in [
        ("SELECT a.id FROM alpha a JOIN (beta b JOIN gamma c ON b.id = c.id) ON a.id = b.id", vec!["alpha", "beta", "gamma"]),
        ("SELECT a.id FROM alpha a JOIN ((beta b JOIN gamma c ON b.id = c.id) LEFT JOIN delta d ON c.id = d.id) ON a.id = b.id", vec!["alpha", "beta", "delta", "gamma"]),
        ("WITH chosen AS (SELECT id FROM gamma) SELECT a.id FROM alpha a JOIN (beta b JOIN chosen c ON b.id = c.id) ON a.id = b.id", vec!["alpha", "beta", "gamma"]),
        ("SELECT a.id FROM alpha a JOIN (beta b JOIN gamma c ON b.id = c.id) AS grouped ON a.id = grouped.id", vec!["alpha", "beta", "gamma"]),
        ("SELECT a.id FROM alpha a JOIN (beta b JOIN gamma c ON b.id = c.id) AS grouped(x, y) ON a.id = grouped.x", vec!["alpha", "beta", "gamma"]),
        ("SELECT a.id FROM alpha a JOIN (beta b JOIN gamma c ON c.id IN (SELECT id FROM delta)) ON a.id = b.id", vec!["alpha", "beta", "delta", "gamma"]),
        ("SELECT a.id FROM alpha a JOIN (beta b JOIN gamma c ON c.id IN (SELECT id FROM delta)) AS grouped(x, y) ON a.id = grouped.x", vec!["alpha", "beta", "delta", "gamma"]),
    ] {
        let analysis = analyze_query(
            sql,
            AnalyzeQueryOptions {
                dialect: DialectType::DuckDB,
                ..Default::default()
            },
        )
        .expect("parenthesized join should analyze");
        let names: Vec<_> = analysis
            .base_tables
            .iter()
            .map(|table| table.name.as_str())
            .collect();
        assert_eq!(names, expected, "missing join member for {sql}");
    }
}

#[test]
fn aliased_join_groups_own_their_predicate_facts() {
    use polyglot_sql::{ColumnUseContext, ReferenceConfidence};
    for group in [
        "(beta b JOIN gamma c ON b.id = c.id)",
        "((beta b JOIN gamma c ON b.id = c.id))",
        "(((beta b JOIN gamma c ON b.id = c.id)))",
    ] {
        for alias in ["", " AS g", " AS g(x, y)"] {
            let sql = format!("SELECT 1 FROM {group}{alias}");
            let analysis = analyze_query(
                &sql,
                AnalyzeQueryOptions {
                    dialect: DialectType::DuckDB,
                    ..Default::default()
                },
            )
            .unwrap();
            assert_eq!(
                analysis
                    .base_tables
                    .iter()
                    .map(|table| table.name.as_str())
                    .collect::<Vec<_>>(),
                vec!["beta", "gamma"],
                "{sql}"
            );
            let joins: Vec<_> = analysis
                .column_uses
                .iter()
                .filter(|fact| fact.context == ColumnUseContext::Join)
                .collect();
            assert_eq!(joins.len(), 1, "{sql}: {joins:?}");
            assert_eq!(
                joins[0].scope_path,
                if alias.is_empty() {
                    "root"
                } else {
                    "root.derived[0]"
                },
                "{sql}"
            );
            assert_eq!(joins[0].references.len(), 2, "{sql}");
            for (reference, table) in joins[0].references.iter().zip(["beta", "gamma"]) {
                assert_eq!(reference.reference.table.as_deref(), Some(table), "{sql}");
                assert_eq!(
                    reference.reference.confidence,
                    ReferenceConfidence::Resolved,
                    "{sql}"
                );
            }
        }
    }
}

#[test]
fn wrapped_join_aliases_keep_nested_dependencies_and_alias_isolation() {
    use polyglot_sql::scope::build_scope;
    for alias in ["g", "g(x, y)"] {
        let sql = format!("SELECT 1 FROM (((beta b JOIN gamma c ON c.id IN (SELECT id FROM delta)) JOIN epsilon e ON b.id = e.id)) AS {alias}");
        let expression = parse_one_statement(&sql, DialectType::DuckDB);
        let scope = build_scope(&expression);
        assert_eq!(scope.sources.len(), 1);
        assert!(scope.sources.contains_key("g"));
        assert_eq!(scope.derived_table_scopes.len(), 1);
        assert_eq!(
            scope.derived_table_scopes[0].outer_columns,
            if alias.contains('(') {
                vec!["x", "y"]
            } else {
                vec![]
            }
        );
        let analysis = analyze_query(
            &sql,
            AnalyzeQueryOptions {
                dialect: DialectType::DuckDB,
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(
            analysis
                .base_tables
                .iter()
                .map(|table| table.name.as_str())
                .collect::<Vec<_>>(),
            vec!["beta", "delta", "epsilon", "gamma"],
            "{sql}"
        );
    }
}

#[test]
fn re_aliased_join_groups_preserve_dependencies_and_scope_boundaries() {
    use polyglot_sql::scope::build_scope;
    use polyglot_sql::{ColumnUseContext, ReferenceConfidence};

    for group in [
        "((beta b JOIN gamma c ON b.id = c.id) AS inner_g(x, y))",
        "((((beta b JOIN gamma c ON b.id = c.id) AS inner_g(x, y))))",
    ] {
        for alias in ["outer_g", "outer_g(u, v)"] {
            let sql = format!("SELECT 1 FROM {group} AS {alias}");
            let expression = parse_one_statement(&sql, DialectType::DuckDB);
            let scope = build_scope(&expression);
            assert_eq!(scope.sources.len(), 1, "{sql}");
            assert!(scope.sources.contains_key("outer_g"), "{sql}");
            assert_eq!(scope.derived_table_scopes.len(), 1, "{sql}");
            let child = &scope.derived_table_scopes[0];
            assert_eq!(child.sources.len(), 2, "{sql}");
            assert!(child.sources.contains_key("b"), "{sql}");
            assert!(child.sources.contains_key("c"), "{sql}");
            assert_eq!(
                child.outer_columns,
                if alias.contains('(') {
                    vec!["u", "v"]
                } else {
                    vec![]
                },
                "{sql}"
            );
            let analysis = analyze_query(
                &sql,
                AnalyzeQueryOptions {
                    dialect: DialectType::DuckDB,
                    ..Default::default()
                },
            )
            .unwrap();
            assert_eq!(
                analysis
                    .base_tables
                    .iter()
                    .map(|table| table.name.as_str())
                    .collect::<Vec<_>>(),
                vec!["beta", "gamma"],
                "{sql}"
            );
            let joins: Vec<_> = analysis
                .column_uses
                .iter()
                .filter(|fact| fact.context == ColumnUseContext::Join)
                .collect();
            assert_eq!(joins.len(), 1, "{sql}");
            assert_eq!(joins[0].scope_path, "root.derived[0]", "{sql}");
            assert_eq!(joins[0].references.len(), 2, "{sql}");
            for (reference, table) in joins[0].references.iter().zip(["beta", "gamma"]) {
                assert_eq!(reference.reference.table.as_deref(), Some(table), "{sql}");
                assert_eq!(
                    reference.reference.confidence,
                    ReferenceConfidence::Resolved,
                    "{sql}"
                );
            }
        }
    }
}
