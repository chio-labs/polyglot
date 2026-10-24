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
