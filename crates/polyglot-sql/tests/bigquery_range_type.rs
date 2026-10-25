//! BigQuery RANGE<T> types parse wherever a data type does.

use polyglot_sql::{
    parse_data_type, transpile, validate, validate_with_schema, DialectType,
    SchemaValidationOptions, ValidationSchema,
};

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

#[test]
fn casts_to_bigquery_range_types_pass_type_checks() {
    let schema = ValidationSchema {
        tables: vec![],
        strict: None,
    };
    let options = SchemaValidationOptions {
        semantic: true,
        check_types: true,
        ..Default::default()
    };
    for sql in [
        "SELECT CAST(NULL AS RANGE<TIMESTAMP>) AS r",
        "SELECT SAFE_CAST(NULL AS RANGE<DATE>) AS r",
    ] {
        let result = validate_with_schema(sql, DialectType::BigQuery, &schema, &options);
        assert!(result.errors.is_empty(), "{sql}: {:?}", result.errors);
    }
}
