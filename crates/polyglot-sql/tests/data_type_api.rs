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

/// A caller-supplied `TokenType::Eof` is end of input for the standalone type parser too.
///
/// `parse_data_type` above goes through a string, so it cannot carry the variant; this uses
/// the token-stream entry point, which is what a caller reaching `Parser::new` has. The
/// terminator itself is not input, so `INT` with one appended is `INT` — and tokens *after*
/// a terminator are refused rather than silently dropped, which is the case that regressed
/// when the end-of-input check recognised the variant but the trailing-token check did not.
#[test]
fn parse_standalone_data_type_treats_an_eof_token_as_end_of_input() {
    use polyglot_sql::tokens::Span;
    use polyglot_sql::{Parser, Token, TokenType, Tokenizer};

    let stream = |parts: &[&str]| {
        let mut out: Vec<Token> = Vec::new();
        for part in parts {
            if *part == "<EOF>" {
                out.push(Token::new(TokenType::Eof, "", Span::default()));
            } else {
                out.extend(Tokenizer::default().tokenize(part).expect("tokenizing"));
            }
        }
        out
    };

    // A trailing terminator is not a trailing token.
    assert_eq!(
        Parser::new(stream(&["INT", "<EOF>"]))
            .parse_standalone_data_type()
            .expect("a trailing terminator is end of input"),
        int_type()
    );
    assert_eq!(
        Parser::new(stream(&["INT"]))
            .parse_standalone_data_type()
            .expect("and so is the end of the stream"),
        int_type()
    );

    // Tokens after one are a stream built wrongly. Refused, not dropped: the parse used to
    // stop at the terminator and return `Ok(Int)` with `SELECT 2` ignored.
    let error = Parser::new(stream(&["INT", "<EOF>", "SELECT 2"]))
        .parse_standalone_data_type()
        .expect_err("tokens after the terminator should fail");
    assert!(
        error
            .to_string()
            .contains("Unexpected token after end of input"),
        "unexpected error: {error}"
    );
}

/// A terminator at the *front* leaves the standalone type parser nothing to read.
///
/// Normalizing the stream in the constructor truncates at the terminator, so these are all
/// empty by the time the parse begins. They panicked with `Token list should not be empty`
/// before the empty case was handled; `Parser::new(Vec::new())` panicked the same way even
/// before the terminator was normalized at all.
#[test]
fn parse_standalone_data_type_errors_on_an_empty_or_eof_first_stream() {
    use polyglot_sql::tokens::Span;
    use polyglot_sql::{Parser, Token, TokenType, Tokenizer};

    let eof = || Token::new(TokenType::Eof, "", Span::default());

    // Nothing to read: end of input, not a panic and not a type.
    for tokens in [vec![eof()], Vec::new()] {
        let error = Parser::new(tokens)
            .parse_standalone_data_type()
            .expect_err("an empty stream is not a data type");
        assert!(
            error.to_string().contains("Unexpected end of input"),
            "unexpected error: {error}"
        );
    }

    // A terminator with a type after it is a stream built wrongly, and the error names the
    // token that followed rather than the empty remainder.
    let mut leading = vec![eof()];
    leading.extend(Tokenizer::default().tokenize("INT").expect("tokenizing"));
    let error = Parser::new(leading)
        .parse_standalone_data_type()
        .expect_err("a type after the terminator should not be read");
    assert!(
        error
            .to_string()
            .contains("Unexpected token after end of input"),
        "unexpected error: {error}"
    );

    // Two terminators: the second is a token after the first.
    let error = Parser::new(vec![eof(), eof()])
        .parse_standalone_data_type()
        .expect_err("a second terminator is a token after the first");
    assert!(
        error
            .to_string()
            .contains("Unexpected token after end of input"),
        "unexpected error: {error}"
    );
}
