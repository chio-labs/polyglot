//! The analysis-only type assertion `__polyglot_typed(expr, 'TYPE')`.

use polyglot_sql::expressions::{Annotated, Select, StructField};
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
    let bigint_array = DataType::Array {
        element_type: Box::new(bigint.clone()),
        dimension: None,
    };
    assert_eq!(
        type_assertion_result(Some(&unknown_array), &varchar),
        &unknown_array
    );
    assert_eq!(
        type_assertion_result(Some(&unknown_array), &bigint_array),
        &bigint_array
    );
    let partial_struct = DataType::Struct {
        fields: vec![
            StructField::new("a".into(), bigint.clone()),
            StructField::new("b".into(), DataType::Unknown),
        ],
        nested: true,
    };
    let full_struct = DataType::Struct {
        fields: vec![
            StructField::new("a".into(), bigint.clone()),
            StructField::new("b".into(), varchar.clone()),
        ],
        nested: true,
    };
    let other_struct = DataType::Struct {
        fields: vec![
            StructField::new("a".into(), varchar.clone()),
            StructField::new("b".into(), varchar.clone()),
        ],
        nested: true,
    };
    assert_eq!(
        type_assertion_result(Some(&partial_struct), &full_struct),
        &full_struct
    );
    assert_eq!(
        type_assertion_result(Some(&partial_struct), &other_struct),
        &partial_struct
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
    fn partly_unknown_inferred_types_take_an_assertion_of_their_shape() {
        let sql = "SELECT __polyglot_typed([bar(amount, 0)], 'VARCHAR[]') AS x FROM orders";
        assert_eq!(
            output_type(sql, DialectType::DuckDB).as_deref(),
            Some("TEXT[]")
        );
        for (body, asserted) in [
            ("[]", "BIGINT"),
            ("[NULL]", "BIGINT"),
            ("[]", "MAP(VARCHAR, INT)"),
        ] {
            for template in [
                "SELECT {m} AS x FROM orders",
                "WITH c AS (SELECT {m} AS x FROM orders) SELECT x FROM c",
                "SELECT CAST(NULL AS BIGINT) AS x FROM orders UNION ALL SELECT {m} AS x FROM orders",
            ] {
                let typed = template.replace("{m}", &format!("__polyglot_typed({body}, '{asserted}')"));
                let plain = template.replace("{m}", body);
                assert_eq!(
                    output_type(&typed, DialectType::DuckDB),
                    output_type(&plain, DialectType::DuckDB),
                    "{typed}"
                );
            }
        }
        let refined = "SELECT __polyglot_typed([], 'BIGINT[]') AS x FROM orders";
        assert_eq!(
            output_type(refined, DialectType::DuckDB).as_deref(),
            Some("BIGINT[]")
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

    #[test]
    fn typed_like_assertions_take_the_type_of_their_argument_in_every_position() {
        for (dialect, _) in dialects() {
            let mut positions = POSITIONS.to_vec();
            if dialect != DialectType::PostgreSQL {
                positions.push(STAR_MODIFIER);
            }
            for (label, template) in positions {
                for (like, cast_type) in [("note", "VARCHAR"), ("orders.amount", "BIGINT")] {
                    let typed = template.replace(
                        "{m}",
                        &format!("__polyglot_typed_like(bar(amount, 0), {like})"),
                    );
                    let cast =
                        template.replace("{m}", &format!("CAST(bar(amount, 0) AS {cast_type})"));
                    assert_eq!(
                        output_type(&typed, dialect),
                        output_type(&cast, dialect),
                        "{dialect:?} {label}: {typed}"
                    );
                    let plain = template.replace("{m}", "bar(amount, 0)");
                    assert_eq!(
                        validation_errors(&typed, dialect),
                        validation_errors(&plain, dialect),
                        "{dialect:?} {label}"
                    );
                }
            }
        }
    }

    #[test]
    fn typed_like_assertions_of_unknown_arguments_assert_nothing() {
        for (dialect, _) in dialects() {
            for template in [
                "SELECT {m} AS x FROM orders",
                "WITH c AS (SELECT {m} AS x FROM orders) SELECT x FROM c",
            ] {
                let plain = template.replace("{m}", "bar(amount, 0)");
                let typed =
                    template.replace("{m}", "__polyglot_typed_like(bar(amount, 0), baz(note))");
                assert_eq!(
                    output_type(&typed, dialect),
                    output_type(&plain, dialect),
                    "{dialect:?}: {typed}"
                );
                let known = template.replace("{m}", "__polyglot_typed_like(UPPER(note), amount)");
                let upper = template.replace("{m}", "UPPER(note)");
                assert_eq!(
                    output_type(&known, dialect),
                    output_type(&upper, dialect),
                    "{dialect:?}: {known}"
                );
            }
        }
    }

    #[test]
    fn typed_like_assertions_leave_every_fact_unchanged() {
        for (dialect, _) in dialects() {
            for (body, like) in [
                ("UPPER(note)", "note"),
                ("amount + k", "amount"),
                ("bar(note)", "k"),
            ] {
                let marker = format!("__polyglot_typed_like({body}, {like})");
                for template in [
                    "SELECT {b} AS a FROM orders",
                    "WITH c AS (SELECT {b} AS a FROM orders) SELECT a FROM c",
                ] {
                    let plain = analyze(&template.replace("{b}", body), dialect);
                    let typed = analyze(&template.replace("{b}", &marker), dialect);
                    let mut plain_facts = facts(&plain);
                    let mut typed_facts = facts(&typed);
                    for value in [&mut plain_facts, &mut typed_facts] {
                        strip_types(value);
                    }
                    assert_eq!(typed_facts, plain_facts, "{dialect:?}: {marker}");
                }
            }
        }
    }

    fn semantic_errors(sql: &str, dialect: DialectType) -> Vec<String> {
        let options = SchemaValidationOptions {
            semantic: true,
            ..Default::default()
        };
        validate_with_schema(sql, dialect, &schema(), &options)
            .errors
            .iter()
            .map(|e| format!("{}: {}", e.code, e.message))
            .collect()
    }

    /// Every way to mark the `{a}`, `{b}` and `{c}` slots of `template`: plain or asserted.
    fn marked_variants(template: &str, plain: &str, marked: &str) -> Vec<String> {
        let mut variants = Vec::new();
        for mask in 0..8 {
            let mut sql = template.to_owned();
            for (bit, slot) in ["{a}", "{b}", "{c}"].iter().enumerate() {
                let value = if mask & (1 << bit) == 0 {
                    plain
                } else {
                    marked
                };
                sql = sql.replace(slot, value);
            }
            variants.push(sql);
        }
        variants
    }

    #[test]
    fn type_assertions_are_transparent_to_grouping_and_ordering_keys() {
        let key = "(amount + k)";
        let marker = "__polyglot_typed((amount + k), 'BIGINT')";
        let like = "__polyglot_typed_like((amount + k), amount)";
        let templates = [
            "SELECT {a} AS g, COUNT(*) AS n FROM orders GROUP BY {b}",
            "SELECT {a} AS g, COUNT(*) AS n FROM orders GROUP BY {b} ORDER BY {c}",
            "SELECT {a} AS g, COUNT(*) AS n FROM orders GROUP BY ROLLUP ({b})",
            "SELECT {a} AS g, COUNT(*) AS n FROM orders GROUP BY CUBE ({b})",
            "SELECT {a} AS g, COUNT(*) AS n FROM orders GROUP BY GROUPING SETS (({b}), ())",
            "SELECT COUNT(*) AS n FROM orders GROUP BY {b} HAVING {a} > 0",
            "SELECT {a} + 1 AS g FROM orders GROUP BY {b} HAVING SUM({c}) > 0",
            "SELECT SUM(amount) OVER (PARTITION BY {a} ORDER BY {b}) AS w FROM orders",
            "SELECT {a} AS g, COUNT(*) OVER (PARTITION BY {b}) AS w FROM orders GROUP BY {c}",
        ];
        for (dialect, _) in dialects() {
            let mut all = templates.to_vec();
            if dialect != DialectType::Snowflake {
                all.push("SELECT DISTINCT ON ({a}) {b} AS g FROM orders ORDER BY {c}");
            }
            for template in all {
                let plain = template
                    .replace("{a}", key)
                    .replace("{b}", key)
                    .replace("{c}", key);
                let expected = semantic_errors(&plain, dialect);
                assert!(
                    !expected.iter().any(|error| error.starts_with("E230")),
                    "{dialect:?}: {plain}: {expected:?}"
                );
                for asserted in [marker, like] {
                    for typed in marked_variants(template, key, asserted) {
                        assert_eq!(
                            semantic_errors(&typed, dialect),
                            expected,
                            "{dialect:?}: {typed}"
                        );
                    }
                }
            }
        }
    }

    /// The facts without inferred types, which a typed-like assertion exists to change.
    fn strip_types(value: &mut serde_json::Value) {
        match value {
            serde_json::Value::Object(object) => {
                object.remove("typeHint");
                object.remove("type_hint");
                object.values_mut().for_each(strip_types);
            }
            serde_json::Value::Array(items) => items.iter_mut().for_each(strip_types),
            _ => {}
        }
    }
}

#[test]
fn typed_like_expressions_lower_and_round_trip() {
    for dialect in [
        DialectType::DuckDB,
        DialectType::Snowflake,
        DialectType::PostgreSQL,
    ] {
        let sql = "SELECT __polyglot_typed_like(bar(amount), o.note) AS x FROM orders AS o";
        let item = unaliased(first_projection(sql, dialect));
        let Expression::Annotated(annotated) = &item else {
            panic!("{dialect:?}: not lowered: {item:?}");
        };
        assert!(annotated.type_assertion.is_none());
        assert!(matches!(
            annotated.type_like.as_deref(),
            Some(Expression::Column(_))
        ));
        let generated = Dialect::get(dialect)
            .generate(&parse_one(sql, dialect).unwrap())
            .unwrap();
        assert!(
            generated
                .to_lowercase()
                .contains("__polyglot_typed_like(bar(amount), o.note)"),
            "{dialect:?}: {generated}"
        );
    }
    let three = unaliased(first_projection(
        "SELECT __polyglot_typed_like(a, b, c) FROM t",
        DialectType::DuckDB,
    ));
    assert!(matches!(three, Expression::Function(_)));
}
