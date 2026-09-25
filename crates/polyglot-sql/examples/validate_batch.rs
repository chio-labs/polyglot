//! Validate newline-delimited schema-aware requests from standard input.
use polyglot_sql::validation::{validate_with_schema, SchemaValidationOptions, ValidationSchema};
use polyglot_sql::DialectType;
use serde::Deserialize;
use std::io::{self, BufRead};

#[derive(Deserialize)]
struct Request {
    sql: String,
    dialect: String,
    schema: ValidationSchema,
    options: SchemaValidationOptions,
}

fn main() {
    for line in io::stdin().lock().lines() {
        let request: Request = serde_json::from_str(&line.unwrap()).unwrap();
        let dialect: DialectType = request.dialect.parse().unwrap();
        let result = validate_with_schema(&request.sql, dialect, &request.schema, &request.options);
        println!("{}", serde_json::to_string(&result).unwrap());
    }
}
