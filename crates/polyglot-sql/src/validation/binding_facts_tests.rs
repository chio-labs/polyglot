use super::*;
use binding_facts::OccurrenceBinding;

#[test]
fn named_windows_and_filters_retain_their_lexical_cte_scope() {
    let sql = "WITH inputs AS (SELECT 1 AS amount, 2 AS category), staged AS (SELECT SUM(amount) FILTER(WHERE category > 0) OVER chosen AS total FROM inputs WINDOW chosen AS (PARTITION BY category ORDER BY amount ROWS BETWEEN amount PRECEDING AND CURRENT ROW)) SELECT total FROM staged";
    let dialect = DialectType::DuckDB;
    let schema = ValidationSchema {
        tables: vec![],
        strict: Some(true),
    };
    let facts = validate_parsed_with_binding_facts(
        crate::parse(sql, dialect).unwrap(),
        dialect,
        &schema,
        &SchemaValidationOptions::default(),
    );
    assert!(facts.validation.valid, "{:?}", facts.validation.errors);
    let staged = facts
        .scopes
        .iter()
        .find(|scope| scope.name.as_deref() == Some("staged"))
        .unwrap();
    let inputs: Vec<_> = facts
        .occurrences
        .iter()
        .filter(|occurrence| matches!(occurrence.name.as_str(), "amount" | "category"))
        .collect();
    assert!(!inputs.is_empty());
    assert!(
        inputs
            .iter()
            .all(|occurrence| occurrence.scope_id == staged.id),
        "{inputs:?}"
    );
    for clause in [
        "aggregate_filter",
        "window_partition",
        "window_order",
        "window_frame",
    ] {
        assert!(
            inputs.iter().any(|occurrence| occurrence.clause == clause),
            "{clause}: {inputs:?}"
        );
    }
}

#[test]
fn relation_aliases_bind_by_ordinal_before_original_output_names() {
    for dialect in [
        DialectType::Snowflake,
        DialectType::DuckDB,
        DialectType::PostgreSQL,
        DialectType::BigQuery,
        DialectType::Generic,
    ] {
        let sql = "WITH items AS (SELECT 1 AS order_id, 2 AS quantity) SELECT orders.quantity FROM items AS orders(quantity, order_id)";
        let schema = ValidationSchema {
            tables: vec![],
            strict: Some(true),
        };
        let facts = validate_parsed_with_binding_facts(
            crate::parse(sql, dialect).unwrap(),
            dialect,
            &schema,
            &SchemaValidationOptions::default(),
        );
        assert!(
            facts.validation.valid,
            "{dialect:?}: {:?}",
            facts.validation.errors
        );
        assert!(
            matches!(&facts.occurrences[0].binding, OccurrenceBinding::OutputSlot { slot } if slot.ordinal == 0 && slot.name.eq_ignore_ascii_case("order_id")),
            "{dialect:?}: {:?}",
            facts.occurrences
        );
    }
}

#[test]
fn lambda_capture_uses_the_same_table_function_scope_as_reference_validation() {
    let dialect = DialectType::Snowflake;
    let sql = "WITH inputs AS (SELECT 1 AS amount) SELECT TRANSFORM(ARRAY_CONSTRUCT(1), item -> item.value + inputs.amount) FROM inputs, customer_rows() AS item";
    let schema = ValidationSchema {
        tables: vec![],
        strict: Some(true),
    };
    let facts = validate_parsed_with_binding_facts(
        crate::parse(sql, dialect).unwrap(),
        dialect,
        &schema,
        &SchemaValidationOptions::default(),
    );
    assert!(facts.validation.valid, "{:?}", facts.validation.errors);
    assert!(facts.occurrences.iter().any(|occurrence| matches!(&occurrence.binding, OccurrenceBinding::OpenSourceColumn { source, column, .. } if source.eq_ignore_ascii_case("item") && column.eq_ignore_ascii_case("value"))), "{:?}", facts.occurrences);
}

#[test]
fn open_inputs_preserve_named_outputs_and_partial_star_interfaces() {
    for dialect in [
        DialectType::Snowflake,
        DialectType::DuckDB,
        DialectType::PostgreSQL,
        DialectType::BigQuery,
        DialectType::Generic,
    ] {
        for projection in ["o.status AS status, 1 AS priority", "o.*, 1 AS priority"] {
            let sql = format!(
                "WITH staged AS (SELECT {projection} FROM orders o) SELECT priority FROM staged"
            );
            let schema = ValidationSchema {
                tables: vec![],
                strict: Some(false),
            };
            let facts = validate_parsed_with_binding_facts(
                crate::parse(&sql, dialect).unwrap(),
                dialect,
                &schema,
                &SchemaValidationOptions::default(),
            );
            let cte = facts
                .scopes
                .iter()
                .find(|scope| scope.kind == "Cte")
                .unwrap();
            assert_eq!(cte.partially_checked, projection.contains('*'));
            for output in &cte.outputs {
                if let binding_facts::BindingOutput::Slot { slot } = output {
                    if slot.name.eq_ignore_ascii_case("priority") {
                        assert_eq!(slot.physical_ordinal.is_none(), projection.contains('*'));
                    }
                }
            }
            assert!(cte.outputs.iter().any(|output| matches!(output, binding_facts::BindingOutput::Slot { slot } if slot.name.eq_ignore_ascii_case("priority"))));
            assert!(facts.occurrences.iter().any(|occurrence| matches!(&occurrence.binding, OccurrenceBinding::OutputSlot { slot } if slot.scope_id == cte.id && slot.name.eq_ignore_ascii_case("priority"))));
            assert!(!facts.occurrences.iter().any(|occurrence| matches!(
                occurrence.binding,
                OccurrenceBinding::Unresolved { .. }
            )));
            if !projection.contains('*') {
                assert!(facts.occurrences.iter().any(|occurrence| matches!(&occurrence.binding, OccurrenceBinding::OpenSourceColumn { column, .. } if column.eq_ignore_ascii_case("status"))));
            }
        }
    }
}

#[test]
fn ordinary_validation_does_not_allocate_a_binding_observer() {
    let sql = "WITH items AS (SELECT 1 AS order_id) SELECT TRANSFORM(ARRAY_CONSTRUCT(order_id), item -> item + 1) FROM items";
    let schema = ValidationSchema {
        tables: vec![],
        strict: Some(true),
    };
    let options = SchemaValidationOptions::default();
    let before = binding_facts::OBSERVERS_CREATED.with(|count| count.get());
    let ordinary = validate_parsed_with_schema(
        crate::parse(sql, DialectType::Snowflake).unwrap(),
        DialectType::Snowflake,
        &schema,
        &options,
    );
    assert_eq!(
        before,
        binding_facts::OBSERVERS_CREATED.with(|count| count.get())
    );
    let observed = validate_parsed_with_binding_facts(
        crate::parse(sql, DialectType::Snowflake).unwrap(),
        DialectType::Snowflake,
        &schema,
        &options,
    );
    assert_eq!(
        before + 1,
        binding_facts::OBSERVERS_CREATED.with(|count| count.get())
    );
    assert_eq!(
        serde_json::to_value(ordinary).unwrap(),
        serde_json::to_value(observed.validation).unwrap()
    );
}

#[test]
fn literal_origin_reads_are_observed_in_the_validation_pass() {
    for dialect in [
        DialectType::Snowflake,
        DialectType::DuckDB,
        DialectType::PostgreSQL,
        DialectType::BigQuery,
        DialectType::Generic,
    ] {
        let sql = "WITH staged AS (SELECT 1 AS order_id, 2 AS priority), projected AS (SELECT order_id FROM staged WHERE priority > 0) SELECT order_id FROM projected";
        let statements = crate::parse(sql, dialect).unwrap();
        let schema = ValidationSchema {
            tables: vec![],
            strict: Some(true),
        };
        let options = SchemaValidationOptions::default();
        let facts = validate_parsed_with_binding_facts(statements, dialect, &schema, &options);
        assert!(
            facts.validation.valid,
            "{dialect:?}: {:?}",
            facts.validation.errors
        );
        assert_eq!(facts.occurrences.len(), 3, "{dialect:?}");
        assert!(
            facts.occurrences.iter().all(|occurrence| matches!(
                occurrence.binding,
                OccurrenceBinding::OutputSlot { .. }
            )),
            "{dialect:?}: {:?}",
            facts.occurrences
        );
    }
}

#[test]
fn lambda_and_pseudocolumn_bindings_are_observed() {
    for sql in [
        "WITH items AS (SELECT 1 AS order_id, 2 AS priority) SELECT TRANSFORM(ARRAY_CONSTRUCT(order_id), item -> item + priority) AS values FROM items",
        "WITH items AS (SELECT 1 AS order_id, 2 AS parent_id) SELECT order_id, LEVEL AS depth FROM items START WITH parent_id IS NULL CONNECT BY parent_id = PRIOR order_id",
    ] {
        let schema = ValidationSchema { tables: vec![], strict: Some(true) };
        let facts = validate_parsed_with_binding_facts(crate::parse(sql, DialectType::Snowflake).unwrap(), DialectType::Snowflake, &schema, &SchemaValidationOptions::default());
        assert!(facts.validation.valid, "{:?}", facts.validation.errors);
        assert!(facts.occurrences.iter().all(|occurrence| !matches!(occurrence.binding, OccurrenceBinding::Unresolved { .. })), "{:?}", facts.occurrences);
    }
}

#[test]
fn stars_and_lateral_inputs_preserve_intermediate_slot_identities() {
    for sql in [
        "WITH items AS (SELECT 1 AS x, 2 AS y) SELECT id FROM customer_rows((SELECT x FROM items))",
        "WITH items AS (SELECT 1 AS x, 2 AS y) SELECT f.value FROM items, LATERAL FLATTEN(INPUT => (SELECT ARRAY_AGG(x) FROM items)) f",
        "WITH items AS (SELECT 1 AS x, 2 AS y) SELECT f.value FROM items, LATERAL FLATTEN(INPUT => (SELECT ARRAY_CONSTRUCT(items.x))) f",
        "WITH items AS (SELECT 1 AS x, 2 AS y) SELECT * FROM items PIVOT(SUM(x) FOR y IN (2))",
        "WITH items AS (SELECT 1 AS x, 2 AS y), passed AS (SELECT * FROM items) SELECT x FROM passed",
        "WITH items AS (SELECT 1 AS x, 2 AS y) SELECT f.value FROM items, LATERAL FLATTEN(INPUT => ARRAY_CONSTRUCT(items.x, items.y)) f",
        "WITH items AS (SELECT 1 AS x, 2 AS y) SELECT x AS selected, selected + y AS calculated FROM items ORDER BY calculated",
        "WITH items AS (SELECT 1 AS x, 2 AS y) SELECT x FROM items WHERE EXISTS (SELECT 1 WHERE items.y > 0)",
    ] {
        let schema = ValidationSchema { tables: vec![], strict: Some(true) };
        let facts = validate_parsed_with_binding_facts(crate::parse(sql, DialectType::Snowflake).unwrap(), DialectType::Snowflake, &schema, &SchemaValidationOptions::default());
        assert!(facts.validation.valid, "{:?}", facts.validation.errors);
        assert!(facts.occurrences.iter().all(|occurrence| !matches!(occurrence.binding, OccurrenceBinding::Unresolved { .. })), "{sql}: {:?}", facts.occurrences);
    }
}

#[test]
fn fixture_occurrences_are_observed_without_panics_or_silent_loss() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/sqlglot_fixtures");
    let schema = ValidationSchema {
        tables: vec![],
        strict: Some(true),
    };
    let options = SchemaValidationOptions::default();
    let mut checked = 0;
    let mut missing = Vec::new();
    for (file, dialect) in [
        ("identity.json", DialectType::Generic),
        ("dialects/snowflake.json", DialectType::Snowflake),
        ("dialects/duckdb.json", DialectType::DuckDB),
        ("dialects/postgres.json", DialectType::PostgreSQL),
        ("dialects/bigquery.json", DialectType::BigQuery),
    ] {
        let path = root.join(file);
        if !path.exists() {
            continue;
        }
        let data: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
        let mut pending = vec![&data];
        while let Some(value) = pending.pop() {
            match value {
                serde_json::Value::Object(object) => {
                    if let Some(sql) = object.get("sql").and_then(|value| value.as_str()) {
                        if let Ok(statements) = crate::parse(sql, dialect) {
                            let facts = validate_parsed_with_binding_facts(
                                statements, dialect, &schema, &options,
                            );
                            if facts.validation.valid
                                && facts.occurrences.iter().any(|occurrence| {
                                    matches!(
                                        occurrence.binding,
                                        OccurrenceBinding::Unresolved { .. }
                                    )
                                })
                            {
                                missing
                                    .push(format!("{dialect:?}: {sql}\n{:?}", facts.occurrences));
                            }
                            checked += 1;
                        }
                    }
                    pending.extend(object.values());
                }
                serde_json::Value::Array(values) => pending.extend(values),
                _ => (),
            }
        }
    }
    assert!(checked > 100);
    assert!(
        missing.is_empty(),
        "{} incomplete queries of {checked}:\n{}",
        missing.len(),
        missing
            .iter()
            .take(30)
            .cloned()
            .collect::<Vec<_>>()
            .join("\n")
    );
}

#[test]
fn derived_shadowed_and_set_scopes_have_separate_output_identities() {
    for dialect in [
        DialectType::Snowflake,
        DialectType::DuckDB,
        DialectType::PostgreSQL,
        DialectType::BigQuery,
        DialectType::Generic,
    ] {
        for sql in [
            "WITH items AS (SELECT 1 AS x, 2 AS y) SELECT orders.order_id FROM items AS orders(order_id, quantity)",
            "WITH items AS (SELECT 1 AS x, 2 AS y) SELECT order_id FROM items AS orders(order_id, quantity)",
            "WITH items(order_id) AS (SELECT 1 AS x, 2 AS y) SELECT order_id, y FROM items",
            "SELECT left_orders.id, right_orders.id FROM (SELECT 1 AS id) left_orders CROSS JOIN (SELECT 1 AS id) right_orders",
            "WITH items AS (SELECT 1 AS id), nested AS (WITH items AS (SELECT 2 AS id) SELECT id FROM items) SELECT items.id, nested.id FROM items CROSS JOIN nested",
            "WITH items AS (SELECT 1 AS id UNION ALL SELECT 2 AS id) SELECT id FROM items",
        ] {
            let schema = ValidationSchema { tables: vec![], strict: Some(true) };
            let facts = validate_parsed_with_binding_facts(crate::parse(sql, dialect).unwrap(), dialect, &schema, &SchemaValidationOptions::default());
            assert!(facts.validation.valid, "{dialect:?} {sql}: {:?}", facts.validation.errors);
            assert!(facts.occurrences.iter().all(|occurrence| matches!(occurrence.binding, OccurrenceBinding::OutputSlot { .. })), "{dialect:?} {sql}: {:?}", facts.occurrences);
            if facts.occurrences.len() == 2 {
                assert_ne!(facts.occurrences[0].binding, facts.occurrences[1].binding, "{sql}");
            }
        }
    }
}

#[test]
fn using_and_natural_join_observations_retain_all_merged_inputs() {
    for dialect in [
        DialectType::Snowflake,
        DialectType::DuckDB,
        DialectType::PostgreSQL,
        DialectType::BigQuery,
        DialectType::Generic,
    ] {
        for join in ["JOIN second USING (id)", "NATURAL JOIN second"] {
            let sql = format!("WITH first AS (SELECT 1 AS id), second AS (SELECT 2 AS id) SELECT id FROM first {join}");
            let schema = ValidationSchema {
                tables: vec![],
                strict: Some(true),
            };
            let facts = validate_parsed_with_binding_facts(
                crate::parse(&sql, dialect).unwrap(),
                dialect,
                &schema,
                &SchemaValidationOptions::default(),
            );
            assert!(
                facts.validation.valid,
                "{dialect:?} {sql}: {:?}",
                facts.validation.errors
            );
            assert!(facts.occurrences.iter().all(|occurrence| matches!(&occurrence.binding, OccurrenceBinding::Merged { inputs } if inputs.len() == 2)), "{dialect:?} {sql}: {:?}", facts.occurrences);
        }
    }
}
