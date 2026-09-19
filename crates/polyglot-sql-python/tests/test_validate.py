import pytest
import polyglot_sql


@pytest.mark.parametrize("arity", [True, False, 1.5, -1])
def test_declarative_catalog_rejects_invalid_arities(arity):
    with pytest.raises(ValueError):
        polyglot_sql.validate_with_schema(
            "SELECT foo(1)", {"tables": []}, check_types=True,
            function_catalog={"functions": [{"name": "foo", "signatures": [{"minArity": arity}]}]},
        )

@pytest.fixture
def review_schema():
    return {"tables": [{"name": "items", "columns": [
        {"name": "quantity", "type": "INT"}, {"name": "active", "type": "BOOLEAN"},
    ]}]}


@pytest.mark.parametrize("sql, code", [
    ("UPDATE nonexistent SET x=1", "E200"),
    ("DELETE FROM nonexistent", "E200"),
    ("INSERT INTO nonexistent(x) VALUES(1)", "E200"),
    ("UPDATE items SET quantity=missing", "E201"),
    ("DELETE FROM items WHERE missing=1", "E201"),
])
def test_review_dml_references(review_schema, sql, code):
    for check_types in (False, True):
        result = polyglot_sql.validate_with_schema(sql, review_schema, "postgresql", check_types=check_types)
        assert not result.valid
        assert any(error.code == code for error in result.errors)


def test_review_scoped_types_and_name_aligned_union(review_schema):
    result = polyglot_sql.validate_with_schema(
        "WITH q AS (SELECT active AS flag FROM items) SELECT flag+1 FROM q",
        review_schema, "postgresql", check_types=True,
    )
    assert not result.valid and any(e.code == "E212" for e in result.errors)
    for dialect in ("snowflake", "duckdb", "bigquery"):
        result = polyglot_sql.validate_with_schema(
            "SELECT quantity AS a, active AS b FROM items UNION ALL BY NAME SELECT active AS b, quantity AS a FROM items",
            review_schema, dialect, check_types=True,
        )
        assert result.valid, result.errors


def test_review_semantic_errors_and_window_scopes():
    for sql, code in [
        ("SELECT quantity AS q, SUM(active) FROM items", "E230"),
        ("SELECT SUM(SUM(quantity)) FROM items", "E231"),
        ("SELECT quantity FROM items WHERE ROW_NUMBER() OVER(ORDER BY quantity)=1", "E232"),
    ]:
        assert polyglot_sql.validate(sql).valid
        result = polyglot_sql.validate(sql, semantic=True)
        assert not result.valid and any(e.code == code for e in result.errors)
    assert polyglot_sql.validate("SELECT quantity, SUM(quantity) OVER() FROM items", semantic=True).valid
    assert polyglot_sql.validate(
        "SELECT TRANSFORM(ARRAY_CONSTRUCT(1), x -> x + 1), COUNT(*) FROM items",
        "snowflake", semantic=True,
    ).valid


def test_review_function_catalog_and_typed_contracts(review_schema):
    catalog: polyglot_sql.FunctionCatalogSpec = {"functions": [
        {"name": "Foo", "nameCase": "sensitive", "signatures": [{"minArity": 1, "maxArity": 2}]},
    ]}
    for sql, valid in [("SELECT Foo(1)", True), ("SELECT Foo(1,2,3)", False), ("SELECT FOO(1)", False), ("SELECT Unknown(1)", False)]:
        result = polyglot_sql.validate_with_schema(sql, review_schema, check_types=True, function_catalog=catalog)
        assert result.valid == valid, result.errors
    assert polyglot_sql.validate_with_schema("SELECT Unknown(1)", review_schema, function_catalog=catalog).valid
    for invalid in [
        {"functions": [], "typo": True},
        {"functions": [{"name": "f", "signatures": []}]},
        {"functions": [{"name": "f", "signatures": [{"minArity": 2, "maxArity": 1}]}]},
    ]:
        with pytest.raises(ValueError):
            polyglot_sql.validate_with_schema("SELECT 1", review_schema, function_catalog=invalid)


def test_review_rejects_schema_key_typos():
    for schema in [
        {"tables": [], "typo": True},
        {"tables": [{"name": "t", "columns": [], "typo": True}]},
        {"tables": [{"name": "t", "columns": [{"name": "x", "dataType": "INT"}]}]},
    ]:
        with pytest.raises(ValueError):
            polyglot_sql.validate_with_schema("SELECT 1", schema)


@pytest.mark.parametrize("with_schema", [False, True])
def test_validation_function_depth_guard_options(with_schema):
    schema = {"tables": [{"name": "records", "columns": [{"name": "value", "type": "INTEGER"}]}]}
    validate = polyglot_sql.validate_with_schema if with_schema else polyglot_sql.validate
    extra = (schema,) if with_schema else ()
    for depth in (64, 65):
        sql = "SELECT " + "COALESCE(" * depth + "value" + ", 0)" * depth + " FROM records"
        result = validate(sql, *extra, dialect="snowflake")
        assert result.valid == (depth == 64)
        if depth == 65:
            assert "E_GUARD_FUNCTION_NESTING_DEPTH_EXCEEDED" in result.errors[0].message
        for limit in (128, None):
            assert validate(sql, *extra, dialect="snowflake", complexity_guard={"maxFunctionCallDepth": limit}).valid
    result = validate(sql, *extra, complexity_guard={"maxFunctionCallDepth": None, "maxInputBytes": 1})
    assert not result.valid
    assert "E_GUARD_INPUT_TOO_LARGE" in result.errors[0].message
    assert validate("SELECT 1", *extra).valid
    assert not validate("SELECT 1, FROM records", *extra, strict_syntax=True, complexity_guard={"maxFunctionCallDepth": 128}).valid


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


@pytest.mark.parametrize("dialect", ["snowflake", "duckdb", "postgres", "bigquery", "tsql"])
@pytest.mark.parametrize("alias", ["item_id", "merged_id"])
def test_validate_with_schema_order_by_output_alias(dialect, alias):
    # Issue #458: ORDER BY refers to the output, not either joined input.
    schema = {"strict": True, "tables": [
        {"name": name, "columns": [{"name": "item_id", "type": "NUMBER"}]}
        for name in ["current_items", "archived_items"]
    ]}
    sql = (
        f"SELECT COALESCE(c.item_id, a.item_id) AS {alias} "
        "FROM current_items AS c "
        "FULL JOIN archived_items AS a ON c.item_id = a.item_id "
        f"ORDER BY {alias}"
    )
    result = polyglot_sql.validate_with_schema(
        sql, schema, dialect=dialect, check_references=True, strict=True,
    )
    assert result.valid, result.errors
    assert result.errors == []


@pytest.mark.parametrize("strict", [False, True])
def test_validate_with_schema_order_by_alias_preserves_input_diagnostics(schema, strict):
    sql = (
        "SELECT order_id AS order_id FROM orders o "
        "JOIN customers c ON o.order_id = c.order_id ORDER BY order_id"
    )
    result = polyglot_sql.validate_with_schema(
        sql, schema, dialect="snowflake", check_references=True, strict=strict,
    )
    assert result.valid is not strict
    assert len(result.errors) == 1
    error = result.errors[0]
    assert error.code == ("E221" if strict else "W222")
    assert error.severity == ("error" if strict else "warning")
    assert error.start == sql.index("order_id")
    assert sql[error.start:error.end] == "order_id"


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


@pytest.fixture
def lambda_schema():
    return {"strict": True, "tables": [
        {"name": "items", "columns": [{"name": "item_id", "type": "NUMBER"}]},
    ]}


@pytest.mark.parametrize("sql", [
    "SELECT quantity + 1 AS adjusted_quantity, adjusted_quantity * 2 AS doubled_quantity FROM items",
    "SELECT quantity + 1 AS a, a * 2 AS b, b + a AS c FROM items",
    'SELECT quantity AS "Adjusted", "Adjusted" + 1 AS b FROM items',
    "WITH q AS (SELECT quantity AS a, a + 1 AS b FROM items) SELECT b FROM q",
    "SELECT quantity AS a, TRANSFORM(ARRAY_CONSTRUCT(quantity), x INT -> x + a) AS b FROM items",
    "SELECT quantity > 0 AS x, TRANSFORM(ARRAY_CONSTRUCT(quantity), x INT -> x + 1) AS b FROM items",
])
def test_validate_with_schema_snowflake_projection_aliases(sql):
    # The first case is the exact issue #460 query and schema.
    schema = {"strict": True, "tables": [
        {"name": "items", "columns": [{"name": "quantity", "type": "NUMBER"}]},
    ]}
    result = polyglot_sql.validate_with_schema(
        sql, schema, dialect="snowflake", check_references=True, check_types=True,
    )
    assert result.valid, result.errors
    assert result.errors == []


@pytest.mark.parametrize("strict", [False, True])
def test_validate_with_schema_projection_alias_diagnostics(strict):
    schema = {"tables": [
        {"name": "items", "columns": [{"name": "quantity", "type": "NUMBER"}]},
    ]}
    sql = 'SELECT \'😀\', quantity AS "数量", "数量" + missing AS doubled FROM items'
    result = polyglot_sql.validate_with_schema(
        sql, schema, dialect="snowflake", check_references=True, strict=strict,
    )
    assert result.valid is not strict
    assert len(result.errors) == 1
    error = result.errors[0]
    assert error.code == "E201"
    assert error.severity == ("error" if strict else "warning")
    assert error.start == sql.index("missing")
    assert sql[error.start:error.end] == "missing"


def test_validate_with_schema_projection_alias_types_and_input_precedence():
    schema = {"tables": [
        {"name": "items", "columns": [
            {"name": "quantity", "type": "NUMBER"},
            {"name": "active", "type": "BOOLEAN"},
        ]},
    ]}
    for sql in [
        "SELECT quantity AS active, active + 1 AS doubled FROM items",
        "SELECT active AS a, a AS b, b + 1 AS doubled FROM items",
        "WITH q AS (SELECT active AS flag FROM items), r AS (SELECT flag AS a, a + 1 AS b FROM q) SELECT b FROM r",
    ]:
        result = polyglot_sql.validate_with_schema(
            sql, schema, dialect="snowflake", check_references=True, check_types=True,
        )
        assert not result.valid
        assert [e.code for e in result.errors] == ["E212"]
    # Input quantity, not the BOOLEAN alias, determines this reference's type.
    for sql in [
        "SELECT active AS quantity, quantity + 1 AS doubled FROM items",
        'SELECT quantity AS "active", "active" + 1 AS doubled FROM items',
    ]:
        result = polyglot_sql.validate_with_schema(
            sql, schema, dialect="snowflake", check_references=True, check_types=True,
        )
        assert result.valid, result.errors


@pytest.mark.parametrize("dialect,expression", [
    ("snowflake", "TRANSFORM(ARRAY_CONSTRUCT(item_id), value -> value + 1)"),
    ("snowflake", "FILTER(ARRAY_CONSTRUCT(item_id), value -> value > 0)"),
    ("snowflake", "REDUCE(ARRAY_CONSTRUCT(item_id), 0, (acc, value) -> acc + value)"),
    ("snowflake", "TRANSFORM(ARRAY_CONSTRUCT(item_id), value INT -> value + item_id)"),
    ("duckdb", "list_transform([item_id], lambda value: value + 1)"),
    ("duckdb", "list_transform([{'amount': item_id}], value -> value.amount)"),
    ("spark", "transform(array(item_id), value -> value + 1)"),
    ("trino", "transform(ARRAY[item_id], value -> value + 1)"),
    ("clickhouse", "arrayMap(value -> value + 1, [item_id])"),
])
def test_validate_with_schema_lambda_parameters(lambda_schema, dialect, expression):
    # The first case is the exact issue #459 query and schema.
    result = polyglot_sql.validate_with_schema(
        f"SELECT {expression} FROM items", lambda_schema,
        dialect=dialect, check_references=True, strict=True,
    )
    assert result.valid, result.errors
    assert result.errors == []


@pytest.mark.parametrize("strict", [False, True])
def test_validate_with_schema_lambda_capture_diagnostic(lambda_schema, strict):
    sql = (
        "SELECT TRANSFORM(ARRAY_CONSTRUCT(item_id), value -> value + missing) "
        "FROM items"
    )
    result = polyglot_sql.validate_with_schema(
        sql, lambda_schema, dialect="snowflake", check_references=True, strict=strict,
    )
    assert result.valid is not strict
    assert len(result.errors) == 1
    error = result.errors[0]
    assert error.code == "E201"
    assert error.severity == ("error" if strict else "warning")
    assert sql[error.start:error.end] == "missing"


@pytest.mark.parametrize("parameter_type", ["", "INT"])
def test_validate_with_schema_lambda_type_shadowing(lambda_schema, parameter_type):
    lambda_schema["tables"][0]["columns"].append({"name": "value", "type": "BOOLEAN"})
    sql = (
        f"SELECT TRANSFORM(ARRAY_CONSTRUCT(item_id), value {parameter_type} -> value + 1) "
        "FROM items"
    )
    result = polyglot_sql.validate_with_schema(
        sql, lambda_schema, dialect="snowflake", check_references=True, check_types=True,
    )
    assert result.valid, result.errors
    assert result.errors == []
    invalid = polyglot_sql.validate_with_schema(
        "SELECT TRANSFORM(ARRAY_CONSTRUCT(TRUE), item_id BOOLEAN -> item_id + 1) FROM items",
        lambda_schema, dialect="snowflake", check_references=True, check_types=True,
    )
    assert not invalid.valid
    assert [error.code for error in invalid.errors] == ["E212"]


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
    aggregate = next(e for e in result.errors if e.code == "E230")
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
