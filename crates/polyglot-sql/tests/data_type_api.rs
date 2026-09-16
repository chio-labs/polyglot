use polyglot_sql::expressions::StructField;
use polyglot_sql::{generate_data_type, parse_data_type, DataType, DialectType};

fn named_struct(name: &str, data_type: DataType) -> DataType {
    DataType::Struct {
        fields: vec![StructField::new(name.to_owned(), data_type)],
        nested: false,
    }
}

fn int_type() -> DataType {
    DataType::Int {
        length: None,
        integer_spelling: false,
    }
}

#[test]
fn generate_struct_field_names_as_identifiers() {
    for (name, identifier) in [
        ("field name", r#""field name""#),
        ("normal", "normal"),
        ("select", r#""select""#),
        ("1st", r#""1st""#),
        ("a-b", r#""a-b""#),
        ("a.b", r#""a.b""#),
        ("a\"b", r#""a""b""#),
        ("a`b", r#""a`b""#),
        ("a INT, b", r#""a INT, b""#),
        ("a(16)", r#""a(16)""#),
        ("a DESC", r#""a DESC""#),
        ("a ASC", r#""a ASC""#),
        ("two\nlines", "\"two\nlines\""),
        (r"a\b", r#""a\b""#),
        (r#""field name""#, r#""field name""#),
        (r#""a""b""#, r#""a""b""#),
        // Older parsed ASTs retained delimiters but did not double internal quotes.
        (r#""a"b""#, r#""a""b""#),
        (r#""""x""""#, r#""""x""""#),
    ] {
        let data_type = named_struct(
            name,
            DataType::VarChar {
                length: None,
                parenthesized_length: false,
            },
        );
        let sql = generate_data_type(&data_type, DialectType::DuckDB).unwrap();
        assert_eq!(sql, format!("STRUCT({identifier} TEXT)"), "{name:?}");
        let parsed = parse_data_type(&sql, DialectType::DuckDB).unwrap();
        let DataType::Struct { fields, .. } = &parsed else {
            panic!("{parsed:?}")
        };
        assert_eq!(fields.len(), 1, "{name:?}");
        assert_eq!(fields[0].name, identifier, "{name:?}");
        assert_eq!(
            generate_data_type(&parsed, DialectType::DuckDB).unwrap(),
            sql
        );
    }
}

#[test]
fn struct_fields_use_target_dialect_quotes() {
    for (dialect, expected) in [
        (DialectType::DuckDB, "STRUCT(\"field name\" INT)"),
        (DialectType::BigQuery, "STRUCT<`field name` INT64>"),
        (DialectType::Spark, "STRUCT<`field name`: INT>"),
        (DialectType::Hive, "STRUCT<`field name`: INT>"),
        (DialectType::Databricks, "STRUCT<`field name`: INT>"),
        (DialectType::Presto, "ROW(\"field name\" INTEGER)"),
        (DialectType::Trino, "ROW(\"field name\" INTEGER)"),
        (DialectType::Snowflake, "OBJECT(\"field name\" INT)"),
        (
            DialectType::ClickHouse,
            "Tuple(\"field name\" Nullable(Int32))",
        ),
        (DialectType::SingleStore, "RECORD(`field name` INT)"),
    ] {
        for name in [
            "field name",
            "\"field name\"",
            "`field name`",
            "[field name]",
        ] {
            let data_type = named_struct(name, int_type());
            assert_eq!(
                generate_data_type(&data_type, dialect).unwrap(),
                expected,
                "{dialect:?}: {name:?}"
            );
        }
    }
}

#[test]
fn quoted_type_fields_roundtrip_without_losing_escapes() {
    for (dialect, sql) in [
        (DialectType::DuckDB, r#"STRUCT("a""b" INT, """x""" TEXT)"#),
        (
            DialectType::DuckDB,
            r#"UNION("a""b" INT, "field name" TEXT)"#,
        ),
        (
            DialectType::Snowflake,
            r#"OBJECT("a""b" INT NOT NULL, "MiXeD" INT)"#,
        ),
        (DialectType::Spark, "STRUCT<`a``b`: INT>"),
        (DialectType::Spark, r"STRUCT<`a\`: INT>"),
        (DialectType::Hive, r"STRUCT<`a\`: INT>"),
        (DialectType::BigQuery, "STRUCT<`a\\`b` INT64>"),
        (DialectType::BigQuery, r"STRUCT<`a\\b` INT64>"),
        (DialectType::BigQuery, r"STRUCT<`a\nb` INT64>"),
    ] {
        let parsed = parse_data_type(sql, dialect).unwrap();
        let generated = generate_data_type(&parsed, dialect).unwrap();
        assert_eq!(generated, sql, "{dialect:?}");
        assert_eq!(parse_data_type(&generated, dialect).unwrap(), parsed);
    }
}

#[test]
fn nested_and_anonymous_struct_fields_preserve_structure() {
    let data_type = named_struct("outer field", named_struct("a\"b", int_type()));
    let sql = generate_data_type(&data_type, DialectType::DuckDB).unwrap();
    assert_eq!(sql, r#"STRUCT("outer field" STRUCT("a""b" INT))"#);
    let parsed = parse_data_type(&sql, DialectType::DuckDB).unwrap();
    assert_eq!(
        generate_data_type(&parsed, DialectType::Spark).unwrap(),
        "STRUCT<`outer field`: STRUCT<`a\"b`: INT>>"
    );
    assert_eq!(
        generate_data_type(&parsed, DialectType::BigQuery).unwrap(),
        "STRUCT<`outer field` STRUCT<`a\"b` INT64>>"
    );
    let anonymous = named_struct("", int_type());
    assert_eq!(
        generate_data_type(&anonymous, DialectType::BigQuery).unwrap(),
        "STRUCT<INT64>"
    );
}

#[test]
fn union_and_object_fields_use_identifier_generation() {
    for (dialect, data_type, expected) in [
        (
            DialectType::DuckDB,
            DataType::Union {
                fields: vec![("field name".into(), int_type())],
            },
            r#"UNION("field name" INT)"#,
        ),
        (
            DialectType::Snowflake,
            DataType::Object {
                fields: vec![("field name".into(), int_type(), true)],
                modifier: None,
            },
            r#"OBJECT("field name" INT NOT NULL)"#,
        ),
    ] {
        assert_eq!(generate_data_type(&data_type, dialect).unwrap(), expected);
        let parsed = parse_data_type(expected, dialect).unwrap();
        assert_eq!(generate_data_type(&parsed, dialect).unwrap(), expected);
    }
}

#[test]
fn parse_standalone_decimal_type() {
    let data_type =
        parse_data_type("DECIMAL(10, 2)", DialectType::DuckDB).expect("decimal should parse");

    assert_eq!(
        data_type,
        DataType::Decimal {
            precision: Some(10),
            scale: Some(2),
        }
    );
}

#[test]
fn render_standalone_data_type_for_target_dialect() {
    let data_type =
        parse_data_type("VARCHAR(255)", DialectType::DuckDB).expect("varchar should parse");

    assert_eq!(
        generate_data_type(&data_type, DialectType::DuckDB).expect("duckdb render"),
        "TEXT(255)"
    );
    assert_eq!(
        generate_data_type(&data_type, DialectType::PostgreSQL).expect("postgres render"),
        "VARCHAR(255)"
    );
}

#[test]
fn parse_standalone_array_type() {
    let data_type = parse_data_type("INT[]", DialectType::DuckDB).expect("array should parse");

    match data_type {
        DataType::Array {
            element_type,
            dimension,
        } => {
            assert_eq!(
                *element_type,
                DataType::Int {
                    length: None,
                    integer_spelling: false,
                }
            );
            assert_eq!(dimension, None);
        }
        other => panic!("expected array data type, got {other:?}"),
    }
}

#[test]
fn parse_standalone_struct_type() {
    let data_type = parse_data_type("STRUCT(a INT, b VARCHAR)", DialectType::DuckDB)
        .expect("struct should parse");

    assert_eq!(
        generate_data_type(&data_type, DialectType::DuckDB).expect("duckdb struct render"),
        "STRUCT(a INT, b TEXT)"
    );
}

#[test]
fn parse_standalone_custom_type_preserves_name() {
    let data_type =
        parse_data_type("MyCustomType", DialectType::DuckDB).expect("custom type should parse");

    assert_eq!(
        data_type,
        DataType::Custom {
            name: "MyCustomType".to_string(),
        }
    );
}

#[test]
fn parse_standalone_data_type_rejects_trailing_sql() {
    let error = parse_data_type("DECIMAL(10, 2) SELECT 1", DialectType::DuckDB)
        .expect_err("trailing SQL should fail");

    assert!(error
        .to_string()
        .contains("Unexpected token after data type"));
}
