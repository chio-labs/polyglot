use polyglot_sql::{generate, parse_one, DialectType, Expression, ExpressionWalk};

fn assert_shapes(sql: &str, dialect: DialectType, json_count: usize, lambda_count: usize) {
    let expression = parse_one(sql, dialect).unwrap_or_else(|error| panic!("{sql}: {error}"));
    assert_eq!(
        expression.count(|node| matches!(node, Expression::JsonExtract(_))),
        json_count,
        "JSON extraction count: {sql}"
    );
    assert_eq!(
        expression.count(|node| matches!(node, Expression::Lambda(_))),
        lambda_count,
        "lambda count: {sql}"
    );
    let generated = generate(&expression, dialect).unwrap();
    let reparsed = parse_one(&generated, dialect).unwrap();
    assert_eq!(
        reparsed.count(|node| matches!(node, Expression::JsonExtract(_))),
        json_count,
        "generated SQL: {generated}"
    );
    assert_eq!(
        reparsed.count(|node| matches!(node, Expression::Lambda(_))),
        lambda_count,
        "generated SQL: {generated}"
    );
}

#[test]
fn json_arrows_accept_path_lists_and_dynamic_paths() {
    for input in ["payload", "s.payload", "(payload)", "CAST(payload AS JSON)"] {
        for path in ["['$.a', '$.b']", "path", "0", "'$.a'"] {
            for expression in [
                format!("{input} -> {path}"),
                format!("COALESCE({input} -> {path}, NULL)"),
            ] {
                assert_shapes(
                    &format!("SELECT {expression} FROM samples AS s"),
                    DialectType::DuckDB,
                    1,
                    0,
                );
            }
        }
    }
}

#[test]
fn lambda_arguments_keep_array_returning_and_nested_bodies() {
    for function in [
        "apply",
        "array_apply",
        "array_transform",
        "list_apply",
        "list_transform",
    ] {
        for body in ["[x, x]", "['$.a', '$.b']", "(x -> ['$.a', '$.b'])"] {
            assert_shapes(
                &format!("SELECT {function}(items, x -> {body}) FROM samples"),
                DialectType::DuckDB,
                usize::from(body.starts_with("(x ->")),
                1,
            );
        }
    }
    for function in ["filter", "array_filter", "list_filter"] {
        assert_shapes(
            &format!("SELECT {function}(items, x -> (x -> '$.a') IS NOT NULL) FROM samples"),
            DialectType::DuckDB,
            1,
            1,
        );
    }
    for function in ["reduce", "array_reduce", "list_reduce"] {
        assert_shapes(
            &format!("SELECT {function}(items, (x, y) -> x + y, 0) FROM samples"),
            DialectType::DuckDB,
            0,
            1,
        );
    }
}

#[test]
fn json_inputs_and_nested_lambdas_use_their_own_argument_context() {
    for sql in [
        "SELECT list_transform(JSON_EXTRACT(payload, ['$.a', '$.b']), x -> CAST(x AS INTEGER)) FROM samples",
        "SELECT list_transform(items, x -> list_transform([x], y -> (y -> ['$.a']))) FROM samples",
    ] {
        let lambdas = if sql.contains("y ->") { 2 } else { 1 };
        assert_shapes(sql, DialectType::DuckDB, 1, lambdas);
    }
    for sql in [
        "SELECT COLUMNS(c -> starts_with(c, 'a')) FROM samples",
        "SELECT COALESCE(*COLUMNS(c -> starts_with(c, 'a'))) FROM samples",
        "SELECT list_transform(items, LAMBDA x : x -> ['$.a']) FROM samples",
        "SELECT main.list_transform(items, (x) -> [x, x]) FROM samples",
        "SELECT list_transform(items, (x -> [x, x])) FROM samples",
        "SELECT list_transform(items, ((x) -> [x, x])) FROM samples",
        "SELECT list_transform(items, \"x\" -> [\"x\", \"x\"]) FROM samples",
    ] {
        assert_shapes(
            sql,
            DialectType::DuckDB,
            usize::from(sql.contains("['$.a']")),
            1,
        );
    }
}

#[test]
fn other_dialects_keep_arrow_lambda_support() {
    for dialect in [
        DialectType::Generic,
        DialectType::ClickHouse,
        DialectType::Spark,
    ] {
        assert_shapes(
            "SELECT transform(items, x -> [x, x]) FROM samples",
            dialect,
            0,
            1,
        );
    }
}

fn projected(sql: &str) -> Expression {
    let Expression::Select(select) = parse_one(sql, DialectType::DuckDB).unwrap() else {
        panic!("expected SELECT")
    };
    select.expressions[0].clone()
}

#[test]
fn generated_json_arrows_keep_their_operand_boundary() {
    use polyglot_sql::expressions::{BinaryOp, IsNull};
    for json in [
        "payload -> '$.a'",
        "payload -> ['$.a', '$.b']",
        "payload ->> '$.a'",
    ] {
        let extraction = projected(&format!("SELECT {json}"));
        let comparison = Expression::Eq(Box::new(BinaryOp::new(
            extraction.clone(),
            projected("SELECT other"),
        )));
        let generated = generate(&comparison, DialectType::DuckDB).unwrap();
        assert_eq!(generated, format!("({json}) = other"));
        assert!(matches!(
            projected(&format!("SELECT {generated}")),
            Expression::Eq(_)
        ));
        let null_test = Expression::IsNull(Box::new(IsNull {
            this: extraction,
            not: false,
            postfix_form: false,
        }));
        assert_eq!(
            generate(&null_test, DialectType::DuckDB).unwrap(),
            format!("({json}) IS NULL")
        );
    }
}

#[test]
fn standalone_json_arrows_keep_native_rendering() {
    for sql in [
        "SELECT payload -> '$.a'",
        "SELECT payload -> ['$.a', '$.b']",
        "SELECT payload -> '$.a' -> '$[0]'",
        "SELECT (payload -> '$.a') = other",
        "SELECT (payload -> '$.a')[1]",
    ] {
        let expression = parse_one(sql, DialectType::DuckDB).unwrap();
        assert_eq!(generate(&expression, DialectType::DuckDB).unwrap(), sql);
    }
}

#[test]
fn dot_calls_keep_their_lambda_argument_context() {
    for receiver in ["items", "s.items", "([1, 2])"] {
        for function in ["list_transform", "LIST_FILTER", "array_apply"] {
            assert_shapes(
                &format!("SELECT {receiver}.{function}(x -> x + 1) FROM samples AS s"),
                DialectType::DuckDB,
                0,
                1,
            );
        }
        assert_shapes(
            &format!("SELECT {receiver}.list_reduce((x, y) -> x + y, 0) FROM samples AS s"),
            DialectType::DuckDB,
            0,
            1,
        );
    }
    for sql in [
        "SELECT main.list_transform(payload -> '$.items', x -> x) FROM samples",
        "SELECT main.list_reduce(payload -> '$.items', (x, y) -> x + y, 0) FROM samples",
        "SELECT list_transform(payload -> '$.items', x -> x) FROM samples",
    ] {
        assert_shapes(sql, DialectType::DuckDB, 1, 1);
    }
}

#[test]
fn receiver_reductions_keep_json_initial_values() {
    assert_shapes(
        "SELECT items.list_reduce((x, y) -> x + y, initial -> '$.value') FROM samples",
        DialectType::DuckDB,
        1,
        1,
    );
}

#[test]
fn computed_json_paths_bind_inside_duckdb_arrows() {
    for path in ["'a' || 'b'", "0 + 1", "2 * 3", "path || '.value'"] {
        let sql = format!("SELECT payload -> {path}");
        let expression = projected(&sql);
        let Expression::JsonExtract(extract) = &expression else {
            panic!("arrow must own its computed path: {sql}: {expression:?}")
        };
        let expected_path = projected(&format!("SELECT {path}"));
        assert_eq!(
            generate(&extract.path, DialectType::DuckDB).unwrap(),
            generate(&expected_path, DialectType::DuckDB).unwrap(),
            "{sql}"
        );
        let output = generate(&expression, DialectType::DuckDB).unwrap();
        assert_eq!(
            generate(&projected(&format!("SELECT {output}")), DialectType::DuckDB).unwrap(),
            output
        );
    }
}

#[test]
fn parenthesized_lambdas_respect_parser_depth_and_leave_following_arguments_intact() {
    use polyglot_sql::{parse_one_with_options, ComplexityGuardOptions, ParseOptions};
    for wrappers in [16, 32, 64] {
        let sql = format!(
            "SELECT list_transform([1,2], {}x -> x + 1{})",
            "(".repeat(wrappers),
            ")".repeat(wrappers)
        );
        let options = ParseOptions {
            complexity_guard: Some(ComplexityGuardOptions {
                max_parser_depth: Some(8),
                ..Default::default()
            }),
        };
        let error = parse_one_with_options(&sql, DialectType::DuckDB, &options)
            .expect_err("lambda wrappers must consume parser depth");
        assert!(
            error.to_string().contains("E_GUARD_PARSER_DEPTH_EXCEEDED"),
            "{error}"
        );
        let options = ParseOptions {
            complexity_guard: Some(ComplexityGuardOptions {
                max_parser_depth: Some(256),
                ..Default::default()
            }),
        };
        assert!(parse_one_with_options(&sql, DialectType::DuckDB, &options).is_ok());
    }
    for sql in [
        "SELECT list_reduce([1,2], ((x, y) -> x + y), ((0 + 1)))",
        "SELECT list_transform([1,2], ((x -> x + 1))), ((2 + 3))",
        "SELECT list_transform([1,2], x -> x + 1), list_transform([3], ((x -> x)))",
    ] {
        assert!(parse_one(sql, DialectType::DuckDB).is_ok(), "{sql}");
    }
}

#[test]
fn scalar_json_paths_bind_arithmetic_but_not_concatenation() {
    let expression = projected("SELECT payload ->> 0 + 1");
    let Expression::JsonExtractScalar(extract) = expression else {
        panic!("scalar arrow must own its arithmetic path")
    };
    assert!(matches!(extract.path, Expression::Add(_)));
    let expression = projected("SELECT payload ->> 'a' || 'b'");
    let Expression::Concat(concat) = expression else {
        panic!("concatenation must follow scalar extraction")
    };
    assert!(matches!(concat.left, Expression::JsonExtractScalar(_)));
    let expression = projected("SELECT payload ->> 0 + 1 = other");
    let Expression::Eq(comparison) = expression else {
        panic!("comparison must follow scalar extraction")
    };
    assert!(matches!(comparison.left, Expression::JsonExtractScalar(_)));
}

#[test]
fn mixed_arrow_chains_keep_extraction_on_the_input_side() {
    for sql in [
        "SELECT payload -> 'a' ->> 0",
        "SELECT payload -> 'a' || 'b' ->> 0",
    ] {
        let expression = projected(sql);
        let Expression::JsonExtractScalar(extract) = expression else {
            panic!("scalar extraction must follow JSON extraction: {sql}")
        };
        assert!(matches!(extract.this, Expression::JsonExtract(_)), "{sql}");
    }
    for sql in [
        "SELECT payload -> ('a' || ('b' ->> 0))",
        "SELECT payload -> concat('a', other ->> 0)",
    ] {
        let expression = projected(sql);
        assert!(matches!(expression, Expression::JsonExtract(_)), "{sql}");
    }
}

#[test]
fn casts_accept_json_arrows_without_consuming_following_clauses() {
    for function in ["CAST", "TRY_CAST"] {
        for operand in [
            "payload -> 'a'",
            "payload -> 'a' || 'b'",
            "payload -> 0 + 1",
            "payload ->> 'a'",
            "payload ->> 0 + 1",
            "(payload -> 'a')",
            "payload -> CAST(path ->> 0 AS VARCHAR)",
        ] {
            let sql = format!("SELECT {function}({operand} AS INT), 7 FROM samples WHERE active");
            let expression = parse_one(&sql, DialectType::DuckDB)
                .unwrap_or_else(|error| panic!("{sql}: {error}"));
            let Expression::Select(select) = &expression else {
                panic!("expected SELECT");
            };
            assert_eq!(select.expressions.len(), 2, "{sql}");
            assert!(
                select.from.is_some() && select.where_clause.is_some(),
                "{sql}"
            );
            let cast = match &select.expressions[0] {
                Expression::Cast(cast) | Expression::TryCast(cast) => cast,
                other => panic!("expected cast for {sql}: {other:?}"),
            };
            assert_eq!(
                generate(&cast.this, DialectType::DuckDB).unwrap(),
                generate(
                    &projected(&format!("SELECT {operand}")),
                    DialectType::DuckDB
                )
                .unwrap(),
                "{sql}"
            );
            let output = generate(&expression, DialectType::DuckDB).unwrap();
            let reparsed = parse_one(&output, DialectType::DuckDB).unwrap();
            assert_eq!(generate(&reparsed, DialectType::DuckDB).unwrap(), output);
        }
    }
}

#[test]
fn casts_isolate_and_restore_nested_json_path_context() {
    for function in ["CAST", "TRY_CAST"] {
        for arrow in ["->", "->>"] {
            for tail in ["", " ->> 0"] {
                let sql = format!("SELECT payload -> {function}(path {arrow} 0 AS VARCHAR){tail}");
                let expression = projected(&sql);
                let outer = if tail.is_empty() {
                    &expression
                } else {
                    let Expression::JsonExtractScalar(extract) = &expression else {
                        panic!("the trailing scalar arrow must apply to the document: {sql}");
                    };
                    &extract.this
                };
                let Expression::JsonExtract(extract) = outer else {
                    panic!("expected outer JSON extraction: {sql}");
                };
                let cast = match &extract.path {
                    Expression::Cast(cast) | Expression::TryCast(cast) => cast,
                    other => panic!("expected cast path for {sql}: {other:?}"),
                };
                assert!(
                    matches!(
                        (&cast.this, arrow),
                        (Expression::JsonExtract(_), "->")
                            | (Expression::JsonExtractScalar(_), "->>")
                    ),
                    "{sql}: {:?}",
                    cast.this
                );
                let output = generate(&expression, DialectType::DuckDB).unwrap();
                assert_eq!(
                    generate(&projected(&format!("SELECT {output}")), DialectType::DuckDB).unwrap(),
                    output
                );
            }
        }
    }
}
