import pytest

import polyglot_sql


def test_given_computed_variant_keys_when_formatting_then_preserves_source_and_roundtrips():
    for path in [
        "payload:customers[TO_VARCHAR(order_id)]",
        "payload:customers[order_id + 1]",
        "payload:customers[(order_id + 1)]",
        "payload:customers[1 + order_id]",
        "payload:customers[TO_VARCHAR(order_id)].name",
        "payload:customers[TO_VARCHAR(order_id)][0]",
    ]:
        sql = f"SELECT {path} FROM orders"
        parsed = polyglot_sql.parse_one(sql, dialect="snowflake")
        formatted = polyglot_sql.format_sql(sql, dialect="snowflake")
        assert "".join(formatted.split()) == "".join(sql.split())
        assert polyglot_sql.parse_one(formatted, dialect="snowflake") == parsed
        assert polyglot_sql.format_sql(formatted, dialect="snowflake") == formatted


def test_format_sql_contains_newlines():
    formatted = polyglot_sql.format_sql("SELECT a,b FROM t WHERE x=1", dialect="postgres")
    assert "\n" in formatted


def test_format_alias_is_available():
    formatted = polyglot_sql.format("SELECT a,b FROM t", dialect="postgres")
    assert isinstance(formatted, str)


@pytest.mark.parametrize("dialect", ["snowflake", "duckdb", "postgres"])
@pytest.mark.parametrize("formatter", [polyglot_sql.format, polyglot_sql.format_sql])
def test_format_preserves_explicit_null_ordering(dialect, formatter):
    # Exact issue #457 query, plus omitted clauses to guard against default injection.
    for ordering in [
        "category NULLS LAST, created_at DESC NULLS FIRST",
        "category, created_at DESC",
    ]:
        sql = f"SELECT id FROM items ORDER BY {ordering}"
        formatted = formatter(sql, dialect=dialect)
        assert " ".join(formatted.split()) == sql
        assert formatter(formatted, dialect=dialect) == formatted


@pytest.mark.parametrize("dialect", ["snowflake", "duckdb"])
def test_generate_and_transpile_preserve_null_ordering(dialect):
    for ordering in [
        "category NULLS LAST, created_at DESC NULLS FIRST",
        "category, created_at DESC",
    ]:
        sql = f"SELECT id FROM items ORDER BY {ordering}"
        expr = polyglot_sql.parse_one(sql, dialect=dialect)
        assert polyglot_sql.generate(expr, dialect=dialect) == [sql]
        assert polyglot_sql.transpile(sql, read=dialect, write=dialect) == [sql]


def test_format_preserves_sql_semantics_for_simple_query():
    raw = "SELECT a,b FROM t WHERE x=1"
    formatted = polyglot_sql.format_sql(raw, dialect="postgres")
    assert polyglot_sql.parse_one(formatted, dialect="postgres") == polyglot_sql.parse_one(
        raw, dialect="postgres"
    )


def test_format_unknown_dialect_raises_value_error():
    with pytest.raises(ValueError):
        polyglot_sql.format_sql("SELECT 1", dialect="not_a_dialect")


def test_format_invalid_sql_raises_parse_error():
    with pytest.raises(polyglot_sql.ParseError):
        polyglot_sql.format_sql("SELECT FROM", dialect="postgres")


def test_format_with_options_guard_override_rejects_set_op_chain():
    sql = "SELECT 1 UNION ALL SELECT 2 UNION ALL SELECT 3"
    with pytest.raises(polyglot_sql.GenerateError) as excinfo:
        polyglot_sql.format_sql(sql, dialect="generic", max_set_op_chain=1)
    assert "E_GUARD_SET_OP_CHAIN_EXCEEDED" in str(excinfo.value)


def test_format_with_options_guard_override_rejects_input_bytes():
    with pytest.raises(polyglot_sql.GenerateError) as excinfo:
        polyglot_sql.format_sql("SELECT 1", dialect="generic", max_input_bytes=7)
    assert "E_GUARD_INPUT_TOO_LARGE" in str(excinfo.value)
