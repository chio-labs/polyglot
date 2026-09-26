//! Replays engine-recorded DuckDB and PostgreSQL result types.
//!
//! The fixtures under `tests/fixtures/engine_types` were produced by
//! `tools/engine-type-truth/generate.py`, which asks the real engines for the
//! bound type of every synthetic expression. Polyglot must report the same type
//! or no type at all; a wrong concrete type becomes a false schema change in
//! downstream consumers. Unknown results are accepted only where listed in
//! [`DOCUMENTED_UNKNOWNS`], and engine-valid SQL must never fail validation.

use std::collections::BTreeMap;

use polyglot_sql::{
    analyze_query, compile_required_query_analysis, validate_with_schema, AnalyzeQueryOptions,
    DialectType, SchemaValidationOptions, ValidationSchema, ValidationSeverity,
};

#[derive(serde::Deserialize)]
struct Fixture {
    engine: String,
    schema: ValidationSchema,
    cases: Vec<Case>,
}

#[derive(serde::Deserialize)]
struct Case {
    id: String,
    category: String,
    sql: String,
    #[serde(rename = "type")]
    engine_type: Option<String>,
}

/// Engine types that Polyglot deliberately leaves unknown, with the reason.
/// Entries match a case ID prefix within one engine.
const DOCUMENTED_UNKNOWNS: &[(&str, &str, &str)] = &[];

fn canonical_duckdb(raw: &str) -> String {
    let upper = raw.trim().to_ascii_uppercase().replace(", ", ",");
    if let Some(element) = upper.strip_suffix("[]") {
        return format!("{}[]", canonical_duckdb(element));
    }
    let (base, params) = match upper.split_once('(') {
        Some((base, rest)) => (
            base.trim().to_string(),
            Some(rest.trim_end_matches(')').to_string()),
        ),
        None => (upper.clone(), None),
    };
    match base.as_str() {
        "DECIMAL" | "NUMERIC" | "DEC" => match params.as_deref() {
            None => "DECIMAL(18,3)".into(),
            Some(p) if !p.contains(',') => format!("DECIMAL({p},0)"),
            Some(p) => format!("DECIMAL({p})"),
        },
        "TINYINT" | "INT1" => "TINYINT".into(),
        "SMALLINT" | "INT2" | "SHORT" => "SMALLINT".into(),
        "INTEGER" | "INT" | "INT4" | "SIGNED" => "INTEGER".into(),
        "BIGINT" | "INT8" | "LONG" => "BIGINT".into(),
        "HUGEINT" | "INT128" => "HUGEINT".into(),
        "UTINYINT" | "UINT8" => "UTINYINT".into(),
        "USMALLINT" | "UINT16" => "USMALLINT".into(),
        "UINTEGER" | "UINT32" => "UINTEGER".into(),
        "UBIGINT" | "UINT64" => "UBIGINT".into(),
        "UHUGEINT" | "UINT128" => "UHUGEINT".into(),
        "FLOAT" | "FLOAT4" | "REAL" => "FLOAT".into(),
        "DOUBLE" | "FLOAT8" | "DOUBLE PRECISION" => "DOUBLE".into(),
        "VARCHAR" | "TEXT" | "STRING" | "CHAR" | "BPCHAR" | "CHARACTER VARYING" => "VARCHAR".into(),
        "BOOLEAN" | "BOOL" => "BOOLEAN".into(),
        "TIMESTAMP" | "DATETIME" | "TIMESTAMP WITHOUT TIME ZONE" => "TIMESTAMP".into(),
        "TIMESTAMPTZ" | "TIMESTAMP WITH TIME ZONE" => "TIMESTAMPTZ".into(),
        "TIME" | "TIME WITHOUT TIME ZONE" => "TIME".into(),
        "TIMETZ" | "TIME WITH TIME ZONE" => "TIMETZ".into(),
        "BLOB" | "BYTEA" | "VARBINARY" => "BLOB".into(),
        _ => match params {
            Some(p) => format!("{base}({p})"),
            None => base,
        },
    }
}

fn canonical_postgres(raw: &str) -> String {
    let upper = raw.trim().to_ascii_uppercase().replace(", ", ",");
    if let Some(element) = upper.strip_suffix("[]") {
        return format!("{}[]", canonical_postgres(element));
    }
    let (base, params) = match upper.split_once('(') {
        Some((base, rest)) => {
            let (params, suffix) = rest.split_once(')').unwrap_or((rest, ""));
            (
                format!("{}{}", base.trim(), suffix).trim().to_string(),
                Some(params.to_string()),
            )
        }
        None => (upper.clone(), None),
    };
    let with_params = |name: &str| match &params {
        Some(p) => format!("{name}({p})"),
        None => name.to_string(),
    };
    match base.as_str() {
        "SMALLINT" | "INT2" => "SMALLINT".into(),
        "INTEGER" | "INT" | "INT4" => "INTEGER".into(),
        "BIGINT" | "INT8" => "BIGINT".into(),
        "NUMERIC" | "DECIMAL" => match params.as_deref() {
            Some(p) if !p.contains(',') => format!("NUMERIC({p},0)"),
            _ => with_params("NUMERIC"),
        },
        "REAL" | "FLOAT4" => "REAL".into(),
        "DOUBLE PRECISION" | "FLOAT8" | "DOUBLE" => "DOUBLE PRECISION".into(),
        "FLOAT" => match params.as_deref().map(str::parse::<u8>) {
            Some(Ok(bits)) if bits <= 24 => "REAL".into(),
            _ => "DOUBLE PRECISION".into(),
        },
        "TEXT" => "TEXT".into(),
        "VARCHAR" | "CHARACTER VARYING" => with_params("VARCHAR"),
        "CHAR" | "CHARACTER" | "BPCHAR" => with_params("CHAR"),
        "BOOLEAN" | "BOOL" => "BOOLEAN".into(),
        "TIMESTAMP" | "TIMESTAMP WITHOUT TIME ZONE" => "TIMESTAMP".into(),
        "TIMESTAMPTZ" | "TIMESTAMP WITH TIME ZONE" => "TIMESTAMPTZ".into(),
        "TIME" | "TIME WITHOUT TIME ZONE" => "TIME".into(),
        "TIMETZ" | "TIME WITH TIME ZONE" => "TIMETZ".into(),
        _ => with_params(&base),
    }
}

#[derive(Default)]
struct Tally {
    compared: usize,
    exact: usize,
    documented_unknown: usize,
    failures: BTreeMap<String, Vec<String>>,
}

fn replay(fixture_json: &str, dialect: DialectType) -> Tally {
    let fixture: Fixture = serde_json::from_str(fixture_json).unwrap();
    let canonical: fn(&str) -> String = match dialect {
        DialectType::DuckDB => canonical_duckdb,
        _ => canonical_postgres,
    };
    let sqlbuild_validation = SchemaValidationOptions {
        check_types: false,
        check_references: true,
        strict: Some(true),
        semantic: false,
        strict_syntax: false,
        ..Default::default()
    };
    let semantic_validation = SchemaValidationOptions {
        check_types: true,
        check_references: true,
        strict: Some(true),
        semantic: true,
        ..Default::default()
    };
    let options = AnalyzeQueryOptions {
        dialect,
        schema: Some(fixture.schema.clone()),
        ..Default::default()
    };
    let mut tally = Tally::default();
    for case in &fixture.cases {
        let Some(engine_type) = case.engine_type.as_deref() else {
            continue;
        };
        tally.compared += 1;
        let expected = canonical(engine_type);
        let mut record = |kind: &str, detail: String| {
            tally
                .failures
                .entry(format!("{} {kind}", case.category))
                .or_default()
                .push(format!("{}: {detail}", case.id));
        };

        let compiled = compile_required_query_analysis(
            &case.sql,
            options.clone(),
            &fixture.schema,
            &sqlbuild_validation,
            true,
        );
        let errors: Vec<_> = compiled
            .validation
            .errors
            .iter()
            .filter(|error| error.severity == ValidationSeverity::Error)
            .map(|error| format!("{} {}", error.code, error.message))
            .collect();
        if !errors.is_empty() {
            record("validation", format!("{errors:?}"));
        }
        let semantic =
            validate_with_schema(&case.sql, dialect, &fixture.schema, &semantic_validation);
        let semantic_errors: Vec<_> = semantic
            .errors
            .iter()
            .filter(|error| error.severity == ValidationSeverity::Error)
            .map(|error| format!("{} {}", error.code, error.message))
            .collect();
        if !semantic_errors.is_empty() {
            record("semantic validation", format!("{semantic_errors:?}"));
        }

        let project_type = compiled.analysis.ok().and_then(|analysis| {
            analysis
                .projections
                .first()
                .and_then(|p| p.type_hint.clone())
        });
        let default_type = analyze_query(&case.sql, options.clone())
            .ok()
            .and_then(|analysis| {
                analysis
                    .projections
                    .first()
                    .and_then(|p| p.type_hint.clone())
            });

        let mut unknown = false;
        let mut wrong = Vec::new();
        for (path, reported) in [("project", &project_type), ("analysis", &default_type)] {
            match reported.as_deref().map(canonical) {
                None => unknown = true,
                Some(actual) if actual == "UNKNOWN" => unknown = true,
                Some(actual) if actual == expected => {}
                Some(actual) => wrong.push(format!("{path} {actual}")),
            }
        }
        if !wrong.is_empty() {
            record(
                "wrong type",
                format!("{} != engine {expected}", wrong.join(", ")),
            );
            continue;
        }
        if unknown {
            if let Some((_, _, _)) = DOCUMENTED_UNKNOWNS.iter().find(|(engine, prefix, _)| {
                *engine == fixture.engine && case.id.starts_with(prefix)
            }) {
                tally.documented_unknown += 1;
            } else {
                record("unknown", format!("engine {expected}"));
            }
        } else {
            tally.exact += 1;
        }
    }
    tally
}

fn assert_clean(name: &str, tally: Tally) {
    let mut summary = format!(
        "{name}: {} compared, {} exact, {} documented unknown\n",
        tally.compared, tally.exact, tally.documented_unknown
    );
    for (category, failures) in &tally.failures {
        summary.push_str(&format!("  {category}: {}\n", failures.len()));
    }
    eprintln!("{summary}");
    if let Ok(path) = std::env::var("ENGINE_TYPE_TRUTH_REPORT") {
        let details: String = tally
            .failures
            .iter()
            .flat_map(|(category, failures)| {
                failures
                    .iter()
                    .map(move |failure| format!("[{category}] {failure}\n"))
            })
            .collect();
        std::fs::write(format!("{path}.{name}.txt"), details).unwrap();
    }
    let total: usize = tally.failures.values().map(Vec::len).sum();
    assert_eq!(total, 0, "{summary}");
}

#[test]
fn duckdb_engine_type_matrix_replays_without_mismatches() {
    let tally = replay(
        include_str!("fixtures/engine_types/duckdb.json"),
        DialectType::DuckDB,
    );
    assert_clean("duckdb", tally);
}

#[test]
fn postgres_engine_type_matrix_replays_without_mismatches() {
    let tally = replay(
        include_str!("fixtures/engine_types/postgres.json"),
        DialectType::PostgreSQL,
    );
    assert_clean("postgres", tally);
}
