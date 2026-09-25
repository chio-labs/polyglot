# Schema-aware semantic types

## Diagnostic contract

An **error** is emitted only when the selected concrete dialect definitely
rejects the SQL at binding/compilation. An accepted implicit conversion that can
fail on data at execution time is a **warning**, using the existing W21x codes.
This includes DuckDB VARCHAR/numeric equality, conversion of invalid string
literals, predicates converted from VARCHAR, and casts unsupported at execution.
Unknown or incompletely modelled built-in types must not cause errors. The type
catalogue is not an authoritative list of installed types: unrecognized cast
targets receive W213 at most, including application or extension types. Register
`known_types` to suppress that uncertainty notice. No complete authoritative
DuckDB type-existence catalogue is claimed.

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
| Boolean/numeric set-operation combination | yes | no | no | no |
| Numeric WHERE/HAVING/ON predicates | yes | no | no | no |

Numeric-family widening and date/timestamp compatibility are shared. Arithmetic
has directional date/integer and temporal/interval overload tables; an integer
minus a timestamp is not the inverse of a timestamp plus an interval. Unsupported
dialects do not get coercion errors from these tables; remaining legacy type
checks are warnings. Generic SQL retains its historical strict abstract rules.
The table is conservative and partial, not a claim to model every engine overload.
DuckDB and Snowflake column/expression VARCHAR-to-numeric equality is accepted
with implicit-conversion warnings, never blocking errors. DuckDB's ordering and
IN-subquery binders are stricter: a VARCHAR column ordered against INTEGER, or an
INTEGER tested against a VARCHAR subquery column, is rejected at binding. These
are distinct from comparisons involving coercible string literals. Snowflake's
documented scalar coercions are context-dependent: logical operands can be
coerced, while WHERE/HAVING/ON and searched CASE require Boolean expressions.
NUMBER/TIMESTAMP comparisons and unification are rejected, even though explicit
numeric-to-timestamp conversion is available. LIKE and string functions permit
scalar-to-VARCHAR conversion. DATE_TRUNC/EXTRACT reject VARCHAR inputs, unlike
DATEDIFF's accepted string-date overloads.

DuckDB combination casting accepts boolean/numeric CASE and COALESCE inputs and
VARCHAR set-operation outputs, but does not allow arbitrary VARCHAR columns in
COALESCE with numeric inputs. Recursive branches must convert to the anchor's
type, unlike ordinary UNION output unification. Fractional and numeric-string
LIMIT values are accepted by DuckDB and are not rejected as non-integers.
DuckDB scalar set-operation casts also defer unsupported conversions until
execution: INTEGER UNION TIMESTAMP therefore produces W214, not E215.

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
Snowflake uses a partial built-in arity table, not an allowlist of function names:
unknown warehouse-defined functions never receive E202 by default.

## Snowflake engine regression fixture

`tests/fixtures/snowflake_semantic_truth.json` contains 211 synthetic cases:
126 compile failures, 30 execution-only failures, and 55 valid queries. The
verdicts were obtained with `EXPLAIN USING TEXT`, followed by execution when
compilation succeeded. `executed_sql` retains the typed inline-CTE inputs and
synthetic rows. Tests replay `sql` using matching explicit schemas, as native
consumers do. Only numeric Snowflake error codes are retained, never raw warehouse
responses, account/host/user identifiers, or connection details.

Tests prohibit errors on every valid or execution-only case and require errors
on the 119 claimed compile-failure cases. Unknown functions and unrecognized type
names remain intentionally unchecked. Snowflake accepts duplicate CTE names, but
requires matching CTE alias counts and selected DISTINCT ordering columns;
DuckDB's different behavior is covered by controls. WITHIN GROUP ordering belongs
to the aggregate, rather than being an ungrouped projection. Snowflake rejects the
documented impossible timestamp/number/boolean casts at compilation; DuckDB defers
those conversions until execution and continues to receive warnings.

## Diagnostics

Existing codes are reused: E201 for missing star-modifier/output columns,
E202/E203 for function names/arity, E211 for conditions, E212 for arithmetic,
E213 for argument/unification types, E215 for set-operation types, E216 for
subquery/row column counts, E217 for comparisons, and E232 for window usage.
W210/W211/W212/W213/W215/W216 report runtime comparison, arithmetic, assignment,
cast, predicate, and function-argument conversions respectively. E218 remains
reserved for proven bind-time cast rejection. New codes:

- **E233:** duplicate CTE name or duplicate relation alias in a scope.
- **E234:** invalid LIMIT/OFFSET value or column-valued bound.

Temporal literal validation recognizes invalid ISO calendar dates and clearly
invalid text, but reports runtime conversion warnings; alternate formats remain
unchecked. DuckDB timestamp/boolean and timestamp/integer casts likewise warn
because PREPARE accepts them. PostgreSQL interval SUM/AVG and DuckDB interval AVG
are supported; the tested DuckDB version rejects SUM(INTERVAL) at bind time.
Remaining overload and dialect coverage should be added with accepted-form
controls rather than importing one engine's rules into another.
