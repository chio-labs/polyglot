//! The analysis-only type assertion `__polyglot_typed(expr, 'TYPE')`.

use polyglot_sql::expressions::{Annotated, Select};
use polyglot_sql::{parse_one, DataType, Dialect, DialectType, Expression};

fn first_projection(sql: &str, dialect: DialectType) -> Expression {
    let Expression::Select(select) = parse_one(sql, dialect).unwrap() else {
        panic!("expected a SELECT: {sql}");
    };
    let Select { expressions, .. } = *select;
    expressions.into_iter().next().unwrap()
}

fn unaliased(expression: Expression) -> Expression {
    match expression {
        Expression::Alias(alias) => alias.this,
        other => other,
    }
}

fn assertion(expression: &Expression) -> Option<&Annotated> {
    match expression {
        Expression::Annotated(annotated) if annotated.type_assertion.is_some() => Some(annotated),
        _ => None,
    }
}

#[test]
fn typed_expression_lowers_to_an_annotated_node_with_the_asserted_type() {
    for dialect in [
        DialectType::DuckDB,
        DialectType::Snowflake,
        DialectType::PostgreSQL,
    ] {
        for name in ["__polyglot_typed", "__POLYGLOT_TYPED"] {
            let item = unaliased(first_projection(
                &format!("SELECT {name}(bar(amount), 'DECIMAL(10, 2)') AS x FROM orders"),
                dialect,
            ));
            let annotated = assertion(&item).expect("lowered");
            assert_eq!(
                annotated.type_assertion,
                Some(DataType::Decimal {
                    precision: Some(10),
                    scale: Some(2),
                }),
                "{dialect:?}"
            );
            assert!(matches!(&annotated.this, Expression::Function(f) if f.name == "bar"));
        }
    }
}

#[test]
fn malformed_typed_expressions_stay_ordinary_functions() {
    for sql in [
        "SELECT __polyglot_typed(amount) FROM orders",
        "SELECT __polyglot_typed(amount, 'BIGINT', 1) FROM orders",
        "SELECT __polyglot_typed(amount, 1) FROM orders",
        "SELECT __polyglot_typed(amount, note) FROM orders",
        "SELECT __polyglot_typed(amount, 'NOT A TYPE (') FROM orders",
        "SELECT __polyglot_typed(DISTINCT amount, 'BIGINT') FROM orders",
    ] {
        let item = first_projection(sql, DialectType::DuckDB);
        assert!(
            matches!(
                &item,
                Expression::Function(_) | Expression::AggregateFunction(_)
            ),
            "{sql}: {item:?}"
        );
    }
}

#[test]
fn typed_expressions_round_trip_through_the_generator() {
    for (dialect, sql, expected) in [
        (
            DialectType::DuckDB,
            "SELECT __polyglot_typed(bar(amount), 'VARCHAR') AS x FROM orders",
            "SELECT __polyglot_typed(BAR(amount), 'TEXT') AS x FROM orders",
        ),
        (
            DialectType::Snowflake,
            "SELECT __polyglot_typed(a + 1, 'NUMBER(38, 0)') + 1 FROM orders",
            "SELECT __polyglot_typed(a + 1, 'NUMBER(38, 0)') + 1 FROM orders",
        ),
        (
            DialectType::PostgreSQL,
            "SELECT __polyglot_typed(__polyglot_typed(x, 'INT'), 'BIGINT') FROM orders",
            "SELECT __polyglot_typed(__polyglot_typed(x, 'INT'), 'BIGINT') FROM orders",
        ),
    ] {
        let dialect = Dialect::get(dialect);
        let parsed = dialect.parse(sql).unwrap();
        let generated = dialect.generate(&parsed[0]).unwrap();
        assert_eq!(generated, expected);
        assert_eq!(
            dialect.parse(&generated).unwrap(),
            dialect.parse(expected).unwrap()
        );
    }
}

#[test]
fn comment_annotations_serialize_without_a_type_assertion() {
    let annotated = Annotated {
        this: Expression::Null(polyglot_sql::expressions::Null),
        trailing_comments: vec!["c".into()],
        type_assertion: None,
        type_like: None,
    };
    let mut json = serde_json::to_value(&annotated).unwrap();
    assert!(json.get("type_assertion").is_none());
    json.as_object_mut().unwrap().remove("type_assertion");
    let restored: Annotated = serde_json::from_value(json).unwrap();
    assert_eq!(restored, annotated);
}

#[test]
fn type_assertions_apply_only_where_the_inferred_type_is_unknown() {
    use polyglot_sql::expressions::type_assertion_result;
    let varchar = DataType::VarChar {
        length: None,
        parenthesized_length: false,
    };
    let bigint = DataType::BigInt { length: None };
    let unknown_array = DataType::Array {
        element_type: Box::new(DataType::Unknown),
        dimension: None,
    };
    assert_eq!(type_assertion_result(None, &varchar), &varchar);
    assert_eq!(
        type_assertion_result(Some(&DataType::Unknown), &varchar),
        &varchar
    );
    assert_eq!(
        type_assertion_result(Some(&unknown_array), &varchar),
        &varchar
    );
    assert_eq!(type_assertion_result(Some(&bigint), &varchar), &bigint);
    assert_eq!(
        type_assertion_result(Some(&DataType::Text), &varchar),
        &DataType::Text
    );
    assert!(unknown_array.contains_unknown());
    assert!(!bigint.contains_unknown());
}

mod analysis {
    use polyglot_sql::{
        analyze_query, validate_with_schema, AnalyzeQueryOptions, DialectType, QueryAnalysis,
        SchemaValidationOptions, ValidationSchema,
    };
    use serde_json::json;

    fn schema() -> ValidationSchema {
        serde_json::from_value(json!({
            "tables": [{
                "name": "orders",
                "columns": [
                    {"name": "amount", "type": "BIGINT", "nullable": false},
                    {"name": "note", "type": "VARCHAR", "nullable": true},
                    {"name": "k", "type": "BIGINT", "nullable": false}
                ]
            }]
        }))
        .unwrap()
    }

    pub(super) fn analyze(sql: &str, dialect: DialectType) -> QueryAnalysis {
        analyze_query(
            sql,
            AnalyzeQueryOptions {
                dialect,
                schema: Some(schema()),
                ..Default::default()
            },
        )
        .unwrap_or_else(|error| panic!("{sql}: {error}"))
    }

    /// Type of the output column named `x` (or the only output).
    fn output_type(sql: &str, dialect: DialectType) -> Option<String> {
        let analysis = analyze(sql, dialect);
        let projections = &analysis.projections;
        projections
            .iter()
            .find(|p| p.name.as_deref() == Some("x"))
            .or_else(|| (projections.len() == 1).then(|| &projections[0]))
            .unwrap_or_else(|| panic!("{sql}: no output x in {projections:?}"))
            .type_hint
            .clone()
    }

    fn validation_errors(sql: &str, dialect: DialectType) -> Vec<String> {
        let result =
            validate_with_schema(sql, dialect, &schema(), &SchemaValidationOptions::default());
        result
            .errors
            .iter()
            .filter(|e| e.severity == polyglot_sql::ValidationSeverity::Error)
            .map(|e| format!("{}: {}", e.code, e.message))
            .collect()
    }

    /// `{m}` is a call whose type the engine cannot infer.
    const POSITIONS: &[(&str, &str)] = &[
        ("root", "SELECT {m} AS x FROM orders"),
        ("unaliased", "SELECT {m} FROM orders"),
        ("concat", "SELECT {m} || 'x' AS x FROM orders"),
        ("coalesce", "SELECT COALESCE({m}, {m}) AS x FROM orders"),
        (
            "case",
            "SELECT CASE WHEN amount > 0 THEN {m} ELSE NULL END AS x FROM orders",
        ),
        (
            "derived",
            "SELECT s.x FROM (SELECT {m} AS x FROM orders) AS s",
        ),
        (
            "scalar_subquery",
            "SELECT (SELECT {m} FROM orders LIMIT 1) AS x",
        ),
        (
            "cte",
            "WITH c AS (SELECT {m} AS x FROM orders) SELECT x FROM c",
        ),
        (
            "cte_star",
            "WITH c AS (SELECT {m} AS x FROM orders) SELECT * FROM c",
        ),
        (
            "nested_with",
            "WITH c AS (WITH d AS (SELECT {m} AS x FROM orders) SELECT x FROM d) SELECT x FROM c",
        ),
        (
            "union_right",
            "SELECT note AS x FROM orders UNION ALL SELECT {m} AS x FROM orders",
        ),
        (
            "union_left",
            "SELECT {m} AS x FROM orders UNION ALL SELECT note AS x FROM orders",
        ),
        (
            "union_both",
            "SELECT {m} AS x FROM orders UNION ALL SELECT {m} AS x FROM orders",
        ),
        (
            "parenthesized_branch",
            "SELECT {m} AS x FROM orders UNION ALL (SELECT {m} AS x FROM orders)",
        ),
        (
            "cte_union",
            "WITH c AS (SELECT {m} AS x FROM orders UNION ALL SELECT {m} AS x FROM orders) \
             SELECT x FROM c",
        ),
    ];

    const STAR_MODIFIER: (&str, &str) = (
        "star_exclude",
        "SELECT * EXCLUDE (note), {m} AS x FROM orders",
    );

    fn dialects() -> [(DialectType, &'static str); 3] {
        [
            (DialectType::DuckDB, "TEXT"),
            (DialectType::Snowflake, "VARCHAR"),
            (DialectType::PostgreSQL, "VARCHAR"),
        ]
    }

    #[test]
    fn asserted_types_reach_every_position_when_the_body_is_unknown() {
        for (dialect, text) in dialects() {
            let mut positions = POSITIONS.to_vec();
            if dialect != DialectType::PostgreSQL {
                positions.push(STAR_MODIFIER);
            }
            for (label, template) in positions {
                let plain = template.replace("{m}", "bar(amount, 0)");
                let typed = template.replace("{m}", "__polyglot_typed(bar(amount, 0), 'VARCHAR')");
                let cast = template.replace("{m}", "CAST(bar(amount, 0) AS VARCHAR)");
                let expected = output_type(&cast, dialect);
                assert!(expected.is_some(), "{dialect:?} {label}: {cast} is untyped");
                assert_eq!(
                    output_type(&typed, dialect),
                    expected,
                    "{dialect:?} {label}: {typed}"
                );
                let plain_type = output_type(&plain, dialect);
                assert!(
                    plain_type.is_none()
                        || plain_type.as_deref() == Some(text)
                        || label == "concat",
                    "{dialect:?} {label}: the plain query types as {plain_type:?}"
                );
                assert_eq!(
                    validation_errors(&typed, dialect),
                    validation_errors(&plain, dialect),
                    "{dialect:?} {label}"
                );
            }
        }
    }

    #[test]
    fn known_inferred_types_stand_against_a_different_assertion() {
        for (dialect, _) in dialects() {
            let mut positions = POSITIONS.to_vec();
            if dialect != DialectType::PostgreSQL {
                positions.push(STAR_MODIFIER);
            }
            for (label, template) in positions {
                if label == "concat" || label.starts_with("union_right") || label == "union_left" {
                    continue;
                }
                let plain = template.replace("{m}", "amount");
                let typed = template.replace("{m}", "__polyglot_typed(amount, 'VARCHAR')");
                assert_eq!(
                    output_type(&typed, dialect),
                    output_type(&plain, dialect),
                    "{dialect:?} {label}: {typed}"
                );
            }
        }
    }

    #[test]
    fn asserted_types_flow_through_operators_aggregates_and_windows() {
        for (dialect, _) in dialects() {
            for (template, asserted) in [
                ("SELECT {m} + 1 AS x FROM orders", "BIGINT"),
                ("SELECT LENGTH({m}) AS x FROM orders", "VARCHAR"),
                (
                    "SELECT SUM({m}) OVER (ORDER BY k) AS x FROM orders",
                    "BIGINT",
                ),
                ("SELECT SUM({m}) AS x FROM orders", "BIGINT"),
                ("SELECT MAX({m}) AS x FROM orders", "DATE"),
            ] {
                let typed = template.replace(
                    "{m}",
                    &format!("__polyglot_typed(bar(amount, 0), '{asserted}')"),
                );
                let cast = template.replace("{m}", &format!("CAST(bar(amount, 0) AS {asserted})"));
                let expected = output_type(&cast, dialect);
                assert!(expected.is_some(), "{dialect:?}: {cast} is untyped");
                assert_eq!(
                    output_type(&typed, dialect),
                    expected,
                    "{dialect:?}: {typed}"
                );
            }
            let aggregate_body =
                "SELECT __polyglot_typed(SUM(bar(amount, 0)), 'BIGINT') AS x FROM orders";
            let expected = output_type("SELECT SUM(bar(amount, 0)) AS x FROM orders", dialect)
                .or_else(|| {
                    output_type(
                        "SELECT CAST(SUM(bar(amount, 0)) AS BIGINT) AS x FROM orders",
                        dialect,
                    )
                });
            assert_eq!(
                output_type(aggregate_body, dialect),
                expected,
                "{dialect:?}"
            );
        }
    }

    #[test]
    fn nested_assertions_use_the_innermost_known_type() {
        for (dialect, text) in dialects() {
            let sql = "WITH c AS (SELECT __polyglot_typed(__polyglot_typed(bar(amount, 0), 'VARCHAR'), 'BIGINT') AS x FROM orders) SELECT x FROM c";
            assert_eq!(
                output_type(sql, dialect).as_deref(),
                Some(text),
                "{dialect:?}"
            );
        }
    }

    #[test]
    fn partly_unknown_inferred_types_take_the_assertion() {
        let sql = "SELECT __polyglot_typed([bar(amount, 0)], 'VARCHAR[]') AS x FROM orders";
        assert_eq!(
            output_type(sql, DialectType::DuckDB).as_deref(),
            Some("TEXT[]")
        );
    }

    /// Every fact except source text, which differs by the marker itself.
    fn facts(analysis: &QueryAnalysis) -> serde_json::Value {
        fn without_sql(value: &mut serde_json::Value) {
            match value {
                serde_json::Value::Object(object) => {
                    object.remove("bodySql");
                    object.values_mut().for_each(without_sql);
                }
                serde_json::Value::Array(items) => items.iter_mut().for_each(without_sql),
                _ => {}
            }
        }
        let mut facts = json!({
            "shape": analysis.shape,
            "ctes": analysis.ctes,
            "cte_facts": analysis.cte_facts,
            "projections": analysis.projections,
            "relations": analysis.relations,
            "star_projections": analysis.star_projections,
            "set_operations": analysis.set_operations,
        });
        without_sql(&mut facts);
        facts
    }

    #[test]
    fn asserting_the_inferred_type_leaves_every_fact_unchanged() {
        for (dialect, _) in dialects() {
            for body in [
                "amount",
                "note",
                "COALESCE(note, 'x')",
                "CAST(amount AS INT)",
                "SUM(amount)",
                "amount + k",
                "UPPER(note)",
                "NULL",
            ] {
                let plain_analysis = analyze(&format!("SELECT {body} AS a FROM orders"), dialect);
                let Some(inferred) = plain_analysis.projections[0].type_hint.clone() else {
                    assert_eq!(body, "NULL", "{dialect:?}: {body} is untyped");
                    continue;
                };
                let marker = format!("__polyglot_typed({body}, '{inferred}')");
                for template in [
                    "SELECT {b} AS a, {b} FROM orders",
                    "WITH c AS (SELECT {b} AS a FROM orders) SELECT a, * FROM c",
                    "SELECT s.a FROM (SELECT {b} AS a FROM orders) AS s",
                    "SELECT {b} AS a FROM orders UNION ALL SELECT {b} AS a FROM orders",
                ] {
                    let plain = template.replace("{b}", body);
                    let typed = template.replace("{b}", &marker);
                    assert_eq!(
                        facts(&analyze(&typed, dialect)),
                        facts(&analyze(&plain, dialect)),
                        "{dialect:?}: {typed}"
                    );
                    assert_eq!(
                        validation_errors(&typed, dialect),
                        validation_errors(&plain, dialect),
                        "{dialect:?}: {typed}"
                    );
                }
            }
        }
    }
}
