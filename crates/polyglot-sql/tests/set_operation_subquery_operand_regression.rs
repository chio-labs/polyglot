//! A parenthesized query used as an expression operand must not absorb a set operation
//! that follows it. The set operation belongs to the enclosing query:
//! `SELECT ... WHERE x < (SELECT ...) UNION ALL SELECT ...` is a UNION of two SELECTs.

use polyglot_sql::traversal::ExpressionWalk;
use polyglot_sql::{format, generate, parse_one, DialectType, Expression};

const DIALECTS: [DialectType; 7] = [
    DialectType::Generic,
    DialectType::DuckDB,
    DialectType::Snowflake,
    DialectType::BigQuery,
    DialectType::PostgreSQL,
    DialectType::TSQL,
    DialectType::Databricks,
];

fn is_set_operation(expression: &Expression) -> bool {
    matches!(
        expression,
        Expression::Union(_) | Expression::Intersect(_) | Expression::Except(_)
    )
}

fn set_operation_count(expression: &Expression) -> usize {
    expression
        .dfs()
        .filter(|node| is_set_operation(node))
        .count()
}

fn set_operation_operands(expression: &Expression) -> (&Expression, &Expression) {
    match expression {
        Expression::Union(union) => (&union.left, &union.right),
        Expression::Intersect(intersect) => (&intersect.left, &intersect.right),
        Expression::Except(except) => (&except.left, &except.right),
        other => panic!("expected a top-level set operation, got {other:?}"),
    }
}

fn parse_stable(sql: &str, dialect: DialectType) -> Expression {
    let parsed =
        parse_one(sql, dialect).unwrap_or_else(|error| panic!("{dialect:?}: {sql}: {error}"));
    let generated = generate(&parsed, dialect).unwrap();
    let reparsed = parse_one(&generated, dialect)
        .unwrap_or_else(|error| panic!("{dialect:?}: {generated}: {error}"));
    assert_eq!(
        generate(&reparsed, dialect).unwrap(),
        generated,
        "unstable generation: {dialect:?}: {sql}"
    );
    let formatted = format(&generated, dialect).unwrap().remove(0);
    let formatted_ast = parse_one(&formatted, dialect)
        .unwrap_or_else(|error| panic!("{dialect:?}: {formatted}: {error}"));
    assert_eq!(
        generate(&formatted_ast, dialect).unwrap(),
        generated,
        "formatter changed the query: {dialect:?}: {sql}"
    );
    parsed
}

#[test]
fn set_operation_after_subquery_operand_applies_to_enclosing_query() {
    let cases = [
        "SELECT o.id, o.total FROM orders AS o WHERE o.total < (SELECT MAX(l.amount) FROM limits AS l) UNION ALL SELECT o.id, o.total FROM orders AS o",
        "SELECT o.id FROM orders AS o WHERE o.total < (SELECT MAX(l.amount) FROM limits AS l) UNION SELECT o.id FROM orders AS o",
        "SELECT o.id FROM orders AS o WHERE o.total < (SELECT MAX(l.amount) FROM limits AS l) EXCEPT SELECT o.id FROM orders AS o",
        "SELECT o.id FROM orders AS o WHERE o.total < (SELECT MAX(l.amount) FROM limits AS l) INTERSECT SELECT o.id FROM orders AS o",
        "SELECT o.id FROM orders AS o WHERE o.total < (SELECT l.amount FROM limits AS l LIMIT 1) UNION ALL SELECT o.id FROM orders AS o",
        "SELECT o.id FROM orders AS o WHERE o.total < ((SELECT MAX(l.amount) FROM limits AS l)) UNION ALL SELECT o.id FROM orders AS o",
        "SELECT o.id FROM orders AS o WHERE o.total < (SELECT MAX(l.amount) FROM limits AS l) AND o.qty > (SELECT 2) UNION ALL SELECT o.id FROM orders AS o",
        "SELECT o.id FROM orders AS o WHERE o.total BETWEEN (SELECT 1) AND (SELECT 2) UNION ALL SELECT o.id FROM orders AS o",
        "SELECT o.id FROM orders AS o WHERE o.total = (SELECT MAX(l.amount) FROM limits AS l WHERE l.amount < (SELECT 5)) UNION ALL SELECT o.id FROM orders AS o",
        "SELECT o.id FROM orders AS o WHERE o.id IN (SELECT c.order_id FROM customers AS c) UNION ALL SELECT o.id FROM orders AS o",
        "SELECT o.id FROM orders AS o WHERE EXISTS (SELECT 1 FROM customers AS c) UNION ALL SELECT o.id FROM orders AS o",
        "SELECT o.id FROM orders AS o WHERE NOT EXISTS (SELECT 1 FROM customers AS c) EXCEPT SELECT o.id FROM orders AS o",
        "SELECT (SELECT MAX(l.amount) FROM limits AS l) AS cap UNION ALL SELECT 2",
        "SELECT o.id, (SELECT MAX(l.amount) FROM limits AS l) + 1 AS cap FROM orders AS o UNION ALL SELECT 1, 2",
        "SELECT o.id FROM orders AS o GROUP BY o.id HAVING COUNT(*) > (SELECT 1) UNION ALL SELECT o.id FROM orders AS o",
        "SELECT o.id FROM orders AS o JOIN customers AS c ON o.customer_id = (SELECT MIN(c2.id) FROM customers AS c2) UNION ALL SELECT o.id FROM orders AS o",
        "SELECT o.id FROM orders AS o WHERE (SELECT TRUE) UNION ALL SELECT o.id FROM orders AS o",
    ];
    for dialect in DIALECTS {
        for sql in cases {
            let parsed = parse_stable(sql, dialect);
            let (left, right) = set_operation_operands(&parsed);
            assert!(
                matches!(left, Expression::Select(_)) && matches!(right, Expression::Select(_)),
                "{dialect:?}: {sql}: {parsed:?}"
            );
            assert_eq!(
                set_operation_count(&parsed),
                1,
                "{dialect:?}: set operation absorbed by a subquery operand: {sql}"
            );
        }
    }
}

#[test]
fn set_operation_inside_enclosing_parentheses_stays_nested() {
    let cases = [
        "SELECT o.id FROM orders AS o WHERE o.id IN ((SELECT 1) UNION ALL (SELECT 2))",
        "SELECT o.id FROM orders AS o WHERE o.id IN ((SELECT 1) UNION ALL SELECT 2)",
        "SELECT o.id FROM orders AS o WHERE o.id IN (SELECT 1 UNION ALL SELECT 2)",
        "SELECT o.id FROM orders AS o WHERE o.id = ((SELECT 1) EXCEPT (SELECT 2))",
        "SELECT o.id FROM orders AS o WHERE EXISTS ((SELECT 1) INTERSECT (SELECT 2))",
        "SELECT * FROM ((SELECT 1 AS id) UNION ALL (SELECT 2 AS id)) AS t",
        "WITH t AS ((SELECT 1 AS id) UNION ALL (SELECT 2 AS id)) SELECT * FROM t",
    ];
    for dialect in DIALECTS {
        for sql in cases {
            let parsed = parse_stable(sql, dialect);
            assert!(
                matches!(parsed, Expression::Select(_)),
                "{dialect:?}: {sql}: {parsed:?}"
            );
            assert_eq!(set_operation_count(&parsed), 1, "{dialect:?}: {sql}");
        }
    }
}

#[test]
fn nested_set_operation_operand_and_outer_set_operation_keep_their_levels() {
    let sql = "SELECT o.id FROM orders AS o WHERE o.id < ((SELECT 1) UNION ALL (SELECT 2)) UNION ALL SELECT 3";
    for dialect in DIALECTS {
        let parsed = parse_stable(sql, dialect);
        let (left, right) = set_operation_operands(&parsed);
        assert!(matches!(right, Expression::Select(_)), "{dialect:?}");
        let Expression::Select(select) = left else {
            panic!("{dialect:?}: expected SELECT on the left, got {left:?}")
        };
        let predicate = &select.where_clause.as_ref().unwrap().this;
        assert_eq!(set_operation_count(predicate), 1, "{dialect:?}");
    }
}

#[test]
fn parenthesized_query_set_operations_keep_their_meaning() {
    let cases = [
        ("(SELECT 1) UNION ALL (SELECT 2)", 1),
        ("(SELECT 1) UNION ALL SELECT 2", 1),
        ("SELECT 1 UNION ALL (SELECT 2)", 1),
        ("(SELECT 1) EXCEPT (SELECT 2)", 1),
        ("(SELECT 1) INTERSECT (SELECT 2)", 1),
        ("(SELECT 1) UNION ALL (SELECT 2) UNION ALL (SELECT 3)", 2),
        ("((SELECT 1) UNION ALL (SELECT 2)) UNION ALL (SELECT 3)", 2),
    ];
    for dialect in DIALECTS {
        for (sql, expected_set_operations) in cases {
            let parsed = parse_stable(sql, dialect);
            assert!(is_set_operation(&parsed), "{dialect:?}: {sql}: {parsed:?}");
            assert_eq!(
                set_operation_count(&parsed),
                expected_set_operations,
                "{dialect:?}: {sql}"
            );
        }
    }
}

#[test]
fn create_table_as_parenthesized_set_operation_keeps_set_operation() {
    let sql = "CREATE TABLE t AS (SELECT 1 AS id) UNION ALL (SELECT 2 AS id)";
    for dialect in [
        DialectType::Generic,
        DialectType::DuckDB,
        DialectType::Snowflake,
        DialectType::PostgreSQL,
        DialectType::Databricks,
    ] {
        let parsed = parse_one(sql, dialect).unwrap_or_else(|error| panic!("{dialect:?}: {error}"));
        let Expression::CreateTable(create) = &parsed else {
            panic!("{dialect:?}: expected CREATE TABLE, got {parsed:?}")
        };
        let query = create.as_select.as_ref().unwrap();
        assert!(is_set_operation(query), "{dialect:?}: {query:?}");
        assert_eq!(
            generate(&parsed, dialect).unwrap(),
            sql,
            "{dialect:?}: CREATE TABLE AS changed"
        );
    }
}
