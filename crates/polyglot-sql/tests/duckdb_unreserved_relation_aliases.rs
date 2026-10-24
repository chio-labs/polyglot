use polyglot_sql::{generate, parse_one, DialectType, Expression};

#[test]
fn duckdb_accepts_unreserved_relation_aliases() {
    for name in ["owned", "match", "final"] {
        for alias in [name.to_string(), format!("AS {name}")] {
            for relation in ["samples", "(SELECT id FROM samples)", "UNNEST([1, 2])"] {
                let sql =
                    format!("SELECT {name}.id FROM {relation} {alias}(id) WHERE {name}.id > 1");
                let expr = parse_one(&sql, DialectType::DuckDB)
                    .unwrap_or_else(|error| panic!("{sql}: {error}"));
                let output = generate(&expr, DialectType::DuckDB).unwrap();
                parse_one(&output, DialectType::DuckDB)
                    .unwrap_or_else(|error| panic!("{output}: {error}"));
                let Expression::Select(select) = expr else {
                    panic!("expected SELECT")
                };
                assert!(select.where_clause.is_some(), "lost WHERE in {output}");
                let source = &select.from.as_ref().unwrap().expressions[0];
                let (actual, columns) = match source {
                    Expression::Table(table) => {
                        (table.alias.as_ref().unwrap(), &table.column_aliases)
                    }
                    Expression::Subquery(query) => {
                        (query.alias.as_ref().unwrap(), &query.column_aliases)
                    }
                    Expression::Alias(alias) => (&alias.alias, &alias.column_aliases),
                    other => panic!("unexpected source: {other:?}"),
                };
                assert_eq!(actual.name, name);
                assert!(!actual.quoted);
                assert_eq!(columns.len(), 1);
                assert_eq!(columns[0].name, "id");
            }
        }
    }
}

#[test]
fn duckdb_keeps_keyword_aliases_in_join_and_correlated_scopes() {
    for sql in [
        "SELECT t.id FROM samples t JOIN permissions match ON match.id = t.id",
        "SELECT t.id FROM samples t WHERE EXISTS (SELECT 1 FROM permissions owned WHERE owned.id = t.id)",
    ] {
        let expr = parse_one(sql, DialectType::DuckDB).unwrap();
        let output = generate(&expr, DialectType::DuckDB).unwrap();
        parse_one(&output, DialectType::DuckDB).unwrap();
        assert!(output.contains("permissions"), "{output}");
    }
}

#[test]
fn duckdb_clause_boundaries_are_not_relation_aliases() {
    for sql in [
        "SELECT id FROM samples WHERE id > 1",
        "SELECT id FROM samples GROUP BY id ORDER BY id LIMIT 1",
        "SELECT id FROM samples UNION ALL SELECT id FROM permissions",
        "SELECT * FROM samples POSITIONAL JOIN permissions",
    ] {
        let expr = parse_one(sql, DialectType::DuckDB).unwrap();
        let output = generate(&expr, DialectType::DuckDB).unwrap();
        parse_one(&output, DialectType::DuckDB).unwrap();
    }
}

#[test]
fn quoted_keyword_relation_aliases_keep_their_spelling() {
    for name in ["Owned", "Match"] {
        for keyword in ["", "AS "] {
            let sql = format!("SELECT \"{name}\".id FROM samples {keyword}\"{name}\"");
            let Expression::Select(query) = parse_one(&sql, DialectType::DuckDB).unwrap() else {
                panic!("expected SELECT")
            };
            let Expression::Table(table) = &query.from.unwrap().expressions[0] else {
                panic!("expected table")
            };
            let alias = table.alias.as_ref().unwrap();
            assert_eq!(alias.name, name);
            assert!(alias.quoted);
        }
    }
}

#[test]
fn clickhouse_keeps_final_as_a_table_modifier() {
    for sql in [
        "SELECT id FROM samples FINAL",
        "SELECT s.id FROM samples AS s FINAL",
    ] {
        let Expression::Select(query) = parse_one(sql, DialectType::ClickHouse).unwrap() else {
            panic!("expected SELECT")
        };
        let Expression::Table(table) = &query.from.unwrap().expressions[0] else {
            panic!("expected table")
        };
        assert!(table.final_);
    }
}
