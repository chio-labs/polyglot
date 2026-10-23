use polyglot_sql::expressions::{IsNull, IsTrueFalse};
use polyglot_sql::{generate, parse_one, DialectType, Expression};

fn projected(sql: &str) -> Expression {
    let Expression::Select(select) = parse_one(sql, DialectType::DuckDB).unwrap() else {
        panic!("expected SELECT")
    };
    select.expressions[0].clone()
}

#[test]
fn null_predicates_group_unparenthesized_boolean_operands() {
    for operand in ["a OR b", "a AND b", "NOT a"] {
        for not in [false, true] {
            for postfix_form in [false, true] {
                for dialect in [
                    DialectType::DuckDB,
                    DialectType::PostgreSQL,
                    DialectType::Generic,
                ] {
                    let expression = Expression::IsNull(Box::new(IsNull {
                        this: projected(&format!("SELECT {operand}")),
                        not,
                        postfix_form,
                    }));
                    let output = generate(&expression, dialect).unwrap();
                    let prefix = not
                        && (dialect == DialectType::Generic
                            || (postfix_form && dialect != DialectType::PostgreSQL));
                    let expected = if prefix {
                        format!("NOT ({operand}) IS NULL")
                    } else {
                        format!("({operand}) IS {}NULL", if not { "NOT " } else { "" })
                    };
                    assert_eq!(output, expected, "{dialect:?}");
                    assert_eq!(
                        projected(&format!("SELECT {output}")),
                        projected(&format!("SELECT {expected}"))
                    );
                }
            }
        }
    }
}

#[test]
fn truth_predicates_group_unparenthesized_boolean_operands() {
    for operand in ["a OR b", "a AND b", "NOT a"] {
        for not in [false, true] {
            for truth in [false, true] {
                let test = Box::new(IsTrueFalse {
                    this: projected(&format!("SELECT {operand}")),
                    not,
                });
                let expression = if truth {
                    Expression::IsTrue(test)
                } else {
                    Expression::IsFalse(test)
                };
                let output = generate(&expression, DialectType::DuckDB).unwrap();
                let expected = format!(
                    "({operand}) IS {}{}",
                    if not { "NOT " } else { "" },
                    if truth { "TRUE" } else { "FALSE" }
                );
                assert_eq!(output, expected);
                assert_eq!(
                    projected(&format!("SELECT {output}")),
                    projected(&format!("SELECT {expected}"))
                );
            }
        }
    }
}

#[test]
fn atomic_and_already_parenthesized_operands_keep_their_shape() {
    for operand in ["a", "(a OR b)", "(a AND b)", "(NOT a)", "COALESCE(a, b)"] {
        let expression = Expression::IsNull(Box::new(IsNull {
            this: projected(&format!("SELECT {operand}")),
            not: true,
            postfix_form: false,
        }));
        assert_eq!(
            generate(&expression, DialectType::DuckDB).unwrap(),
            format!("{operand} IS NOT NULL")
        );
    }
}

#[test]
fn builder_is_predicates_preserve_boolean_operands() {
    use polyglot_sql::builder::col;
    for operand in [col("a").or(col("b")), col("a").and(col("b"))] {
        for not in [false, true] {
            let expression = if not {
                operand.clone().is_not_null().0
            } else {
                operand.clone().is_null().0
            };
            let output = generate(&expression, DialectType::DuckDB).unwrap();
            let expected = format!(
                "SELECT {}({}) IS NULL",
                if not { "NOT " } else { "" },
                generate(&operand.0, DialectType::DuckDB).unwrap()
            );
            assert_eq!(projected(&format!("SELECT {output}")), projected(&expected));
        }
    }
}

#[test]
fn null_safe_predicates_preserve_both_boolean_operands() {
    use polyglot_sql::expressions::BinaryOp;
    for distinct in [false, true] {
        let operands = Box::new(BinaryOp::new(
            projected("SELECT a OR b"),
            projected("SELECT c AND d"),
        ));
        let expression = if distinct {
            Expression::NullSafeNeq(operands)
        } else {
            Expression::NullSafeEq(operands)
        };
        let output = generate(&expression, DialectType::DuckDB).unwrap();
        let expected = format!(
            "SELECT (a OR b) IS {}DISTINCT FROM (c AND d)",
            if distinct { "" } else { "NOT " }
        );
        assert_eq!(projected(&format!("SELECT {output}")), projected(&expected));
    }
}

#[test]
fn clickhouse_not_wraps_the_null_predicate() {
    let Expression::Select(select) =
        parse_one("SELECT NOT a IS NULL", DialectType::ClickHouse).unwrap()
    else {
        panic!("expected SELECT")
    };
    assert!(matches!(&select.expressions[0], Expression::Not(_)));
}

#[test]
fn json_predicates_preserve_boolean_operands() {
    use polyglot_sql::expressions::IsJson;
    for operand in ["a OR b", "a AND b", "NOT a"] {
        for negated in [false, true] {
            let expression = Expression::IsJson(Box::new(IsJson {
                this: projected(&format!("SELECT {operand}")),
                json_type: None,
                unique_keys: None,
                negated,
            }));
            assert_eq!(
                generate(&expression, DialectType::PostgreSQL).unwrap(),
                format!("({operand}) IS {}JSON", if negated { "NOT " } else { "" })
            );
        }
    }
}

#[test]
fn clickhouse_transformed_not_like_operands_remain_grouped_in_comparisons() {
    use polyglot_sql::dialects::Dialect;
    let dialect = Dialect::get(DialectType::ClickHouse);
    let input = "SELECT notLike(a, '%x%') != notLike(b, '%x%')";
    let parsed = parse_one(input, DialectType::ClickHouse).unwrap();
    let transformed = dialect.transform(parsed).unwrap();
    let first = generate(&transformed, DialectType::ClickHouse).unwrap();
    assert_eq!(first, "SELECT (NOT a LIKE '%x%') <> (NOT b LIKE '%x%')");
    let reparsed = parse_one(&first, DialectType::ClickHouse).unwrap();
    let second = generate(
        &dialect.transform(reparsed).unwrap(),
        DialectType::ClickHouse,
    )
    .unwrap();
    assert_eq!(first, second);
}

#[test]
fn comparison_builders_preserve_both_boolean_operands() {
    use polyglot_sql::expressions::BinaryOp;
    for operand in ["a OR b", "a AND b", "NOT a"] {
        for operator in ["=", "<>", "<", "<=", ">", ">="] {
            let operands = Box::new(BinaryOp::new(
                projected(&format!("SELECT {operand}")),
                projected("SELECT NOT c"),
            ));
            let expression = match operator {
                "=" => Expression::Eq(operands),
                "<>" => Expression::Neq(operands),
                "<" => Expression::Lt(operands),
                "<=" => Expression::Lte(operands),
                ">" => Expression::Gt(operands),
                ">=" => Expression::Gte(operands),
                _ => unreachable!(),
            };
            for dialect in [
                DialectType::DuckDB,
                DialectType::PostgreSQL,
                DialectType::ClickHouse,
            ] {
                let sql = generate(&expression, dialect).unwrap();
                assert_eq!(sql, format!("({operand}) {operator} (NOT c)"));
                assert!(parse_one(&format!("SELECT {sql}"), dialect).is_ok());
            }
        }
    }
}

#[test]
fn clickhouse_operand_not_preserves_operands_and_following_clauses() {
    for (input, expected) in [
        ("1 = NOT 0", "1 = (NOT 0)"),
        ("1 + NOT 0", "1 + (NOT 0)"),
        ("-NOT 0", "-NOT 0"),
        ("1 = NOT 0 = 0", "1 = (NOT 0 = 0)"),
        ("1 + NOT 0 = 0", "1 + (NOT 0 = 0)"),
        ("-NOT 0 = 0", "-NOT 0 = 0"),
        ("1 + NOT 0 IS NULL", "1 + (NOT 0 IS NULL)"),
        ("0 = NOT 1 AND 0", "0 = (NOT 1) AND 0"),
        ("1 + NOT NOT 0 IS NULL", "1 + (NOT NOT 0 IS NULL)"),
        ("a = NOT b", "a = (NOT b)"),
    ] {
        let suffix = " AS result FROM t WHERE keep = 1";
        let sql = format!("SELECT {input}{suffix}");
        let parsed = parse_one(&sql, DialectType::ClickHouse).unwrap();
        let output = generate(&parsed, DialectType::ClickHouse).unwrap();
        assert_eq!(output, format!("SELECT {expected}{suffix}"), "{sql}");
        let reparsed = parse_one(&output, DialectType::ClickHouse).unwrap();
        assert_eq!(
            generate(&reparsed, DialectType::ClickHouse).unwrap(),
            output
        );
    }
}

#[test]
fn clickhouse_operand_not_binds_around_comparisons() {
    for input in ["1 + NOT 0 = 0", "1 = NOT 0 = 0", "-NOT 0 = 0"] {
        let Expression::Select(select) =
            parse_one(&format!("SELECT {input}"), DialectType::ClickHouse).unwrap()
        else {
            panic!("expected SELECT");
        };
        let operand = match &select.expressions[0] {
            Expression::Add(op) | Expression::Eq(op) => &op.right,
            Expression::Neg(op) => &op.this,
            other => panic!("unexpected outer expression for {input}: {other:?}"),
        };
        assert!(
            matches!(operand, Expression::Not(op) if matches!(op.this, Expression::Eq(_))),
            "{input}: {operand:?}"
        );
    }
}

#[test]
fn clickhouse_negated_like_preserves_quoted_identifiers() {
    for operator in ["LIKE", "ILIKE"] {
        let sql = format!("SELECT NOT `a b` {operator} '%x%' FROM t");
        let parsed = parse_one(&sql, DialectType::ClickHouse).unwrap();
        let Expression::Select(select) = &parsed else {
            panic!("expected SELECT");
        };
        assert!(matches!(
            &select.expressions[0],
            Expression::Not(op) if matches!(op.this, Expression::Like(_) | Expression::ILike(_))
        ));
        let output = generate(&parsed, DialectType::ClickHouse).unwrap();
        assert_eq!(
            output,
            format!("SELECT NOT \"a b\" {operator} '%x%' FROM t")
        );
        assert!(parse_one(&output, DialectType::ClickHouse).is_ok());
        for target in [DialectType::DuckDB, DialectType::PostgreSQL] {
            let output = polyglot_sql::transpile(&sql, DialectType::ClickHouse, target)
                .unwrap()
                .remove(0);
            assert_eq!(
                output,
                format!("SELECT \"a b\" NOT {operator} '%x%' FROM t")
            );
            assert!(parse_one(&output, target).is_ok());
        }
    }
}
