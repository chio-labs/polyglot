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
fn clickhouse_prefix_not_encloses_the_predicate() {
    let parsed = stable("SELECT 1 WHERE NOT x LIKE 'b%'", DialectType::ClickHouse);
    assert!(
        matches!(predicate(&parsed), Expression::Not(not) if matches!(&not.this, Expression::Like(_)))
    );
    let parsed = stable("SELECT 1 WHERE NOT (x LIKE 'b%')", DialectType::ClickHouse);
    assert!(matches!(predicate(&parsed), Expression::Not(_)));
    // ClickHouse's operator table gives NOT priority 5 and comparisons 9.
    // Engine control: NOT 2 = 1 -> 1, while (NOT 2) = 1 -> 0.
    let parsed = stable("SELECT 1 WHERE NOT 2 = 1", DialectType::ClickHouse);
    assert!(
        matches!(predicate(&parsed), Expression::Not(not) if matches!(&not.this, Expression::Eq(_)))
    );
}

#[test]
fn clickhouse_not_like_function_normalization_is_idempotent() {
    for sql in [
        "SELECT notLike(status, 'a%') FROM orders",
        "SELECT sum(notLike(status, 'a%') != notLike(upper(status), 'a%')) FROM orders",
        "SELECT throwIf(notLike(status, 'a%'), 'invalid status') FROM orders",
        "SELECT sum(throwIf(notLike(status, 'a%'), 'invalid status')) FROM orders",
    ] {
        let first =
            polyglot_sql::transpile(sql, DialectType::ClickHouse, DialectType::ClickHouse).unwrap();
        let second =
            polyglot_sql::transpile(&first[0], DialectType::ClickHouse, DialectType::ClickHouse)
                .unwrap();
        assert_eq!(first, second, "{sql}");
        stable(&first[0], DialectType::ClickHouse);
    }
    let sql = "SELECT notLike(status, 'a%') != notLike(upper(status), 'a%') FROM orders";
    let generated = polyglot_sql::transpile(sql, DialectType::ClickHouse, DialectType::ClickHouse)
        .unwrap()
        .remove(0);
    assert_eq!(
        generated,
        "SELECT (NOT status LIKE 'a%') <> (NOT UPPER(status) LIKE 'a%') FROM orders"
    );
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

#[test]
fn quantified_prefix_and_infix_negation_are_distinct() {
    for dialect in [
        DialectType::Generic,
        DialectType::PostgreSQL,
        DialectType::ClickHouse,
        DialectType::TSQL,
    ] {
        for operator in ["LIKE", "ILIKE"] {
            for quantifier in ["", " ANY", " ALL", " SOME"] {
                let rhs = if quantifier.is_empty() {
                    "'a%'"
                } else {
                    "('a%', 'x%')"
                };
                let prefix = if dialect == DialectType::ClickHouse {
                    format!("SELECT 1 WHERE NOT ('abc' {operator}{quantifier} {rhs})")
                } else {
                    format!("SELECT 1 WHERE NOT 'abc' {operator}{quantifier} {rhs}")
                };
                let infix = format!("SELECT 1 WHERE 'abc' NOT {operator}{quantifier} {rhs}");
                let a = stable(&prefix, dialect);
                let b = stable(&infix, dialect);
                assert!(matches!(predicate(&a), Expression::Not(_)));
                assert!(
                    matches!(predicate(&b), Expression::Like(op) | Expression::ILike(op) if op.negated)
                );
                assert_ne!(
                    generate(&a, dialect).unwrap(),
                    generate(&b, dialect).unwrap()
                );
                let b = parse_one(&generate(&b, dialect).unwrap(), dialect).unwrap();
                assert!(
                    matches!(predicate(&b), Expression::Like(op) | Expression::ILike(op) if op.negated)
                );
            }
        }
    }
    let sql = "SELECT NOT 'testa 1' LIKE ALL (ARRAY['testa%', 'testb%'])";
    assert_eq!(
        generate(
            &parse_one(sql, DialectType::PostgreSQL).unwrap(),
            DialectType::PostgreSQL
        )
        .unwrap(),
        sql
    );
}

#[test]
fn datafusion_only_canonicalizes_unquantified_infix_not_like() {
    for operator in ["LIKE", "ILIKE"] {
        let sql = format!("SELECT * FROM orders WHERE status NOT {operator} '%foo%'");
        let parsed = parse_one(&sql, DialectType::DataFusion).unwrap();
        assert_eq!(
            generate(&parsed, DialectType::DataFusion).unwrap(),
            format!("SELECT * FROM orders WHERE NOT status {operator} '%foo%'")
        );
        for quantifier in ["ANY", "ALL", "SOME"] {
            let sql = format!("SELECT 1 WHERE status NOT {operator} {quantifier} ('a%', 'x%')");
            let parsed = stable(&sql, DialectType::DataFusion);
            assert!(generate(&parsed, DialectType::DataFusion)
                .unwrap()
                .contains(&format!("status NOT {operator} {quantifier}")));
        }
    }
}

#[test]
fn snowflake_quantified_infix_negation_uses_the_dual_prefix() {
    for operator in ["LIKE", "ILIKE"] {
        for (quantifier, dual) in [("ALL", "ANY"), ("ANY", "ALL"), ("SOME", "ALL")] {
            let sql = format!("SELECT 'abc' NOT {operator} {quantifier} ('a%', 'x%') ESCAPE '#'");
            let parsed = parse_one(&sql, DialectType::Generic).unwrap();
            let expected = format!("SELECT NOT ('abc' {operator} {dual} ('a%', 'x%') ESCAPE '#')");
            assert_eq!(generate(&parsed, DialectType::Snowflake).unwrap(), expected);
            assert_eq!(
                polyglot_sql::transpile(&sql, DialectType::Generic, DialectType::Snowflake)
                    .unwrap(),
                vec![expected.clone()]
            );
            stable(&expected, DialectType::Snowflake);
        }
    }
    for quantifier in ["ALL", "ANY"] {
        let sql = format!("SELECT NOT 'abc' LIKE {quantifier} ('a%', 'x%')");
        assert_eq!(
            generate(
                &parse_one(&sql, DialectType::Snowflake).unwrap(),
                DialectType::Snowflake
            )
            .unwrap(),
            sql
        );
    }
}

#[test]
fn clickhouse_quantified_prefix_not_requires_parentheses() {
    for operator in ["LIKE", "ILIKE"] {
        for quantifier in ["ANY", "ALL", "SOME"] {
            let sql = format!("SELECT 1 WHERE NOT 'abc' {operator} {quantifier} ('a%', 'x%')");
            let parsed = parse_one(&sql, DialectType::Generic).unwrap();
            let expected =
                format!("SELECT 1 WHERE NOT ('abc' {operator} {quantifier} ('a%', 'x%'))");
            assert_eq!(
                generate(&parsed, DialectType::ClickHouse).unwrap(),
                expected
            );
            let reparsed = stable(&expected, DialectType::ClickHouse);
            assert!(matches!(predicate(&reparsed), Expression::Not(_)));
        }
    }
}

#[test]
fn quantified_negation_duckdb_execution() {
    use std::io::Write;
    use std::process::{Command, Stdio};
    let mut cases = Vec::new();
    for operator in ["LIKE", "ILIKE"] {
        for quantifier in ["ANY", "ALL", "SOME"] {
            let all = quantifier == "ALL";
            for prefix in [false, true] {
                for array in [false, true] {
                    let rhs = if array {
                        "(ARRAY['a%', 'x%'])"
                    } else {
                        "('a%', 'x%')"
                    };
                    let source = if array {
                        DialectType::PostgreSQL
                    } else if !prefix {
                        DialectType::Generic
                    } else {
                        DialectType::Snowflake
                    };
                    let sql = if prefix {
                        format!("SELECT NOT 'abc' {operator} {quantifier} {rhs}")
                    } else {
                        format!("SELECT 'abc' NOT {operator} {quantifier} {rhs}")
                    };
                    let reference = format!(
                        "SELECT {}('abc' {}{operator} 'a%' {} 'abc' {}{operator} 'x%')",
                        if prefix { "NOT " } else { "" },
                        if prefix { "" } else { "NOT " },
                        if all { "AND" } else { "OR" },
                        if prefix { "" } else { "NOT " }
                    );
                    let generated = polyglot_sql::transpile(&sql, source, DialectType::DuckDB)
                        .unwrap()
                        .remove(0);
                    let pretty = format(&generated, DialectType::DuckDB).unwrap().remove(0);
                    stable(&generated, DialectType::DuckDB);
                    let ast = parse_one(&generated, DialectType::DuckDB).unwrap();
                    assert_typed(&ast);
                    // A prefix reduction must not become per-pattern negation.
                    let Expression::Select(select) = &ast else {
                        panic!()
                    };
                    assert_eq!(
                        matches!(&select.expressions[0], Expression::Not(_)),
                        prefix,
                        "{sql} -> {generated}"
                    );
                    cases.push(serde_json::json!({"source":sql,"reference":reference,"generated":generated,"pretty":pretty,"expected":if prefix { all } else { !all }}));
                    for intermediate in [
                        DialectType::PostgreSQL,
                        DialectType::Snowflake,
                        DialectType::MySQL,
                    ] {
                        let intermediate_sql = polyglot_sql::transpile(&sql, source, intermediate)
                            .unwrap()
                            .remove(0);
                        let generated = polyglot_sql::transpile(
                            &intermediate_sql,
                            intermediate,
                            DialectType::DuckDB,
                        )
                        .unwrap()
                        .remove(0);
                        let pretty = format(&generated, DialectType::DuckDB).unwrap().remove(0);
                        cases.push(serde_json::json!({"source":sql,"reference":reference,"generated":generated,"pretty":pretty,"expected":if prefix { all } else { !all }}));
                    }
                }
            }
        }
    }
    // CI always exercises lowering and precedence above; opt into real engine
    // execution with an interpreter containing DuckDB (no warehouse required).
    let Ok(python) = std::env::var("POLYGLOT_DUCKDB_PYTHON") else {
        return;
    };
    let script = r#"
import json, sys, duckdb
cases = json.load(sys.stdin)
unsupported = 0
with duckdb.connect() as con:
    for case in cases:
        try:
            original = con.execute(case['source']).fetchone()[0]
        except duckdb.ParserException:
            unsupported += 1
        else:
            assert original == case['expected'], (case['source'], original)
        for field in ['reference', 'generated', 'pretty']:
            actual = con.execute(case[field]).fetchone()[0]
            assert actual == case['expected'], (case['source'], field, case[field], actual, case['expected'])
print('DuckDB', duckdb.__version__, len(cases), 'differing-meaning cases passed;', unsupported, 'native quantified forms unsupported')
"#;
    let mut child = Command::new(python)
        .args(["-c", script])
        .stdin(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(serde_json::to_string(&cases).unwrap().as_bytes())
        .unwrap();
    assert!(child.wait().unwrap().success());
}
