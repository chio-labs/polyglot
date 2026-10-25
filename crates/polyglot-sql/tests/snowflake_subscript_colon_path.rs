//! A Snowflake bracket subscript may be followed by a `:` path.

use polyglot_sql::{transpile, validate, DialectType};

#[test]
fn bracket_subscript_followed_by_colon_path_parses() {
    let cases: [(&str, &str); 4] = [
        ("SELECT o.fields[0]:name FROM objects AS o", "GET_PATH(o.fields[0], 'name')"),
        ("SELECT o.fields['k']:name FROM objects AS o", "GET_PATH(o.fields['k'], 'name')"),
        (
            "SELECT o.fields[r.index::VARCHAR]:name::VARCHAR AS name FROM objects AS o, LATERAL FLATTEN(input => o.items) AS r",
            "CAST(GET_PATH(o.fields[CAST(r.index AS VARCHAR)], 'name') AS VARCHAR) AS name",
        ),
        ("SELECT o.fields[0]:a.b FROM objects AS o", "GET_PATH(o.fields[0], 'a.b')"),
    ];
    for (sql, expected_fragment) in cases {
        assert!(validate(sql, DialectType::Snowflake).valid, "{sql}");
        let generated: String = transpile(sql, DialectType::Snowflake, DialectType::Snowflake)
            .unwrap()
            .remove(0);
        assert!(
            generated.contains(expected_fragment),
            "{sql}\n=> {generated}"
        );
    }
}
