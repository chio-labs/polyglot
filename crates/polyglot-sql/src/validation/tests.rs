use super::*;
use crate::function_catalog::{FunctionNameCase, FunctionSignature, HashMapFunctionCatalog};
use std::sync::Arc;

#[test]
fn snowflake_directional_set_operation_matrix() {
    let fixture = include_str!("../../tests/fixtures/snowflake_set_operation_matrix.txt");
    let mut lines = fixture.lines();
    let types: Vec<_> = lines.next().unwrap().split_whitespace().collect();
    let rows: Vec<Vec<_>> = lines
        .map(|line| line.split_whitespace().collect())
        .collect();
    assert_eq!(types.len(), 13);
    assert_eq!(rows.len(), 13);
    let static_type = |name| match name {
        "VARCHAR_NUM" | "VARCHAR_TXT" => "VARCHAR",
        other => other,
    };
    let sql_value = |name| match name {
        "BOOLEAN" => "TRUE",
        "NUMBER" => "1::NUMBER(38,0)",
        "DECIMAL" => "1.25::NUMBER(10,2)",
        "FLOAT" => "1.5::FLOAT",
        "VARCHAR_NUM" => "'7'::VARCHAR",
        "VARCHAR_TXT" => "'abc'::VARCHAR",
        "DATE" => "'2026-01-01'::DATE",
        "TIMESTAMP_NTZ" => "'2026-01-01 10:00:00'::TIMESTAMP_NTZ",
        "TIMESTAMP_TZ" => "'2026-01-01 10:00:00 +00:00'::TIMESTAMP_TZ",
        "TIME" => "'10:00:00'::TIME",
        "VARIANT" => "PARSE_JSON('1')",
        "ARRAY" => "ARRAY_CONSTRUCT(1)",
        "NULL" => "NULL",
        _ => panic!("unknown fixture type"),
    };
    let schema = ValidationSchema {
        tables: vec![],
        strict: Some(true),
    };
    let options = SchemaValidationOptions {
        check_types: true,
        check_references: true,
        semantic: true,
        ..Default::default()
    };
    let mut failures = Vec::new();
    for (i, first) in types.iter().enumerate() {
        assert_eq!(rows[i].len(), 14);
        assert_eq!(&rows[i][0], first);
        for (j, later) in types.iter().enumerate() {
            let verdict = rows[i][j + 1];
            // Literal-dependent conversion failures require a warning for the
            // static pair, even when this particular sample value succeeds.
            let runtime_risk = rows.iter().any(|row| {
                static_type(row[0]) == static_type(first)
                    && types.iter().enumerate().any(|(k, ty)| {
                        static_type(ty) == static_type(later) && row[k + 1] == "runtime"
                    })
            });
            for operator in [
                "UNION ALL",
                "UNION",
                "INTERSECT",
                "EXCEPT",
                "MINUS",
                "UNION ALL BY NAME",
                "UNION BY NAME",
            ] {
                let sql = format!(
                    "SELECT {} AS x {operator} SELECT {} AS x",
                    sql_value(first),
                    sql_value(later)
                );
                let result = validate_with_schema(&sql, DialectType::Snowflake, &schema, &options);
                let error = result.errors.iter().any(|e| e.code == "E215");
                let warning = result.errors.iter().any(|e| e.code == "W214");
                if error != (verdict == "error")
                    || warning != runtime_risk
                    || result.valid == (verdict == "error")
                {
                    failures.push(format!("{first}|{later} {operator}: engine={verdict}, runtime_risk={runtime_risk}, {:?}", result.errors));
                }
            }
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn snowflake_set_operation_chains_keep_first_target() {
    let schema = ValidationSchema {
        tables: vec![],
        strict: Some(true),
    };
    let options = SchemaValidationOptions {
        check_types: true,
        semantic: true,
        ..Default::default()
    };
    for (sql, valid) in [
        ("SELECT TRUE x UNION ALL SELECT 1 UNION ALL SELECT 1.5", true),
        ("SELECT 1 x UNION ALL SELECT TRUE UNION ALL SELECT 1.5", false),
        ("SELECT TRUE x UNION ALL SELECT 1 UNION ALL SELECT CURRENT_DATE", false),
        ("SELECT 'abc'::VARCHAR x UNION ALL SELECT 1 UNION ALL SELECT TRUE", true),
        ("SELECT NULL x UNION ALL SELECT 1 UNION ALL SELECT TRUE", false),
        ("SELECT NULL x UNION ALL SELECT TRUE UNION ALL SELECT 1", true),
        ("SELECT TRUE x UNION ALL SELECT NULL UNION ALL SELECT 1", true),
        ("SELECT CURRENT_DATE x UNION ALL SELECT NULL::TIMESTAMP_NTZ UNION ALL SELECT NULL::TIMESTAMP_TZ", true),
        ("SELECT NULL::TIMESTAMP_NTZ x UNION ALL SELECT CURRENT_DATE UNION ALL SELECT NULL::TIMESTAMP_TZ", false),
        ("SELECT PARSE_JSON('1') x UNION ALL SELECT 1 UNION ALL SELECT 'abc'::VARCHAR", false),
        ("SELECT TRUE x, 1 y UNION ALL BY NAME SELECT 2 y, 1 x UNION ALL BY NAME SELECT 3 x, 3 y", true),
        ("SELECT TRUE x, 1 y UNION ALL BY NAME SELECT 2 y, 1 x UNION ALL BY NAME SELECT 3 x, TRUE y", false),
        ("SELECT 1 x UNION ALL BY NAME SELECT TRUE y UNION ALL BY NAME SELECT 1 y", true),
        ("SELECT 1 x UNION ALL BY NAME SELECT 1 y UNION ALL BY NAME SELECT TRUE y", false),
        ("WITH orders AS (SELECT TRUE x UNION ALL SELECT 1) SELECT x FROM orders UNION ALL SELECT 2", true),
        ("WITH orders AS (SELECT '2026-01-01'::DATE x UNION ALL SELECT NULL::TIMESTAMP_NTZ) SELECT x FROM orders UNION ALL SELECT NULL::TIMESTAMP_TZ", true),
        ("WITH orders AS (SELECT PARSE_JSON('1') x) SELECT x FROM orders UNION ALL SELECT 'abc'::VARCHAR", false),
        ("WITH orders AS (SELECT ARRAY_CONSTRUCT(1) x) SELECT x FROM orders UNION ALL SELECT TRUE", false),
        ("SELECT order_flag() x UNION ALL SELECT 1 UNION ALL SELECT TRUE", true),
    ] {
        let result = validate_with_schema(sql, DialectType::Snowflake, &schema, &options);
        assert_eq!(result.valid, valid, "{sql}: {:?}", result.errors);
    }
}

fn snowflake_orders_schema() -> ValidationSchema {
    serde_json::from_value(
        serde_json::json!({"tables": [{"name": "orders", "columns": [
            {"name":"id","type":"NUMBER(38,0)"}, {"name":"parent_id","type":"NUMBER(38,0)"},
            {"name":"quantity","type":"NUMBER(38,0)"}, {"name":"amount","type":"FLOAT"},
            {"name":"ordered_on","type":"DATE"}, {"name":"ordered_at","type":"TIMESTAMP_NTZ"},
            {"name":"status","type":"VARCHAR"}, {"name":"active","type":"BOOLEAN"},
            {"name":"payload","type":"VARIANT"}, {"name":"value","type":"VARCHAR"}
        ]}]}),
    )
    .unwrap()
}

#[test]
fn snowflake_realworld_scope_and_aggregate_regressions() {
    let schema = snowflake_orders_schema();
    for check_types in [false, true] {
        let options = SchemaValidationOptions {
            semantic: true,
            check_types,
            check_references: true,
            ..Default::default()
        };
        for sql in [
            "SELECT id, LEVEL - 1, CONNECT_BY_ROOT id, SYS_CONNECT_BY_PATH(id::VARCHAR, ',') FROM orders START WITH parent_id IS NULL CONNECT BY parent_id = PRIOR id",
            "SELECT LEVEL, CONNECT_BY_ISLEAF, CONNECT_BY_ISCYCLE FROM orders CONNECT BY parent_id = PRIOR id",
            "SELECT COUNT(id, quantity, amount), COUNT(DISTINCT id, quantity) FROM orders",
            "SELECT status, COUNT(id, quantity) FROM orders GROUP BY status",
            "SELECT COUNT(id, quantity) OVER () FROM orders",
            "SELECT OBJECT_AGG(status,payload), CORR(quantity,amount), COVAR_POP(quantity,amount), REGR_SLOPE(quantity,amount) FROM orders",
            "SELECT m.value FROM orders, LATERAL FLATTEN(input => PARSE_JSON(value):customers) m",
            "SELECT m.value FROM orders, LATERAL FLATTEN(input => payload:customers) m",
            "SELECT m.value FROM orders o JOIN orders c ON o.id=c.id, LATERAL FLATTEN(input => c.payload) m",
            "SELECT m.value, c.value FROM orders, LATERAL FLATTEN(input => PARSE_JSON(value):customers) m, LATERAL FLATTEN(input => m.value:shipments) c",
            "SELECT f.value FROM (SELECT payload FROM orders) source, LATERAL FLATTEN(input => payload:customers), LATERAL FLATTEN(input => value) f",
            "SELECT m.value, c.value FROM orders, LATERAL FLATTEN(input => payload:customers) m, LATERAL FLATTEN(input => m.value:shipments) c",
        ] {
            let result = validate_with_schema(sql, DialectType::Snowflake, &schema, &options);
            assert!(result.valid, "types={check_types}: {sql}: {:?}", result.errors);
        }
        for (sql, code) in [
            ("SELECT LEVEL FROM orders", "E201"),
            ("SELECT CONNECT_BY_ISLEAF FROM orders", "E201"),
            (
                "SELECT missing FROM orders CONNECT BY parent_id = PRIOR id",
                "E201",
            ),
            (
                "SELECT orders.LEVEL FROM orders CONNECT BY parent_id = PRIOR id",
                "E201",
            ),
            ("SELECT status, COUNT(id,quantity) FROM orders", "E230"),
            ("SELECT id FROM orders WHERE COUNT(id,quantity)>1", "E231"),
            (
                "SELECT value FROM orders, LATERAL FLATTEN(input => payload:customers) m",
                "E221",
            ),
            (
                "SELECT m.value FROM orders, LATERAL FLATTEN(input => missing) m",
                "E201",
            ),
            (
                "SELECT m.value FROM orders, LATERAL FLATTEN(input => m.value) m",
                "E222",
            ),
        ] {
            let result = validate_with_schema(sql, DialectType::Snowflake, &schema, &options);
            assert!(
                !result.valid && result.errors.iter().any(|e| e.code == code),
                "types={check_types}: {sql}: {:?}",
                result.errors
            );
        }
    }
}

#[test]
fn snowflake_realworld_function_types_and_timestamp_aliases() {
    let schema = snowflake_orders_schema();
    let options = SchemaValidationOptions {
        semantic: true,
        check_types: true,
        check_references: true,
        ..Default::default()
    };
    for (expression, expected) in [
        ("DATE_FROM_PARTS(2026,1,1)", TypeFamily::Date),
        ("DAYOFWEEKISO(CURRENT_DATE)", TypeFamily::Integer),
        (
            "CASE WHEN TRUE THEN TO_TIMESTAMP(0) ELSE TRY_TO_TIMESTAMP('2026-01-01') END",
            TypeFamily::Timestamp,
        ),
        ("CASE WHEN TRUE THEN order_date(1) END", TypeFamily::Unknown),
    ] {
        let mut statement =
            crate::parse_one(&format!("SELECT {expression}"), DialectType::Snowflake).unwrap();
        annotate_types(&mut statement, None, Some(DialectType::Snowflake));
        let Expression::Select(select) = statement else {
            unreachable!()
        };
        let actual = select.expressions[0]
            .inferred_type()
            .map(data_type_family)
            .unwrap_or(TypeFamily::Unknown);
        assert_eq!(actual, expected, "{expression}");
    }
    for sql in [
        "WITH shipments AS (SELECT CASE WHEN active THEN TO_TIMESTAMP(quantity) ELSE TRY_TO_TIMESTAMP(status) END AS shipped_at FROM orders) SELECT DATE_PART(EPOCH_SECOND, shipped_at) FROM shipments",
        "WITH shipments AS (SELECT CASE WHEN active THEN TO_TIMESTAMP(quantity) ELSE TRY_TO_TIMESTAMP(REGEXP_SUBSTR(status,'[0-9-]+')) END AS shipped_at FROM orders) SELECT DATE_PART(EPOCH_SECOND, shipped_at) FROM shipments",
        "SELECT COALESCE(TRY_CAST(status AS DATE), ordered_on, CASE WHEN active THEN DATE_FROM_PARTS(YEAR(ordered_on)-quantity,1,1) END) FROM orders",
        "SELECT COALESCE(ordered_on, CASE WHEN active THEN DATE_FROM_PARTS(YEAR(ordered_on)-quantity,1,1) END) FROM orders",
        "WITH shipments AS (SELECT 5 + DAYOFWEEKISO(ordered_on) AS delivery_days, DATEDIFF(day,ordered_on,CURRENT_DATE) AS elapsed_days FROM orders) SELECT delivery_days >= elapsed_days FROM shipments",
        "SELECT COALESCE(ordered_on, CASE WHEN active THEN order_date(quantity) END) FROM orders",
    ] {
        let result = validate_with_schema(sql, DialectType::Snowflake, &schema, &options);
        assert!(result.valid, "{sql}: {:?}", result.errors);
    }
    for target in [
        "TIMESTAMP_NTZ",
        "TIMESTAMP_LTZ",
        "TIMESTAMP_TZ",
        "TIMESTAMPNTZ",
        "TIMESTAMPLTZ",
        "TIMESTAMPTZ",
        "TIMESTAMP_LTZ(9)",
    ] {
        let sql = format!("SELECT CAST(ordered_at AS {target}) FROM orders");
        let result = validate_with_schema(&sql, DialectType::Snowflake, &schema, &options);
        assert!(
            result.valid && !result.errors.iter().any(|e| e.code == "W213"),
            "{sql}: {:?}",
            result.errors
        );
    }
    let result = validate_with_schema(
        "SELECT CAST(status AS UNKNOWN_ORDER_TYPE) FROM orders",
        DialectType::Snowflake,
        &schema,
        &options,
    );
    let issue = result.errors.iter().find(|e| e.code == "W213").unwrap();
    assert!(
        issue.message.contains("UNKNOWN_ORDER_TYPE")
            && !issue.message.contains("Custom {")
            && !issue.message.contains("name:")
    );
    for sql in [
        "SELECT quantity AS result FROM orders UNION ALL SELECT active FROM orders",
        "SELECT DATE_PART(day,quantity) FROM orders",
    ] {
        assert!(
            !validate_with_schema(sql, DialectType::Snowflake, &schema, &options).valid,
            "{sql}"
        );
    }
}

#[test]
fn snowflake_realworld_commented_named_union_keeps_column_identity() {
    let schema = snowflake_orders_schema();
    let options = SchemaValidationOptions {
        semantic: true,
        check_types: true,
        check_references: true,
        ..Default::default()
    };
    let sql = "WITH shipments AS (SELECT * FROM orders) SELECT id,\n-- availability\nactive,\n-- subtotal\namount FROM shipments UNION ALL BY NAME SELECT amount,id,\n-- availability\nactive FROM shipments UNION ALL BY NAME SELECT amount,id,\n-- availability\nactive FROM shipments";
    let result = validate_with_schema(sql, DialectType::Snowflake, &schema, &options);
    assert!(result.valid, "{:?}", result.errors);
    let sql = "WITH shipments AS (SELECT * FROM orders) SELECT id, amount AS active FROM shipments UNION ALL BY NAME SELECT id,\n-- availability\nactive FROM shipments";
    let result = validate_with_schema(sql, DialectType::Snowflake, &schema, &options);
    assert!(
        result.errors.iter().any(|e| e.code == "E215"),
        "{:?}",
        result.errors
    );
}

#[test]
fn semantic_followups_closed_relation_negative_controls() {
    let result = validate_with_schema(
        "SELECT t.a FROM (SELECT COLUMNS(*) FROM orders) t(a,b)",
        DialectType::DuckDB,
        &semantic_type_schema(),
        &SchemaValidationOptions {
            semantic: true,
            ..Default::default()
        },
    );
    assert!(
        !result
            .errors
            .iter()
            .any(|error| error.code == validation_codes::E_CTE_COLUMN_COUNT_MISMATCH),
        "{:?}",
        result.errors
    );
    for dialect in [DialectType::DuckDB, DialectType::Snowflake] {
        for check_types in [false, true] {
            let options = SchemaValidationOptions {
                semantic: true,
                check_types,
                ..Default::default()
            };
            for (sql, code) in [
                ("SELECT t.a FROM (SELECT 1,2) t(a,b,c)", validation_codes::E_CTE_COLUMN_COUNT_MISMATCH),
                ("SELECT v.a FROM (VALUES (1,2)) v(a,b,c)", validation_codes::E_CTE_COLUMN_COUNT_MISMATCH),
                ("SELECT t.a FROM (SELECT * FROM orders) t(a,b,c,d,e,f,g,h,i,j,k,l,m,n,o,p)", validation_codes::E_CTE_COLUMN_COUNT_MISMATCH),
                ("SELECT value,name FROM (SELECT 1 col1,2 col2) UNPIVOT (value FOR name IN (col1,missing))", "E201"),
                ("WITH input AS (SELECT 1 col1,2 col2) SELECT value,name FROM input UNPIVOT (value FOR name IN (col1,missing))", "E201"),
            ] {
                let result = validate_with_schema(sql, dialect, &semantic_type_schema(), &options);
                assert!(!result.valid && result.errors.iter().any(|error| error.code == code), "{dialect:?}, types={check_types}: {sql}: {:?}", result.errors);
            }
            for sql in [
                "SELECT t.a FROM (SELECT 1,2) t(a)",
                "SELECT v.a FROM (VALUES (1,2)) v(a)",
                "SELECT t.a FROM (SELECT 1,2) t(a,b)",
                "SELECT v.a FROM (VALUES (1,2)) v(a,b)",
                "SELECT value,name FROM (SELECT 1 col1,2 col2) UNPIVOT (value FOR name IN (col1,col2))",
            ] {
                let result = validate_with_schema(sql, dialect, &semantic_type_schema(), &options);
                assert!(result.valid, "{dialect:?}, types={check_types}: {sql}: {:?}", result.errors);
            }
            for sql in [
                "SELECT t.a FROM (SELECT * FROM unknown_orders) t(a,b,c)",
                "SELECT value,name FROM unknown_orders UNPIVOT (value FOR name IN (col1,missing))",
            ] {
                let result = validate_with_schema(
                    sql,
                    dialect,
                    &ValidationSchema {
                        tables: vec![],
                        strict: None,
                    },
                    &options,
                );
                assert!(
                    !result
                        .errors
                        .iter()
                        .any(|error| matches!(error.code.as_str(), "E201")
                            || error.code == validation_codes::E_CTE_COLUMN_COUNT_MISMATCH),
                    "{sql}: {:?}",
                    result.errors
                );
            }
        }
    }
}

#[test]
fn semantic_followups_relation_aliases_unpivot_and_grouping() {
    let schema = semantic_type_schema();
    for dialect in [DialectType::DuckDB, DialectType::Snowflake] {
        for check_types in [false, true] {
            let options = SchemaValidationOptions {
                semantic: true,
                check_types,
                check_references: true,
                ..Default::default()
            };
            for sql in [
                "SELECT t.a FROM (SELECT 1, 2) AS t(a, b)",
                "SELECT v.a FROM (VALUES (1, 2)) AS v(a, b)",
                "WITH c AS (SELECT t.a FROM (SELECT 1, 2) t(a,b)) SELECT a FROM c",
                "SELECT value,name FROM (SELECT 1 col1,2 col2) UNPIVOT (value FOR name IN (col1,col2))",
                "SELECT u.value,u.name FROM (SELECT 1 col1,2 col2) UNPIVOT (value FOR name IN (col1,col2)) u",
                "SELECT a,b,COUNT(*),GROUPING(a) FROM (SELECT 1 a,2 b) t GROUP BY GROUPING SETS ((a),(a,b))",
                "SELECT a,b,COUNT(*),GROUPING(a) FROM (SELECT 1 a,2 b) t GROUP BY CUBE(a,b)",
                "SELECT a,b,COUNT(*),GROUPING(a) FROM (SELECT 1 a,2 b) t GROUP BY ROLLUP(a,b)",
                "SELECT a+1,COUNT(*) FROM (SELECT 1 a) t GROUP BY GROUPING SETS ((a+1),())",
            ] {
                let result = validate_with_schema(sql, dialect, &schema, &options);
                assert!(result.valid, "{dialect:?} types={check_types} {sql}: {:?}", result.errors);
            }
            for sql in [
                "SELECT t.missing FROM (SELECT 1,2) t(a,b)",
                "SELECT v.missing FROM (VALUES (1,2)) v(a,b)",
                "SELECT missing FROM (SELECT 1 col1,2 col2) UNPIVOT (value FOR name IN (col1,col2))",
                "SELECT b,COUNT(*) FROM (SELECT 1 a,2 b) t GROUP BY GROUPING SETS ((a),())",
            ] {
                assert!(!validate_with_schema(sql,dialect,&schema,&options).valid, "{dialect:?}: {sql}");
            }
            if dialect == DialectType::DuckDB {
                for sql in [
                    "SELECT t.\"1\" FROM (SELECT 1) t",
                    "UNPIVOT (SELECT 1 col1,2 col2) ON col1,col2 INTO NAME name VALUE value",
                    "SELECT value,name FROM (UNPIVOT (SELECT 1 col1,2 col2) ON col1,col2 INTO NAME name VALUE value)",
                ] {
                    let result = validate_with_schema(sql,dialect,&schema,&options);
                    assert!(result.valid, "{sql}: {:?}", result.errors);
                }
            }
        }
    }
}

#[test]
fn semantic_followups_count_placement_location() {
    let sql = "SELECT i FROM orders\nWHERE COUNT(*) > 1";
    let result = validate_with_schema(
        sql,
        DialectType::DuckDB,
        &semantic_type_schema(),
        &SchemaValidationOptions {
            semantic: true,
            ..Default::default()
        },
    );
    let error = result.errors.iter().find(|e| e.code == "E231").unwrap();
    assert_eq!(error.line, Some(2));
    assert!(error.column.is_some());
    assert_eq!(&sql[error.start.unwrap()..error.end.unwrap()], "COUNT(*)");
}

#[test]
fn semantic_followups_dense_named_union_is_bounded() {
    // Independent synthetic generator: a wide, name-aligned set operation
    // followed by a CTE chain. The old repeated-prefix algorithm took >30s.
    let columns = (0..64)
        .map(|i| format!("quantity_{i}"))
        .collect::<Vec<_>>()
        .join(", ");
    let outputs = (0..64)
        .map(|i| format!("CAST(i AS DOUBLE) AS quantity_{i}"))
        .collect::<Vec<_>>()
        .join(", ");
    let branches = (0..128)
        .map(|_| format!("SELECT {columns} FROM projected"))
        .collect::<Vec<_>>()
        .join(" UNION ALL BY NAME ");
    let mut sql = format!("WITH imported AS (SELECT * FROM orders), projected AS (SELECT {outputs} FROM imported), combined AS ({branches})");
    let mut source = "combined".to_owned();
    for i in 0..12 {
        sql.push_str(&format!(", stage_{i} AS (SELECT {columns} FROM {source})"));
        source = format!("stage_{i}");
    }
    sql.push_str(&format!(" SELECT {columns} FROM {source}"));
    let start = std::time::Instant::now();
    let result = validate_with_schema(
        &sql,
        DialectType::DuckDB,
        &semantic_type_schema(),
        &SchemaValidationOptions {
            semantic: true,
            check_types: true,
            check_references: true,
            ..Default::default()
        },
    );
    assert!(result.valid, "{:?}", result.errors);
    assert!(
        start.elapsed() < std::time::Duration::from_secs(12),
        "wide named UNION validation took {:?}",
        start.elapsed()
    );
}

#[test]
fn order_by_all_expands_selected_columns_in_supported_dialects() {
    let schema = semantic_type_schema();
    for dialect in [DialectType::DuckDB, DialectType::Snowflake] {
        for check_types in [false, true] {
            let options = SchemaValidationOptions {
                check_types,
                semantic: true,
                check_references: true,
                ..Default::default()
            };
            for distinct in ["", "DISTINCT "] {
                for ordering in ["ALL", "all ASC", "ALL DESC NULLS FIRST", "ALL NULLS LAST"] {
                    let sql = format!(
                        "SELECT {distinct}a, b FROM (SELECT 1 AS a, 2 AS b) t ORDER BY {ordering}"
                    );
                    let result = validate_with_schema(&sql, dialect, &schema, &options);
                    assert!(
                        result.valid,
                        "{dialect:?}, types={check_types}, {sql}: {:?}",
                        result.errors
                    );
                    for operation in ["UNION ALL", "INTERSECT", "EXCEPT"] {
                        let sql = format!("SELECT {distinct}a FROM (SELECT 1 AS a) t {operation} SELECT 2 AS a ORDER BY {ordering}");
                        let result = validate_with_schema(&sql, dialect, &schema, &options);
                        assert!(
                            result.valid,
                            "{dialect:?}, types={check_types}, {sql}: {:?}",
                            result.errors
                        );
                    }
                }
            }
            for sql in [
                "WITH q AS (SELECT a FROM (SELECT 1 a) t ORDER BY ALL DESC) SELECT a FROM q ORDER BY ALL",
                "SELECT a FROM (SELECT 1 a) t GROUP BY ALL ORDER BY ALL",
                "SELECT a FROM (SELECT 1 a, 2 AS \"all\") t ORDER BY ALL",
                "SELECT 1 AS all ORDER BY ALL",
            ] {
                let result = validate_with_schema(sql, dialect, &schema, &options);
                assert!(result.valid, "{dialect:?}, types={check_types}, {sql}: {:?}", result.errors);
            }
        }
    }
    let result = validate_with_schema(
        "SELECT a, COUNT(*) FROM (VALUES (1), (2)) t(a) GROUP BY ALL ORDER BY ALL DESC",
        DialectType::DuckDB,
        &schema,
        &SchemaValidationOptions {
            check_types: true,
            semantic: true,
            ..Default::default()
        },
    );
    assert!(result.valid, "{:?}", result.errors);
}

#[test]
fn order_by_all_preserves_real_column_resolution() {
    let schema = semantic_type_schema();
    for dialect in [DialectType::DuckDB, DialectType::Snowflake] {
        for check_types in [false, true] {
            let options = SchemaValidationOptions {
                check_types,
                semantic: true,
                check_references: true,
                ..Default::default()
            };
            for sql in [
                "SELECT DISTINCT \"all\" FROM (SELECT 1 AS \"all\") t ORDER BY \"all\" DESC NULLS LAST",
                "SELECT DISTINCT t.\"all\" FROM (SELECT 1 AS \"all\") t ORDER BY t.\"all\"",
                "SELECT a FROM (SELECT 1 AS a, 2 AS \"ALL\") t ORDER BY t.all",
                "SELECT a FROM (SELECT 1 AS a, 2 AS \"ALL\") t ORDER BY t.all + 1",
                "SELECT 1 AS \"all\" UNION ALL SELECT 2 AS \"all\" ORDER BY \"all\"",
            ] {
                let result = validate_with_schema(sql, dialect, &schema, &options);
                assert!(result.valid, "{dialect:?}, types={check_types}, {sql}: {:?}", result.errors);
            }
            for sql in [
                "SELECT a FROM (SELECT 1 AS a) t ORDER BY \"all\"",
                "SELECT DISTINCT a FROM (SELECT 1 AS a) t ORDER BY \"all\"",
                "SELECT a FROM (SELECT 1 AS a) t ORDER BY t.all",
                "SELECT a FROM (SELECT 1 AS a) t ORDER BY ALL, missing",
                "SELECT 1 AS a UNION ALL SELECT 2 AS a ORDER BY \"all\"",
            ] {
                let result = validate_with_schema(sql, dialect, &schema, &options);
                assert!(
                    !result.valid && result.errors.iter().any(|e| e.code == "E201"),
                    "{dialect:?}, types={check_types}, {sql}: {:?}",
                    result.errors
                );
            }
        }
    }
    let options = SchemaValidationOptions {
        semantic: true,
        ..Default::default()
    };
    assert!(
        !validate_with_schema(
            "SELECT a FROM (SELECT 1 a) t ORDER BY ALL",
            DialectType::Generic,
            &schema,
            &options
        )
        .valid
    );
    // A real, non-selected column must still trigger Snowflake DISTINCT's rule.
    assert!(
        !validate_with_schema(
            "SELECT DISTINCT a FROM (SELECT 1 a, 2 AS \"all\") t ORDER BY \"all\"",
            DialectType::Snowflake,
            &schema,
            &options
        )
        .valid
    );
}

#[test]
fn snowflake_engine_truth_regressions() {
    #[derive(serde::Deserialize)]
    struct Case {
        id: String,
        sql: String,
        verdict: String,
        expected_error: bool,
    }
    #[derive(serde::Deserialize)]
    struct Fixture {
        schema: ValidationSchema,
        cases: Vec<Case>,
    }
    // SQL is replayed with the same typed inputs used by the inline CTEs in
    // executed_sql. Only synthetic SQL, verdicts and numeric engine codes are
    // retained; raw warehouse error responses are deliberately excluded.
    let fixture: Fixture = serde_json::from_str(include_str!(
        "../../tests/fixtures/snowflake_semantic_truth.json"
    ))
    .unwrap();
    assert_eq!(fixture.cases.len(), 211);
    let options = SchemaValidationOptions {
        check_types: true,
        check_references: true,
        semantic: true,
        ..Default::default()
    };
    let mut failures = Vec::new();
    let mut counts = [0; 3];
    let mut claimed = 0;
    for case in fixture.cases {
        let result =
            validate_with_schema(&case.sql, DialectType::Snowflake, &fixture.schema, &options);
        let errors: Vec<_> = result
            .errors
            .iter()
            .filter(|e| e.severity == crate::ValidationSeverity::Error)
            .collect();
        assert!(
            !errors.iter().any(|e| e.code == "E202"),
            "{}: Snowflake UDF names must remain open",
            case.id
        );
        match case.verdict.as_str() {
            "valid" => counts[0] += 1,
            "runtime_error" => counts[1] += 1,
            "compile_error" => counts[2] += 1,
            other => panic!("Unknown engine verdict {other}"),
        }
        if case.expected_error {
            claimed += 1;
        }
        if (case.verdict != "compile_error" && !result.valid)
            || (case.expected_error && result.valid)
            || (["f01", "F17", "F18", "F04", "F06", "R03"].contains(&case.id.as_str())
                && !result.valid)
        {
            failures.push(format!(
                "{} ({}, covered={}): {}: {:?}",
                case.id, case.verdict, case.expected_error, case.sql, errors
            ));
        }
    }
    assert_eq!(counts, [55, 30, 126]);
    assert_eq!(claimed, 119);
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn snowflake_ordered_set_aggregates_and_dialect_controls() {
    let schema = semantic_type_schema();
    let options = SchemaValidationOptions {
        check_types: true,
        semantic: true,
        ..Default::default()
    };
    for (dialect, sql) in [
        (
            DialectType::Snowflake,
            "SELECT LISTAGG(s, ',') WITHIN GROUP (ORDER BY s) FROM orders",
        ),
        (
            DialectType::Snowflake,
            "SELECT ARRAY_AGG(i) WITHIN GROUP (ORDER BY i) FROM orders",
        ),
        (
            DialectType::Snowflake,
            "SELECT PERCENTILE_CONT(0.5) WITHIN GROUP (ORDER BY i) FROM orders",
        ),
        (
            DialectType::PostgreSQL,
            "SELECT PERCENTILE_CONT(0.5) WITHIN GROUP (ORDER BY i) FROM orders",
        ),
        (
            DialectType::DuckDB,
            "SELECT PERCENTILE_CONT(0.5) WITHIN GROUP (ORDER BY i) FROM orders",
        ),
        (
            DialectType::Snowflake,
            "SELECT UPPER(i), LOWER(ts), LEFT(i, 1), SPLIT_PART(ts, '-', 1), LENGTH(i) FROM orders",
        ),
        (
            DialectType::Snowflake,
            "SELECT COALESCE(i,s), s = TRUE FROM orders",
        ),
    ] {
        let result = validate_with_schema(sql, dialect, &schema, &options);
        assert!(result.valid, "{dialect:?}: {sql}: {:?}", result.errors);
    }
    let result = validate_with_schema(
        "SELECT i, LISTAGG(s, ',') WITHIN GROUP (ORDER BY s) FROM orders",
        DialectType::Snowflake,
        &schema,
        &options,
    );
    assert!(result.errors.iter().any(|e| e.code == "E230"));
    for sql in [
        "SELECT AVG(ts) FROM orders",
        "SELECT SUM(b) FROM orders",
        "SELECT i FROM orders WHERE i",
        "SELECT i FROM orders UNION ALL SELECT b FROM orders",
        "WITH c(a,b) AS (SELECT i FROM orders) SELECT a FROM c",
        "WITH c(a) AS (SELECT i,s FROM orders) SELECT a FROM c",
        "SELECT DISTINCT s FROM orders ORDER BY i",
        "SELECT LAG(i, i) OVER (ORDER BY i) FROM orders",
    ] {
        let result = validate_with_schema(sql, DialectType::DuckDB, &schema, &options);
        assert!(result.valid, "DuckDB control {sql}: {:?}", result.errors);
    }
    for sql in [
        "WITH c(a) AS (SELECT i,s FROM orders) SELECT a FROM c",
        "SELECT LAG(i, i) OVER (ORDER BY i) FROM orders",
    ] {
        let result = validate_with_schema(sql, DialectType::Snowflake, &schema, &options);
        assert!(!result.valid, "Snowflake error {sql}: {:?}", result.errors);
    }
}

#[test]
fn snowflake_builtin_arity_does_not_close_function_namespace() {
    for check_types in [false, true] {
        let options = SchemaValidationOptions {
            semantic: true,
            check_types,
            ..Default::default()
        };
        for sql in [
            "SELECT SUM(i, i) FROM orders",
            "SELECT DATE_TRUNC('day')",
            "SELECT SUBSTRING(s) FROM orders",
        ] {
            let result = validate_with_schema(
                sql,
                DialectType::Snowflake,
                &semantic_type_schema(),
                &options,
            );
            assert!(
                result.errors.iter().any(|e| e.code == "E203"),
                "{sql}: {:?}",
                result.errors
            );
        }
        for sql in [
            "SELECT order_total(i, s) FROM orders",
            "SELECT DATE_DIFF('day', s, ts) FROM orders",
            "SELECT not_a_function(i) FROM orders",
        ] {
            let result = validate_with_schema(
                sql,
                DialectType::Snowflake,
                &semantic_type_schema(),
                &options,
            );
            assert!(result.valid, "{sql}: {:?}", result.errors);
        }
    }
}

#[test]
fn review_bind_time_coercion_controls() {
    use DialectType::{BigQuery, DuckDB, Generic, PostgreSQL, Snowflake};
    let schema: ValidationSchema = serde_json::from_value(serde_json::json!({"tables":[{"name":"orders","columns":[{"name":"x","type":"NUMBER(38,0)"},{"name":"i","type":"INTEGER"},{"name":"s","type":"VARCHAR"},{"name":"ts","type":"TIMESTAMP"}]}]})).unwrap();
    for check_types in [false, true] {
        let options = SchemaValidationOptions {
            semantic: true,
            check_types,
            ..Default::default()
        };
        for (dialect, sql) in [
            (BigQuery, "SELECT DATE_DIFF(DATE '2024-01-02', DATE '2024-01-01', DAY)"),
            (BigQuery, "SELECT CAST(1 AS BIGNUMERIC)"),
            (Snowflake, "SELECT SUBSTR('abc', x, 1), ROUND(1.234, x) FROM orders"),
            (Snowflake, "SELECT DATEDIFF(day, '2024-01-01', '2024-01-02')"),
            (DuckDB, "SELECT TRUE IN (SELECT 1)"),
            (Snowflake, "SELECT '1' IN (SELECT 1)"),
            (PostgreSQL, "SELECT (1, 2) = (SELECT 1, 2)"),
            (DuckDB, "SELECT * FROM (SELECT 1 a, 2 b) PIVOT (sum(b) FOR a IN (1))"),
            (DuckDB, "SELECT COLUMNS(*) FROM (SELECT 1 a, 2 b) ORDER BY 2"),
            (PostgreSQL, "SELECT SUM(INTERVAL '1 day'), AVG(INTERVAL '1 day')"),
            (DuckDB, "SELECT AVG(INTERVAL '1 day')"),
            (DuckDB, "SELECT AVG(INTERVAL '1 day') + INTERVAL '1 day'"),
            (PostgreSQL, "SELECT SUM(INTERVAL '1 day') + INTERVAL '1 day', AVG(INTERVAL '1 day') + INTERVAL '1 day'"),
            (DuckDB, "SELECT 1 LIMIT -(-1)"),
            (DuckDB, "SELECT 1 LIMIT -0"),
            (Generic, "SELECT TIMESTAMP '2024-01-01 00:00:00' + 1"),
            (DuckDB, "SELECT CAST('1' AS VARCHAR) = 1"),
            (DuckDB, "SELECT INTERVAL '1 day' * 2, INTERVAL '1 day' / 2"),
            (PostgreSQL, "SELECT INTERVAL '1 day' * 2"),
            (BigQuery, "SELECT SUBSTR(b'abc', 1, 2)"),
            (PostgreSQL, "SELECT substring('abc'::bytea, 1, 2)"),
            (DuckDB, "SELECT round(1.25, '1')"),
            (DuckDB, "INSERT INTO orders(i,s) (SELECT i,s FROM orders)"),
            (DuckDB, "SELECT i * '1', i / '2', i % '2', i - '2' FROM orders"),
            (PostgreSQL, "SELECT i + '1', i * '2' FROM orders"),
        ] {
            let result = validate_with_schema(sql, dialect, &schema, &options);
            assert!(result.valid, "{dialect:?}, types={check_types}, {sql}: {:?}", result.errors);
        }
    }
    assert_eq!(canonical_type_family("BIGNUMERIC"), TypeFamily::Numeric);
    assert_eq!(canonical_type_family("NUMBER(38,0)"), TypeFamily::Integer);
    let options = SchemaValidationOptions {
        semantic: true,
        check_types: true,
        ..Default::default()
    };
    for (dialect, sql, code) in [
        (DuckDB, "SELECT ts > 5 FROM orders", "E217"),
        (DuckDB, "SELECT 1 LIMIT -1", "E234"),
        (PostgreSQL, "SELECT (1, 2) = (SELECT 1, 2, 3)", "E216"),
        (DuckDB, "SELECT SUM(INTERVAL '1 day')", "E213"),
        (
            DuckDB,
            "INSERT INTO orders(i) SELECT i,s FROM orders",
            "E214",
        ),
    ] {
        let result = validate_with_schema(sql, dialect, &schema, &options);
        assert!(
            !result.valid && result.errors.iter().any(|e| e.code == code),
            "{sql}: {:?}",
            result.errors
        );
    }
}

#[test]
fn review_runtime_conversions_only_warn() {
    let options = SchemaValidationOptions {
        semantic: true,
        check_types: true,
        ..Default::default()
    };
    for sql in [
        "SELECT CAST('1' AS VARCHAR) = 1",
        "SELECT i = s FROM orders",
        "SELECT i IN ('a','b') FROM orders",
        "SELECT NULLIF(i,s) FROM orders",
        "SELECT LAG(i,s) OVER () FROM orders",
        "SELECT NTILE(s) OVER () FROM orders",
        "SELECT ts = 'invalid' FROM orders",
        "SELECT CAST(ts AS INTEGER) FROM orders",
        "SELECT i FROM orders WHERE s",
        "SELECT DATE 'invalid'",
        "SELECT i FROM orders UNION ALL SELECT ts FROM orders",
        "SELECT i FROM orders WHERE ts",
        "SELECT NOT ts FROM orders",
        "SELECT ts = i FROM orders",
        "INSERT INTO orders(i) SELECT s FROM orders",
        "UPDATE orders SET i = s",
        "SELECT round(1.25, 'invalid')",
        "SELECT i * 'invalid' FROM orders",
    ] {
        let result =
            validate_with_schema(sql, DialectType::DuckDB, &semantic_type_schema(), &options);
        assert!(
            result.valid && result.errors.iter().any(|e| e.code.starts_with("W21")),
            "{sql}: {:?}",
            result.errors
        );
    }
    let result = validate_with_schema(
        "SELECT i = s FROM orders",
        DialectType::Snowflake,
        &semantic_type_schema(),
        &options,
    );
    assert!(result.valid && result.errors.iter().any(|e| e.code == "W210"));
}

#[test]
fn review_builtin_and_unknown_cast_targets_do_not_error() {
    for (dialect, types) in [
        (
            DialectType::Snowflake,
            &[
                "VARIANT",
                "OBJECT",
                "ARRAY",
                "GEOGRAPHY",
                "NUMBER(38,0)",
                "TIMESTAMP_NTZ",
                "TIMESTAMP_LTZ",
                "TIMESTAMP_TZ",
            ][..],
        ),
        (
            DialectType::BigQuery,
            &[
                "INT64",
                "FLOAT64",
                "NUMERIC",
                "BIGNUMERIC",
                "STRING",
                "BYTES",
                "JSON",
                "GEOGRAPHY",
                "STRUCT<id INT64>",
                "ARRAY<INT64>",
            ][..],
        ),
        (
            DialectType::PostgreSQL,
            &[
                "TEXT",
                "BYTEA",
                "JSONB",
                "UUID",
                "TIMESTAMPTZ",
                "INTERVAL",
                "NUMERIC",
            ][..],
        ),
        (
            DialectType::DuckDB,
            &[
                "HUGEINT",
                "UHUGEINT",
                "UTINYINT",
                "USMALLINT",
                "UINTEGER",
                "UBIGINT",
                "VARINT",
                "UUID",
                "JSON",
                "INTEGER[]",
                "STRUCT(id INTEGER)",
                "MAP(INTEGER, VARCHAR)",
                "UNION(id INTEGER, label VARCHAR)",
                "BLOB",
                "NOT_A_TYPE",
            ][..],
        ),
    ] {
        for ty in types {
            let result = validate_with_schema(
                &format!("SELECT CAST(NULL AS {ty})"),
                dialect,
                &semantic_type_schema(),
                &SchemaValidationOptions {
                    check_types: true,
                    ..Default::default()
                },
            );
            assert!(result.valid, "{dialect:?}: {ty}: {:?}", result.errors);
        }
    }
}

fn semantic_type_schema() -> ValidationSchema {
    serde_json::from_value(serde_json::json!({"tables":[{"name":"orders","columns":[
        {"name":"i","type":"INTEGER"}, {"name":"s","type":"VARCHAR"},
        {"name":"ts","type":"TIMESTAMP"}, {"name":"d","type":"DATE"},
        {"name":"b","type":"BOOLEAN"}
    ]}]}))
    .unwrap()
}

#[test]
fn semantic_type_literal_coercion_and_controls() {
    let schema = semantic_type_schema();
    let options = SchemaValidationOptions {
        check_types: true,
        ..Default::default()
    };
    for dialect in [
        DialectType::DuckDB,
        DialectType::PostgreSQL,
        DialectType::Snowflake,
        DialectType::BigQuery,
    ] {
        for sql in [
            "SELECT ts > '2026-01-01' FROM orders",
            "SELECT d = '2026-01-01' FROM orders",
            "SELECT ts BETWEEN '2026-01-01' AND '2026-12-31' FROM orders",
            "SELECT d IN ('2026-01-01') FROM orders",
            "SELECT COALESCE(ts, '2026-01-01') FROM orders",
            "SELECT CASE WHEN b THEN ts ELSE '2026-01-01' END FROM orders",
        ] {
            let result = validate_with_schema(sql, dialect, &schema, &options);
            assert!(result.valid, "{dialect:?}: {sql}: {:?}", result.errors);
        }
    }
    for sql in [
        "SELECT i = '5' FROM orders",
        "SELECT ts FROM orders UNION ALL SELECT s FROM orders",
        "SELECT i FROM orders UNION ALL SELECT b FROM orders",
        "SELECT i FROM orders WHERE i",
        "SELECT NOT i FROM orders",
    ] {
        let result = validate_with_schema(sql, DialectType::DuckDB, &schema, &options);
        assert!(result.valid, "{sql}: {:?}", result.errors);
    }
    for (sql, code) in [
        ("SELECT ts > 5 FROM orders", "E217"),
        ("SELECT i * s FROM orders", "E212"),
        ("SELECT i - ts FROM orders", "E212"),
        ("SELECT SUM(s) FROM orders", "E213"),
        ("SELECT SUM(s) OVER () FROM orders", "E213"),
        ("SELECT COALESCE(i, ts) FROM orders", "E213"),
        ("SELECT CASE WHEN s THEN i ELSE 0 END FROM orders", "W215"),
        ("SELECT -s FROM orders", "E212"),
        ("SELECT CAST(ts AS BOOLEAN) FROM orders", "W213"),
        ("SELECT i IN (SELECT i, s FROM orders) FROM orders", "E216"),
        ("SELECT not_a_function(i) FROM orders", "E202"),
        ("SELECT COALESCE() FROM orders", "E203"),
    ] {
        let result = validate_with_schema(sql, DialectType::DuckDB, &schema, &options);
        assert!(
            result.errors.iter().any(|e| e.code == code),
            "{sql}: {:?}",
            result.errors
        );
    }
}

#[test]
fn semantic_type_registered_functions_are_allowed() {
    let options: SchemaValidationOptions = serde_json::from_value(
        serde_json::json!({"check_types":true,"known_functions":["order_total"]}),
    )
    .unwrap();
    let result = validate_with_schema(
        "SELECT order_total(i) FROM orders",
        DialectType::DuckDB,
        &semantic_type_schema(),
        &options,
    );
    assert!(result.valid, "{:?}", result.errors);
    let result = validate_with_schema(
        "SELECT order_total(s) + 1 FROM orders",
        DialectType::DuckDB,
        &semantic_type_schema(),
        &options,
    );
    assert!(result.valid, "{:?}", result.errors);
    let options: SchemaValidationOptions = serde_json::from_value(
        serde_json::json!({"check_types":true,"known_types":["order_state"]}),
    )
    .unwrap();
    assert!(
        validate_with_schema(
            "SELECT CAST(s AS order_state) FROM orders",
            DialectType::DuckDB,
            &semantic_type_schema(),
            &options
        )
        .valid
    );
}

#[test]
fn semantic_type_dialect_boundaries() {
    let schema = semantic_type_schema();
    let options = SchemaValidationOptions {
        check_types: true,
        ..Default::default()
    };
    for sql in [
        "SELECT i = s FROM orders",
        "SELECT COALESCE(i,s) FROM orders",
        "SELECT UPPER(i) FROM orders",
        "SELECT ABS(s) FROM orders",
    ] {
        let result = validate_with_schema(sql, DialectType::Snowflake, &schema, &options);
        assert!(result.valid, "{sql}: {:?}", result.errors);
    }
    for sql in [
        "SELECT i FROM orders WHERE i",
        "SELECT i FROM orders WHERE s",
        "SELECT ts > 5 FROM orders",
    ] {
        let result = validate_with_schema(sql, DialectType::Snowflake, &schema, &options);
        assert!(
            !result.valid,
            "Snowflake binder rejection {sql}: {:?}",
            result.errors
        );
    }
    for dialect in [
        DialectType::PostgreSQL,
        DialectType::BigQuery,
        DialectType::Snowflake,
    ] {
        for sql in [
            "SELECT i FROM orders WHERE i",
            "SELECT i FROM orders UNION ALL SELECT b FROM orders",
        ] {
            assert!(
                !validate_with_schema(sql, dialect, &schema, &options).valid,
                "{dialect}: {sql}"
            );
        }
    }
    assert!(
        !validate_with_schema(
            "SELECT i = '5' FROM orders",
            DialectType::BigQuery,
            &schema,
            &options
        )
        .valid
    );
    assert!(
        validate_with_schema(
            "SELECT i = '5' FROM orders",
            DialectType::PostgreSQL,
            &schema,
            &options
        )
        .valid
    );
    for sql in [
        "SELECT NOT 'true'",
        "SELECT i FROM orders WHERE 'false'",
        "SELECT CASE WHEN 'true' THEN 1 ELSE 0 END",
        "SELECT AVG(ts) > TIMESTAMP '2026-01-01' FROM orders",
    ] {
        let result = validate_with_schema(sql, DialectType::DuckDB, &schema, &options);
        assert!(result.valid, "{sql}: {:?}", result.errors);
    }
}

#[test]
fn semantic_type_structure_and_negative_controls() {
    let schema = semantic_type_schema();
    let options = SchemaValidationOptions {
        semantic: true,
        check_types: true,
        ..Default::default()
    };
    for (sql, code) in [
        ("WITH q AS (SELECT * FROM orders) SELECT missing FROM q", "E201"),
        ("SELECT missing FROM (SELECT * FROM orders) q", "E201"),
        ("SELECT * EXCLUDE (missing) FROM orders", "E201"),
        ("SELECT missing.* FROM orders", "E200"),
        ("SELECT a.i FROM orders a JOIN orders a ON true", "E233"),
        ("WITH q AS (SELECT 1), q AS (SELECT 2) SELECT * FROM q", "E233"),
        ("SELECT a.i FROM orders a JOIN orders b ON a.i = c.i JOIN orders c ON true", "E200"),
        ("SELECT ROW_NUMBER() FROM orders", "E232"),
        ("SELECT SUM(i) OVER absent FROM orders", "E232"),
        ("SELECT i FROM orders QUALIFY i > 0", "E232"),
        ("SELECT i FROM orders LIMIT -1", "E234"),
        ("SELECT i FROM orders LIMIT i", "E234"),
        ("SELECT (SELECT i, s FROM orders) FROM orders", "E216"),
        ("SELECT (SELECT i, s FROM orders) AS total FROM orders", "E216"),
        ("SELECT DATE '2026-02-30'", "W213"),
        ("SELECT TIMESTAMP 'invalid'", "W213"),
        ("SELECT TIMESTAMP '2026-01-01 99:99:99'", "W213"),
        ("SELECT INTERVAL 'invalid'", "W213"),
        ("SELECT SUBSTRING(s) FROM orders", "E203"),
        ("SELECT DATE_TRUNC('day') FROM orders", "E203"),
        ("SELECT SUM(i, i) FROM orders", "E203"),
        ("SELECT DATE_TRUNC('day', s) FROM orders", "E213"),
        ("SELECT LAG(i, s) OVER () FROM orders", "W216"),
        ("SELECT NTILE(s) OVER () FROM orders", "W216"),
        ("SELECT i FROM orders ORDER BY 9", "E201"),
        ("SELECT i FROM orders UNION ALL SELECT i FROM orders ORDER BY s", "E201"),
        ("SELECT COLUMNS('missing') FROM orders", "E201"),
        ("SELECT a.i FROM orders a JOIN orders b USING (missing)", "E201"),
        ("SELECT CAST(i AS unknown_type) FROM orders", "W213"),
        ("SELECT * FROM (SELECT i FROM orders) a JOIN (SELECT s AS i FROM orders) b USING (i)", "W210"),
        ("SELECT STRING_AGG(i, 5) FROM orders", "E213"),
        ("SELECT i IN ('invalid') FROM orders", "W213"),
        ("SELECT ts = 'invalid' FROM orders", "W213"),
        ("SELECT i BETWEEN 'invalid' AND 'other' FROM orders", "W213"),
        ("WITH RECURSIVE q(n) AS (SELECT 1 UNION ALL SELECT 'invalid' FROM q WHERE n < 3) SELECT n FROM q", "W214"),
        ("SELECT SUM(i) OVER (ORDER BY s RANGE BETWEEN 1 PRECEDING AND CURRENT ROW) FROM orders", "E232"),
    ] {
        let result = validate_with_schema(sql, DialectType::DuckDB, &schema, &options);
        assert!(result.errors.iter().any(|e| e.code == code), "{sql}: {:?}", result.errors);
    }
    for sql in [
        "WITH q AS (SELECT * FROM orders) SELECT i FROM q",
        "SELECT i FROM (SELECT * FROM orders) q",
        "SELECT a.i FROM orders a JOIN orders b ON a.i=b.i",
        "SELECT ROW_NUMBER() OVER w FROM orders WINDOW w AS (ORDER BY i)",
        "SELECT i FROM orders QUALIFY ROW_NUMBER() OVER () = 1",
        "SELECT i FROM orders LIMIT 1.5", "SELECT i FROM orders LIMIT '2'",
        "SELECT EXISTS (SELECT i, s FROM orders) FROM orders",
        "SELECT * FROM (SELECT i, s FROM orders) q",
        "SELECT q.i FROM orders o, LATERAL (SELECT i, s FROM orders) q",
        "SELECT SUM(b) FROM orders", "SELECT AVG(ts) FROM orders",
        "SELECT COALESCE(i,b) FROM orders", "SELECT GREATEST(i,b) FROM orders",
        "SELECT DATE '2024-02-29'", "SELECT DATE 'infinity'",
        "SELECT TIMESTAMP '2026-01-01 24:00:00'",
        "SELECT COLUMNS('^[is]$') FROM orders",
        "SELECT a.i FROM orders a JOIN orders b USING (i)",
        "SELECT * FROM (SELECT i FROM orders) a JOIN (SELECT i FROM orders) b USING (i)",
        "SELECT STRING_AGG(i, ',') FROM orders",
        "SELECT i FROM orders UNION ALL SELECT i FROM orders ORDER BY i",
        "SELECT SUM(i) OVER (ORDER BY i RANGE BETWEEN 1 PRECEDING AND CURRENT ROW) FROM orders",
        "SELECT SUM(i) OVER (ORDER BY s RANGE BETWEEN UNBOUNDED PRECEDING AND CURRENT ROW) FROM orders",
        "SELECT STRFTIME('%Y', ts) FROM orders",
        "SELECT STRFTIME(ts, '%Y') FROM orders",
        "SELECT CASE WHEN true THEN 1 ELSE 'unused' END",
    ] {
        let result = validate_with_schema(sql, DialectType::DuckDB, &schema, &options);
        assert!(result.valid, "{sql}: {:?}", result.errors);
    }
}

#[test]
fn cte_resolution_views_preserve_alias_types_and_union_error_locations() {
    let schema = review_schema();
    let options = SchemaValidationOptions {
        check_types: true,
        check_references: true,
        ..Default::default()
    };
    let valid = "WITH base AS (SELECT quantity FROM items) SELECT quantity AS total, total + 1 AS next_total FROM base";
    assert!(validate_with_schema(valid, DialectType::Snowflake, &schema, &options).valid);
    let invalid = "WITH base AS (SELECT active FROM items) SELECT active AS flag, flag + 1 AS invalid_total FROM base";
    let result = validate_with_schema(invalid, DialectType::Snowflake, &schema, &options);
    assert!(!result.valid);
    assert!(result.errors.iter().any(|error| error.code == "E212"));

    let mut branches = vec!["SELECT quantity FROM base"; 128];
    branches[17] = "SELECT missing_first FROM base";
    branches[119] = "SELECT missing_last FROM base";
    let sql = format!(
        "WITH base AS (SELECT quantity FROM items) {}",
        branches.join(" UNION ALL ")
    );
    let result = validate_with_schema(
        &sql,
        DialectType::Snowflake,
        &schema,
        &SchemaValidationOptions {
            check_types: false,
            check_references: true,
            ..Default::default()
        },
    );
    let errors: Vec<_> = result
        .errors
        .iter()
        .filter(|error| error.code == "E201")
        .collect();
    assert_eq!(errors.len(), 2, "{:?}", result.errors);
    assert!(errors[0].message.contains("missing_first"));
    assert!(errors[1].message.contains("missing_last"));
    assert_eq!(
        errors[0].column,
        Some(sql.find("missing_first").unwrap() + "missing_first".len() + 1)
    );
    assert_eq!(
        errors[1].column,
        Some(sql.find("missing_last").unwrap() + "missing_last".len() + 1)
    );
}

fn review_schema() -> ValidationSchema {
    serde_json::from_value(serde_json::json!({"tables": [
        {"name": "items", "columns": [{"name":"quantity","type":"INTEGER"},{"name":"active","type":"BOOLEAN"}]},
        {"name": "other", "columns": [{"name":"quantity","type":"BOOLEAN"},{"name":"active","type":"INTEGER"}]}
    ]})).unwrap()
}

#[test]
fn review_scoped_type_validation() {
    let schema = review_schema();
    let options = SchemaValidationOptions {
        check_types: true,
        check_references: true,
        ..Default::default()
    };
    for sql in [
        "SELECT active + 1 FROM items",
        "WITH q AS (SELECT active AS flag FROM items) SELECT flag + 1 FROM q",
        "SELECT flag + 1 FROM (SELECT active AS flag FROM items) q",
        "SELECT quantity FROM (SELECT active AS flag, quantity FROM items) q WHERE flag + 1 > 0",
        "SELECT (SELECT t.quantity + 1 FROM other t) FROM items t",
        "WITH q AS (SELECT true AS \"Camel\") SELECT \"Camel\" + 1 FROM q",
        "WITH \"Query\" AS (SELECT true AS \"Camel\") SELECT \"Query\".\"Camel\" + 1 FROM \"Query\"",
    ] {
        let result = validate_with_schema(sql, DialectType::PostgreSQL, &schema, &options);
        assert!(!result.valid && result.errors.iter().any(|e| e.code == "E212"), "{sql}: {:?}", result.errors);
    }
    for sql in [
        "WITH q AS (SELECT quantity AS n FROM items) SELECT n + 1 FROM q",
        "SELECT (SELECT t.quantity + 1 FROM items t) FROM other t",
    ] {
        let result = validate_with_schema(sql, DialectType::PostgreSQL, &schema, &options);
        assert!(result.valid, "{sql}: {:?}", result.errors);
    }
}

#[test]
fn review_dml_reference_checks_do_not_require_types() {
    let schema = review_schema();
    for check_types in [false, true] {
        for strict in [false, true] {
            let options = SchemaValidationOptions {
                check_types,
                check_references: true,
                strict: Some(strict),
                ..Default::default()
            };
            for (sql, code) in [
                ("UPDATE nonexistent SET x = 1", "E200"),
                ("DELETE FROM nonexistent", "E200"),
                ("INSERT INTO nonexistent(x) VALUES(1)", "E200"),
                ("UPDATE items SET quantity = missing", "E201"),
                ("UPDATE items SET missing = 1", "E201"),
                ("DELETE FROM items WHERE missing = 1", "E201"),
                ("INSERT INTO items(missing) VALUES(1)", "E201"),
            ] {
                let result = validate_with_schema(sql, DialectType::PostgreSQL, &schema, &options);
                assert_eq!(result.valid, !strict, "{sql}: {:?}", result.errors);
                assert!(
                    result.errors.iter().any(|e| e.code == code),
                    "{sql}: {:?}",
                    result.errors
                );
            }
            for sql in [
                "UPDATE items SET quantity = quantity + 1 WHERE active",
                "DELETE FROM items WHERE quantity = 1",
                "INSERT INTO items(quantity) VALUES(1)",
            ] {
                let result = validate_with_schema(sql, DialectType::PostgreSQL, &schema, &options);
                assert!(result.valid, "{sql}: {:?}", result.errors);
            }
        }
    }
}

#[test]
fn review_name_aligned_set_operation_validation() {
    let schema = review_schema();
    let options = SchemaValidationOptions {
        check_types: true,
        ..Default::default()
    };
    for dialect in [
        DialectType::Snowflake,
        DialectType::DuckDB,
        DialectType::BigQuery,
    ] {
        let sql = "SELECT quantity AS a, active AS b FROM items UNION ALL BY NAME SELECT active AS b, quantity AS a FROM items";
        let result = validate_with_schema(sql, dialect, &schema, &options);
        assert!(result.valid, "{dialect}: {:?}", result.errors);
        let result = validate_with_schema("SELECT quantity AS a FROM items UNION ALL BY NAME SELECT quantity AS a, active AS b FROM items", dialect, &schema, &options);
        assert_eq!(
            result.valid,
            dialect != DialectType::BigQuery,
            "{dialect}: {:?}",
            result.errors
        );
    }
    for sql in [
        "SELECT * FROM items UNION ALL BY NAME SELECT active, quantity FROM items",
        "(SELECT quantity AS a FROM items UNION ALL BY NAME SELECT active AS b FROM items) UNION ALL BY NAME SELECT quantity AS a, active AS b FROM items",
    ] {
        let result = validate_with_schema(sql, DialectType::DuckDB, &schema, &options);
        assert!(result.valid, "{sql}: {:?}", result.errors);
    }
    assert!(
        validate_with_schema(
            "SELECT * FROM items UNION ALL BY NAME SELECT * FROM other",
            DialectType::DuckDB,
            &schema,
            &options
        )
        .valid
    );
}

#[test]
fn review_dml_clause_boundaries() {
    let schema = review_schema();
    let captured = validate_with_schema(
        "UPDATE items AS x SET quantity = TRANSFORM(ARRAY_CONSTRUCT(quantity), x -> x.missing)",
        DialectType::Snowflake,
        &schema,
        &SchemaValidationOptions::default(),
    );
    assert!(
        captured.errors.iter().any(|e| e.code == "E201"),
        "{:?}",
        captured.errors
    );
    for check_types in [false, true] {
        let options = SchemaValidationOptions {
            check_types,
            check_references: true,
            ..Default::default()
        };
        for (dialect, sql, valid) in [
            (DialectType::PostgreSQL, "INSERT INTO items(quantity) VALUES(quantity)", false),
            (DialectType::PostgreSQL, "INSERT INTO items(quantity) VALUES(1) ON CONFLICT(quantity) DO UPDATE SET quantity = excluded.quantity + quantity", true),
            (DialectType::PostgreSQL, "INSERT INTO items(quantity) VALUES(1) RETURNING excluded.quantity", false),
            (DialectType::TSQL, "UPDATE items SET quantity = quantity + 1 OUTPUT inserted.quantity", true),
            (DialectType::TSQL, "UPDATE items SET quantity = inserted.quantity OUTPUT inserted.quantity", false),
            (DialectType::TSQL, "DELETE FROM items OUTPUT deleted.quantity", true),
            (DialectType::TSQL, "DELETE FROM items OUTPUT inserted.quantity", false),
            (DialectType::PostgreSQL, "WITH q AS(SELECT 1 AS n) INSERT INTO items(quantity) SELECT n FROM q", true),
            (DialectType::Snowflake, "MERGE INTO items t USING other s ON t.quantity=s.active WHEN MATCHED THEN UPDATE SET quantity=s.active", true),
            (DialectType::Snowflake, "MERGE INTO items t USING other s ON t.quantity=s.active WHEN MATCHED THEN UPDATE SET missing=1", false),
            (DialectType::Snowflake, "MERGE INTO items t USING other s ON t.quantity=s.active WHEN NOT MATCHED THEN INSERT (missing) VALUES(s.active)", false),
            (DialectType::PostgreSQL, "CREATE TABLE new_items AS SELECT quantity FROM items", true),
        ] {
            let result = validate_with_schema(sql, dialect, &schema, &options);
            assert_eq!(result.valid, valid, "{sql}, types={check_types}: {:?}", result.errors);
        }
    }
    let options = SchemaValidationOptions {
        check_types: true,
        ..Default::default()
    };
    for sql in [
        "UPDATE items SET quantity=1 WHERE quantity",
        "DELETE FROM items WHERE quantity",
    ] {
        assert!(
            !validate_with_schema(sql, DialectType::PostgreSQL, &schema, &options).valid,
            "{sql}"
        );
        assert!(
            validate_with_schema(sql, DialectType::MySQL, &schema, &options).valid,
            "{sql}"
        );
    }
}

#[test]
fn review_grouping_aliases_and_windows() {
    let options = SchemaValidationOptions {
        semantic: true,
        strict: Some(false),
        ..Default::default()
    };
    for (sql, valid) in [
        ("SELECT quantity AS a, SUM(active) FROM items GROUP BY quantity", true),
        ("SELECT quantity + 1 AS a FROM items GROUP BY a", true),
        ("SELECT SUM(quantity) AS a FROM items WHERE a > 0", false),
        ("SELECT SUM(quantity) AS a, SUM(a) FROM items", false),
        ("SELECT quantity AS a, a, SUM(quantity) FROM items", false),
        ("SELECT i.quantity, SUM(o.active) FROM items i JOIN other o ON TRUE GROUP BY o.quantity", false),
        ("SELECT i.quantity, SUM(o.active) FROM items i JOIN other o ON TRUE GROUP BY i.quantity", true),
        ("SELECT SUM(quantity) FROM items HAVING active", false),
        ("SELECT SUM(quantity) FROM items ORDER BY active", false),
        ("SELECT SUM(quantity) AS total FROM items HAVING total > 0 ORDER BY total", true),
        ("SELECT quantity, SUM(quantity) OVER (PARTITION BY active) FROM items GROUP BY quantity", false),
        ("SELECT SUM(quantity) FROM items GROUP BY 1", false),
        ("SELECT quantity, SUM(quantity) OVER() FROM items", true),
    ] {
        let result = validate_with_schema(sql, DialectType::Snowflake, &review_schema(), &options);
        assert_eq!(result.valid, valid, "{sql}: {:?}", result.errors);
    }
}

#[test]
fn review_dialect_aliases_and_quoting() {
    let schema = review_schema();
    for dialect in [DialectType::Snowflake, DialectType::DuckDB] {
        for check_types in [false, true] {
            let options = SchemaValidationOptions {
                check_types,
                check_references: true,
                ..Default::default()
            };
            for sql in [
                "SELECT quantity + 1 AS a, a * 2 AS b FROM items",
                "SELECT quantity + 1 AS a FROM items WHERE a > 0",
                "SELECT quantity + 1 AS a FROM items GROUP BY a",
                "SELECT SUM(quantity) AS a FROM items HAVING a > 0",
                "SELECT ROW_NUMBER() OVER(ORDER BY quantity) AS rn FROM items QUALIFY rn = 1",
            ] {
                let result = validate_with_schema(sql, dialect, &schema, &options);
                assert!(result.valid, "{dialect}: {sql}: {:?}", result.errors);
            }
        }
    }
    for (name, valid) in [("Camel", true), ("camel", false), ("CAMEL", false)] {
        let sql = format!("WITH q AS (SELECT 1 AS \"Camel\") SELECT \"{name}\" FROM q");
        let result = validate_with_schema(
            &sql,
            DialectType::PostgreSQL,
            &schema,
            &SchemaValidationOptions::default(),
        );
        assert_eq!(result.valid, valid, "{sql}: {:?}", result.errors);
    }
    let options = SchemaValidationOptions {
        check_types: true,
        ..Default::default()
    };
    assert!(
        validate_with_schema(
            "SELECT quantity FROM items WHERE quantity",
            DialectType::MySQL,
            &schema,
            &options
        )
        .valid
    );
    assert!(
        !validate_with_schema(
            "SELECT quantity FROM items WHERE quantity",
            DialectType::PostgreSQL,
            &schema,
            &options
        )
        .valid
    );
}

#[test]
fn review_semantic_errors_are_scope_local_and_opt_in() {
    for (sql, code) in [
        ("SELECT quantity AS n, SUM(active) FROM items", "E230"),
        ("SELECT quantity FROM items WHERE SUM(quantity)>0", "E231"),
        ("SELECT SUM(SUM(quantity)) FROM items", "E231"),
        (
            "SELECT SUM(SUM(quantity)) OVER(), quantity FROM items",
            "E230",
        ),
        (
            "SELECT quantity AS quantity, SUM(quantity) FROM items",
            "E230",
        ),
        (
            "SELECT quantity FROM items WHERE ROW_NUMBER() OVER(ORDER BY quantity)=1",
            "E232",
        ),
    ] {
        assert!(crate::validate(sql, DialectType::Snowflake).valid);
        let result = crate::validate_with_options(
            sql,
            DialectType::Snowflake,
            &crate::ValidationOptions {
                semantic: true,
                ..Default::default()
            },
        );
        assert!(
            !result.valid && result.errors.iter().any(|e| e.code == code),
            "{sql}: {:?}",
            result.errors
        );
    }
    for sql in [
        "SELECT SUM(quantity) OVER(), quantity FROM items",
        "SELECT quantity, (SELECT SUM(quantity) FROM other) FROM items",
        "SELECT quantity, SUM(SUM(quantity)) OVER() FROM items GROUP BY quantity",
        "SELECT quantity + 1 AS a FROM items GROUP BY a",
        "SELECT SUM(quantity) AS a FROM items HAVING a > 0",
        "SELECT TRANSFORM(ARRAY_CONSTRUCT(1), x -> x + 1), COUNT(*) FROM items",
        "WITH q AS (SELECT quantity FROM items LIMIT 1) SELECT quantity FROM q",
    ] {
        let result = crate::validate_with_options(
            sql,
            DialectType::Snowflake,
            &crate::ValidationOptions {
                semantic: true,
                ..Default::default()
            },
        );
        assert!(result.valid, "{sql}: {:?}", result.errors);
    }
}

#[test]
fn review_strict_configuration_and_catalog_specs() {
    assert!(serde_json::from_value::<crate::AnalyzeQueryOptions>(
        serde_json::json!({"scheam": {"tables": []}})
    )
    .is_err());
    assert!(serde_json::from_value::<ValidationSchema>(
        serde_json::json!({"tables":[{"name":"t","columns":[{"name":"x","dataType":"INT"}]}]})
    )
    .is_err());
    use crate::function_catalog::FunctionCatalogSpec;
    let spec: FunctionCatalogSpec = serde_json::from_value(serde_json::json!({"functions":[{"name":"Foo","nameCase":"sensitive","signatures":[{"minArity":1,"maxArity":2}]}]})).unwrap();
    let options = SchemaValidationOptions {
        check_types: true,
        function_catalog: Some(Arc::new(spec.build(DialectType::Generic).unwrap())),
        ..Default::default()
    };
    for (sql, valid) in [
        ("SELECT Foo(1)", true),
        ("SELECT Foo(1,2)", true),
        ("SELECT Foo(1,2,3)", false),
        ("SELECT FOO(1)", false),
        ("SELECT Missing(1)", false),
    ] {
        let result = validate_with_schema(sql, DialectType::Generic, &review_schema(), &options);
        assert_eq!(result.valid, valid, "{sql}: {:?}", result.errors);
    }
}

#[test]
fn test_schema_validation_options_json_names() {
    for json in [
        r#"{"check_types":true,"check_references":true,"strict_syntax":true,"strict":false}"#,
        r#"{"checkTypes":true,"checkReferences":true,"strictSyntax":true,"strict":false}"#,
    ] {
        let options: SchemaValidationOptions = serde_json::from_str(json).unwrap();
        assert!(options.check_types && options.check_references && options.strict_syntax);
        assert_eq!(options.strict, Some(false));
    }
    assert!(serde_json::from_str::<SchemaValidationOptions>(r#"{"checkType":true}"#).is_err());
}

#[test]
fn test_schema_validation_lexical_scopes() {
    let schema = base_schema();
    for check_references in [false, true] {
        let options = SchemaValidationOptions {
            check_references,
            ..Default::default()
        };
        for sql in [
            "WITH a AS (SELECT id AS k FROM users), b AS (SELECT k FROM a) SELECT k FROM b",
            "WITH a(k) AS (SELECT id FROM users), b AS (SELECT k FROM a) SELECT b.k FROM b",
            "SELECT u.id FROM users u WHERE EXISTS (SELECT 1 FROM orders o WHERE o.user_id = u.id)",
            "SELECT u.id FROM users u WHERE EXISTS (SELECT 1 FROM orders o WHERE o.user_id = age)",
            "SELECT u.id FROM users u WHERE EXISTS (SELECT 1 FROM orders o WHERE EXISTS (SELECT 1 WHERE u.id = o.user_id))",
            "SELECT u.id FROM users u WHERE EXISTS (SELECT u.id FROM orders u)",
            "WITH q AS (SELECT id FROM users) SELECT q.id FROM q WHERE EXISTS (WITH q AS (SELECT total FROM orders) SELECT total FROM q)",
            "WITH unused AS (SELECT id FROM orders) SELECT id FROM users",
            "SELECT q.id FROM (SELECT id FROM users) q",
            "SELECT u.id FROM users u WHERE EXISTS (SELECT u.id UNION ALL SELECT u.id)",
            "SELECT u.id FROM users u JOIN orders o ON EXISTS (SELECT 1 WHERE o.user_id = u.id)",
        ] {
            let result = validate_with_schema(sql, DialectType::Snowflake, &schema, &options);
            assert!(result.valid, "{sql}: {:?}", result.errors);
        }
        for (sql, code) in [
            ("SELECT id FROM q WHERE EXISTS (WITH q AS (SELECT id FROM users) SELECT id FROM q)", "E200"),
            ("WITH q AS (SELECT id FROM users) SELECT q.id FROM users", "E222"),
            ("WITH q AS (SELECT id FROM users) SELECT age FROM q", "E201"),
            ("SELECT u.id FROM users u WHERE EXISTS (SELECT u.age FROM orders u)", "E201"),
            ("SELECT u.id FROM users u JOIN (SELECT u.id) q ON TRUE", "E222"),
            ("SELECT u.id FROM users u WHERE EXISTS (SELECT 1 FROM orders o WHERE u.missing = o.id)", "E201"),
            ("WITH q AS (SELECT id FROM users) SELECT id FROM public.q", "E200"),
        ] {
            let result = validate_with_schema(sql, DialectType::Snowflake, &schema, &options);
            assert!(!result.valid && result.errors.iter().any(|error| error.code == code), "{sql}: {:?}", result.errors);
        }
    }
}

#[test]
fn test_schema_validation_issue_441_nested_scopes() {
    let schema = ValidationSchema {
        tables: ["t1", "t2"]
            .into_iter()
            .map(|name| SchemaTable {
                name: name.to_string(),
                schema: None,
                columns: vec![SchemaColumn {
                    name: "id".to_string(),
                    data_type: "NUMBER".to_string(),
                    nullable: None,
                    primary_key: false,
                    unique: false,
                    references: None,
                }],
                aliases: vec![],
                primary_key: vec![],
                unique_keys: vec![],
                foreign_keys: vec![],
            })
            .collect(),
        strict: Some(true),
    };
    let options = SchemaValidationOptions {
        check_types: false,
        check_references: true,
        strict: Some(true),
        semantic: false,
        strict_syntax: false,
        ..Default::default()
    };

    for (case, sql) in [
        (
            "qualified_cte_column_is_not_ambiguous",
            "WITH a AS (SELECT id FROM t1), b AS (SELECT id FROM t2) \
             SELECT a.id FROM a JOIN b ON a.id = b.id",
        ),
        (
            "correlated_subquery_resolves_outer_alias",
            "SELECT outer_table.id FROM t1 outer_table \
             WHERE NOT EXISTS ( \
               SELECT 1 FROM t2 inner_table \
               WHERE inner_table.id = outer_table.id \
             )",
        ),
    ] {
        let result = validate_with_schema(sql, DialectType::Snowflake, &schema, &options);
        assert!(result.valid, "{case}: {:#?}", result.errors);
        assert!(result.errors.is_empty(), "{case}: {:#?}", result.errors);
    }
}

#[test]
fn test_schema_validation_issue_442_prior_cte_outputs() {
    let schema = ValidationSchema {
        tables: vec![SchemaTable {
            name: "t1".to_string(),
            schema: None,
            columns: ["id", "value"]
                .into_iter()
                .map(|name| SchemaColumn {
                    name: name.to_string(),
                    data_type: "NUMBER".to_string(),
                    nullable: None,
                    primary_key: false,
                    unique: false,
                    references: None,
                })
                .collect(),
            aliases: vec![],
            primary_key: vec![],
            unique_keys: vec![],
            foreign_keys: vec![],
        }],
        strict: Some(true),
    };
    let options = SchemaValidationOptions {
        check_types: false,
        check_references: true,
        strict: Some(true),
        semantic: false,
        strict_syntax: false,
        ..Default::default()
    };

    for (case, sql) in [
        (
            "subsequent_cte_resolves_prior_cte_projection",
            "WITH derived AS (SELECT value AS derived_value FROM t1), \
             next AS (SELECT derived_value FROM derived) \
             SELECT derived_value FROM next",
        ),
        (
            "window_order_by_resolves_prior_cte_projection",
            "WITH scored AS (SELECT id, value AS info_score FROM t1), \
             ranked AS ( \
               SELECT id, ROW_NUMBER() OVER ( \
                 PARTITION BY id ORDER BY info_score DESC \
               ) AS rn \
               FROM scored \
             ) \
             SELECT id FROM ranked",
        ),
    ] {
        let result = validate_with_schema(sql, DialectType::Snowflake, &schema, &options);
        assert!(result.valid, "{case}: {:#?}", result.errors);
        assert!(result.errors.is_empty(), "{case}: {:#?}", result.errors);
    }
}

#[test]
fn test_schema_validation_snowflake_projection_aliases() {
    let schema = projection_alias_schema();
    for check_references in [false, true] {
        for check_types in [false, true] {
            let options = SchemaValidationOptions {
                check_references,
                check_types,
                ..Default::default()
            };
            for sql in [
                // Exact issue #460 example.
                "SELECT quantity + 1 AS adjusted_quantity, adjusted_quantity * 2 AS doubled_quantity FROM items",
                "SELECT quantity + 1 AS a, a * 2 AS b, b + a AS c FROM items",
                "SELECT 1 AS a, a + 1 AS b",
                "SELECT quantity + 1 AS quantity, quantity * 2 AS doubled FROM items",
                "SELECT quantity AS Adjusted, ADJUSTED + adjusted AS doubled FROM items",
                "SELECT quantity AS \"Adjusted\", \"Adjusted\" + 1 AS doubled FROM items",
                "SELECT quantity AS adjusted, \"ADJUSTED\" + 1 AS doubled FROM items",
                "SELECT quantity AS \"数量\", \"数量\" + 1 AS doubled FROM items",
                "SELECT quantity AS a, CASE WHEN a > 0 THEN COALESCE(a, 0) ELSE 0 END AS b FROM items",
                "SELECT SUM(quantity) AS a, a * 2 AS b FROM items",
                "SELECT ROW_NUMBER() OVER (ORDER BY quantity) AS a, a + 1 AS b FROM items",
                "SELECT quantity AS a, a + 1 AS b FROM items ORDER BY b, a + 1",
                "WITH q AS (SELECT quantity AS a, a + 1 AS b FROM items) SELECT b FROM q",
                "WITH q(n) AS (SELECT quantity FROM items) SELECT n AS a, a + 1 AS b FROM q",
                "SELECT q.b FROM (SELECT quantity AS a, a + 1 AS b FROM items) q",
                "SELECT quantity AS a, a + 1 AS b FROM items UNION ALL SELECT quantity AS a, a + 2 AS b FROM other",
                "SELECT i.quantity FROM items i WHERE EXISTS (SELECT o.quantity AS a, a + 1 AS b FROM other o WHERE o.quantity = i.quantity)",
                "SELECT quantity AS a, TRANSFORM(ARRAY_CONSTRUCT(quantity), x INT -> x + a) AS b FROM items",
                "SELECT quantity > 0 AS x, TRANSFORM(ARRAY_CONSTRUCT(quantity), x INT -> x + 1) AS b FROM items",
                "SELECT quantity AS a, TRANSFORM(ARRAY_CONSTRUCT(quantity), x INT -> TRANSFORM(ARRAY_CONSTRUCT(x), y INT -> y + a)) AS b FROM items",
                "SELECT OBJECT_CONSTRUCT('n', quantity) AS obj, obj:n AS n FROM items",
            ] {
                let result = validate_with_schema(sql, DialectType::Snowflake, &schema, &options);
                assert!(result.valid && result.errors.is_empty(), "{sql}, refs={check_references}, types={check_types}: {:?}", result.errors);
            }
        }
    }
}

#[test]
fn test_schema_validation_projection_alias_diagnostics_and_boundaries() {
    let schema = projection_alias_schema();
    for strict in [false, true] {
        let options = SchemaValidationOptions {
            check_references: true,
            strict: Some(strict),
            ..Default::default()
        };
        for (sql, code, token) in [
            ("SELECT missing AS a, a + 1 AS b FROM items", "E201", "missing"),
            ("SELECT a + 1 AS b, quantity AS a FROM items", "E201", "a"),
            ("SELECT a + 1 AS a FROM items", "E201", "a"),
            ("SELECT quantity AS a, items.a + 1 AS b FROM items", "E201", "a"),
            ("SELECT quantity AS a, absent.a + 1 AS b FROM items", "E222", "absent"),
            ("SELECT quantity AS \"Adjusted\", adjusted + 1 AS b FROM items", "E201", "adjusted"),
            ("SELECT quantity AS adjusted, \"adjusted\" + 1 AS b FROM items", "E201", "\"adjusted\""),
            ("SELECT quantity AS a, (SELECT a + 1) AS b FROM items", "E201", "a"),
            ("SELECT quantity AS a, quantity + 1 AS a, a * 2 AS b FROM items", "E201", "a"),
            ("SELECT i.quantity AS quantity, quantity + 1 AS b FROM items i JOIN other o ON i.quantity=o.quantity", "E221", "quantity"),
            ("SELECT quantity AS a, a + 1 AS b FROM items UNION ALL SELECT a FROM other", "E201", "a"),
            ("SELECT quantity AS a, TRANSFORM(ARRAY_CONSTRUCT(quantity), x -> x + missing) AS b FROM items", "E201", "missing"),
            ("SELECT '😀', quantity AS \"数量\", \"数量\" + míssing AS b FROM items", "E201", "míssing"),
        ] {
            let result = validate_with_schema(sql, DialectType::Snowflake, &schema, &options);
            assert_eq!(result.valid, !strict, "{sql}: {:?}", result.errors);
            assert_eq!(result.errors.len(), 1, "{sql}: {:?}", result.errors);
            let error = &result.errors[0];
            assert_eq!(error.code, if !strict && code == "E221" { "W222" } else { code });
            assert_eq!(error.severity, if strict { crate::ValidationSeverity::Error } else { crate::ValidationSeverity::Warning });
            let actual: String = sql.chars().skip(error.start.unwrap()).take(error.end.unwrap() - error.start.unwrap()).collect();
            assert_eq!(actual, token, "{sql}");
        }
        let result = validate_with_schema(
            "SELECT b + 1 AS a, a + 1 AS b FROM items",
            DialectType::Snowflake,
            &schema,
            &options,
        );
        assert_eq!(result.valid, !strict);
        assert_eq!(result.errors.len(), 1, "{:?}", result.errors);
        assert_eq!(result.errors[0].code, "E201");
    }
    for dialect in [
        DialectType::PostgreSQL,
        DialectType::BigQuery,
        DialectType::TSQL,
        DialectType::MySQL,
        DialectType::Oracle,
    ] {
        let result = validate_with_schema(
            "SELECT quantity AS a, a + 1 AS b FROM items",
            dialect,
            &schema,
            &SchemaValidationOptions::default(),
        );
        assert!(
            !result.valid,
            "out-of-scope dialect {dialect}: {:?}",
            result.errors
        );
        assert_eq!(result.errors[0].code, "E201");
    }
}

#[test]
fn test_schema_validation_projection_alias_types_and_input_precedence() {
    let mut schema = projection_alias_schema();
    schema.tables[0].columns.push(
        serde_json::from_value(serde_json::json!({"name": "active", "type": "BOOLEAN"})).unwrap(),
    );
    for strict in [false, true] {
        let options = SchemaValidationOptions {
            check_types: true,
            check_references: true,
            strict: Some(strict),
            ..Default::default()
        };
        for sql in [
            "SELECT active AS a, NOT a AS b FROM items",
            "SELECT quantity AS a, a + 1 AS b, b * 2 AS c FROM items",
            // The BOOLEAN input wins over the numeric alias.
            "SELECT quantity AS active, NOT active AS b FROM items",
            "SELECT active AS quantity, quantity + 1 AS b FROM items",
            "SELECT quantity AS \"active\", \"active\" + 1 AS b FROM items",
            "SELECT quantity AS \"active\", \"active\" + 1 AS b FROM (SELECT active, quantity FROM items) q",
            "SELECT quantity AS a, a + 1 AS b FROM items WHERE EXISTS (SELECT active AS a, NOT a AS b FROM items)",
        ] {
            let result = validate_with_schema(sql, DialectType::Snowflake, &schema, &options);
            assert!(result.valid && result.errors.is_empty(), "{sql}: {:?}", result.errors);
        }
        for sql in [
            "SELECT quantity > 0 AS a, a + 1 AS b FROM items",
            "SELECT active AS a, a AS b, b + 1 AS c FROM items",
            "SELECT quantity AS active, active + 1 AS b FROM items",
            "SELECT quantity AS \"ACTIVE\", \"ACTIVE\" + 1 AS b FROM items",
            "SELECT quantity AS \"active\", \"active\" + 1 AS b FROM (SELECT active AS \"active\", quantity FROM items) q",
            "SELECT active AS a, TRANSFORM(ARRAY_CONSTRUCT(quantity), x INT -> x + a) AS b FROM items",
            "WITH q AS (SELECT active AS a, a + 1 AS b FROM items) SELECT b FROM q",
            "WITH q AS (SELECT active AS flag FROM items), r AS (SELECT flag AS a, a + 1 AS b FROM q) SELECT b FROM r",
            "WITH q AS (SELECT active AS a, a AS b FROM items) SELECT b AS flag, flag + 1 AS n FROM q",
            "SELECT q.b AS flag, flag + 1 AS n FROM (SELECT active AS a, a AS b FROM items) q",
            "SELECT (SELECT b FROM (SELECT active AS a, a AS b FROM items) q LIMIT 1) AS flag, flag + 1 AS n",
            "SELECT q.b FROM (SELECT active AS a, a + 1 AS b FROM items) q",
        ] {
            let result = validate_with_schema(sql, DialectType::Snowflake, &schema, &options);
            assert_eq!(result.valid, !strict, "{sql}: {:?}", result.errors);
            assert_eq!(result.errors.len(), 1, "{sql}: {:?}", result.errors);
            assert_eq!(result.errors[0].code, if strict { "E212" } else { "W211" });
        }
    }
}

#[test]
fn test_schema_validation_projection_alias_chains_are_bounded() {
    let schema = projection_alias_schema();
    let mut sql = String::from("SELECT quantity AS a0");
    for i in 1..128 {
        sql.push_str(&format!(", a{} + a{} AS a{i}", i - 1, i - 1));
    }
    sql.push_str(" FROM items");
    let options = SchemaValidationOptions {
        check_types: true,
        check_references: true,
        ..Default::default()
    };
    let original = Dialect::get(DialectType::Snowflake)
        .parse(&sql)
        .unwrap()
        .remove(0);
    let scope = build_scope(&original);
    let mut selected = selected_validation_scope(&scope);
    let mut bindings = ProjectionAliasBindings::new();
    bind_scope_projection_aliases(
        &mut selected,
        &[],
        &build_resolver_schema(&schema),
        DialectType::Snowflake,
        true,
        &mut bindings,
    );
    assert_eq!(bindings.len(), 254);
    let bound = apply_projection_alias_bindings(original.clone(), &bindings);
    assert!(bound.dfs().count() < original.dfs().count() * 2);
    assert_eq!(
        original,
        Dialect::get(DialectType::Snowflake)
            .parse(&sql)
            .unwrap()
            .remove(0)
    );
    let result = validate_with_schema(&sql, DialectType::Snowflake, &schema, &options);
    assert!(
        result.valid && result.errors.is_empty(),
        "{:?}",
        result.errors
    );
    let options: SchemaValidationOptions =
        serde_json::from_value(serde_json::json!({"complexity_guard": {"maxInputBytes": 10}}))
            .unwrap();
    assert!(!validate_with_schema(&sql, DialectType::Snowflake, &schema, &options).valid);
}

fn projection_alias_schema() -> ValidationSchema {
    serde_json::from_value(serde_json::json!({"strict": true, "tables": [
        {"name": "items", "columns": [{"name": "quantity", "type": "NUMBER"}]},
        {"name": "other", "columns": [{"name": "quantity", "type": "NUMBER"}]}
    ]}))
    .unwrap()
}

#[test]
fn test_schema_validation_lambda_parameters() {
    let schema = lambda_validation_schema();
    for check_types in [false, true] {
        let options = SchemaValidationOptions {
            check_references: true,
            check_types,
            strict: Some(true),
            ..Default::default()
        };
        for sql in [
            // Exact issue #459 example.
            "SELECT TRANSFORM(ARRAY_CONSTRUCT(item_id), value -> value + 1) FROM items",
            "SELECT FILTER(ARRAY_CONSTRUCT(item_id), value -> value > 0) FROM items",
            "SELECT REDUCE(ARRAY_CONSTRUCT(item_id), 0, (acc, value) -> acc + value) FROM items",
            "SELECT TRANSFORM(ARRAY_CONSTRUCT(item_id), value INT -> value + 1) FROM items",
            "SELECT TRANSFORM(ARRAY_CONSTRUCT(item_id), value -> value + item_id) FROM items",
            "SELECT TRANSFORM(ARRAY_CONSTRUCT(item_id), value -> value + items.item_id) FROM items",
            "SELECT TRANSFORM(ARRAY_CONSTRUCT(item_id), items -> items + items.item_id) FROM items",
            "SELECT TRANSFORM(ARRAY_CONSTRUCT(item_id), x -> TRANSFORM(ARRAY_CONSTRUCT(x), y -> x + y + item_id)) FROM items",
            "SELECT TRANSFORM(ARRAY_CONSTRUCT(item_id), x -> TRANSFORM(ARRAY_CONSTRUCT(x), x -> x + 1)) FROM items",
            "SELECT TRANSFORM(ARRAY_CONSTRUCT(i.item_id), item_id -> item_id + 1) FROM items i JOIN other o ON i.item_id = o.item_id",
            "SELECT TRANSFORM(ARRAY_CONSTRUCT(item_id), Value -> VALUE + value) FROM items",
            "SELECT TRANSFORM(ARRAY_CONSTRUCT(item_id), \"Value\" -> \"Value\" + 1) FROM items",
            "SELECT TRANSFORM(ARRAY_CONSTRUCT(item_id), \"Value\" INT -> \"Value\" + 1) FROM items",
            "SELECT FILTER(ARRAY_CONSTRUCT(item_id), \"Value\" -> \"Value\" > 0) FROM items",
            "SELECT FILTER(ARRAY_CONSTRUCT(item_id), \"Value\" INT -> \"Value\" > 0) FROM items",
            "SELECT REDUCE(ARRAY_CONSTRUCT(item_id), 0, (\"Acc\", \"Value\") -> \"Acc\" + \"Value\") FROM items",
            "SELECT TRANSFORM(ARRAY_CONSTRUCT(item_id), value -> \"VALUE\" + 1) FROM items",
            "SELECT TRANSFORM(ARRAY_CONSTRUCT(OBJECT_CONSTRUCT('amount', item_id)), value -> value:amount) FROM items",
            "SELECT item_id FROM items WHERE ARRAY_SIZE(FILTER(ARRAY_CONSTRUCT(item_id), value -> value > 0)) > 0",
            "SELECT item_id AS merged FROM items ORDER BY TRANSFORM(ARRAY_CONSTRUCT(item_id), value -> value + 1), merged",
            "SELECT TRANSFORM(ARRAY_CONSTRUCT(item_id), value -> value + (SELECT MAX(item_id) FROM other)) FROM items",
            "WITH q AS (SELECT TRANSFORM(ARRAY_CONSTRUCT(item_id), value -> value + 1) AS values_array FROM items) SELECT values_array FROM q",
            "SELECT q.values_array FROM (SELECT TRANSFORM(ARRAY_CONSTRUCT(item_id), value -> value + 1) AS values_array FROM items) q",
            "SELECT i.item_id FROM items i WHERE EXISTS (SELECT TRANSFORM(ARRAY_CONSTRUCT(o.item_id), value -> value + i.item_id) FROM other o)",
            "SELECT i.item_id FROM items i WHERE EXISTS (SELECT 1 FROM (SELECT TRANSFORM(ARRAY_CONSTRUCT(1), i INT -> i + i.item_id)) q)",
            "SELECT TRANSFORM(ARRAY_CONSTRUCT(item_id), value -> value + 1) FROM items UNION ALL SELECT TRANSFORM(ARRAY_CONSTRUCT(item_id), value -> value + 1) FROM other",
        ] {
            let result = validate_with_schema(sql, DialectType::Snowflake, &schema, &options);
            assert!(result.valid && result.errors.is_empty(), "{sql}, types={check_types}: {:?}", result.errors);
        }
    }
}

#[test]
fn test_schema_validation_lambda_dialects_and_fields() {
    let schema = lambda_validation_schema();
    let options = SchemaValidationOptions {
        check_references: true,
        ..Default::default()
    };
    for (dialect, sql) in [
        (DialectType::DuckDB, "SELECT list_transform([item_id], value -> value + 1) FROM items"),
        (DialectType::DuckDB, "SELECT list_transform([item_id], lambda value: value + 1) FROM items"),
        (DialectType::DuckDB, "SELECT list_transform([item_id], lambda value, idx: value + idx + item_id) FROM items"),
        (DialectType::DuckDB, "SELECT list_transform([{'amount': item_id}], value -> value.amount) FROM items"),
        (DialectType::DuckDB, "SELECT list_transform([{'a': {'b': item_id}}], value -> value.a.b) FROM items"),
        (DialectType::DuckDB, "SELECT list_transform([item_id], \"Value\" -> value + 1) FROM items"),
        (DialectType::DuckDB, "SELECT list_transform([item_id], value -> value + q.item_id) FROM (SELECT item_id FROM items) q"),
        (DialectType::DuckDB, "SELECT list_reduce([item_id], lambda items, acc: items + acc + items.item_id, 0) FROM items"),
        (DialectType::DuckDB, "WITH q AS (SELECT list_transform([{'a': item_id}], q -> q.a) AS vals FROM items) SELECT vals FROM q"),
        (DialectType::DuckDB, "SELECT q.vals FROM (SELECT list_transform([{'a': item_id}], q -> q.a) AS vals FROM items) q"),
        (DialectType::Spark, "SELECT transform(array(item_id), value -> value + 1) FROM items"),
        (DialectType::Databricks, "SELECT transform(array(item_id), (value, idx) -> value + idx) FROM items"),
        (DialectType::Trino, "SELECT transform(ARRAY[item_id], value -> value + 1) FROM items"),
        (DialectType::Presto, "SELECT transform(ARRAY[item_id], value -> value + 1) FROM items"),
        (DialectType::ClickHouse, "SELECT arrayMap(value -> value + 1, [item_id]) FROM items"),
    ] {
        let result = validate_with_schema(sql, dialect, &schema, &options);
        assert!(result.valid && result.errors.is_empty(), "{dialect}: {sql}: {:?}", result.errors);
    }
}

#[test]
fn test_schema_validation_lambda_captures_and_boundaries() {
    let schema = lambda_validation_schema();
    for strict in [false, true] {
        let options = SchemaValidationOptions {
            check_references: true,
            strict: Some(strict),
            ..Default::default()
        };
        for (sql, code, token) in [
            ("SELECT TRANSFORM(ARRAY_CONSTRUCT(item_id), value -> value + missing) FROM items", "E201", "missing"),
            ("SELECT TRANSFORM(ARRAY_CONSTRUCT(missing), value -> value + 1) FROM items", "E201", "missing"),
            ("SELECT TRANSFORM(ARRAY_CONSTRUCT(item_id), value -> value + items.missing) FROM items", "E201", "missing"),
            ("SELECT TRANSFORM(ARRAY_CONSTRUCT(item_id), items -> items + items.missing) FROM items", "E201", "missing"),
            ("SELECT TRANSFORM(ARRAY_CONSTRUCT(item_id), value -> value + absent.item_id) FROM items", "E222", "absent"),
            ("SELECT TRANSFORM(ARRAY_CONSTRUCT(item_id), value -> value + 1), value FROM items", "E201", "value"),
            ("SELECT TRANSFORM(ARRAY_CONSTRUCT(item_id), x -> x + 1), TRANSFORM(ARRAY_CONSTRUCT(item_id), y -> x + y) FROM items", "E201", "x"),
            ("SELECT TRANSFORM(ARRAY_CONSTRUCT(item_id), value -> value + (SELECT MAX(value) FROM other)) FROM items", "E201", "value"),
            ("SELECT TRANSFORM(ARRAY_CONSTRUCT(item_id), value -> value + (SELECT MAX(missing) FROM other)) FROM items", "E201", "missing"),
            ("SELECT i.item_id FROM items i WHERE EXISTS (SELECT 1 FROM (SELECT TRANSFORM(ARRAY_CONSTRUCT(1), i INT -> i + i.missing)) q)", "E201", "missing"),
            ("SELECT i.item_id FROM items i WHERE EXISTS (WITH q AS (SELECT TRANSFORM(ARRAY_CONSTRUCT(1), i INT -> i + i.missing)) SELECT * FROM q)", "E201", "missing"),
            ("SELECT TRANSFORM(ARRAY_CONSTRUCT(item_id), \"Value\" -> value + 1) FROM items", "E201", "value"),
            ("SELECT TRANSFORM(ARRAY_CONSTRUCT(item_id), value -> \"value\" + 1) FROM items", "E201", "\"value\""),
            ("SELECT TRANSFORM(ARRAY_CONSTRUCT(i.item_id), value -> value + item_id) FROM items i JOIN other o ON i.item_id = o.item_id", "E221", "item_id"),
        ] {
            let result = validate_with_schema(sql, DialectType::Snowflake, &schema, &options);
            assert_eq!(result.valid, !strict, "{sql}: {:?}", result.errors);
            assert_eq!(result.errors.len(), 1, "{sql}: {:?}", result.errors);
            let error = &result.errors[0];
            assert_eq!(error.code, if !strict && code == "E221" { "W222" } else { code });
            assert_eq!(error.severity, if strict { crate::ValidationSeverity::Error } else { crate::ValidationSeverity::Warning });
            assert_eq!(&sql[error.start.unwrap()..error.end.unwrap()], token, "{sql}");
        }
    }
}

#[test]
fn test_schema_validation_lambda_parameter_types() {
    let mut schema = lambda_validation_schema();
    schema.tables[0].columns.push(
        serde_json::from_value(serde_json::json!({"name": "value", "type": "BOOLEAN"})).unwrap(),
    );
    for strict in [false, true] {
        let options = SchemaValidationOptions {
            check_types: true,
            check_references: true,
            strict: Some(strict),
            ..Default::default()
        };
        for sql in [
            "SELECT TRANSFORM(ARRAY_CONSTRUCT(item_id), value INT -> value + 1) FROM items",
            // Untyped bindings remain unknown, not the shadowed BOOLEAN type.
            "SELECT TRANSFORM(ARRAY_CONSTRUCT(item_id), value -> value + 1) FROM items",
            "SELECT TRANSFORM(ARRAY_CONSTRUCT(item_id), value INT -> value + item_id) FROM items",
            "SELECT TRANSFORM(ARRAY_CONSTRUCT(item_id), x INT -> ARRAY_CONSTRUCT(TRANSFORM(ARRAY_CONSTRUCT(TRUE), x BOOLEAN -> NOT x), x + 1)) FROM items",
            "SELECT TRANSFORM(ARRAY_CONSTRUCT(TRUE), x BOOLEAN -> TRANSFORM(ARRAY_CONSTRUCT(item_id), x -> x + 1)) FROM items",
        ] {
            let result = validate_with_schema(sql, DialectType::Snowflake, &schema, &options);
            assert!(result.valid && result.errors.is_empty(), "{sql}: {:?}", result.errors);
        }
        for sql in [
            "SELECT TRANSFORM(ARRAY_CONSTRUCT(TRUE), item_id BOOLEAN -> item_id + 1) FROM items",
            "SELECT TRANSFORM(ARRAY_CONSTRUCT(item_id), x INT -> x + value) FROM items",
            "SELECT TRANSFORM(ARRAY_CONSTRUCT(item_id), value INT -> value + items.value) FROM items",
            "SELECT TRANSFORM(ARRAY_CONSTRUCT(item_id), x INT -> TRANSFORM(ARRAY_CONSTRUCT(TRUE), x BOOLEAN -> x + 1)) FROM items",
        ] {
            let result = validate_with_schema(sql, DialectType::Snowflake, &schema, &options);
            assert_eq!(result.valid, !strict, "{sql}: {:?}", result.errors);
            assert_eq!(result.errors.len(), 1, "{sql}: {:?}", result.errors);
            assert_eq!(result.errors[0].code, if strict { "E212" } else { "W211" });
        }
    }
}

#[test]
fn test_validation_lambda_binding_keeps_public_ast_and_guards() {
    let dialect = Dialect::get(DialectType::Snowflake);
    let sql = "SELECT TRANSFORM(ARRAY_CONSTRUCT(item_id), value INT -> value + 1) FROM items";
    let original = dialect.parse(sql).unwrap().remove(0);
    let bound = bind_validation_lambdas(original.clone(), DialectType::Snowflake);
    assert!(original
        .dfs()
        .any(|node| matches!(node, Expression::Column(column) if column.name.name == "value")));
    assert!(!bound
        .dfs()
        .any(|node| matches!(node, Expression::Column(column) if column.name.name == "value")));
    assert_eq!(original, dialect.parse(sql).unwrap().remove(0));

    // The parser keeps quoting and source locations on typed and untyped
    // parameters in both specialized and generic function-argument paths.
    for function in ["TRANSFORM", "FILTER"] {
        for parameter_type in ["", " INT"] {
            let sql = format!(
                "SELECT {function}(ARRAY_CONSTRUCT(item_id), \"Value\"{parameter_type} -> \"Value\" + 1) FROM items"
            );
            let parsed = dialect.parse(&sql).unwrap().remove(0);
            let parameter = parsed
                .dfs()
                .find_map(|node| match node {
                    Expression::Lambda(lambda) => lambda.parameters.first(),
                    _ => None,
                })
                .unwrap();
            assert!(parameter.quoted, "{sql}");
            assert_eq!(parameter.name, "Value");
            let span = parameter.span.unwrap();
            assert_eq!(&sql[span.start..span.end], "\"Value\"");
        }
    }

    let sql = format!(
        "SELECT {}item_id{} FROM items",
        "TRANSFORM(ARRAY_CONSTRUCT(item_id), x -> ".repeat(20),
        ")".repeat(20)
    );
    let schema = lambda_validation_schema();
    let result = validate_with_schema(
        &sql,
        DialectType::Snowflake,
        &schema,
        &SchemaValidationOptions::default(),
    );
    assert!(result.valid, "{:?}", result.errors);
    let options: SchemaValidationOptions = serde_json::from_value(
        serde_json::json!({"complexity_guard": {"maxFunctionCallDepth": 4}}),
    )
    .unwrap();
    let result = validate_with_schema(&sql, DialectType::Snowflake, &schema, &options);
    assert!(!result.valid);
    assert!(result.errors[0]
        .message
        .contains("E_GUARD_FUNCTION_NESTING_DEPTH_EXCEEDED"));
}

fn lambda_validation_schema() -> ValidationSchema {
    serde_json::from_value(serde_json::json!({
        "strict": true,
        "tables": [
            {"name": "items", "columns": [{"name": "item_id", "type": "NUMBER"}]},
            {"name": "other", "columns": [{"name": "item_id", "type": "NUMBER"}]}
        ]
    }))
    .unwrap()
}

#[test]
fn test_schema_validation_order_by_output_names() {
    let schema: ValidationSchema = serde_json::from_value(serde_json::json!({
        "strict": true,
        "tables": [
            {"name": "current_items", "columns": [{"name": "item_id", "type": "NUMBER"}]},
            {"name": "archived_items", "columns": [{"name": "item_id", "type": "NUMBER"}]}
        ]
    }))
    .unwrap();
    for dialect in [
        DialectType::Snowflake,
        DialectType::DuckDB,
        DialectType::PostgreSQL,
        DialectType::BigQuery,
        DialectType::TSQL,
        DialectType::Fabric,
    ] {
        for check_references in [false, true] {
            for strict in [false, true] {
                let options = SchemaValidationOptions {
                    check_references,
                    strict: Some(strict),
                    ..Default::default()
                };
                for (projection, ordering) in [
                    // Exact issue #458 projection and ordering.
                    ("COALESCE(c.item_id, a.item_id) AS item_id", "item_id"),
                    ("COALESCE(c.item_id, a.item_id) AS merged_id", "merged_id"),
                    ("c.item_id AS item_id", "item_id"),
                    ("c.item_id", "item_id"),
                    ("c.item_id AS merged_id", "merged_id DESC, c.item_id"),
                    ("COALESCE(c.item_id, a.item_id) AS item_id", "1"),
                ] {
                    let sql = format!(
                        "SELECT {projection} FROM current_items AS c \
                         FULL JOIN archived_items AS a ON c.item_id = a.item_id ORDER BY {ordering}"
                    );
                    let result = validate_with_schema(&sql, dialect, &schema, &options);
                    assert!(result.valid, "{dialect}: {sql}: {:?}", result.errors);
                    assert!(
                        result.errors.is_empty(),
                        "{dialect}: {sql}: {:?}",
                        result.errors
                    );
                }
            }
        }
    }
}

#[test]
fn test_schema_validation_order_by_alias_expression_dialects() {
    let schema = base_schema();
    let options = SchemaValidationOptions {
        check_references: true,
        ..Default::default()
    };
    for dialect in [
        DialectType::Snowflake,
        DialectType::DuckDB,
        DialectType::BigQuery,
        DialectType::PostgreSQL,
        DialectType::TSQL,
        DialectType::Fabric,
    ] {
        let scalar_aliases = matches!(
            dialect,
            DialectType::Snowflake | DialectType::DuckDB | DialectType::BigQuery
        );
        for ordering in ["merged_id + 1", "ABS(merged_id)", "COALESCE(merged_id, 0)"] {
            let sql = format!("SELECT u.id AS merged_id FROM users u ORDER BY {ordering}");
            let result = validate_with_schema(&sql, dialect, &schema, &options);
            assert_eq!(
                result.valid, scalar_aliases,
                "{dialect}: {sql}: {:?}",
                result.errors
            );
            if !scalar_aliases {
                assert!(result.errors.iter().any(|error| error.code == "E201"));
            }
        }
        // In dialects requiring standalone aliases, compound expressions can
        // still reference actual input columns of the same name.
        let sql = "SELECT u.age AS id FROM users u ORDER BY id + 1";
        assert!(validate_with_schema(sql, dialect, &schema, &options).valid);
    }
    // DuckDB allows aliases in expressions only as a fallback to input names.
    // The bare name binds the output; the compound expression is ambiguous.
    for (ordering, valid) in [
        ("id", true),
        ("(id)", true),
        ("id + 1", false),
        ("ABS(id)", false),
    ] {
        let sql = format!(
            "SELECT COALESCE(u.id, o.id) AS id FROM users u \
             FULL JOIN orders o ON u.id = o.id ORDER BY {ordering}"
        );
        let result = validate_with_schema(&sql, DialectType::DuckDB, &schema, &options);
        assert_eq!(result.valid, valid, "{sql}: {:?}", result.errors);
        if !valid {
            assert!(result.errors.iter().any(|error| error.code == "E221"));
        }
    }
}

#[test]
fn test_schema_validation_order_by_alias_quoting() {
    let schema = base_schema();
    for (dialect, alias, reference, valid) in [
        (DialectType::Snowflake, "\"MergedId\"", "\"MergedId\"", true),
        (DialectType::Snowflake, "\"MergedId\"", "mergedid", false),
        (DialectType::Snowflake, "mergedid", "\"MERGEDID\"", true),
        (DialectType::Snowflake, "mergedid", "\"mergedid\"", false),
        (
            DialectType::PostgreSQL,
            "\"MergedId\"",
            "\"MergedId\"",
            true,
        ),
        (DialectType::PostgreSQL, "\"MergedId\"", "mergedid", false),
        (DialectType::PostgreSQL, "MERGEDID", "\"mergedid\"", true),
        (DialectType::DuckDB, "\"MergedId\"", "mergedid", true),
        (DialectType::BigQuery, "`MergedId`", "mergedid", true),
    ] {
        let sql = format!("SELECT u.id AS {alias} FROM users u ORDER BY {reference}");
        let result = validate_with_schema(
            sql.as_str(),
            dialect,
            &schema,
            &SchemaValidationOptions::default(),
        );
        assert_eq!(result.valid, valid, "{dialect}: {sql}: {:?}", result.errors);
    }
}

#[test]
fn test_schema_validation_order_by_alias_scope_boundaries() {
    let schema = base_schema();
    let options = SchemaValidationOptions {
        check_references: true,
        ..Default::default()
    };
    for sql in [
        "WITH q AS (SELECT COALESCE(u.id, o.id) AS id FROM users u FULL JOIN orders o ON u.id = o.id ORDER BY id) SELECT id FROM q",
        "SELECT q.id FROM (SELECT COALESCE(u.id, o.id) AS id FROM users u FULL JOIN orders o ON u.id = o.id ORDER BY id) q",
        "SELECT u.id FROM users u ORDER BY (SELECT o.id AS merged_id FROM orders o ORDER BY merged_id LIMIT 1)",
        "SELECT u.id AS merged_id FROM users u ORDER BY (SELECT o.id FROM orders o LIMIT 1), merged_id",
        "SELECT u.id FROM users u WHERE EXISTS (SELECT o.id AS id FROM orders o ORDER BY id)",
    ] {
        let result = validate_with_schema(sql, DialectType::Snowflake, &schema, &options);
        assert!(result.valid, "{sql}: {:?}", result.errors);
    }
    for (sql, code) in [
        ("SELECT id AS id FROM users u JOIN orders o ON u.id = o.id ORDER BY id", "E221"),
        ("SELECT u.id AS id, o.id AS id FROM users u JOIN orders o ON u.id = o.id ORDER BY id", "E221"),
        ("SELECT u.id AS merged_id FROM users u ORDER BY u.merged_id", "E201"),
        ("SELECT u.id AS merged_id FROM users u ORDER BY absent.merged_id", "E222"),
        ("SELECT u.missing AS merged_id FROM users u ORDER BY merged_id", "E201"),
        ("SELECT u.id AS merged_id FROM users u ORDER BY missing", "E201"),
        ("SELECT u.id AS merged_id FROM users u ORDER BY (SELECT merged_id FROM orders)", "E201"),
        ("SELECT u.id FROM users u ORDER BY (SELECT missing AS merged_id FROM orders ORDER BY merged_id LIMIT 1)", "E201"),
        ("SELECT *, u.id AS id FROM users u JOIN orders o ON u.id = o.id ORDER BY id", "E221"),
    ] {
        let result = validate_with_schema(sql, DialectType::Snowflake, &schema, &options);
        assert!(!result.valid && result.errors.iter().any(|error| error.code == code), "{sql}: {:?}", result.errors);
    }
    // These clauses bind inputs independently of query-level ORDER BY.
    for (dialect, sql, code) in [
        (DialectType::PostgreSQL, "SELECT u.id AS id FROM users u JOIN orders o ON id = o.id ORDER BY id", "E221"),
        (DialectType::PostgreSQL, "SELECT u.id AS id FROM users u JOIN orders o ON u.id = o.id WHERE id > 0 ORDER BY id", "E221"),
        (DialectType::PostgreSQL, "SELECT u.id AS id FROM users u JOIN orders o ON u.id = o.id GROUP BY id ORDER BY id", "E221"),
        (DialectType::DuckDB, "SELECT u.id AS id FROM users u JOIN orders o ON u.id = o.id ORDER BY ROW_NUMBER() OVER (ORDER BY id)", "E221"),
        (DialectType::DuckDB, "SELECT u.id AS merged_id FROM users u ORDER BY SUM(merged_id)", "E201"),
        (DialectType::DuckDB, "SELECT u.id AS merged_id FROM users u ORDER BY STRING_AGG(u.name, ',' ORDER BY merged_id)", "E201"),
        (DialectType::DuckDB, "SELECT u.id AS merged_id FROM users u ORDER BY SUM(u.id) FILTER (WHERE merged_id > 0)", "E201"),
        (DialectType::PostgreSQL, "SELECT u.id AS merged_id FROM users u ORDER BY PERCENTILE_CONT(0.5) WITHIN GROUP (ORDER BY merged_id)", "E201"),
    ] {
        let result = validate_with_schema(sql, dialect, &schema, &options);
        assert!(!result.valid && result.errors.iter().any(|error| error.code == code), "{dialect}: {sql}: {:?}", result.errors);
    }
}

#[test]
fn test_schema_validation_order_by_alias_diagnostics() {
    let schema = base_schema();
    for strict in [false, true] {
        let options = SchemaValidationOptions {
            check_references: true,
            strict: Some(strict),
            ..Default::default()
        };
        // The output reference is valid, but must not hide an ambiguous input
        // in the projection. Preserve severity and original source location.
        let sql = "SELECT id AS id FROM users u JOIN orders o ON u.id = o.id ORDER BY id";
        let result = validate_with_schema(sql, DialectType::Snowflake, &schema, &options);
        assert_eq!(result.valid, !strict);
        assert_eq!(result.errors.len(), 1, "{:?}", result.errors);
        let error = &result.errors[0];
        assert_eq!(error.code, if strict { "E221" } else { "W222" });
        assert_eq!((error.start, error.end), (Some(7), Some(9)));

        let sql = "SELECT u.id AS merged_id FROM users u ORDER BY missing";
        let result = validate_with_schema(sql, DialectType::Snowflake, &schema, &options);
        assert_eq!(result.valid, !strict);
        let error = &result.errors[0];
        assert_eq!(error.code, "E201");
        assert_eq!(error.start, Some(sql.find("missing").unwrap()));
        assert_eq!(error.end, Some(sql.len()));
    }
}

#[test]
fn test_schema_validation_open_sources() {
    for columns in [serde_json::json!([]), serde_json::json!([{"name": "*"}])] {
        let mut schema = base_schema();
        schema.tables[0].columns = serde_json::from_value(columns).unwrap();
        for sql in [
            "SELECT missing FROM users",
            "SELECT u.missing FROM users u",
            "SELECT payload.field FROM users",
            "SELECT o.id FROM orders o WHERE EXISTS (SELECT 1 FROM users u WHERE u.missing = o.id)",
            "SELECT missing FROM users u JOIN orders o ON TRUE",
            "SELECT missing FROM orders o JOIN users u ON TRUE",
            "WITH q AS (SELECT * FROM users) SELECT missing FROM q",
            "SELECT q.missing FROM (SELECT * FROM users) q",
        ] {
            let result = validate_with_schema(
                sql,
                DialectType::Snowflake,
                &schema,
                &SchemaValidationOptions {
                    check_references: true,
                    ..Default::default()
                },
            );
            assert!(result.valid, "{sql}: {:?}", result.errors);
        }
        let result = validate_with_schema(
            "SELECT o.missing FROM users u JOIN orders o ON TRUE",
            DialectType::Snowflake,
            &schema,
            &SchemaValidationOptions::default(),
        );
        assert!(!result.valid);
    }
}

#[test]
fn test_schema_reference_spans_and_ambiguity_options() {
    let schema = base_schema();
    for (sql, code, token) in [
        (
            "SELECT u.id FROM users u WHERE u.missing = TRUE",
            "E201",
            "missing",
        ),
        ("SELECT x.id FROM users", "E222", "x"),
        ("SELECT * FROM absent", "E200", "absent"),
        (
            "SELECT '😀', u.\"míssing\" FROM users u",
            "E201",
            "\"míssing\"",
        ),
        (
            "SELECT id FROM users u JOIN orders o ON u.id = o.user_id",
            "E221",
            "id",
        ),
    ] {
        let result = validate_with_schema(
            sql,
            DialectType::Snowflake,
            &schema,
            &SchemaValidationOptions {
                check_references: true,
                ..Default::default()
            },
        );
        let error = result
            .errors
            .iter()
            .find(|error| error.code == code)
            .unwrap_or_else(|| panic!("{sql}: {:?}", result.errors));
        let start = sql[..sql.find(token).unwrap()].chars().count();
        assert_eq!(error.start, Some(start), "{sql}");
        assert_eq!(error.end, Some(start + token.chars().count()), "{sql}");
        assert_eq!(error.line, Some(1));
        assert!(error.column.is_some());
    }
    let sql = "SELECT missing, missing FROM users";
    let result = validate_with_schema(
        sql,
        DialectType::Snowflake,
        &schema,
        &SchemaValidationOptions::default(),
    );
    let starts: Vec<_> = result
        .errors
        .iter()
        .filter(|e| e.code == "E201")
        .map(|e| e.start)
        .collect();
    assert_eq!(starts, vec![Some(7), Some(16)]);
    let sql = "SELECT id FROM users u JOIN orders o ON u.id = o.user_id";
    assert!(
        validate_with_schema(
            sql,
            DialectType::Snowflake,
            &schema,
            &SchemaValidationOptions::default()
        )
        .valid
    );
    let result = validate_with_schema(
        sql,
        DialectType::Snowflake,
        &schema,
        &SchemaValidationOptions {
            check_references: true,
            strict: Some(false),
            ..Default::default()
        },
    );
    assert!(result.valid);
    assert!(result
        .errors
        .iter()
        .any(|e| e.code == "W222" && e.start == Some(7)));
    let error = reference_diagnostic("Unknown column".into(), "E201", true, None);
    assert_eq!(
        (error.start, error.end, error.line, error.column),
        (None, None, None, None)
    );
}

#[test]
fn test_canonical_type_family_aliases() {
    assert_eq!(canonical_type_family("INT4"), TypeFamily::Integer);
    for name in ["HUGEINT", "INT128", "LARGEINT", "Nullable(Int128)"] {
        assert_eq!(canonical_type_family(name), TypeFamily::Integer);
    }
    assert_eq!(data_type_family(&DataType::Int128), TypeFamily::Integer);
    for name in [
        "UTINYINT",
        "UINT8",
        "USMALLINT",
        "UINT16",
        "UINTEGER",
        "UINT32",
        "UBIGINT",
        "UINT64",
        "UHUGEINT",
        "UINT128",
    ] {
        assert_eq!(canonical_type_family(name), TypeFamily::Integer);
        let dt = crate::parse_data_type(name, DialectType::DuckDB).unwrap();
        assert_eq!(data_type_family(&dt), TypeFamily::Integer);
    }
    assert_eq!(
        canonical_type_family("double precision"),
        TypeFamily::Numeric
    );
    assert_eq!(canonical_type_family("VARCHAR(255)"), TypeFamily::String);
    assert_eq!(
        canonical_type_family("timestamp with time zone"),
        TypeFamily::Timestamp
    );
    assert_eq!(canonical_type_family("JSONB"), TypeFamily::Json);
    assert_eq!(canonical_type_family("UUID"), TypeFamily::Uuid);
}

#[test]
fn test_canonical_type_family_wrappers_and_collections() {
    assert_eq!(
        canonical_type_family("Nullable(Int64)"),
        TypeFamily::Integer
    );
    assert_eq!(
        canonical_type_family("LowCardinality(String)"),
        TypeFamily::String
    );
    assert_eq!(canonical_type_family("Array(String)"), TypeFamily::Array);
    assert_eq!(canonical_type_family("list(varchar)"), TypeFamily::Array);
    assert_eq!(canonical_type_family("Map(String, Int64)"), TypeFamily::Map);
    assert_eq!(canonical_type_family("STRUCT<a INT>"), TypeFamily::Struct);
    assert_eq!(canonical_type_family(""), TypeFamily::Unknown);
}

fn base_schema() -> ValidationSchema {
    ValidationSchema {
        tables: vec![
            SchemaTable {
                name: "users".to_string(),
                schema: None,
                columns: vec![
                    SchemaColumn {
                        name: "id".to_string(),
                        data_type: "integer".to_string(),
                        nullable: None,
                        primary_key: false,
                        unique: false,
                        references: None,
                    },
                    SchemaColumn {
                        name: "name".to_string(),
                        data_type: "varchar".to_string(),
                        nullable: None,
                        primary_key: false,
                        unique: false,
                        references: None,
                    },
                    SchemaColumn {
                        name: "email".to_string(),
                        data_type: "varchar".to_string(),
                        nullable: None,
                        primary_key: false,
                        unique: false,
                        references: None,
                    },
                    SchemaColumn {
                        name: "age".to_string(),
                        data_type: "integer".to_string(),
                        nullable: None,
                        primary_key: false,
                        unique: false,
                        references: None,
                    },
                ],
                aliases: vec![],
                primary_key: vec![],
                unique_keys: vec![],
                foreign_keys: vec![],
            },
            SchemaTable {
                name: "orders".to_string(),
                schema: None,
                columns: vec![
                    SchemaColumn {
                        name: "id".to_string(),
                        data_type: "integer".to_string(),
                        nullable: None,
                        primary_key: false,
                        unique: false,
                        references: None,
                    },
                    SchemaColumn {
                        name: "user_id".to_string(),
                        data_type: "integer".to_string(),
                        nullable: None,
                        primary_key: false,
                        unique: false,
                        references: None,
                    },
                    SchemaColumn {
                        name: "total".to_string(),
                        data_type: "decimal".to_string(),
                        nullable: None,
                        primary_key: false,
                        unique: false,
                        references: None,
                    },
                ],
                aliases: vec![],
                primary_key: vec![],
                unique_keys: vec![],
                foreign_keys: vec![],
            },
        ],
        strict: Some(true),
    }
}

#[test]
fn test_schema_validation_handles_long_set_operation_cte_without_revalidating_prefixes() {
    let schema = base_schema();
    let options = SchemaValidationOptions::default();
    let branches: Vec<String> = (0..256)
        .map(|index| format!("SELECT id, {index} AS sequence_number FROM users"))
        .collect();
    let valid_sql = format!(
        "WITH order_sequence AS ({}) SELECT id, sequence_number FROM order_sequence",
        branches.join(" UNION ALL ")
    );

    let valid = validate_with_schema(&valid_sql, DialectType::Snowflake, &schema, &options);

    assert!(valid.valid, "{:?}", valid.errors);

    let invalid_sql = valid_sql.replace(
        "SELECT id, 255 AS sequence_number FROM users",
        "SELECT missing, 255 AS sequence_number FROM users",
    );
    let invalid = validate_with_schema(&invalid_sql, DialectType::Snowflake, &schema, &options);

    assert!(
        !invalid.valid
            && invalid
                .errors
                .iter()
                .any(|error| error.code == validation_codes::E_UNKNOWN_COLUMN),
        "{:?}",
        invalid.errors
    );
}

fn attach_column_fk(
    schema: &mut ValidationSchema,
    table_name: &str,
    column_name: &str,
    target_table: &str,
    target_column: &str,
) {
    if let Some(table) = schema.tables.iter_mut().find(|t| t.name == table_name) {
        if let Some(column) = table.columns.iter_mut().find(|c| c.name == column_name) {
            column.references = Some(SchemaColumnReference {
                table: target_table.to_string(),
                column: target_column.to_string(),
                schema: None,
            });
        }
    }
}

fn mark_primary_key(schema: &mut ValidationSchema, table_name: &str, column_name: &str) {
    if let Some(table) = schema.tables.iter_mut().find(|t| t.name == table_name) {
        table.primary_key = vec![column_name.to_string()];
        if let Some(column) = table.columns.iter_mut().find(|c| c.name == column_name) {
            column.primary_key = true;
        }
    }
}

fn test_function_catalog() -> Arc<HashMapFunctionCatalog> {
    let mut catalog = HashMapFunctionCatalog::default();
    catalog.register(
        DialectType::Generic,
        "abs",
        vec![FunctionSignature::exact(1)],
    );
    catalog.register(
        DialectType::Generic,
        "coalesce",
        vec![FunctionSignature::variadic(1)],
    );
    catalog.register(
        DialectType::Generic,
        "foo",
        vec![FunctionSignature::exact(1)],
    );
    Arc::new(catalog)
}

fn schema_validation_dialects() -> [DialectType; 7] {
    [
        DialectType::Generic,
        DialectType::Snowflake,
        DialectType::DuckDB,
        DialectType::BigQuery,
        DialectType::Databricks,
        DialectType::PostgreSQL,
        DialectType::TSQL,
    ]
}

#[test]
fn test_validate_with_schema_known_table_column() {
    let schema = base_schema();
    let opts = SchemaValidationOptions::default();
    let result = validate_with_schema(
        "SELECT id, name FROM users",
        DialectType::Generic,
        &schema,
        &opts,
    );
    assert!(result.valid);
    assert!(result.errors.is_empty());
}

#[test]
fn test_validate_with_schema_qualified_cte_column_is_not_ambiguous() {
    let schema = base_schema();
    let opts = SchemaValidationOptions {
        check_references: true,
        ..Default::default()
    };
    for dialect in schema_validation_dialects() {
        let result = validate_with_schema(
            "WITH selected_users AS (SELECT id FROM users), \
             selected_orders AS (SELECT id FROM orders) \
             SELECT selected_users.id FROM selected_users \
             JOIN selected_orders ON selected_users.id = selected_orders.id",
            dialect,
            &schema,
            &opts,
        );

        assert!(result.valid, "{dialect}: {:#?}", result.errors);
    }
}

#[test]
fn test_validate_with_schema_correlated_subquery_resolves_outer_alias() {
    let schema = base_schema();
    let opts = SchemaValidationOptions {
        check_references: true,
        ..Default::default()
    };
    for dialect in schema_validation_dialects() {
        let result = validate_with_schema(
            "SELECT outer_users.id FROM users outer_users \
             WHERE EXISTS (SELECT 1 FROM orders inner_orders \
             WHERE inner_orders.user_id = outer_users.id)",
            dialect,
            &schema,
            &opts,
        );

        assert!(result.valid, "{dialect}: {:#?}", result.errors);
    }
}

#[test]
fn test_validate_with_schema_correlated_subquery_prefers_inner_unqualified_column() {
    let schema = base_schema();
    let opts = SchemaValidationOptions {
        check_references: true,
        ..Default::default()
    };
    for dialect in schema_validation_dialects() {
        let result = validate_with_schema(
            "SELECT outer_users.id FROM users outer_users \
             WHERE EXISTS (SELECT 1 FROM orders inner_orders \
             WHERE id = outer_users.id)",
            dialect,
            &schema,
            &opts,
        );

        assert!(result.valid, "{dialect}: {:#?}", result.errors);
    }
}

#[test]
fn test_validate_with_schema_subsequent_cte_resolves_prior_projection() {
    let schema = base_schema();
    let opts = SchemaValidationOptions {
        check_references: true,
        ..Default::default()
    };
    for dialect in schema_validation_dialects() {
        let result = validate_with_schema(
            "WITH derived AS (SELECT total AS derived_total FROM orders), \
             next AS (SELECT derived_total FROM derived) \
             SELECT derived_total FROM next",
            dialect,
            &schema,
            &opts,
        );

        assert!(result.valid, "{dialect}: {:#?}", result.errors);
    }
}

#[test]
fn test_validate_with_schema_window_resolves_prior_cte_projection() {
    let schema = base_schema();
    let opts = SchemaValidationOptions {
        check_references: true,
        ..Default::default()
    };
    for dialect in schema_validation_dialects() {
        let result = validate_with_schema(
            "WITH scored AS (SELECT user_id, total AS order_total FROM orders), \
             ranked AS (SELECT user_id, ROW_NUMBER() OVER ( \
             PARTITION BY user_id ORDER BY order_total DESC) AS row_number FROM scored) \
             SELECT user_id FROM ranked",
            dialect,
            &schema,
            &opts,
        );
        assert!(result.valid, "{dialect}: {:#?}", result.errors);
    }
}

#[test]
fn test_validate_with_schema_joined_cte_projection_preserves_scope() {
    let schema = base_schema();
    let opts = SchemaValidationOptions {
        check_references: true,
        ..Default::default()
    };
    for dialect in schema_validation_dialects() {
        let result = validate_with_schema(
            "WITH selected_users AS (SELECT id, name FROM users), \
             selected_orders AS (SELECT id, user_id, total FROM orders), \
             combined AS (SELECT selected_users.*, selected_orders.total \
             FROM selected_users JOIN selected_orders \
             ON selected_users.id = selected_orders.user_id) \
             SELECT id, name, total FROM combined",
            dialect,
            &schema,
            &opts,
        );

        assert!(result.valid, "{dialect}: {:#?}", result.errors);
    }
}

#[test]
fn test_validate_with_schema_chained_star_projection_preserves_aliases() {
    let schema = base_schema();
    let opts = SchemaValidationOptions {
        check_references: true,
        ..Default::default()
    };
    for dialect in schema_validation_dialects() {
        let result = validate_with_schema(
            "WITH totals AS (SELECT user_id, total AS order_total FROM orders), \
             copied AS (SELECT * FROM totals), \
             scored AS (SELECT *, order_total + 1 AS adjusted_total FROM copied) \
             SELECT user_id, order_total, adjusted_total FROM scored",
            dialect,
            &schema,
            &opts,
        );

        assert!(result.valid, "{dialect}: {:#?}", result.errors);
    }
}

#[test]
fn test_validate_with_schema_snowflake_qualify_resolves_projection_columns() {
    let schema = base_schema();
    let opts = SchemaValidationOptions {
        check_references: true,
        ..Default::default()
    };
    let result = validate_with_schema(
        "WITH ranked AS (SELECT user_id, total, \
         ROW_NUMBER() OVER (PARTITION BY user_id ORDER BY total DESC) AS row_number \
         FROM orders QUALIFY row_number = 1) \
         SELECT user_id, total FROM ranked",
        DialectType::Snowflake,
        &schema,
        &opts,
    );
    assert!(result.valid, "{:#?}", result.errors);
}

#[test]
fn test_validate_with_schema_snowflake_union_by_name_preserves_projection() {
    let schema = base_schema();
    let opts = SchemaValidationOptions {
        check_references: true,
        ..Default::default()
    };
    let result = validate_with_schema(
        "WITH records AS (SELECT id, name FROM users \
         UNION ALL BY NAME SELECT id, CAST(total AS VARCHAR) AS name FROM orders) \
         SELECT id, name FROM records",
        DialectType::Snowflake,
        &schema,
        &opts,
    );
    assert!(result.valid, "{:#?}", result.errors);
}

#[test]
fn test_validate_with_schema_snowflake_join_using_resolves_cte_column() {
    let schema = base_schema();
    let opts = SchemaValidationOptions {
        check_references: true,
        ..Default::default()
    };
    let result = validate_with_schema(
        "WITH selected_users AS (SELECT id, name FROM users) \
         SELECT orders.id, selected_users.name FROM orders \
         INNER JOIN selected_users USING (id)",
        DialectType::Snowflake,
        &schema,
        &opts,
    );

    assert!(result.valid, "{:#?}", result.errors);
}

#[test]
fn test_validate_with_schema_snowflake_join_using_cte_from_same_table() {
    let schema = base_schema();
    let opts = SchemaValidationOptions {
        check_references: true,
        ..Default::default()
    };
    let result = validate_with_schema(
        "WITH selected_users AS (SELECT id FROM users WHERE age >= 18), \
         combined AS (SELECT users.* FROM users \
         INNER JOIN selected_users USING (id)) \
         SELECT id, name FROM combined",
        DialectType::Snowflake,
        &schema,
        &opts,
    );

    assert!(result.valid, "{:#?}", result.errors);
}

#[test]
fn test_validate_with_schema_snowflake_nested_qualify_resolves_cte_projection() {
    let schema = base_schema();
    let opts = SchemaValidationOptions {
        check_references: true,
        ..Default::default()
    };
    let result = validate_with_schema(
        "WITH customer_orders AS (SELECT user_id, id AS order_id, total FROM orders), \
         deduplicated AS (SELECT * FROM (SELECT user_id, order_id, total \
         FROM customer_orders QUALIFY ROW_NUMBER() OVER (PARTITION BY user_id \
         ORDER BY order_id) = 1)) SELECT user_id, order_id, total FROM deduplicated",
        DialectType::Snowflake,
        &schema,
        &opts,
    );

    assert!(result.valid, "{:#?}", result.errors);
}

#[test]
fn test_validate_with_schema_snowflake_deep_nested_qualify_resolves_cte_projection() {
    let schema = base_schema();
    let opts = SchemaValidationOptions {
        check_references: true,
        ..Default::default()
    };
    let result = validate_with_schema(
        "WITH base AS (SELECT id AS user_id, name, age FROM users), \
         flags AS (SELECT user_id FROM (SELECT user_id FROM ( \
         SELECT user_id, name, age FROM base \
         QUALIFY ROW_NUMBER() OVER (PARTITION BY user_id, name ORDER BY age) = 1))) \
         SELECT user_id FROM flags",
        DialectType::Snowflake,
        &schema,
        &opts,
    );

    assert!(result.valid, "{:#?}", result.errors);
}

#[test]
fn test_validate_with_schema_deep_star_chain_preserves_computed_alias() {
    let schema = base_schema();
    let opts = SchemaValidationOptions {
        check_references: true,
        ..Default::default()
    };
    for dialect in schema_validation_dialects() {
        let result = validate_with_schema(
            "WITH base AS (SELECT user_id, total FROM orders), \
             first_copy AS (SELECT *, total + 1 AS adjusted_total FROM base), \
             second_copy AS (SELECT * FROM first_copy), \
             third_copy AS (SELECT * FROM second_copy), \
             fourth_copy AS (SELECT *, adjusted_total * 2 AS score FROM third_copy) \
             SELECT user_id, adjusted_total, score FROM fourth_copy",
            dialect,
            &schema,
            &opts,
        );

        assert!(result.valid, "{dialect}: {:#?}", result.errors);
    }
}

#[test]
fn test_validate_with_schema_nested_scalar_subquery_resolves_outer_column() {
    let schema = base_schema();
    let opts = SchemaValidationOptions {
        check_references: true,
        ..Default::default()
    };
    for dialect in schema_validation_dialects() {
        let result = validate_with_schema(
            "WITH metrics AS (SELECT user_id, total AS score FROM orders), \
             adjusted AS (SELECT score, CASE WHEN score IS NULL THEN NULL ELSE ( \
             SELECT adjusted_score FROM (SELECT score + 1 AS adjusted_score)) END AS result \
             FROM metrics) SELECT score, result FROM adjusted",
            dialect,
            &schema,
            &opts,
        );

        assert!(result.valid, "{dialect}: {:#?}", result.errors);
    }
}

#[test]
fn test_validate_with_schema_ambiguous_outer_column_stays_invalid() {
    let schema = base_schema();
    let opts = SchemaValidationOptions {
        check_references: true,
        ..Default::default()
    };
    let result = validate_with_schema(
        "SELECT users.id FROM users JOIN orders ON users.id = orders.user_id \
         WHERE EXISTS (SELECT 1 WHERE id > 0)",
        DialectType::Generic,
        &schema,
        &opts,
    );

    assert!(!result.valid);
    assert!(result
        .errors
        .iter()
        .any(|error| { error.code == validation_codes::E_AMBIGUOUS_COLUMN_REFERENCE }));
}

#[test]
fn test_validate_with_schema_unknown_column_after_cte_projection_stays_invalid() {
    let schema = base_schema();
    let opts = SchemaValidationOptions {
        check_references: true,
        ..Default::default()
    };
    let result = validate_with_schema(
        "WITH selected_users AS (SELECT id, name FROM users) \
         SELECT missing_column FROM selected_users",
        DialectType::Generic,
        &schema,
        &opts,
    );

    assert!(!result.valid);
    assert!(result
        .errors
        .iter()
        .any(|error| error.code == validation_codes::E_UNKNOWN_COLUMN));
}

#[test]
fn test_validate_with_schema_unknown_column_in_derived_table() {
    let schema = base_schema();
    let opts = SchemaValidationOptions::default();
    let result = validate_with_schema(
        "SELECT selected.id FROM (SELECT missing FROM users) selected",
        DialectType::Generic,
        &schema,
        &opts,
    );

    assert!(!result.valid);
    assert!(result
        .errors
        .iter()
        .any(|error| error.code == validation_codes::E_UNKNOWN_COLUMN));
}

#[test]
fn test_validate_with_schema_values_columns_are_visible() {
    let schema = base_schema();
    let options = SchemaValidationOptions::default();
    for sql in [
        "WITH inventory AS (SELECT column1 AS product_id FROM VALUES (1)) \
         SELECT product_id FROM inventory",
        "SELECT inventory_rows.product_id FROM VALUES (1) AS inventory_rows(product_id)",
    ] {
        let result = validate_with_schema(sql, DialectType::Snowflake, &schema, &options);

        assert!(result.valid, "{:#?}", result.errors);
    }
}

#[test]
fn test_validate_with_schema_lateral_subquery_resolves_prior_source_alias() {
    let schema = base_schema();
    let options = SchemaValidationOptions::default();
    let result = validate_with_schema(
        "SELECT nested.value FROM orders AS orders_source, \
         LATERAL (SELECT orders_source.total AS value) AS nested",
        DialectType::Snowflake,
        &schema,
        &options,
    );

    assert!(result.valid, "{:#?}", result.errors);
}

#[test]
fn test_validate_with_schema_unknown_column_in_insert_query() {
    let schema = base_schema();
    let opts = SchemaValidationOptions::default();
    let result = validate_with_schema(
        "INSERT INTO users (id) SELECT missing FROM orders",
        DialectType::Generic,
        &schema,
        &opts,
    );

    assert!(!result.valid);
    assert!(result
        .errors
        .iter()
        .any(|error| error.code == validation_codes::E_UNKNOWN_COLUMN));
}

#[test]
fn test_validate_with_schema_unknown_table() {
    let schema = base_schema();
    let opts = SchemaValidationOptions::default();
    let result = validate_with_schema(
        "SELECT * FROM nonexistent",
        DialectType::Generic,
        &schema,
        &opts,
    );
    assert!(!result.valid);
    assert!(result
        .errors
        .iter()
        .any(|e| e.code == validation_codes::E_UNKNOWN_TABLE && e.message.contains("nonexistent")));
}

#[test]
fn test_validate_with_schema_unknown_column() {
    let schema = base_schema();
    let opts = SchemaValidationOptions::default();
    let result = validate_with_schema(
        "SELECT unknown_col FROM users",
        DialectType::Generic,
        &schema,
        &opts,
    );
    assert!(!result.valid);
    assert!(
        result
            .errors
            .iter()
            .any(|e| e.code == validation_codes::E_UNKNOWN_COLUMN
                && e.message.contains("unknown_col"))
    );
}

#[test]
fn test_validate_with_schema_partial_schema_stays_strict() {
    let mut schema = base_schema();
    let users = schema
        .tables
        .iter_mut()
        .find(|table| table.name == "users")
        .expect("users table");
    users.columns.retain(|column| column.name == "id");

    let opts = SchemaValidationOptions::default();
    let result = validate_with_schema(
        "SELECT id, name FROM users",
        DialectType::Generic,
        &schema,
        &opts,
    );
    assert!(!result.valid);
    assert!(result.errors.iter().any(|error| {
        error.code == validation_codes::E_UNKNOWN_COLUMN && error.message.contains("name")
    }));
}

#[test]
fn test_validate_with_schema_function_catalog_unknown_function() {
    let schema = base_schema();
    let opts = SchemaValidationOptions {
        check_types: true,
        function_catalog: Some(test_function_catalog()),
        ..Default::default()
    };
    let result = validate_with_schema(
        "SELECT made_up_fn(id) FROM users",
        DialectType::Generic,
        &schema,
        &opts,
    );
    assert!(!result.valid);
    assert!(result
        .errors
        .iter()
        .any(|e| e.code == validation_codes::E_UNKNOWN_FUNCTION));
}

#[test]
fn test_validate_with_schema_function_catalog_invalid_arity() {
    let schema = base_schema();
    let opts = SchemaValidationOptions {
        check_types: true,
        function_catalog: Some(test_function_catalog()),
        ..Default::default()
    };
    let result = validate_with_schema(
        "SELECT FOO(id, age) FROM users",
        DialectType::Generic,
        &schema,
        &opts,
    );
    assert!(!result.valid);
    assert!(result
        .errors
        .iter()
        .any(|e| e.code == validation_codes::E_INVALID_FUNCTION_ARITY));
}

#[test]
fn test_validate_with_schema_function_catalog_valid_variadic() {
    let schema = base_schema();
    let opts = SchemaValidationOptions {
        check_types: true,
        function_catalog: Some(test_function_catalog()),
        ..Default::default()
    };
    let result = validate_with_schema(
        "SELECT COALESCE(name, email, 'fallback') FROM users",
        DialectType::Generic,
        &schema,
        &opts,
    );
    assert!(result.valid, "{:?}", result.errors);
}

#[test]
fn test_validate_with_schema_function_catalog_dialect_case_sensitive() {
    let schema = base_schema();
    let mut catalog = HashMapFunctionCatalog::default();
    catalog.set_dialect_name_case(DialectType::Generic, FunctionNameCase::Sensitive);
    catalog.register(
        DialectType::Generic,
        "Foo",
        vec![FunctionSignature::exact(1)],
    );

    let opts = SchemaValidationOptions {
        check_types: true,
        function_catalog: Some(Arc::new(catalog)),
        ..Default::default()
    };

    let valid = validate_with_schema(
        "SELECT Foo(id) FROM users",
        DialectType::Generic,
        &schema,
        &opts,
    );
    assert!(valid.valid, "{:?}", valid.errors);

    let invalid = validate_with_schema(
        "SELECT FOO(id) FROM users",
        DialectType::Generic,
        &schema,
        &opts,
    );
    assert!(!invalid.valid);
    assert!(invalid
        .errors
        .iter()
        .any(|e| e.code == validation_codes::E_UNKNOWN_FUNCTION));
}

#[test]
fn test_validate_with_schema_function_catalog_function_case_override() {
    let schema = base_schema();
    let mut catalog = HashMapFunctionCatalog::default();
    catalog.set_dialect_name_case(DialectType::Generic, FunctionNameCase::Insensitive);
    catalog.register(
        DialectType::Generic,
        "Bar",
        vec![FunctionSignature::exact(1)],
    );
    catalog.set_function_name_case(DialectType::Generic, "bar", FunctionNameCase::Sensitive);

    let opts = SchemaValidationOptions {
        check_types: true,
        function_catalog: Some(Arc::new(catalog)),
        ..Default::default()
    };

    let valid = validate_with_schema(
        "SELECT Bar(id) FROM users",
        DialectType::Generic,
        &schema,
        &opts,
    );
    assert!(valid.valid, "{:?}", valid.errors);

    let invalid = validate_with_schema(
        "SELECT BAR(id) FROM users",
        DialectType::Generic,
        &schema,
        &opts,
    );
    assert!(!invalid.valid);
    assert!(invalid
        .errors
        .iter()
        .any(|e| e.code == validation_codes::E_UNKNOWN_FUNCTION));
}

#[cfg(any(
    feature = "function-catalog-clickhouse",
    feature = "function-catalog-all-dialects"
))]
#[test]
fn test_validate_with_schema_uses_embedded_function_catalog_when_unset() {
    let schema = base_schema();
    let opts = SchemaValidationOptions {
        check_types: true,
        ..Default::default()
    };
    let result = validate_with_schema(
        "SELECT made_up_fn(id) FROM users",
        DialectType::ClickHouse,
        &schema,
        &opts,
    );
    assert!(!result.valid);
    assert!(result
        .errors
        .iter()
        .any(|e| e.code == validation_codes::E_UNKNOWN_FUNCTION));
}

#[cfg(any(
    feature = "function-catalog-duckdb",
    feature = "function-catalog-all-dialects"
))]
#[test]
fn test_validate_with_schema_uses_embedded_duckdb_function_catalog_when_unset() {
    let schema = base_schema();
    let opts = SchemaValidationOptions {
        check_types: true,
        ..Default::default()
    };
    let result = validate_with_schema(
        "SELECT made_up_fn(id) FROM users",
        DialectType::DuckDB,
        &schema,
        &opts,
    );
    assert!(!result.valid);
    assert!(result
        .errors
        .iter()
        .any(|e| e.code == validation_codes::E_UNKNOWN_FUNCTION));
}

#[test]
fn test_validate_with_schema_cte_projected_alias_column() {
    let schema = base_schema();
    let opts = SchemaValidationOptions::default();
    let result = validate_with_schema(
        "WITH my_cte AS (SELECT id AS emp_id FROM users) SELECT emp_id FROM my_cte",
        DialectType::ClickHouse,
        &schema,
        &opts,
    );
    assert!(result.valid, "{:?}", result.errors);
    assert!(result.errors.is_empty(), "{:?}", result.errors);
}

#[test]
fn test_validate_with_schema_cte_projected_alias_column_non_strict() {
    let schema = base_schema();
    let opts = SchemaValidationOptions {
        strict: Some(false),
        check_types: true,
        check_references: true,
        ..Default::default()
    };
    let result = validate_with_schema(
        "WITH my_cte AS (SELECT id AS emp_id FROM users) SELECT emp_id FROM my_cte",
        DialectType::ClickHouse,
        &schema,
        &opts,
    );
    assert!(result.valid, "{:?}", result.errors);
    assert!(result.errors.is_empty(), "{:?}", result.errors);
}

#[test]
fn test_validate_with_schema_unknown_cte_projected_alias_column() {
    let schema = base_schema();
    let opts = SchemaValidationOptions::default();
    let result = validate_with_schema(
        "WITH my_cte AS (SELECT id AS emp_id FROM users) SELECT missing_col FROM my_cte",
        DialectType::ClickHouse,
        &schema,
        &opts,
    );
    assert!(!result.valid);
    assert!(result.errors.iter().any(|e| {
        e.code == validation_codes::E_UNKNOWN_COLUMN && e.message.contains("missing_col")
    }));
}

#[test]
fn test_validate_with_schema_join_columns() {
    let schema = base_schema();
    let opts = SchemaValidationOptions::default();
    let result = validate_with_schema(
        "SELECT users.id, orders.total FROM users JOIN orders ON users.id = orders.user_id",
        DialectType::Generic,
        &schema,
        &opts,
    );
    assert!(result.valid);
}

#[test]
fn test_validate_with_schema_non_strict_is_warning() {
    let schema = base_schema();
    let opts = SchemaValidationOptions {
        strict: Some(false),
        ..Default::default()
    };
    let result = validate_with_schema(
        "SELECT unknown FROM users",
        DialectType::Generic,
        &schema,
        &opts,
    );
    assert!(result.valid);
    assert!(result
        .errors
        .iter()
        .all(|e| e.severity == crate::ValidationSeverity::Warning));
}

#[test]
fn test_semantic_warning_uses_column_source_span() {
    let sql = "SELECT customer_id, SUM(amount) FROM orders";
    let result = crate::validate_with_options(
        sql,
        DialectType::Snowflake,
        &crate::ValidationOptions {
            semantic: true,
            ..Default::default()
        },
    );
    let warning = result
        .errors
        .iter()
        .find(|error| error.code == "E230")
        .unwrap();
    assert_eq!((warning.start, warning.end), (Some(7), Some(18)));
    assert_eq!((warning.line, warning.column), (Some(1), Some(19)));
}

#[test]
fn test_validate_with_schema_semantic_warnings() {
    let schema = base_schema();
    let opts = SchemaValidationOptions {
        semantic: true,
        ..Default::default()
    };
    let result = validate_with_schema(
        "SELECT * FROM users LIMIT 10",
        DialectType::Generic,
        &schema,
        &opts,
    );
    assert!(result.valid);
    assert!(result
        .errors
        .iter()
        .any(|e| e.code == validation_codes::W_SELECT_STAR));
    assert!(result
        .errors
        .iter()
        .any(|e| e.code == validation_codes::W_LIMIT_WITHOUT_ORDER_BY));
}

#[test]
fn test_basic_and_schema_validation_share_semantic_diagnostics() {
    let sql = "SELECT * FROM users LIMIT 10";
    let basic = crate::validate_with_options(
        sql,
        DialectType::Generic,
        &crate::ValidationOptions {
            semantic: true,
            ..Default::default()
        },
    );
    let schema = validate_with_schema(
        sql,
        DialectType::Generic,
        &base_schema(),
        &SchemaValidationOptions {
            semantic: true,
            ..Default::default()
        },
    );

    let diagnostics = |result: &ValidationResult| {
        result
            .errors
            .iter()
            .filter(|error| error.code.starts_with('W'))
            .map(|error| {
                (
                    error.code.clone(),
                    error.message.clone(),
                    error.line,
                    error.column,
                    error.start,
                    error.end,
                )
            })
            .collect::<Vec<_>>()
    };
    assert_eq!(diagnostics(&basic), diagnostics(&schema));
}

#[test]
fn test_validate_with_schema_reference_check_valid_column_fk() {
    let mut schema = base_schema();
    mark_primary_key(&mut schema, "users", "id");
    attach_column_fk(&mut schema, "orders", "user_id", "users", "id");

    let opts = SchemaValidationOptions {
        check_references: true,
        ..Default::default()
    };
    let result = validate_with_schema("SELECT 1", DialectType::Generic, &schema, &opts);
    assert!(result.valid, "errors: {:?}", result.errors);
    assert!(result.errors.is_empty());
}

#[test]
fn test_validate_with_schema_reference_check_unknown_target_table() {
    let mut schema = base_schema();
    mark_primary_key(&mut schema, "users", "id");
    attach_column_fk(&mut schema, "orders", "user_id", "missing_users", "id");

    let opts = SchemaValidationOptions {
        check_references: true,
        ..Default::default()
    };
    let result = validate_with_schema("SELECT 1", DialectType::Generic, &schema, &opts);
    assert!(!result.valid);
    assert!(result
        .errors
        .iter()
        .any(|e| e.code == validation_codes::E_INVALID_FOREIGN_KEY_REFERENCE));
}

#[test]
fn test_validate_with_schema_reference_check_unknown_target_column() {
    let mut schema = base_schema();
    mark_primary_key(&mut schema, "users", "id");
    attach_column_fk(&mut schema, "orders", "user_id", "users", "missing_id");

    let opts = SchemaValidationOptions {
        check_references: true,
        ..Default::default()
    };
    let result = validate_with_schema("SELECT 1", DialectType::Generic, &schema, &opts);
    assert!(!result.valid);
    assert!(result
        .errors
        .iter()
        .any(|e| e.code == validation_codes::E_INVALID_FOREIGN_KEY_REFERENCE));
}

#[test]
fn test_validate_with_schema_reference_check_type_mismatch() {
    let mut schema = base_schema();
    mark_primary_key(&mut schema, "users", "id");
    if let Some(orders) = schema.tables.iter_mut().find(|t| t.name == "orders") {
        if let Some(user_id) = orders.columns.iter_mut().find(|c| c.name == "user_id") {
            user_id.data_type = "varchar".to_string();
        }
    }
    attach_column_fk(&mut schema, "orders", "user_id", "users", "id");

    let opts = SchemaValidationOptions {
        check_references: true,
        ..Default::default()
    };
    let result = validate_with_schema("SELECT 1", DialectType::Generic, &schema, &opts);
    assert!(!result.valid);
    assert!(result
        .errors
        .iter()
        .any(|e| e.code == validation_codes::E_INVALID_FOREIGN_KEY_REFERENCE));
}

#[test]
fn test_validate_with_schema_reference_check_non_strict_warning() {
    let mut schema = base_schema();
    attach_column_fk(&mut schema, "orders", "user_id", "missing_users", "id");

    let opts = SchemaValidationOptions {
        check_references: true,
        strict: Some(false),
        ..Default::default()
    };
    let result = validate_with_schema("SELECT 1", DialectType::Generic, &schema, &opts);
    assert!(result.valid);
    assert!(result
        .errors
        .iter()
        .any(|e| e.code == validation_codes::W_WEAK_REFERENCE_INTEGRITY));
}

#[test]
fn test_validate_with_schema_reference_check_ambiguous_unqualified_column() {
    let mut schema = base_schema();
    mark_primary_key(&mut schema, "users", "id");
    attach_column_fk(&mut schema, "orders", "user_id", "users", "id");

    let opts = SchemaValidationOptions {
        check_references: true,
        ..Default::default()
    };
    let result = validate_with_schema(
        "SELECT id FROM users JOIN orders ON users.id = orders.user_id",
        DialectType::Generic,
        &schema,
        &opts,
    );
    assert!(!result.valid);
    assert!(result
        .errors
        .iter()
        .any(|e| e.code == validation_codes::E_AMBIGUOUS_COLUMN_REFERENCE));
}

#[test]
fn test_validate_with_schema_reference_check_ambiguous_unqualified_column_non_strict() {
    let mut schema = base_schema();
    mark_primary_key(&mut schema, "users", "id");
    attach_column_fk(&mut schema, "orders", "user_id", "users", "id");

    let opts = SchemaValidationOptions {
        check_references: true,
        strict: Some(false),
        ..Default::default()
    };
    let result = validate_with_schema(
        "SELECT id FROM users JOIN orders ON users.id = orders.user_id",
        DialectType::Generic,
        &schema,
        &opts,
    );
    assert!(result.valid);
    assert!(result
        .errors
        .iter()
        .any(|e| e.code == validation_codes::W_WEAK_REFERENCE_INTEGRITY));
}

#[test]
fn test_validate_with_schema_reference_check_cartesian_join_warning() {
    let schema = base_schema();
    let opts = SchemaValidationOptions {
        check_references: true,
        ..Default::default()
    };
    let result = validate_with_schema(
        "SELECT users.id FROM users CROSS JOIN orders",
        DialectType::Generic,
        &schema,
        &opts,
    );
    assert!(result.valid);
    assert!(result
        .errors
        .iter()
        .any(|e| e.code == validation_codes::W_CARTESIAN_JOIN));
}

#[test]
fn test_validate_with_schema_reference_check_join_not_using_declared_fk_warning() {
    let mut schema = base_schema();
    mark_primary_key(&mut schema, "users", "id");
    attach_column_fk(&mut schema, "orders", "user_id", "users", "id");

    let opts = SchemaValidationOptions {
        check_references: true,
        ..Default::default()
    };
    let result = validate_with_schema(
        "SELECT users.id FROM users JOIN orders ON users.age = orders.total",
        DialectType::Generic,
        &schema,
        &opts,
    );
    assert!(result.valid);
    assert!(result
        .errors
        .iter()
        .any(|e| e.code == validation_codes::W_JOIN_NOT_USING_DECLARED_REFERENCE));
}

#[test]
fn test_validate_with_schema_reference_check_join_using_declared_fk_no_warning() {
    let mut schema = base_schema();
    mark_primary_key(&mut schema, "users", "id");
    attach_column_fk(&mut schema, "orders", "user_id", "users", "id");

    let opts = SchemaValidationOptions {
        check_references: true,
        ..Default::default()
    };
    let result = validate_with_schema(
        "SELECT users.id FROM users JOIN orders ON users.id = orders.user_id",
        DialectType::Generic,
        &schema,
        &opts,
    );
    assert!(result.valid);
    assert!(!result
        .errors
        .iter()
        .any(|e| e.code == validation_codes::W_JOIN_NOT_USING_DECLARED_REFERENCE));
}

#[test]
fn test_validate_with_schema_type_check_comparison_mismatch() {
    let schema = base_schema();
    let opts = SchemaValidationOptions {
        check_types: true,
        ..Default::default()
    };
    let result = validate_with_schema(
        "SELECT id FROM users WHERE age = 'abc'",
        DialectType::Generic,
        &schema,
        &opts,
    );
    assert!(!result.valid);
    assert!(result
        .errors
        .iter()
        .any(|e| e.code == validation_codes::E_INCOMPATIBLE_COMPARISON_TYPES));
}

#[test]
fn test_validate_with_schema_type_check_arithmetic_mismatch() {
    let schema = base_schema();
    let opts = SchemaValidationOptions {
        check_types: true,
        ..Default::default()
    };
    let result = validate_with_schema(
        "SELECT age + name FROM users",
        DialectType::Generic,
        &schema,
        &opts,
    );
    assert!(!result.valid);
    assert!(result
        .errors
        .iter()
        .any(|e| e.code == validation_codes::E_INVALID_ARITHMETIC_TYPE));
}

#[test]
fn test_validate_with_schema_type_check_function_argument_mismatch() {
    let schema = base_schema();
    let opts = SchemaValidationOptions {
        check_types: true,
        ..Default::default()
    };
    let result = validate_with_schema(
        "SELECT ABS(name) FROM users",
        DialectType::Generic,
        &schema,
        &opts,
    );
    assert!(!result.valid);
    assert!(result
        .errors
        .iter()
        .any(|e| e.code == validation_codes::E_INVALID_FUNCTION_ARGUMENT_TYPE));
}

#[test]
fn test_validate_with_schema_type_check_predicate_mismatch() {
    let schema = base_schema();
    let opts = SchemaValidationOptions {
        check_types: true,
        ..Default::default()
    };
    let result = validate_with_schema(
        "SELECT id FROM users WHERE age + 1",
        DialectType::Generic,
        &schema,
        &opts,
    );
    assert!(!result.valid);
    assert!(result
        .errors
        .iter()
        .any(|e| e.code == validation_codes::E_INVALID_PREDICATE_TYPE));
}

#[test]
fn test_validate_with_schema_type_check_non_strict_warnings() {
    let schema = base_schema();
    let opts = SchemaValidationOptions {
        check_types: true,
        strict: Some(false),
        ..Default::default()
    };
    let result = validate_with_schema(
        "SELECT ABS(name) FROM users",
        DialectType::Generic,
        &schema,
        &opts,
    );
    assert!(result.valid);
    assert!(result
        .errors
        .iter()
        .any(|e| e.code == validation_codes::W_FUNCTION_ARGUMENT_COERCION));
}

#[test]
fn test_validate_with_schema_type_check_setop_arity_mismatch() {
    let schema = base_schema();
    let opts = SchemaValidationOptions {
        check_types: true,
        ..Default::default()
    };
    let result = validate_with_schema(
        "SELECT id FROM users UNION SELECT id, total FROM orders",
        DialectType::Generic,
        &schema,
        &opts,
    );
    assert!(!result.valid);
    assert!(result
        .errors
        .iter()
        .any(|e| e.code == validation_codes::E_SETOP_ARITY_MISMATCH));
}

#[test]
fn test_validate_with_schema_type_check_setop_type_mismatch() {
    let schema = base_schema();
    let opts = SchemaValidationOptions {
        check_types: true,
        ..Default::default()
    };
    let result = validate_with_schema(
        "SELECT age FROM users UNION SELECT name FROM users",
        DialectType::Generic,
        &schema,
        &opts,
    );
    assert!(!result.valid);
    assert!(result
        .errors
        .iter()
        .any(|e| e.code == validation_codes::E_SETOP_TYPE_MISMATCH));
}

#[test]
fn test_validate_with_schema_type_check_insert_values_assignment_mismatch() {
    let schema = base_schema();
    let opts = SchemaValidationOptions {
        check_types: true,
        ..Default::default()
    };
    let result = validate_with_schema(
        "INSERT INTO users (age) VALUES ('abc')",
        DialectType::Generic,
        &schema,
        &opts,
    );
    assert!(!result.valid);
    assert!(result
        .errors
        .iter()
        .any(|e| e.code == validation_codes::E_INVALID_ASSIGNMENT_TYPE));
}

#[test]
fn test_validate_with_schema_type_check_insert_query_assignment_mismatch() {
    let schema = base_schema();
    let opts = SchemaValidationOptions {
        check_types: true,
        ..Default::default()
    };
    let result = validate_with_schema(
        "INSERT INTO users (age) SELECT name FROM users",
        DialectType::Generic,
        &schema,
        &opts,
    );
    assert!(!result.valid);
    assert!(result
        .errors
        .iter()
        .any(|e| e.code == validation_codes::E_INVALID_ASSIGNMENT_TYPE));
}

#[test]
fn test_validate_with_schema_type_check_update_assignment_mismatch() {
    let schema = base_schema();
    let opts = SchemaValidationOptions {
        check_types: true,
        ..Default::default()
    };
    let result = validate_with_schema(
        "UPDATE users SET age = name",
        DialectType::Generic,
        &schema,
        &opts,
    );
    assert!(!result.valid);
    assert!(result
        .errors
        .iter()
        .any(|e| e.code == validation_codes::E_INVALID_ASSIGNMENT_TYPE));
}

#[test]
fn test_validate_with_schema_type_check_update_non_strict_warning() {
    let schema = base_schema();
    let opts = SchemaValidationOptions {
        check_types: true,
        strict: Some(false),
        ..Default::default()
    };
    let result = validate_with_schema(
        "UPDATE users SET age = name",
        DialectType::Generic,
        &schema,
        &opts,
    );
    assert!(result.valid);
    assert!(result
        .errors
        .iter()
        .any(|e| e.code == validation_codes::W_IMPLICIT_CAST_ASSIGNMENT));
}

#[test]
fn test_validate_with_schema_unresolved_table_alias_in_join_on() {
    let schema = base_schema();
    let opts = SchemaValidationOptions::default();
    let result = validate_with_schema(
        "SELECT * FROM users u LEFT JOIN orders o ON u.id = q.user_id",
        DialectType::Generic,
        &schema,
        &opts,
    );
    assert!(!result.valid);
    assert!(result.errors.iter().any(|e| {
        e.code == validation_codes::E_UNRESOLVED_REFERENCE && e.message.contains("q")
    }));
}

#[test]
fn test_validate_with_schema_unresolved_table_alias_in_join_on_non_strict() {
    let schema = base_schema();
    let opts = SchemaValidationOptions {
        strict: Some(false),
        ..Default::default()
    };
    let result = validate_with_schema(
        "SELECT * FROM users u LEFT JOIN orders o ON u.id = q.user_id",
        DialectType::Generic,
        &schema,
        &opts,
    );
    // Non-strict: valid but with a warning
    assert!(result.valid);
    assert!(result.errors.iter().any(|e| {
        e.code == validation_codes::E_UNRESOLVED_REFERENCE
            && e.message.contains("q")
            && e.severity == crate::ValidationSeverity::Warning
    }));
}

#[test]
fn test_validate_with_schema_valid_aliases_in_join_on() {
    let schema = base_schema();
    let opts = SchemaValidationOptions::default();
    let result = validate_with_schema(
        "SELECT * FROM users u LEFT JOIN orders o ON u.id = o.user_id",
        DialectType::Generic,
        &schema,
        &opts,
    );
    assert!(result.valid, "{:?}", result.errors);
}

#[test]
fn test_validate_with_schema_accepts_struct_field_access_issue_408() {
    let schema: ValidationSchema = serde_json::from_value(serde_json::json!({
        "tables": [{
            "name": "source_table",
            "columns": [
                {"name": "nested_items", "type": "STRUCT(field_value VARCHAR)[]"},
                {"name": "composite_value", "type": "STRUCT(field_value VARCHAR, label VARCHAR)"}
            ]
        }]
    }))
    .expect("schema");
    let options = SchemaValidationOptions::default();

    for sql in [
        "SELECT composite_value.field_value AS output_value FROM source_table",
        "SELECT item.field_value AS output_value FROM source_table s \
         CROSS JOIN UNNEST(s.nested_items) AS expanded(item)",
    ] {
        let result = validate_with_schema(sql, DialectType::DuckDB, &schema, &options);
        assert!(
            result.valid,
            "validation failed for {sql:?}: {:?}",
            result.errors
        );
    }

    let negative = validate_with_schema(
        "SELECT missing.field_value FROM source_table",
        DialectType::DuckDB,
        &schema,
        &options,
    );
    assert!(!negative.valid);
    assert!(negative.errors.iter().any(|error| {
        error.code == validation_codes::E_UNRESOLVED_REFERENCE && error.message.contains("missing")
    }));
}
#[test]
fn schema_validation_propagates_complexity_guard_to_both_parses() {
    let schema: ValidationSchema = serde_json::from_value(serde_json::json!({
        "tables": [{"name": "records", "columns": [{"name": "value", "type": "INTEGER"}]}]
    }))
    .unwrap();
    let sql = format!(
        "SELECT {}value{} FROM records",
        "COALESCE(".repeat(65),
        ", 0)".repeat(65)
    );
    assert!(
        !validate_with_schema(
            &sql,
            DialectType::Snowflake,
            &schema,
            &SchemaValidationOptions::default()
        )
        .valid
    );
    for limit in [Some(128), None] {
        let options = SchemaValidationOptions {
            complexity_guard: Some(crate::ComplexityGuardOptions {
                max_function_call_depth: limit,
                ..Default::default()
            }),
            check_types: true,
            check_references: true,
            ..Default::default()
        };
        let result = validate_with_schema(&sql, DialectType::Snowflake, &schema, &options);
        assert!(result.valid, "{:?}", result.errors);
        let invalid = validate_with_schema(
            &sql.replace("value", "missing"),
            DialectType::Snowflake,
            &schema,
            &options,
        );
        assert!(!invalid.valid);
        assert!(invalid.errors.iter().any(|error| error.code == "E201"));
    }
}
