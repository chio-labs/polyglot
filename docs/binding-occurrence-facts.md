# Opt-in binding observations

Rust consumers can request intermediate binding facts with
`polyglot_sql::validation::validate_parsed_with_binding_facts`. It takes the same
parsed statements, dialect, validation schema and validation options as
`validate_parsed_with_schema`.

The observations are emitted by the reference-validation pass. Lambda and
projection-alias bindings are observed when the validator makes those bindings;
they are not reconstructed from terminal column lineage. Existing validation
entry points do not create an observer or collect these facts.

```rust
use polyglot_sql::{parse, DialectType, SchemaValidationOptions, ValidationSchema};
use polyglot_sql::validation::validate_parsed_with_binding_facts;

let statements = parse(
    "WITH items AS (SELECT 1 AS order_id, 2 AS priority) \
     SELECT order_id FROM items WHERE priority > 0",
    DialectType::DuckDB,
)?;
let facts = validate_parsed_with_binding_facts(
    statements,
    DialectType::DuckDB,
    &ValidationSchema { tables: vec![], strict: Some(true) },
    &SchemaValidationOptions::default(),
);
assert!(facts.validation.valid);
// The priority occurrence binds to an intermediate output slot even though
// the slot is a literal and has no physical upstream column.
# Ok::<(), polyglot_sql::Error>(())
```

## Identity and completeness

Scope IDs and output ordinals identify positions within one result. They are not
persistent IDs across SQL edits. Scope paths describe lexical nesting; CTE names
alone are insufficient because nested declarations can shadow each other.

Each output interface distinguishes concrete slots from an open expansion.
An open expansion means the supplied schema does not establish all output
positions. Consumers must not interpret it as an empty output list.

Occurrences carry their authored span when available, clause, lexical scope,
and binding. Bindings distinguish output slots, source columns, merged inputs,
lambda parameters, pseudocolumns, open namespaces, and unresolved references.
An unresolved reference includes a reason. A consumer performing automatic
rewrites must require sufficient binding evidence rather than infer that an
unresolved or open occurrence does not read a slot.

`validation` retains ordinary diagnostics. Facts also remain available for
invalid statements, so consumers should consider the diagnostics and binding
completeness together.

## Cost boundary

Observation requires additional indexing and output storage. It is opt-in.
The default-validation regression guard verifies that ordinary validation
constructs no binding observer, and that requesting observations preserves the
validation result for the covered corpus.
