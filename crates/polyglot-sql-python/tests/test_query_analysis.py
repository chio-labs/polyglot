import pytest

import polyglot_sql


def test_review_lambda_analysis_and_confidence():
    schema = {"tables": [{"name": "items", "columns": [{"name": "quantity", "type": "INT"}]}]}
    result = polyglot_sql.analyze_query(
        "SELECT TRANSFORM(ARRAY_CONSTRUCT(quantity), x -> x+1) FROM items",
        {"schema": schema, "dialect": "snowflake"},
    )
    projection = result["projections"][0]
    assert projection["typeHint"].startswith("ARRAY")
    assert [r["column"].lower() for r in projection["upstream"]] == ["quantity"]
    result = polyglot_sql.analyze_query("SELECT missing FROM items WHERE missing>0", {"schema": schema})
    assert result["projections"][0]["upstream"][0]["confidence"] == "unknown"
    assert result["columnUses"][0]["references"][0]["confidence"] == "unknown"
    with pytest.raises(ValueError):
        polyglot_sql.analyze_query("SELECT 1", {"scheam": schema})


def test_analyze_query_complexity_guard_options():
    expr = "COALESCE(" * 65 + "value" + ", 0)" * 65
    sql = f"WITH c AS (SELECT {expr} AS value FROM records) SELECT value FROM c WHERE {expr} > 0"
    with pytest.raises(polyglot_sql.ParseError, match="E_GUARD_FUNCTION_NESTING_DEPTH_EXCEEDED"):
        polyglot_sql.analyze_query(sql, dialect="snowflake")
    for limit in (128, None):
        guard = {"maxFunctionCallDepth": limit}
        for kwargs in ({"complexity_guard": guard}, {"options": {"complexityGuard": guard}}):
            result = polyglot_sql.analyze_query(sql, dialect="snowflake", **kwargs)
            assert len(result["projections"]) == 1
            assert "COALESCE" in result["cteFacts"][0]["bodySql"]
            assert result["columnUses"][0]["expressionSql"]
    for value in (True, 1.0, float("nan"), -1):
        with pytest.raises((TypeError, ValueError)):
            polyglot_sql.analyze_query("SELECT 1", {"complexityGuard": {"maxFunctionCallDepth": value}})
    with pytest.raises(ValueError, match="not both"):
        polyglot_sql.analyze_query("SELECT 1", {"complexityGuard": {}}, complexity_guard={})
    with pytest.raises(polyglot_sql.ParseError, match="E_GUARD_INPUT_TOO_LARGE"):
        polyglot_sql.analyze_query(sql, complexity_guard={"maxFunctionCallDepth": None, "maxInputBytes": 1})
    assert polyglot_sql.analyze_query("SELECT 1")["projections"]


@pytest.mark.parametrize("nullable, expected", [(False, "non_null"), (True, "nullable"), (None, "unknown")])
@pytest.mark.parametrize("dialect", ["snowflake", "duckdb", "postgresql", "bigquery"])
def test_analyze_query_preserves_schema_nullability_through_ctes(nullable, expected, dialect):
    schema = {
        "tables": [{
            "name": "orders",
            "columns": [{"name": "amount", "type": "INTEGER", "nullable": nullable}],
        }]
    }
    for sql in (
        "SELECT amount FROM orders",
        "WITH typed AS (SELECT amount FROM orders) SELECT amount FROM typed",
        "WITH a(value) AS (SELECT amount FROM orders), b AS (SELECT value FROM a) SELECT value FROM b",
        "SELECT value FROM (SELECT amount FROM orders) AS typed(value)",
    ):
        projection = polyglot_sql.analyze_query(sql, {"dialect": dialect, "schema": schema})["projections"][0]
        assert projection["nullability"] == expected, sql


@pytest.mark.parametrize("sql, expected", [
    ("WITH orders AS (SELECT NULL AS amount) SELECT amount FROM orders", "nullable"),
    ("WITH typed AS (SELECT COALESCE(amount, 0) AS amount FROM orders) SELECT amount FROM typed", "non_null"),
    ("WITH typed AS (SELECT o.amount FROM (SELECT 1) AS d LEFT JOIN orders AS o ON TRUE) SELECT amount FROM typed", "nullable"),
    ("WITH typed AS (SELECT amount FROM orders UNION ALL SELECT NULL AS amount) SELECT amount FROM typed", "nullable"),
    ('WITH d AS (SELECT 1 AS x) SELECT "D".x FROM d', "non_null"),
    ('SELECT a.x FROM (SELECT NULL AS x) AS "a" CROSS JOIN (SELECT 1 AS x) AS "A"', "non_null"),
])
def test_analyze_query_cte_nullability_respects_expression_and_scope(sql, expected):
    schema = {"tables": [{"name": "orders", "columns": [{"name": "amount", "type": "INTEGER", "nullable": False}]}]}
    analysis = polyglot_sql.analyze_query(sql, {"dialect": "snowflake", "schema": schema})
    assert analysis["projections"][0]["nullability"] == expected


@pytest.mark.parametrize("with_schema", [False, True])
def test_analyze_query_preserves_cast_type_through_cte_passthroughs(with_schema):
    sql = """
    WITH transformed AS (
      SELECT CAST(amount AS INTEGER) AS amount FROM raw_orders
    ), final AS (
      SELECT amount FROM transformed
    ) SELECT amount FROM final
    """
    options = {"dialect": "snowflake"}
    if with_schema:
        options["schema"] = {
            "tables": [{"name": "raw_orders", "columns": [{"name": "amount", "type": "VARCHAR"}]}]
        }
    projection = polyglot_sql.analyze_query(sql, options)["projections"][0]
    assert projection["typeHint"] == "INT"
    assert projection["transformKind"] == "direct"
    assert projection["castType"] is None
    assert [(ref["table"].lower(), ref["column"].lower()) for ref in projection["upstream"]] == [
        ("raw_orders", "amount")
    ]


def test_analyze_query_column_uses_preserve_occurrences_and_projection_lineage():
    sql = "SELECT '😀', o.id FROM orders o WHERE o.amount > 0 OR o.amount < -1"
    analysis = polyglot_sql.analyze_query(sql, dialect="duckdb")
    uses = analysis["columnUses"]
    assert len(uses) == 1
    fact = uses[0]
    assert fact["context"] == "filter"
    assert fact["scopePath"] == "root"
    assert fact["expressionPath"] == "where_clause.this"
    assert "span" not in fact  # No fabricated whole-expression range.
    assert len(fact["references"]) == 2
    assert fact["references"][0]["span"] != fact["references"][1]["span"]
    for reference in fact["references"]:
        assert reference["sourceName"] == "orders"
        assert reference["sourceAlias"] == "o"
        assert reference["column"] == "amount"
        assert reference["confidence"] == "resolved"
        span = reference["span"]
        assert sql[span["start"]:span["end"]] == "o.amount"
    assert [ref["column"] for ref in analysis["projections"][1]["upstream"]] == ["id"]
    assert polyglot_sql.analyze_query("SELECT 1")["columnUses"] == []


def test_analyze_query_column_uses_resolve_ctes_and_keep_filter_branches():
    analysis = polyglot_sql.analyze_query(
        "WITH base AS (SELECT id, amount FROM orders) "
        "SELECT id FROM base WHERE amount > 0 EXCEPT SELECT id FROM blocked",
        dialect="duckdb",
    )
    uses = analysis["columnUses"]
    predicate = next(fact for fact in uses if fact["context"] == "filter")
    assert predicate["scopePath"] == "root.branches[0]"
    assert predicate["references"][0]["table"] == "orders"
    assert predicate["references"][0]["column"] == "amount"
    branch = next(fact for fact in uses if fact["context"] == "set_operation_filter")
    assert branch["scopePath"] == "root.branches[1]"
    assert branch["references"][0]["table"] == "blocked"


def test_analyze_query_returns_projection_facts():
    result = polyglot_sql.analyze_query("SELECT a FROM t")

    assert result["shape"] == "select"
    assert result["projections"][0]["name"] == "a"
    assert result["projections"][0]["transformKind"] == "direct"
    assert result["projections"][0]["upstream"][0]["column"] == "a"


def test_analyze_query_accepts_schema_options():
    schema = {
        "tables": [
            {
                "name": "orders",
                "columns": [
                    {"name": "total", "type": "INT"},
                    {"name": "user_id", "type": "INT"},
                ],
            }
        ]
    }
    result = polyglot_sql.analyze_query(
        "SELECT CAST(total AS TEXT) AS total_text FROM orders",
        {"schema": schema, "dialect": "generic"},
    )

    assert result["relations"][0]["name"] == "orders"
    assert "total" in result["relations"][0]["columns"]
    assert result["projections"][0]["transformKind"] == "cast"
    assert result["projections"][0]["castType"] == "TEXT"


def test_analyze_query_tolerates_partial_schema():
    schema = {
        "tables": [
            {
                "name": "t",
                "columns": [{"name": "amount", "type": "INT"}],
            }
        ]
    }
    result = polyglot_sql.analyze_query(
        "SELECT order_id, amount FROM t",
        {"schema": schema, "dialect": "duckdb"},
    )

    assert [projection["name"] for projection in result["projections"]] == [
        "order_id",
        "amount",
    ]
    assert any(
        reference["column"] == "order_id"
        and reference["table"] == "t"
        and reference["confidence"] == "unknown"
        for reference in result["projections"][0]["upstream"]
    )
    assert any(
        reference["column"] == "amount" and reference["table"] == "t"
        for reference in result["projections"][1]["upstream"]
    )


def test_analyze_query_reports_transform_function_arguments():
    schema = {
        "tables": [
            {
                "name": "events",
                "columns": [{"name": "created_at", "type": "TIMESTAMP"}],
            }
        ]
    }
    result = polyglot_sql.analyze_query(
        "SELECT DATE_TRUNC('month', created_at) AS bucket FROM events",
        {"schema": schema, "dialect": "duckdb"},
    )

    transform_function = result["projections"][0]["transformFunction"]
    assert transform_function["name"] == "DATE_TRUNC"
    assert transform_function["literalArgs"] == ["month"]
    assert transform_function["columnArgs"][0]["table"] == "events"
    assert transform_function["columnArgs"][0]["column"] == "created_at"


def test_analyze_query_reports_base_tables_aliases_aggregates_and_precise_types():
    schema = {
        "tables": [
            {
                "name": "orders",
                "columns": [
                    {"name": "id", "type": "INT", "nullable": False},
                    {"name": "amount", "type": "DECIMAL(10,2)", "nullable": True},
                ],
            }
        ]
    }

    result = polyglot_sql.analyze_query(
        "SELECT o.id, SUM(o.amount) AS amount_sum FROM orders AS o GROUP BY o.id",
        {"schema": schema, "dialect": "generic"},
    )

    assert result["baseTables"][0]["name"] == "orders"
    assert result["baseTables"][0]["alias"] == "o"
    assert result["projections"][0]["upstream"][0]["table"] == "orders"
    assert result["projections"][0]["upstream"][0]["sourceAlias"] == "o"
    assert result["projections"][1]["transformKind"] == "aggregation"
    assert result["projections"][1]["typeHint"] == "DECIMAL(10, 2)"
    assert result["projections"][0]["nullability"] == "non_null"


def test_analyze_query_reports_structured_table_identity():
    result = polyglot_sql.analyze_query(
        'SELECT id FROM "my.catalog"."my.schema"."orders.table" AS o',
        dialect="duckdb",
    )

    base_table = result["baseTables"][0]
    assert base_table["name"] == "my.catalog.my.schema.orders.table"
    assert base_table["catalog"] == "my.catalog"
    assert base_table["schema"] == "my.schema"
    assert base_table["table"] == "orders.table"
    assert base_table["alias"] == "o"


def test_analyze_query_reports_cte_facts_and_star_projections():
    schema = {
        "tables": [
            {
                "name": "orders",
                "columns": [
                    {"name": "id", "type": "INT", "nullable": False},
                    {"name": "amount", "type": "DECIMAL(10,2)", "nullable": True},
                ],
            }
        ]
    }

    result = polyglot_sql.analyze_query(
        "WITH base AS (SELECT id, amount FROM orders) SELECT * FROM base",
        {"schema": schema, "dialect": "generic"},
    )

    assert result["cteFacts"][0]["name"] == "base"
    assert result["cteFacts"][0]["bodySql"] == "SELECT id, amount FROM orders"
    assert result["cteFacts"][0]["outputColumns"] == ["id", "amount"]
    assert result["starProjections"][0]["index"] == 0
    assert result["starProjections"][0]["expandedColumns"] == ["id", "amount"]


def test_analyze_query_resolves_pivot_alias_columns():
    result = polyglot_sql.analyze_query(
        "SELECT region2, p1 FROM (SELECT region, q, amt FROM sales) "
        "PIVOT(SUM(amt) FOR q IN ('Q1')) AS p(region2, p1)",
        {"dialect": "duckdb"},
    )

    region = next(
        projection
        for projection in result["projections"]
        if projection["name"] == "region2"
    )
    assert any(
        reference["table"] == "sales" and reference["column"] == "region"
        for reference in region["upstream"]
    )

    pivot_value = next(
        projection for projection in result["projections"] if projection["name"] == "p1"
    )
    assert any(
        reference["table"] == "sales" and reference["column"] == "amt"
        for reference in pivot_value["upstream"]
    )


def test_analyze_query_resolves_nested_set_operation_with_schema():
    result = polyglot_sql.analyze_query(
        "SELECT v FROM ((SELECT v FROM t1 UNION ALL SELECT v FROM t2) "
        "UNION ALL SELECT v FROM t3) u",
        {
            "dialect": "duckdb",
            "schema": {
                "tables": [
                    {"name": "t1", "columns": [{"name": "v", "type": "INT"}]},
                    {"name": "t2", "columns": [{"name": "v", "type": "INT"}]},
                    {"name": "t3", "columns": [{"name": "v", "type": "INT"}]},
                ]
            },
        },
    )

    upstream = result["projections"][0]["upstream"]
    assert {reference["table"] for reference in upstream} == {"t1", "t2", "t3"}


def test_analyze_query_resolves_unnest_output_alias_with_schema():
    result = polyglot_sql.analyze_query(
        "SELECT i FROM t, UNNEST(t.arr) AS i",
        {
            "dialect": "duckdb",
            "schema": {
                "tables": [
                    {"name": "t", "columns": [{"name": "arr", "type": "INT"}]},
                ]
            },
        },
    )

    upstream = result["projections"][0]["upstream"]
    assert any(
        reference["table"] == "t" and reference["column"] == "arr"
        for reference in upstream
    )


def test_analyze_query_unknown_dialect_raises_value_error():
    with pytest.raises(ValueError):
        polyglot_sql.analyze_query("SELECT 1", dialect="not_a_dialect")


def test_analyze_query_rejects_invalid_options():
    with pytest.raises(ValueError):
        polyglot_sql.analyze_query("SELECT 1", "not an options object")
