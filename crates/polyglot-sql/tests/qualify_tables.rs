use polyglot_sql::{qualify_tables, Generator, Parser, QualifyTablesOptions};

#[test]
fn generated_subquery_aliases_avoid_explicit_virtual_source_aliases() {
    let test_cases = [
        "SELECT * FROM (SELECT 1 AS id) CROSS JOIN UNNEST([1, 2]) AS _0",
        "SELECT * FROM (SELECT 1 AS id) CROSS JOIN products UNPIVOT(value FOR attribute IN (name)) AS _0",
        "SELECT * FROM (SELECT 1 AS id) LATERAL VIEW EXPLODE(ARRAY(1, 2)) _0 AS value",
    ];
    let options = QualifyTablesOptions::new()
        .with_alias_unaliased_tables(false)
        .with_alias_unaliased_subqueries(true);

    for sql in test_cases {
        let parsed = Parser::parse_sql(sql).expect("virtual-source fixture should parse");
        let qualified = qualify_tables(parsed[0].clone(), &options);
        let generated = Generator::new()
            .generate(&qualified)
            .expect("qualified fixture should render");

        assert!(
            generated.contains(") AS _1"),
            "generated subquery alias should avoid the explicit _0 source: {generated}"
        );
    }
}
