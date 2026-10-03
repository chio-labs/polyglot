//! Allocation scaling of schema-aware column qualification.
//!
//! Allocated bytes are deterministic, unlike timings, so these guards compare
//! the work done for an input against the same input at a quarter of the size.
//! This binary holds a single test so no other test allocates concurrently.

use polyglot_sql::optimizer::{
    qualify_columns, qualify_tables, QualifyColumnsOptions, QualifyTablesOptions,
};
use polyglot_sql::{parse_one, DataType, DialectType, Expression, MappingSchema, Schema};
use stats_alloc::{Region, StatsAlloc, INSTRUMENTED_SYSTEM};
use std::alloc::System;

#[global_allocator]
static GLOBAL: &StatsAlloc<System> = &INSTRUMENTED_SYSTEM;

const DIALECT: DialectType = DialectType::Snowflake;

fn schema(tables: &[&str], width: usize) -> MappingSchema {
    let int = DataType::Int {
        length: None,
        integer_spelling: false,
    };
    let mut schema = MappingSchema::new();
    for table in tables {
        let columns: Vec<_> = (0..width)
            .map(|i| (format!("{table}_c{i}"), int.clone()))
            .chain(std::iter::once(("id".to_string(), int.clone())))
            .collect();
        schema.add_table(table, &columns, None).unwrap();
    }
    schema
}

/// Qualify like query analysis and schema-aware lineage do.
fn qualify(expression: Expression, schema: &MappingSchema) -> Expression {
    let expression = qualify_tables(
        expression,
        &QualifyTablesOptions::new()
            .with_dialect(DIALECT)
            .with_alias_unaliased_tables(false)
            .with_alias_unaliased_subqueries(true)
            .with_normalize_set_operation_subqueries(false),
    );
    qualify_columns(
        expression,
        schema,
        &QualifyColumnsOptions::new()
            .with_dialect(DIALECT)
            .with_allow_partial(true),
    )
    .expect("qualification succeeds")
}

fn allocated_bytes(sql: &str, schema: &MappingSchema) -> usize {
    let parsed = parse_one(sql, DIALECT).expect("query parses");
    let region = Region::new(GLOBAL);
    let qualified = qualify(parsed, schema);
    let bytes = region.change().bytes_allocated;
    drop(qualified);
    bytes
}

/// Bytes allocated for the input built at size 400, relative to size 100.
fn growth(case: impl Fn(usize) -> (String, MappingSchema)) -> f64 {
    let (short_sql, short_schema) = case(100);
    let (long_sql, long_schema) = case(400);
    allocated_bytes(&long_sql, &long_schema) as f64
        / allocated_bytes(&short_sql, &short_schema) as f64
}

fn qualified_star_subquery_references(width: usize) -> (String, MappingSchema) {
    let references = (0..width)
        .map(|i| format!("q.orders_c{i}"))
        .collect::<Vec<_>>()
        .join(", ");
    (
        format!("SELECT {references} FROM (SELECT * FROM orders) AS q WHERE q.id > 0"),
        schema(&["orders"], width),
    )
}

fn unqualified_join_references(width: usize) -> (String, MappingSchema) {
    let tables = ["orders", "customers", "products", "shipments"];
    let references = tables
        .iter()
        .flat_map(|table| (0..width).step_by(4).map(move |i| format!("{table}_c{i}")))
        .collect::<Vec<_>>()
        .join(", ");
    (
        format!(
            "SELECT {references} FROM orders JOIN customers ON customers.id = orders.id \
             JOIN products ON products.id = orders.id JOIN shipments ON shipments.id = orders.id"
        ),
        schema(&tables, width),
    )
}

fn nested_star_subqueries(size: usize) -> (String, MappingSchema) {
    let mut query = "SELECT * FROM orders".to_string();
    for level in 0..size / 10 {
        query = format!("SELECT * FROM ({query}) AS s{level} WHERE s{level}.id > {level}");
    }
    (query, schema(&["orders"], 20))
}

#[test]
fn qualification_allocation_scaling() {
    // Every column reference used to copy, and for ambiguity checks normalize,
    // the complete column lists of its sources.
    for (label, case) in [
        (
            "qualified references to a star subquery",
            qualified_star_subquery_references as fn(usize) -> (String, MappingSchema),
        ),
        (
            "unqualified references across joins",
            unqualified_join_references,
        ),
    ] {
        let growth = growth(case);
        assert!(
            growth < 4.5,
            "{label}: 4x the width allocated {growth:.1}x the bytes"
        );
    }

    // Each nesting level used to rebuild the complete scope tree of every
    // relation below it. A level still copies its own derived relation, so the
    // work grows quadratically rather than cubically with the depth.
    let growth = growth(nested_star_subqueries);
    assert!(
        growth < 20.0,
        "nested star subqueries: 4x the depth allocated {growth:.1}x the bytes"
    );
}
