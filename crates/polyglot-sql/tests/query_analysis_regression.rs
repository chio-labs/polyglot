use polyglot_sql::lineage::{get_source_tables, lineage};
use polyglot_sql::scope::SourceKind;
use polyglot_sql::traversal::get_all_tables;
use polyglot_sql::{
    analyze_query, analyze_query_for_project_projections, generate, parse, AnalyzeQueryOptions,
    DialectType, Expression, TransformKind, ValidationSchema,
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

#[test]
fn required_project_facts_follow_only_consumed_cte_outputs() {
    let columns = (0..128)
        .map(|index| format!("CAST(order_id AS BIGINT) AS value_{index}"))
        .collect::<Vec<_>>()
        .join(", ");
    let sql = format!("WITH expanded AS (SELECT {columns} FROM orders), selected AS (SELECT value_0 AS order_id FROM expanded) SELECT order_id FROM selected");
    let schema: ValidationSchema = serde_json::from_value(serde_json::json!({
        "tables": [{"name": "orders", "columns": [{"name": "order_id", "type": "BIGINT"}]}]
    }))
    .unwrap();
    let options = AnalyzeQueryOptions {
        dialect: DialectType::DuckDB,
        schema: Some(schema.clone()),
        ..Default::default()
    };
    let validation = polyglot_sql::SchemaValidationOptions {
        check_types: false,
        check_references: true,
        semantic: false,
        strict_syntax: false,
        ..Default::default()
    };
    let full =
        polyglot_sql::compile_query_analysis(&sql, options.clone(), &schema, &validation, true);
    let required =
        polyglot_sql::compile_required_query_analysis(&sql, options, &schema, &validation, true);
    assert_eq!(
        serde_json::to_value(full.validation).unwrap(),
        serde_json::to_value(required.validation).unwrap()
    );
    let full = full.analysis.unwrap();
    let required = required.analysis.unwrap();
    assert_eq!(
        required.projections[0].type_hint,
        full.projections[0].type_hint
    );
    assert_eq!(
        required.projections[0].nullability,
        full.projections[0].nullability
    );
    assert_eq!(
        required.projections[0].passthrough_source.as_deref(),
        Some("selected")
    );
    assert_eq!(
        required.projections[0].passthrough_column.as_deref(),
        Some("order_id")
    );
    assert!(required.projections[0].upstream.is_empty());
    assert_eq!(
        serde_json::to_value(&required.cte_facts[0].projections[0].upstream).unwrap(),
        serde_json::to_value(&full.cte_facts[0].projections[0].upstream).unwrap()
    );
    assert_eq!(
        full.cte_facts
            .iter()
            .map(|fact| fact.projections.len())
            .sum::<usize>(),
        129
    );
    assert_eq!(
        required
            .cte_facts
            .iter()
            .map(|fact| fact.projections.len())
            .sum::<usize>(),
        2
    );
    assert_eq!(
        required.cte_facts[0].projections[0].name.as_deref(),
        Some("value_0")
    );
    assert_eq!(
        required.cte_facts[1].projections[0].name.as_deref(),
        Some("order_id")
    );
}

#[test]
fn combined_project_compilation_preserves_binding_and_analysis_boundaries() {
    let schema: ValidationSchema = serde_json::from_value(serde_json::json!({
        "strict": true,
        "tables": [
            {"name": "orders", "columns": [{"name": "order_id", "type": "BIGINT"}, {"name": "customer_id", "type": "BIGINT"}]},
            {"name": "customers", "columns": [{"name": "customer_id", "type": "BIGINT"}]}
        ]
    })).unwrap();
    let validation_options = polyglot_sql::SchemaValidationOptions {
        check_types: false,
        check_references: true,
        strict: Some(true),
        semantic: false,
        strict_syntax: false,
        ..Default::default()
    };
    for sql in [
        "SELECT order_id FROM orders",
        "SELECT missing FROM orders",
        "SELECT customer_id FROM orders JOIN customers ON orders.customer_id = customers.customer_id",
        "WITH selected AS (SELECT order_id FROM orders) SELECT order_id FROM selected WHERE order_id > 0",
        "SELECT order_id AS selected_id FROM orders ORDER BY selected_id",
        "SELECT order_id AS selected_id, selected_id + 1 AS next_id FROM orders",
        "SELECT order_id FROM orders WHERE EXISTS (SELECT 1 FROM customers WHERE customers.customer_id = orders.customer_id)",
        "SELECT order_id FROM orders UNION ALL SELECT customer_id FROM customers",
        "SELECT * FROM orders",
        "SELECT COUNT(*) AS total FROM orders",
        "SELECT order_id FROM orders QUALIFY ROW_NUMBER() OVER (ORDER BY order_id) = 1",
        "SELECT o.order_id FROM orders o JOIN customers c USING (customer_id)",
    ] {
        let analysis_options = AnalyzeQueryOptions {
            dialect: DialectType::Snowflake, schema: Some(schema.clone()), ..Default::default()
        };
        let standalone = analyze_query_for_project_projections(sql, analysis_options.clone()).unwrap();
        let validation = polyglot_sql::validate_with_schema(sql, DialectType::Snowflake, &schema, &validation_options);
        let combined = polyglot_sql::compile_query_analysis(sql, analysis_options, &schema, &validation_options, true);
        assert_eq!(serde_json::to_value(combined.analysis.unwrap()).unwrap(), serde_json::to_value(standalone).unwrap(), "analysis: {sql}");
        assert_eq!(serde_json::to_value(combined.validation).unwrap(), serde_json::to_value(validation).unwrap(), "binding: {sql}");
    }
}

#[test]
fn project_analysis_deduplicates_repeated_cte_lineage_states() {
    let mut ctes = vec!["base AS (SELECT order_id FROM orders)".to_string()];
    for index in 1..=24 {
        let previous = if index == 1 {
            "base".to_string()
        } else {
            format!("stage_{}", index - 1)
        };
        ctes.push(format!(
            "stage_{index} AS (SELECT order_id FROM {previous} UNION ALL SELECT order_id FROM {previous})"
        ));
    }
    let sql = format!("WITH {} SELECT order_id FROM stage_24", ctes.join(", "));

    let analysis = analyze_query_for_project_projections(
        &sql,
        AnalyzeQueryOptions {
            dialect: DialectType::DuckDB,
            ..Default::default()
        },
    )
    .expect("repeated CTE lineage should remain bounded");
    let projection = analysis
        .projections
        .first()
        .expect("expected one projection");

    assert_eq!(projection.upstream.len(), 1);
    assert_eq!(projection.upstream[0].source_kind, SourceKind::Table);
    assert_eq!(projection.upstream[0].table.as_deref(), Some("orders"));
    assert_eq!(projection.upstream[0].column, "order_id");
}

#[test]
fn project_analysis_preserves_declared_type_spelling_for_direct_columns() {
    let schema: ValidationSchema = serde_json::from_value(serde_json::json!({
        "tables": [{
            "name": "orders",
            "columns": [{"name": "attributes", "type": "VARIANT"}]
        }]
    }))
    .expect("schema should deserialize");
    let analysis = analyze_query_for_project_projections(
        "WITH base AS (SELECT attributes FROM orders UNION ALL SELECT attributes FROM orders), selected AS (SELECT attributes FROM base) SELECT attributes FROM selected",
        AnalyzeQueryOptions {
            dialect: DialectType::Snowflake,
            schema: Some(schema),
            ..Default::default()
        },
    )
    .expect("query should analyze");

    assert_eq!(
        analysis.projections[0].type_hint.as_deref(),
        Some("VARIANT")
    );
    assert_eq!(
        analysis.cte_facts[0].projections[0].type_hint.as_deref(),
        Some("VARIANT")
    );
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
fn analyze_query_reports_specialized_aggregate_transform_function() {
    let analysis = analyze_query(
        "SELECT OBJECT_AGG(product_id, TO_VARIANT(quantity)) AS inventory FROM products",
        AnalyzeQueryOptions {
            dialect: DialectType::Snowflake,
            ..Default::default()
        },
    )
    .expect("query should analyze");
    let transform = analysis.projections[0]
        .transform_function
        .as_ref()
        .expect("OBJECT_AGG should be reported");

    assert_eq!(transform.name, "OBJECT_AGG");
    assert_eq!(transform.column_args.len(), 2);
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
