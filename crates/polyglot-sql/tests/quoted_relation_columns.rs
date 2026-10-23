use polyglot_sql::{generate, parse_one, transpile, DialectType, Expression};

#[test]
fn quoted_relation_columns_survive_parse_generate_and_reparse() {
    for dialect in [DialectType::DuckDB, DialectType::PostgreSQL] {
        for sql in [
            "SELECT * FROM (SELECT 1 AS x) AS d(\"a b\")",
            "SELECT * FROM (SELECT 1 AS x) d(\"a b\")",
            "SELECT * FROM records AS d(\"a b\")",
            "SELECT * FROM records d(\"a b\")",
            "WITH d(\"a b\") AS (SELECT 1 AS x) SELECT * FROM d",
            "SELECT * FROM (VALUES (1)) AS d(\"a b\")",
            "SELECT * FROM UNNEST([1]) AS d(\"a b\")",
            "SELECT * FROM LATERAL UNNEST([1]) AS d(\"a b\")",
        ] {
            let parsed = parse_one(sql, dialect).unwrap();
            let output = generate(&parsed, dialect).unwrap();
            assert!(output.contains("\"a b\""), "{sql}: {output}");
            let reparsed = parse_one(&output, dialect).unwrap();
            assert_eq!(generate(&reparsed, dialect).unwrap(), output);
        }
    }
}

#[test]
fn lateral_relation_and_column_alias_quotes_survive_json() {
    for dialect in [DialectType::DuckDB, DialectType::PostgreSQL] {
        for alias in [
            "AS \"item rows\"(\"unit value\")",
            "\"item rows\"(\"unit value\")",
            "AS \"Items\"(\"Value\")",
            "AS d(\"a\"\"b\")",
            "AS d(unquoted, \"unit value\")",
        ] {
            let sql = format!("SELECT * FROM LATERAL UNNEST(ARRAY[1]) {alias}");
            let parsed = parse_one(&sql, dialect).unwrap();
            let output = generate(&parsed, dialect).unwrap();
            assert!(
                output.contains(alias.trim_start_matches("AS ")),
                "{sql}: {output}"
            );
            let json = serde_json::to_string(&parsed).unwrap();
            let restored = serde_json::from_str(&json).unwrap();
            assert_eq!(generate(&restored, dialect).unwrap(), output);
        }
    }
}

#[test]
fn unquoted_lateral_aliases_keep_the_existing_json_shape() {
    let parsed = parse_one(
        "SELECT * FROM LATERAL UNNEST(ARRAY[1]) AS items(value)",
        DialectType::DuckDB,
    )
    .unwrap();
    let json = serde_json::to_string(&parsed).unwrap();
    assert!(!json.contains("column_aliases_quoted"));
    let restored = serde_json::from_str(&json).unwrap();
    assert_eq!(
        generate(&restored, DialectType::DuckDB).unwrap(),
        generate(&parsed, DialectType::DuckDB).unwrap()
    );
}

#[test]
fn quoted_aliases_do_not_interpret_index_or_order_suffixes() {
    for dialect in [DialectType::DuckDB, DialectType::PostgreSQL] {
        for alias in [
            r#"AS items("value(1)")"#,
            r#"AS items("value ASC")"#,
            r#"AS items("value DESC")"#,
            r#"AS "items(1)"(value)"#,
            r#"AS "items ASC"(value)"#,
            r#"AS "items DESC"("value(1)")"#,
        ] {
            for relation in ["LATERAL UNNEST(ARRAY[1])", "(VALUES (1))", "(SELECT 1)"] {
                let sql = format!("SELECT * FROM {relation} {alias}");
                let parsed = parse_one(&sql, dialect).unwrap();
                let json = serde_json::to_string(&parsed).unwrap();
                let restored = serde_json::from_str(&json).unwrap();
                let output = generate(&restored, dialect).unwrap();
                assert!(output.contains(alias), "{sql}: {output}");
                let reparsed = parse_one(&output, dialect).unwrap();
                assert_eq!(generate(&reparsed, dialect).unwrap(), output);
            }
        }
    }
}

#[test]
fn quoted_alias_references_keep_complete_identifier_names() {
    for dialect in [DialectType::DuckDB, DialectType::PostgreSQL] {
        for (relation, column) in [("items(1)", "value(1)"), ("items DESC", "value ASC")] {
            let sql = format!(
                r#"SELECT "{relation}"."{column}" FROM LATERAL UNNEST(ARRAY[1]) AS "{relation}"("{column}")"#
            );
            let parsed = parse_one(&sql, dialect).unwrap();
            let output = generate(&parsed, dialect).unwrap();
            assert!(
                output.starts_with(&format!(r#"SELECT "{relation}"."{column}""#)),
                "{output}"
            );
            assert!(parse_one(&output, dialect).is_ok());
        }
    }
}

#[test]
fn index_column_prefixes_and_ordering_keep_their_syntax() {
    for sql in [
        "CREATE TABLE t (name VARCHAR(64), PRIMARY KEY (`name`(16) DESC))",
        "CREATE TABLE t (name VARCHAR(64), UNIQUE (`name`(16) ASC))",
    ] {
        let parsed = parse_one(sql, DialectType::MySQL).unwrap();
        let output = generate(&parsed, DialectType::MySQL).unwrap();
        assert!(output.contains("`name`(16)"), "{output}");
        assert!(parse_one(&output, DialectType::MySQL).is_ok());
    }
}

#[test]
fn lateral_ordinality_is_emitted_once_with_quoted_aliases() {
    for dialect in [DialectType::DuckDB, DialectType::PostgreSQL] {
        for function in ["UNNEST(ARRAY[1, 2])", "GENERATE_SERIES(1, 2)"] {
            for alias in ["AS ", ""] {
                let sql = format!(
                    "SELECT \"item rows\".\"row number\" FROM LATERAL {function} WITH ORDINALITY {alias}\"item rows\"(\"unit value\", \"row number\")"
                );
                let parsed = parse_one(&sql, dialect).unwrap();
                let json = serde_json::to_string(&parsed).unwrap();
                let restored = serde_json::from_str(&json).unwrap();
                for output in [
                    generate(&restored, dialect).unwrap(),
                    transpile(&sql, dialect, dialect).unwrap().remove(0),
                ] {
                    assert_eq!(output.matches("WITH ORDINALITY").count(), 1, "{output}");
                    assert!(output.contains("\"item rows\"(\"unit value\", \"row number\")"));
                    let reparsed = parse_one(&output, dialect).unwrap();
                    assert_eq!(generate(&reparsed, dialect).unwrap(), output);
                }
            }
        }
    }
}

#[test]
fn quoted_constraint_keys_keep_literal_suffixes_separate_from_modifiers() {
    for dialect in [
        DialectType::MySQL,
        DialectType::DuckDB,
        DialectType::PostgreSQL,
    ] {
        let quote = if dialect == DialectType::MySQL {
            '`'
        } else {
            '"'
        };
        for name in ["name(1)", "name ASC", "name DESC", "a`b(1)", "a\"b DESC"] {
            let identifier = format!(
                "{quote}{}{quote}",
                name.replace(quote, &format!("{quote}{quote}"))
            );
            for key in ["PRIMARY KEY", "UNIQUE"] {
                let suffixes: &[&str] = if dialect == DialectType::MySQL {
                    &["", " ASC", " DESC", "(16)", "(16) DESC"]
                } else {
                    &[""]
                };
                for suffix in suffixes {
                    let sql = format!(
                        "CREATE TABLE t ({identifier} VARCHAR(64), {key} ({identifier}{suffix}))"
                    );
                    let parsed = parse_one(&sql, dialect).unwrap();
                    let json = serde_json::to_string(&parsed).unwrap();
                    let restored = serde_json::from_str(&json).unwrap();
                    let output = generate(&restored, dialect).unwrap();
                    assert!(
                        output.contains(&format!("{key} ({identifier}{suffix})")),
                        "{sql}: {output}"
                    );
                    assert_eq!(
                        generate(&parse_one(&output, dialect).unwrap(), dialect).unwrap(),
                        output
                    );
                }
            }
        }
    }
}

#[test]
fn structured_constraint_keys_preserve_names_during_normalization() {
    let sql = "CREATE TABLE t (`name DESC` INT, PRIMARY KEY (`name DESC` DESC))";
    for target in [
        DialectType::Spark,
        DialectType::Databricks,
        DialectType::Hive,
    ] {
        let output = transpile(sql, DialectType::MySQL, target)
            .unwrap()
            .remove(0);
        assert!(output.contains("PRIMARY KEY (`name DESC`)"), "{output}");
        assert!(!output.contains("`name DESC` DESC"), "{output}");
    }
    let sql = "CREATE TABLE t (`time DESC` TIMESTAMP, PRIMARY KEY (`time DESC` TIMESERIES))";
    for output in [
        generate(
            &parse_one(sql, DialectType::Databricks).unwrap(),
            DialectType::Databricks,
        )
        .unwrap(),
        transpile(sql, DialectType::Databricks, DialectType::Databricks)
            .unwrap()
            .remove(0),
    ] {
        assert!(
            output.contains("PRIMARY KEY (`time DESC` TIMESERIES)"),
            "{output}"
        );
        assert!(parse_one(&output, DialectType::Databricks).is_ok());
    }
    let sql = "CREATE TABLE t (\"id\" INTEGER, PRIMARY KEY (\"id\" DESC))";
    let output = generate(
        &parse_one(sql, DialectType::SQLite).unwrap(),
        DialectType::SQLite,
    )
    .unwrap();
    assert!(output.contains("PRIMARY KEY (\"id\" DESC)"), "{output}");
}

#[test]
fn constraint_index_keys_accept_legacy_json_and_keep_simple_json_shape() {
    for (kind, keyword) in [("PrimaryKey", "PRIMARY KEY"), ("Unique", "UNIQUE")] {
        let sql = format!("CREATE TABLE t (name VARCHAR(64), {keyword} (name))");
        let parsed = parse_one(&sql, DialectType::MySQL).unwrap();
        let mut json = serde_json::to_value(&parsed).unwrap();
        let key = &mut json["create_table"]["constraints"][0][kind];
        assert!(key.get("key_parts").is_none());
        // Historical ASTs stored prefix/order syntax inside the identifier.
        key["columns"][0]["name"] = serde_json::json!("name(16) DESC");
        key["columns"][0]["quoted"] = serde_json::json!(true);
        let restored: Expression = serde_json::from_value(json).unwrap();
        let output = generate(&restored, DialectType::MySQL).unwrap();
        assert!(
            output.contains(&format!("{keyword} (`name`(16) DESC)")),
            "{output}"
        );
        assert!(parse_one(&output, DialectType::MySQL).is_ok());
    }
}
