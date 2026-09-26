use polyglot_sql::traversal::ExpressionWalk;
use polyglot_sql::{format, generate, parse_one, DialectType, Expression};

fn assert_typed(query: &Expression) {
    assert!(
        !query
            .dfs()
            .any(|node| matches!(node, Expression::Raw(_) | Expression::Command(_))),
        "opaque predicate: {query:?}"
    );
}

fn predicate(query: &Expression) -> &Expression {
    let Expression::Select(select) = query else {
        panic!("expected SELECT")
    };
    &select.where_clause.as_ref().unwrap().this
}

fn stable(sql: &str, dialect: DialectType) -> Expression {
    let parsed =
        parse_one(sql, dialect).unwrap_or_else(|error| panic!("{dialect:?}: {sql}: {error}"));
    assert_typed(&parsed);
    let generated = generate(&parsed, dialect).unwrap();
    let reparsed = parse_one(&generated, dialect)
        .unwrap_or_else(|error| panic!("{dialect:?}: {generated}: {error}"));
    assert_typed(&reparsed);
    assert_eq!(
        generate(&reparsed, dialect).unwrap(),
        generated,
        "unstable: {dialect:?}: {sql}"
    );
    let formatted = format(&generated, dialect).unwrap().remove(0);
    let formatted_ast = parse_one(&formatted, dialect).unwrap();
    assert_typed(&formatted_ast);
    assert_eq!(
        generate(&formatted_ast, dialect).unwrap(),
        generated,
        "formatter changed predicate: {dialect:?}: {sql}"
    );
    parsed
}

#[test]
fn prefix_not_like_is_typed_across_dialects() {
    for dialect in [
        DialectType::Generic,
        DialectType::Snowflake,
        DialectType::DuckDB,
        DialectType::PostgreSQL,
        DialectType::SQLite,
        DialectType::MySQL,
        DialectType::TSQL,
        DialectType::Fabric,
        DialectType::BigQuery,
        DialectType::Spark,
        DialectType::Databricks,
        DialectType::Trino,
        DialectType::Redshift,
        DialectType::Oracle,
    ] {
        for operand in ["x", "SPLIT_PART('a:b', ':', 2)", "UPPER(x)"] {
            for operator in ["LIKE", "ILIKE"] {
                let sql = format!("SELECT 1 WHERE NOT {operand} {operator} 'b%' ESCAPE '#'");
                let parsed = stable(&sql, dialect);
                let Expression::Not(not) = predicate(&parsed) else {
                    panic!("NOT must enclose LIKE: {sql}")
                };
                assert!(matches!(
                    &not.this,
                    Expression::Like(_) | Expression::ILike(_)
                ));
            }
        }
    }
}

#[test]
fn prefix_not_quantified_like_and_boolean_precedence() {
    for dialect in [
        DialectType::Generic,
        DialectType::Snowflake,
        DialectType::DuckDB,
        DialectType::PostgreSQL,
    ] {
        for operator in ["LIKE", "ILIKE"] {
            for quantifier in ["ANY", "ALL", "SOME"] {
                let sql = format!(
                    "SELECT 1 WHERE NOT UPPER(x) {operator} {quantifier} ('a%', 'b%') ESCAPE '#'"
                );
                let parsed = stable(&sql, dialect);
                let Expression::Not(not) = predicate(&parsed) else {
                    panic!("{sql}")
                };
                let like = match &not.this {
                    Expression::Like(like) | Expression::ILike(like) => like,
                    _ => panic!("{sql}"),
                };
                assert_eq!(like.quantifier.as_deref(), Some(quantifier));
                assert!(like.escape.is_some());
            }
        }
        let parsed = stable("SELECT 1 WHERE NOT x LIKE 'a%' AND y = 1 OR z = 2", dialect);
        let Expression::Or(or) = predicate(&parsed) else {
            panic!()
        };
        let Expression::And(and) = &or.left else {
            panic!()
        };
        assert!(
            matches!(&and.left, Expression::Not(not) if matches!(&not.this, Expression::Like(_)))
        );
        let parsed = stable("SELECT 1 WHERE NOT NOT x LIKE 'a%'", dialect);
        assert!(
            matches!(predicate(&parsed), Expression::Not(a) if matches!(&a.this, Expression::Not(b) if matches!(&b.this, Expression::Like(_))))
        );
        stable("SELECT 1 WHERE NOT x NOT LIKE 'a%'", dialect);
        stable("SELECT 1 WHERE NOT (x LIKE 'a%' OR y ILIKE 'b%')", dialect);
        stable(
            "SELECT 1 WHERE NOT UPPER(x) /* pattern */ LIKE 'a%'",
            dialect,
        );
    }
}

#[test]
fn prefix_not_other_predicates_remain_structured() {
    for (dialect, predicates) in [
        (
            DialectType::DuckDB,
            vec![
                "x SIMILAR TO 'a.*'",
                "x GLOB 'a*'",
                "x IS DISTINCT FROM y",
                "EXISTS (SELECT 1)",
                "x IN (SELECT y FROM orders)",
                "x BETWEEN 1 AND 2",
                "x IS NULL",
                "x = y",
            ],
        ),
        (
            DialectType::PostgreSQL,
            vec![
                "x SIMILAR TO 'a%' ESCAPE '#'",
                "x IS DISTINCT FROM y",
                "EXISTS (SELECT 1)",
                "x IN (SELECT y FROM orders)",
            ],
        ),
        (
            DialectType::Snowflake,
            vec![
                "x RLIKE 'a.*'",
                "x REGEXP 'a.*'",
                "x IS DISTINCT FROM y",
                "EXISTS (SELECT 1)",
                "x IN (SELECT y FROM orders)",
            ],
        ),
        (
            DialectType::SQLite,
            vec![
                "x GLOB 'a*'",
                "x MATCH 'orders'",
                "x REGEXP 'a.*'",
                "x IS DISTINCT FROM y",
                "EXISTS (SELECT 1)",
                "x IN (SELECT y FROM orders)",
            ],
        ),
    ] {
        for expression in predicates {
            let sql = format!("SELECT 1 WHERE NOT {expression}");
            let parsed = stable(&sql, dialect);
            assert!(
                matches!(predicate(&parsed), Expression::Not(_)),
                "{sql}: {parsed:?}"
            );
        }
    }
}

#[test]
fn clickhouse_retains_its_high_precedence_not() {
    let parsed = stable("SELECT 1 WHERE NOT x LIKE 'b%'", DialectType::ClickHouse);
    assert!(
        matches!(predicate(&parsed), Expression::Like(like) if matches!(&like.left, Expression::Not(_)))
    );
    let parsed = stable("SELECT 1 WHERE NOT (x LIKE 'b%')", DialectType::ClickHouse);
    assert!(matches!(predicate(&parsed), Expression::Not(_)));
}

#[test]
fn prefix_not_like_exposes_columns_to_schema_validation() {
    let schema = serde_json::from_value(serde_json::json!({"tables":[{"name":"orders","columns":[{"name":"status","type":"VARCHAR"}]}],"strict":true})).unwrap();
    let options = polyglot_sql::SchemaValidationOptions {
        semantic: true,
        check_types: true,
        check_references: true,
        ..Default::default()
    };
    for dialect in [DialectType::Snowflake, DialectType::DuckDB] {
        for (column, valid) in [("status", true), ("missing", false)] {
            let sql = format!("SELECT status FROM orders WHERE NOT {column} LIKE 'b%'");
            let result = polyglot_sql::validate_with_schema(&sql, dialect, &schema, &options);
            assert_eq!(result.valid, valid, "{sql}: {:?}", result.errors);
            if !valid {
                assert!(result.errors.iter().any(|error| error.code == "E201"));
            }
        }
    }
}
