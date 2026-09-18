# polyglot-sql-ffi

C Foreign Function Interface bindings for `polyglot-sql`.

`polyglot-sql-ffi` exposes core parsing/transpilation/formatting/validation/analysis features through a small, stable C ABI. It is intended to be used as the native layer for language-specific wrappers. The official Go SDK (`packages/go`) uses this library through PureGo.

## What It Provides

- Shared and static libraries:
  - Linux: `.so` and `.a`
  - macOS: `.dylib` and `.a`
  - Windows: `.dll` and `.lib`
- Auto-generated C header:
  - `polyglot_sql.h`
- String-oriented API with JSON payloads for complex data
- Explicit memory ownership helpers
- Panic-protected FFI boundaries with status codes

## Build

Use the dedicated unwind profile for FFI builds:

```bash
cargo build -p polyglot-sql-ffi --profile ffi_release
```

This profile inherits the release profile's stripping and codegen settings, uses
`opt-level=2` with thin LTO for native throughput, and sets `panic = "unwind"`
so exported functions can catch panics before they cross the FFI boundary.

### Output Paths

- Local (no explicit target triple):
  - `target/ffi_release/`
- Cross-target:
  - `target/<target-triple>/ffi_release/`

### Header Generation

The header is generated automatically by `build.rs` (using `cbindgen`) when the crate is built:

- `crates/polyglot-sql-ffi/polyglot_sql.h`

It is intentionally not tracked in git.

## Quick C Example

See `examples/c/main.c` for a full end-to-end sample.

Minimal usage:

```c
#include "polyglot_sql.h"
#include <stdio.h>

int main(void) {
    polyglot_result_t result = polyglot_transpile(
        "SELECT IFNULL(a, b) FROM t",
        "mysql",
        "postgres"
    );

    if (result.status == 0) {
        printf("%s\n", result.data); // JSON array of SQL strings
    } else {
        printf("Error (%d): %s\n", result.status, result.error);
    }

    polyglot_free_result(result);
    return 0;
}
```

## API Design

### Core Return Type

Most functions return:

```c
typedef struct {
    char *data;   // owned by caller
    char *error;  // owned by caller, NULL on success
    int32_t status;
} polyglot_result_t;
```

Status `0` means success. On failure, `data == NULL` and `error` contains details.

### Validation Return Type

```c
typedef struct {
    int32_t valid;       // 1 valid, 0 invalid
    char *errors_json;   // owned by caller
    char *error;         // top-level error message (optional)
    int32_t status;
} polyglot_validation_result_t;
```

### Exported Functions

- `polyglot_transpile(sql, from_dialect, to_dialect)`
- `polyglot_transpile_with_options(sql, from_dialect, to_dialect, options_json)` (`TranspileOptions` JSON, e.g. `{"pretty": true, "unsupportedLevel": "raise", "complexityGuard": {"maxFunctionCallDepth": 128}}`)
- `polyglot_parse(sql, dialect)`
- `polyglot_parse_one(sql, dialect)`
- `polyglot_parse_data_type(sql, dialect)`
- `polyglot_tokenize(sql, dialect)`
- `polyglot_generate(ast_json, dialect)` (expects `Vec<Expression>` JSON)
- `polyglot_generate_data_type(data_type_json, dialect)` (expects `DataType` JSON)
- `polyglot_qualify_tables(ast_json, options_json)` (expects `Vec<Expression>` JSON)
- `polyglot_set_limit(ast_json, limit)` (expects `Vec<Expression>` JSON)
- `polyglot_set_offset(ast_json, offset)` (expects `Vec<Expression>` JSON)
- `polyglot_set_order_by(ast_json, order_by_json)` (both arguments expect `Vec<Expression>` JSON)
- `polyglot_rename_tables_with_options(ast_json, mapping_json, options_json)` (expects `Vec<Expression>` JSON)
- `polyglot_format(sql, dialect)`
- `polyglot_format_with_options(sql, dialect, options_json)` (`FormatGuardOptions` JSON)
- `polyglot_validate(sql, dialect)`
- `polyglot_validate_with_options(sql, dialect, options_json)` (`ValidationOptions` JSON, e.g. `{"strictSyntax": true, "semantic": true}`)
- `polyglot_validate_with_schema(sql, schema_json, dialect, options_json)` (`ValidationSchema` and `SchemaValidationOptions` JSON; returns `polyglot_validation_result_t`)
- `polyglot_optimize(sql, dialect)` (full optimizer pipeline)
- `polyglot_build(request_json)` (evaluates a versioned, stateless builder plan
  and returns either an expression AST or generated SQL)
- `polyglot_lineage(column_name, sql, dialect)`
- `polyglot_lineage_at(ordinal, sql, dialect)` (zero-based ordinal)
- `polyglot_lineage_at_with_schema(ordinal, sql, schema_json, dialect)` (`ValidationSchema` JSON)
- `polyglot_lineage_with_schema(column_name, sql, schema_json, dialect)` (`ValidationSchema` JSON)
- `polyglot_output_columns(sql, dialect)`
- `polyglot_output_columns_with_schema(sql, schema_json, dialect)` (`ValidationSchema` JSON)
- `polyglot_source_tables(column_name, sql, dialect)`
- `polyglot_analyze_query(sql, options_json)` (`AnalyzeQueryOptions` JSON)
  returns compact `QueryAnalysis` JSON. `relations` contains sources visible in
  the analyzed scope, and `baseTables` contains deduplicated physical table
  dependencies from nested CTEs, derived tables, subqueries, and set-operation
  branches. Physical relation facts keep `name` as the qualified display name
  and expose parsed `catalog`, `schema`, and `table` fields. With a schema,
  parseable detailed type strings such as
  `DECIMAL(10,2)` are preserved in projection `typeHint` values. `cteFacts`
  reports top-level CTE definitions, `starProjections` records original star
  projections and schema-expanded columns, and each projection includes
  conservative `nullability`: `"non_null"`, `"nullable"`, or `"unknown"`.
  Each `setOperations[].branches[]` entry includes a `role` of `"value"` or
  `"filter"`.
- `polyglot_openlineage_column_lineage(sql, options_json)` (`OpenLineageOptions` JSON)
- `polyglot_openlineage_job_event(sql, options_json)` (`OpenLineageOptions` JSON)
- `polyglot_openlineage_run_event(sql, options_json)` (`OpenLineageOptions` JSON)
- `polyglot_diff(sql1, sql2, dialect)`
- `polyglot_dialect_list()`
- `polyglot_dialect_count()`
- `polyglot_version()`
- `polyglot_free_string()`
- `polyglot_free_result()`
- `polyglot_free_validation_result()`

### Parser Depth Guard

Native parsing defaults to 1024 logical levels in the shared Rust core; WASM uses
a separate default of 32. The following APIs accept a `complexityGuard` entry in
their options JSON:

- `polyglot_parse_with_options(sql, dialect, options_json)`
- `polyglot_parse_one_with_options(sql, dialect, options_json)`
- `polyglot_parse_data_type_with_options(sql, dialect, options_json)`
- `polyglot_validate_with_options`, `polyglot_validate_with_schema`
- `polyglot_analyze_query`, `polyglot_transpile_with_options`

The existing no-options parsing symbols retain their signatures and defaults.
The new parsing functions require non-NULL UTF-8, NUL-terminated arguments; pass
`{}` for default options and free results with `polyglot_free_result`.
For example, use `{"complexityGuard":{"maxFunctionCallDepth":128}}` for deeper
function calls, or
`{"complexityGuard":{"maxParserDepth":128}}` to override this limit. Omit the
field for the target's default, use `null` to disable only this check, or use `0`
to reject parsing descents. Exhaustion returns a nonzero status and an error
containing `E_GUARD_PARSER_DEPTH_EXCEEDED`.

All seven shared limits are available: `maxParserDepth`, `maxInputBytes`,
`maxTokens`, `maxAstNodes`, `maxAstDepth`, `maxParenthesisDepth`, and
`maxFunctionCallDepth`. An absent or null guard retains dialect defaults; an
object uses shared defaults for omitted fields. Unknown guard keys and invalid
limit values return `STATUS_SERIALIZATION_ERROR` (6). Parsing guard failures
return status 1; validation guard failures remain validation diagnostics (4).

Other complexity guards remain independent. Raising or disabling limits does not
increase stack space and can permit stack exhaustion and process termination.
The parser guard does not cover arbitrary AST construction or later generation
and traversal stages. APIs without an options argument retain the default.

### Formatting Guard Behavior

`polyglot_format` uses Rust core formatting guards with default limits:
- input bytes: `16 * 1024 * 1024`
- tokens: `1_000_000`
- AST nodes: `1_000_000`
- set-op chain: `256`

When a guard is exceeded, `status != 0` and `error` contains one of:
- `E_GUARD_INPUT_TOO_LARGE`
- `E_GUARD_TOKEN_BUDGET_EXCEEDED`
- `E_GUARD_AST_BUDGET_EXCEEDED`
- `E_GUARD_SET_OP_CHAIN_EXCEEDED`

```c
#include "polyglot_sql.h"
#include <stdio.h>
#include <string.h>

polyglot_result_t r = polyglot_format("SELECT 1", "generic");
if (r.status != 0 && r.error && strstr(r.error, "E_GUARD_") != NULL) {
    printf("Formatting guard triggered: %s\n", r.error);
}
polyglot_free_result(r);
```

Per-call overrides are supported via `polyglot_format_with_options`:

```c
const char *opts = "{\"maxSetOpChain\":1024,\"maxInputBytes\":33554432}";
polyglot_result_t r = polyglot_format_with_options(sql, "generic", opts);
```

## JSON Payload Contracts

### Success payloads (`polyglot_result_t.data`)

- `polyglot_transpile`: JSON array of SQL strings
- `polyglot_parse`: JSON `Vec<Expression>`
- `polyglot_parse_one`: JSON `Expression`
- `polyglot_parse_data_type`: JSON `DataType`
- `polyglot_tokenize`: JSON `Vec<Token>` (each token has `token_type`, `text`, `span`, `comments`, `trailing_comments`)
- `polyglot_generate`: JSON array of SQL strings
- `polyglot_generate_data_type`: SQL string
- `polyglot_qualify_tables`: JSON `Vec<Expression>`
- `polyglot_set_limit`: JSON `Vec<Expression>`
- `polyglot_set_offset`: JSON `Vec<Expression>`
- `polyglot_set_order_by`: JSON `Vec<Expression>`
- `polyglot_rename_tables_with_options`: JSON `Vec<Expression>`
- `polyglot_format`: JSON array of SQL strings
- `polyglot_format_with_options`: JSON array of SQL strings
- `polyglot_optimize`: JSON array of SQL strings
- `polyglot_build`: JSON `Expression` when `output.kind` is `ast`, otherwise a
  SQL string when `output.kind` is `sql`
- `polyglot_lineage`: JSON `LineageNode`
- `polyglot_lineage_at`: JSON `LineageNode`
- `polyglot_lineage_at_with_schema`: JSON `LineageNode`
- `polyglot_lineage_with_schema`: JSON `LineageNode`
- `polyglot_output_columns`: JSON `QueryOutput`
- `polyglot_output_columns_with_schema`: JSON `QueryOutput`
- `polyglot_source_tables`: JSON array of source table names
- `polyglot_analyze_query`: JSON `QueryAnalysis`
- `polyglot_openlineage_column_lineage`: JSON `OpenLineageColumnLineageResult`
- `polyglot_openlineage_job_event`: JSON `OpenLineageEventResult`
- `polyglot_openlineage_run_event`: JSON `OpenLineageEventResult`
- `polyglot_diff`: JSON array of diff edits
- `polyglot_dialect_list`: JSON array of dialect names

`QueryAnalysis.columnUses` contains the shared Rust clause-use facts: `context`,
`scopePath`, `expressionPath`, dialect-rendered `expressionSql`, and `references`
with existing source identity/confidence fields. Optional `span` objects use
half-open Unicode-character offsets into the original SQL; reference spans locate
uses rather than upstream definitions. Unavailable expression spans are omitted.
The field is JSON-additive and requires no new C function or ABI layout change.

`ValidationSchema` JSON used by schema-aware functions and `AnalyzeQueryOptions`
uses this shape:

```json
{
  "strict": true,
  "tables": [
    {
      "name": "orders",
      "schema": "analytics",
      "aliases": ["o"],
      "primaryKey": ["id"],
      "uniqueKeys": [["external_id"]],
      "foreignKeys": [
        {
          "columns": ["customer_id"],
          "references": { "table": "customers", "columns": ["id"] }
        }
      ],
      "columns": [
        { "name": "id", "type": "INT", "nullable": false, "primaryKey": true },
        { "name": "amount", "type": "DECIMAL(10,2)", "nullable": true }
      ]
    }
  ]
}
```

Use the `type` key for column types. `dataType` / `data_type` are not accepted
aliases in this payload.

`LineageNode` includes `source_kind` and optional `source_alias` metadata so
wrappers can distinguish physical table sources from virtual sources such as
BigQuery `UNNEST(...) AS alias`.
Immediate set-operation branch roots also include optional `set_branch`
metadata: `operator`, the original zero-based `ordinal`, and the operation's
`all` flag. Branch-local resolution failures do not renumber surviving nodes.

`QueryOutput.columns` is ordered and uses tagged `named`, `unnamed`, and
`wildcard` entries. Each concrete entry carries a zero-based `ordinal` when it
is knowable; wildcards carry `startOrdinal`. `ordinalComplete` is false when an
unexpanded wildcard prevents later positions from being known.

### Validation payloads

`polyglot_validate_with_schema` uses the same Rust validator as Python, WASM,
TypeScript and Go. Its schema uses the existing `{"tables": [...]}` shape.
All four string arguments are required; pass `"{}"` for default options.
Options are `check_types`, `check_references`, `strict`, `semantic` and
`strict_syntax`; compound names also accept their camelCase equivalents.
Unknown option names are rejected, not silently ignored.

Unknown tables, aliases and columns are checked by default. `check_references`
additionally enables ambiguity and foreign-key checks. `strict` overrides the
schema's strict setting, which defaults to true; false reports reference/type
findings as warnings. Empty column lists and wildcard (`*`) columns represent
open schemas. Syntax errors take precedence over schema checks.

SQL validation failures return status `4` and findings in `errors_json`;
warnings return status `0` with `valid = 1`. Invalid arguments or JSON use the
existing top-level error statuses. Free results with
`polyglot_free_validation_result`, including failure results. The C result
layout is unchanged; consumers must load a library exporting the new symbol.

- `errors_json`: JSON array of validation error objects:
  - `message`
  - optional `line`
  - optional `column`
  - `severity`
  - `code`
  - optional `start` and `end`: zero-based Unicode character offsets in the
    original SQL, with an exclusive end (not UTF-8 byte offsets). Reference
    diagnostics point to offending identifiers when source metadata exists.

## Error Codes

- `0`: success
- `1`: parse error
- `2`: generate error
- `3`: transpile error
- `4`: validation error
- `5`: invalid argument (NULL pointer, bad dialect, invalid UTF-8)
- `6`: JSON serialization/deserialization error
- `7`: requested output column or ordinal not found
- `8`: output ordinal indeterminate because a wildcard could not be expanded
- `9`: requested output name is ambiguous
- `99`: internal panic/error

## Memory Ownership Rules

- Free every returned `char *` with `polyglot_free_string`.
- Free every `polyglot_result_t` with `polyglot_free_result`.
- Free every `polyglot_validation_result_t` with `polyglot_free_validation_result`.
- `polyglot_version()` returns a static pointer. Do not free.

## Dialect Names

Dialect identifiers are string names used in core `polyglot-sql`, for example:

- `generic`
- `postgres` / `postgresql`
- `mysql`
- `bigquery`
- `snowflake`
- `duckdb`
- `clickhouse`

For a complete runtime list, call `polyglot_dialect_list()`.

## Thread Safety

The FFI layer does not maintain mutable global state. Calls are safe to use from multiple threads concurrently.

## Native Library Transport

The FFI crate only builds native libraries and the C header. Runtime transport,
download, and update logic are intentionally left to downstream applications or
packaging systems. The official Go SDK follows the same policy: users provide
the shared library path explicitly or through `POLYGLOT_SQL_FFI_PATH`.

## Make Targets

From repo root:

- `make build-ffi`
- `make build-ffi-static`
- `make generate-ffi-header`
- `make build-ffi-example`
- `make test-ffi`
- `make clean-ffi`

## Release Artifacts

For `v*` tags, CI publishes prebuilt FFI archives and `checksums.sha256` to the corresponding GitHub release.

Expected archive naming:

- `polyglot-sql-ffi-linux-x86_64.tar.gz`
- `polyglot-sql-ffi-linux-aarch64.tar.gz`
- `polyglot-sql-ffi-macos-x86_64.tar.gz`
- `polyglot-sql-ffi-macos-aarch64.tar.gz`
- `polyglot-sql-ffi-windows-x86_64.zip`

## Language Wrapper Guidance

- Python: load shared library via `ctypes`/`cffi`, parse JSON to Python objects
- Go: use `cgo`, convert `char*` via `C.GoString`, always call free helpers
- C#: P/Invoke with `IntPtr` + marshaling + explicit free calls
- Java: JNA/JNI wrapper around C signatures, parse JSON in JVM layer

Keep wrappers thin and treat this crate as the single source of behavior.
