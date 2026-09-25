# Schema-aware semantic types

`check_types` enables expression compatibility checks. `semantic` enables
scope, grouping, window, and shape checks. DuckDB's embedded function catalogue
is now included by the `semantic` Cargo feature, rather than requiring a second
feature in each native consumer. Function names and arities are checked when
either option is enabled. Other embedded catalogues retain their opt-in features.

Application functions can be registered with the additive JSON option
`known_functions` (alias `knownFunctions`), a list of scalar or table-function
names. These names bypass built-in name, arity, and argument checks. Validation
does not guess an unknown function's return type from its first argument (the
low-level standalone annotator retains its legacy fallback). Register cast types
with `known_types` (alias `knownTypes`). These options do not declare output
schemas or signatures; callers still supply table-function output columns as
relation schemas. All existing JSON fields retain their meaning.

## Coercion policy

`validation/coercion.rs` contains the dialect policy. Literal identity is preserved
through comparison, IN, BETWEEN, conditional unification, and set-operation
checks. A string literal is different from a VARCHAR column or an explicit cast
to VARCHAR. NULL and unknown input types are not rejected.

| Rule | DuckDB | PostgreSQL | Snowflake | BigQuery |
| --- | --- | --- | --- | --- |
| String literal to numeric | yes | yes | yes | no |
| String literal to date/time/timestamp | yes | yes | yes | yes |
| String literal to boolean | yes | yes | yes | no |
| String/scalar set-operation combination | yes | no | yes | no |
| Boolean/numeric set-operation combination | yes | no | yes | no |
| Numeric predicates | yes | no | yes | no |

Numeric-family widening and date/timestamp compatibility are shared. Arithmetic
has directional date/integer and temporal/interval overload tables; an integer
minus a timestamp is not the inverse of a timestamp plus an interval. Unsupported
dialects do not get coercion errors from these tables; remaining legacy type
checks are warnings. Generic SQL retains its historical strict abstract rules.
The table is conservative and partial, not a claim to model every engine overload.
A DuckDB column-to-column numeric/VARCHAR comparison is diagnosed even when the
engine can attempt a data-dependent conversion at execution time. Snowflake's
documented scalar coercions (including numeric/string predicates) are accepted.

DuckDB combination casting accepts boolean/numeric CASE and COALESCE inputs and
VARCHAR set-operation outputs, but does not allow arbitrary VARCHAR columns in
COALESCE with numeric inputs. Recursive branches must convert to the anchor's
type, unlike ordinary UNION output unification. Fractional and numeric-string
LIMIT values are accepted by DuckDB and are not rejected as non-integers.

Sources:

- [DuckDB typecasting and combination casting](https://duckdb.org/docs/stable/sql/data_types/typecasting)
- [DuckDB literal types](https://duckdb.org/docs/stable/sql/data_types/literal_types)
- [DuckDB aggregate overloads](https://duckdb.org/docs/stable/sql/functions/aggregates)
- [PostgreSQL type conversion](https://www.postgresql.org/docs/current/typeconv.html)
- [Snowflake conversion](https://docs.snowflake.com/en/sql-reference/data-type-conversion)
- [BigQuery conversion and supertypes](https://cloud.google.com/bigquery/docs/reference/standard-sql/conversion_rules)

The DuckDB regression cases were also executed against the real DuckDB Python
package. In particular, temporal AVG, SUM(BOOLEAN), numeric predicates, literal
comparisons, set-operation coercions, and LIMIT conversions have valid controls.

## Function signatures and inference

`polyglot-sql-function-catalogs/src/types.rs` provides additive per-argument
families and return policies without changing the existing arity-only public
`FunctionSignature` struct. The validator adapts both generic function calls and
dedicated AST nodes to these signatures. The shared annotator consumes return
metadata for additional known functions; established specialized inference (such
as DuckDB SUM widening and DATE_TRUNC) remains authoritative. DuckDB temporal AVG
preserves its temporal result instead of being annotated as DOUBLE.

The catalogue covers common numeric aggregates, value/window functions,
date/time, string, math, and conditional functions. It is intentionally partial:
functions absent from the type catalogue are unchecked for argument types. An
arity catalogue entry does not imply complete type knowledge. Dialect-specific
overloads such as reversed STRFTIME arguments are handled explicitly.

## Diagnostics

Existing codes are reused: E201 for missing star-modifier/output columns,
E202/E203 for function names/arity, E211 for conditions, E212 for arithmetic,
E213 for argument/unification types, E215 for set-operation types, E216 for
subquery/row column counts, E217 for comparisons, E218 for casts/literals, and
E232 for window usage. New codes:

- **E233:** duplicate CTE name or duplicate relation alias in a scope.
- **E234:** invalid LIMIT/OFFSET value or column-valued bound.

Temporal literal validation proves invalid ISO calendar dates and clearly invalid
text; alternate date formats remain unchecked. Cast rejection is conservative:
the impossible-conversion table currently covers DuckDB timestamp/boolean and
timestamp/integer conversions. Unknown application types require registration.
Remaining overload and dialect coverage should be added with accepted-form
controls rather than importing one engine's rules into another.
