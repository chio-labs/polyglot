//! BigQuery RANGE<T> types parse wherever a data type does.

use polyglot_sql::{parse_data_type, transpile, validate, DialectType};

#[test]
fn bigquery_range_types_parse_and_round_trip() {
    for element in ["DATE", "DATETIME", "TIMESTAMP"] {
        let type_sql = format!("RANGE<{element}>");
        assert!(
            parse_data_type(&type_sql, DialectType::BigQuery).is_ok(),
            "{type_sql}"
        );
        let sql = format!("SELECT CAST(NULL AS {type_sql}) AS r");
        assert!(validate(&sql, DialectType::BigQuery).valid, "{sql}");
        let generated = transpile(&sql, DialectType::BigQuery, DialectType::BigQuery)
            .unwrap()
            .remove(0);
        assert_eq!(generated, sql);
    }
    assert!(parse_data_type("RANGE<INT64>", DialectType::BigQuery).is_err());
}
