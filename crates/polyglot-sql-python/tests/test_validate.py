import pytest

import polyglot_sql


@pytest.fixture
def schema():
    return {"tables": [
        {"name": "orders", "columns": [
            {"name": "order_id", "type": "INT"},
            {"name": "active", "type": "BOOLEAN"},
        ]},
        {"name": "customers", "columns": [{"name": "order_id", "type": "INT"}]},
    ]}


def test_validate_with_schema_missing_where_column(schema):
    sql = "SELECT o.order_id FROM orders AS o WHERE o.missing_column = TRUE"
    result = polyglot_sql.validate_with_schema(
        sql, schema, dialect="snowflake", check_types=True, check_references=True,
    )
    assert not result
    error = next(e for e in result.errors if e.code == "E201")
    assert error.severity == "error"
    assert sql[error.start:error.end] == "missing_column"
    assert error.line == 1
    assert error.col > 0
    assert "validate_with_schema" in polyglot_sql.__all__


@pytest.mark.parametrize("sql,code,token", [
    ("SELECT x.order_id FROM orders", "E222", "x"),
    ("SELECT * FROM missing", "E200", "missing"),
    ('SELECT \'😀\', o."míssing" FROM orders o', "E201", '"míssing"'),
    ('SELECT o.order_id\nFROM orders o\nWHERE o.missing = TRUE', "E201", "missing"),
])
def test_validate_with_schema_identifier_positions(schema, sql, code, token):
    result = polyglot_sql.validate_with_schema(sql, schema, "snowflake")
    error = next(e for e in result.errors if e.code == code)
    assert error.start == sql.index(token)
    assert error.end == sql.index(token) + len(token)
    assert sql[error.start:error.end] == token


def test_validate_with_schema_repeated_identifiers(schema):
    result = polyglot_sql.validate_with_schema("SELECT missing, missing FROM orders", schema)
    assert [(e.start, e.end) for e in result.errors if e.code == "E201"] == [(7, 14), (16, 23)]


def test_validate_with_schema_options_and_strict_precedence(schema):
    sql = "SELECT order_id FROM orders o JOIN customers c ON o.order_id=c.order_id"
    assert polyglot_sql.validate_with_schema(sql, schema).valid
    strict = polyglot_sql.validate_with_schema(sql, schema, check_references=True)
    assert not strict
    assert any(e.code == "E221" for e in strict.errors)
    schema["strict"] = False
    warning = polyglot_sql.validate_with_schema(sql, schema, check_references=True)
    assert warning.valid
    assert any(e.code == "W222" and e.severity == "warning" for e in warning.errors)
    assert not polyglot_sql.validate_with_schema(sql, schema, check_references=True, strict=True)
    schema["strict"] = True
    assert polyglot_sql.validate_with_schema(sql, schema, check_references=True, strict=False)

    sql = "SELECT order_id + active FROM orders"
    assert polyglot_sql.validate_with_schema(sql, schema)
    assert not polyglot_sql.validate_with_schema(sql, schema, check_types=True)
    assert polyglot_sql.validate_with_schema(sql, schema, check_types=True, strict=False)
    strict = polyglot_sql.validate_with_schema(
        "SELECT *, FROM orders", schema, strict_syntax=True, semantic=True,
    )
    assert [e.code for e in strict.errors] == ["E005"]
    semantic = polyglot_sql.validate_with_schema("SELECT * FROM orders LIMIT 10", schema, semantic=True)
    assert semantic.valid
    assert {e.code for e in semantic.errors} >= {"W001", "W004"}


@pytest.mark.parametrize("sql,valid", [
    ("WITH a AS (SELECT order_id AS id FROM orders), b AS (SELECT id FROM a) SELECT id FROM b", True),
    ("SELECT o.order_id FROM orders o WHERE EXISTS (SELECT 1 FROM customers c WHERE c.order_id=o.order_id)", True),
    ("SELECT q.id FROM (SELECT order_id AS id FROM orders) q", True),
    ("SELECT order_id FROM q WHERE EXISTS (WITH q AS (SELECT order_id FROM orders) SELECT order_id FROM q)", False),
    ("SELECT o.order_id FROM orders o JOIN (SELECT o.order_id) q ON TRUE", False),
])
def test_validate_with_schema_query_scopes(schema, sql, valid):
    result = polyglot_sql.validate_with_schema(sql, schema, "snowflake", check_references=True)
    assert result.valid is valid, result.errors


@pytest.mark.parametrize("columns", [[], [{"name": "*"}]])
def test_validate_with_schema_open_sources(schema, columns):
    schema["tables"][0]["columns"] = columns
    for sql in [
        "SELECT missing FROM orders",
        "SELECT o.missing FROM orders o",
        "SELECT missing FROM orders o JOIN customers c ON TRUE",
    ]:
        assert polyglot_sql.validate_with_schema(sql, schema, check_references=True), sql
    assert not polyglot_sql.validate_with_schema(
        "SELECT c.missing FROM orders o JOIN customers c ON TRUE", schema,
    )


def test_validate_with_schema_invalid_inputs(schema):
    with pytest.raises(ValueError, match="schema"):
        polyglot_sql.validate_with_schema("SELECT 1", {"orders": {"order_id": "INT"}})
    with pytest.raises(ValueError):
        polyglot_sql.validate_with_schema("SELECT 1", schema, "not_a_dialect")
    with pytest.raises(TypeError):
        polyglot_sql.validate_with_schema("SELECT 1", schema, checkTypes=True)
    with pytest.raises(TypeError):
        polyglot_sql.validate_with_schema("SELECT 1", schema, options={"checkTypes": True})
    result = polyglot_sql.validate_with_schema("SELECT FROM", schema)
    assert not result


def test_validate_exposes_optional_source_positions():
    sql = "SELECT order_id, SUM(active) FROM orders LIMIT 10"
    result = polyglot_sql.validate(sql, semantic=True)
    aggregate = next(e for e in result.errors if e.code == "W002")
    assert sql[aggregate.start:aggregate.end] == "order_id"
    limit = next(e for e in result.errors if e.code == "W004")
    assert limit.start is None and limit.end is None
    assert limit.line == 0 and limit.col == 0


def test_validate_valid_sql():
    result = polyglot_sql.validate("SELECT 1", dialect="postgres")
    assert result.valid is True
    assert result.errors == []
    assert bool(result) is True


def test_validate_invalid_sql():
    result = polyglot_sql.validate("SELECT FROM", dialect="postgres")
    assert result.valid is False
    assert len(result.errors) > 0
    assert isinstance(result.errors[0].message, str)
    assert isinstance(result.errors[0].line, int)
    assert isinstance(result.errors[0].col, int)


def test_validate_bool_false_for_invalid():
    result = polyglot_sql.validate("SELECT FROM", dialect="postgres")
    assert bool(result) is False


def test_validate_repr_is_readable():
    result = polyglot_sql.validate("SELECT 1", dialect="postgres")
    text = repr(result)
    assert "ValidationResult" in text
    assert "valid=" in text


def test_validate_unknown_dialect_raises_value_error():
    with pytest.raises(ValueError):
        polyglot_sql.validate("SELECT 1", dialect="not_a_dialect")


def test_validate_strict_syntax_and_semantic_options():
    strict = polyglot_sql.validate(
        "SELECT *, FROM users", dialect="generic", strict_syntax=True, semantic=True
    )
    assert strict.valid is False
    assert [error.code for error in strict.errors] == ["E005"]

    semantic = polyglot_sql.validate(
        "SELECT * FROM users LIMIT 10", dialect="generic", semantic=True
    )
    assert semantic.valid is True
    assert {error.code for error in semantic.errors} >= {"W001", "W004"}

    default = polyglot_sql.validate("SELECT * FROM users LIMIT 10", dialect="generic")
    assert default.errors == []
