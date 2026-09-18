from concurrent.futures import ThreadPoolExecutor

import pytest

import polyglot_sql


@pytest.mark.parametrize("parse", [polyglot_sql.parse, polyglot_sql.parse_one])
def test_parse_function_depth_guard_options(parse):
    for depth in (64, 65):
        sql = "SELECT " + "COALESCE(" * depth + "value" + ", 0)" * depth + " FROM records"
        for options in (None, {}):
            if depth == 65:
                with pytest.raises(polyglot_sql.ParseError, match="E_GUARD_FUNCTION_NESTING_DEPTH_EXCEEDED"):
                    parse(sql, dialect="snowflake", complexity_guard=options)
            else:
                assert parse(sql, dialect="snowflake", complexity_guard=options)
        for limit in (128, None):
            assert parse(sql, dialect="snowflake", complexity_guard={"maxFunctionCallDepth": limit})
    assert parse(sql, dialect="clickhouse")
    with pytest.raises(polyglot_sql.ParseError, match="E_GUARD_INPUT_TOO_LARGE"):
        parse(sql, complexity_guard={"maxFunctionCallDepth": None, "maxInputBytes": 1})
    assert parse("SELECT 1")


@pytest.mark.parametrize("name", ["parse", "parse_one", "parse_data_type", "validate", "validate_with_schema", "analyze_query", "transpile"])
def test_complexity_options_reject_invalid_values_consistently(name):
    function = getattr(polyglot_sql, name)
    args = ("SELECT 1", {"tables": []}) if name == "validate_with_schema" else ("INT" if name == "parse_data_type" else "SELECT 1",)
    for value in (-1, True, False, 1.0, 1.5, float("nan"), float("inf"), "128", 2**100):
        with pytest.raises((TypeError, ValueError)):
            function(*args, complexity_guard={"maxFunctionCallDepth": value})
    with pytest.raises(ValueError, match="unknown field"):
        function(*args, complexity_guard={"max_function_call_depth": 128})


@pytest.mark.parametrize("into", [False, True])
def test_data_type_parse_guard_options(into):
    parse = polyglot_sql.parse_one if into else polyglot_sql.parse_data_type
    kwargs = {"into": polyglot_sql.DataType} if into else {}
    for options, code in [
        ({"maxInputBytes": 1}, "E_GUARD_INPUT_TOO_LARGE"),
        ({"maxTokens": 0}, "E_GUARD_TOKEN_BUDGET_EXCEEDED"),
        ({"maxAstNodes": 0}, "E_GUARD_AST_BUDGET_EXCEEDED"),
        ({"maxParserDepth": 0}, "E_GUARD_PARSER_DEPTH_EXCEEDED"),
    ]:
        with pytest.raises(polyglot_sql.ParseError, match=code):
            parse("DECIMAL(10, 2)", complexity_guard=options, **kwargs)
    assert parse("DECIMAL(10, 2)", complexity_guard={"maxParserDepth": None}, **kwargs)


@pytest.mark.parametrize(
    "dialect", ["snowflake", "duckdb", "postgres", "mysql", "tsql", "bigquery"]
)
def test_parse_preserves_identifier_and_column_spans(dialect):
    sql = "SELECT customer_id FROM orders"
    token = next(
        t for t in polyglot_sql.tokenize(sql, dialect=dialect)
        if t["text"] == "customer_id"
    )
    ast = polyglot_sql.parse_one(sql, dialect=dialect).to_dict()
    column = ast["select"]["expressions"][0]["column"]
    assert column["span"] == token["span"] == {
        "start": 7, "end": 18, "line": 1, "column": 19
    }
    assert column["name"]["span"] == token["span"]
    assert "span" in ast["select"]["from"]["expressions"][0]["table"]["name"]


def test_parse_spans_use_unicode_character_offsets_and_do_not_affect_equality():
    sql = 'SELECT "é😀", "a"."b" FROM "t"'
    ast = polyglot_sql.parse_one(sql, dialect="snowflake").to_dict()
    column = ast["select"]["expressions"][1]["column"]
    span = column["span"]
    assert sql[span["start"] : span["end"]] == '"a"."b"'
    assert polyglot_sql.parse_one("x") == polyglot_sql.parse_one("   x")
    assert "span" not in polyglot_sql.column("x").to_dict()["column"]["name"]


def test_parse_single_select_returns_list_of_expressions():
    ast_list = polyglot_sql.parse("SELECT 1", dialect="postgres")
    assert isinstance(ast_list, list)
    assert len(ast_list) == 1
    assert isinstance(ast_list[0], polyglot_sql.Expression)
    assert isinstance(ast_list[0], polyglot_sql.Select)
    assert ast_list[0].kind == "select"


def test_parse_one_returns_expression():
    ast = polyglot_sql.parse_one("SELECT 1", dialect="postgres")
    assert isinstance(ast, polyglot_sql.Expression)
    assert isinstance(ast, polyglot_sql.Select)
    assert ast.kind == "select"


def test_parse_one_returns_typed_subclass():
    ast = polyglot_sql.parse_one("SELECT 1", dialect="postgres")
    assert type(ast).__name__ == "Select"
    assert isinstance(ast, polyglot_sql.Select)
    assert isinstance(ast, polyglot_sql.Expression)
    # Not a different subclass
    assert not isinstance(ast, polyglot_sql.Insert)


def test_parse_returns_typed_subclasses():
    results = polyglot_sql.parse("SELECT 1; SELECT 2", dialect="postgres")
    assert all(isinstance(r, polyglot_sql.Select) for r in results)
    assert all(isinstance(r, polyglot_sql.Expression) for r in results)


def test_parse_tsql_try_catch_returns_typed_subclass():
    ast = polyglot_sql.parse_one(
        """
        BEGIN TRY
            INSERT INTO orders (id, amount) VALUES (1, 100.00);
        END TRY
        BEGIN CATCH
            INSERT INTO error_log (msg) VALUES (ERROR_MESSAGE());
        END CATCH
        """,
        dialect="tsql",
    )

    assert isinstance(ast, polyglot_sql.TryCatch)
    assert isinstance(ast, polyglot_sql.Expression)
    assert ast.kind == "try_catch"
    assert ast.sql("tsql").startswith("BEGIN TRY")


def test_parse_postgres_prepare_returns_typed_subclass():
    ast = polyglot_sql.parse_one(
        "PREPARE leak (int) AS SELECT id FROM sensitive_table WHERE id = $1",
        dialect="postgres",
    )

    assert isinstance(ast, polyglot_sql.Prepare)
    assert isinstance(ast, polyglot_sql.Expression)
    assert ast.kind == "prepare"
    assert ast.sql("postgres").startswith("PREPARE leak (INT) AS SELECT")


def test_parse_postgres_execute_prepared_statement():
    ast = polyglot_sql.parse_one("EXECUTE leak(1)", dialect="postgres")

    assert isinstance(ast, polyglot_sql.Execute)
    assert ast.kind == "execute"
    assert ast.sql("postgres") == "EXECUTE leak(1)"


def test_parse_tidb_ddl_returns_typed_statements_and_roundtrips():
    create = polyglot_sql.parse_one(
        "CREATE TABLE posts (id BIGINT AUTO_RANDOM PRIMARY KEY, title VARCHAR(255))",
        dialect="tidb",
    )
    assert isinstance(create, polyglot_sql.CreateTable)
    assert create.sql("tidb") == (
        "CREATE TABLE posts (id BIGINT AUTO_RANDOM PRIMARY KEY, title VARCHAR(255))"
    )

    split = polyglot_sql.parse_one(
        "SPLIT TABLE posts BETWEEN (1) AND (1000) REGIONS 4", dialect="tidb"
    )
    assert isinstance(split, polyglot_sql.SplitTable)
    assert split.sql("tidb") == (
        "SPLIT TABLE posts BETWEEN (1) AND (1000) REGIONS 4"
    )

    flashback = polyglot_sql.parse_one("FLASHBACK TABLE posts", dialect="tidb")
    assert isinstance(flashback, polyglot_sql.FlashbackTable)
    assert flashback.sql("tidb") == "FLASHBACK TABLE posts"


def test_parse_multiple_statements_returns_n_entries():
    ast_list = polyglot_sql.parse("SELECT 1; SELECT 2", dialect="postgres")
    assert len(ast_list) == 2
    assert ast_list[0].kind == "select"
    assert ast_list[1].kind == "select"


def test_parse_one_multiple_statements_raises_parse_error():
    with pytest.raises(polyglot_sql.ParseError):
        polyglot_sql.parse_one("SELECT 1; SELECT 2", dialect="postgres")


def test_parse_unknown_dialect_raises_value_error():
    with pytest.raises(ValueError):
        polyglot_sql.parse("SELECT 1", dialect="not_a_dialect")


def test_parse_one_unknown_dialect_raises_value_error():
    with pytest.raises(ValueError):
        polyglot_sql.parse_one("SELECT 1", dialect="not_a_dialect")


def test_parse_data_type_returns_data_type_expression():
    data_type = polyglot_sql.parse_data_type("DECIMAL(10, 2)", dialect="duckdb")

    assert isinstance(data_type, polyglot_sql.DataType)
    assert data_type.sql("duckdb") == "DECIMAL(10, 2)"


@pytest.mark.parametrize(
    "dialect,name,expected_sql",
    [
        ("duckdb", "HUGEINT", "INT128"),
        ("duckdb", "INT128", "INT128"),
        ("clickhouse", "Int128", "Int128"),
        ("starrocks", "LARGEINT", "LARGEINT"),
    ],
)
def test_parse_data_type_int128(dialect, name, expected_sql):
    data_type = polyglot_sql.parse_data_type(name, dialect=dialect)
    assert isinstance(data_type, polyglot_sql.DataType)
    assert data_type.to_dict() == {"data_type": {"data_type": "int128"}}
    assert data_type.sql(dialect) == expected_sql


def test_parse_data_type_nested_int128():
    data_type = polyglot_sql.parse_data_type("HUGEINT[]", dialect="duckdb")
    assert data_type.to_dict()["data_type"]["element_type"] == {"data_type": "int128"}
    assert data_type.sql("duckdb") == "INT128[]"


@pytest.mark.parametrize(
    "native,alias,tag,output",
    [
        ("UTINYINT", "UINT8", "uint8", "UTINYINT"),
        ("USMALLINT", "UINT16", "uint16", "USMALLINT"),
        ("UINTEGER", "UINT32", "uint32", "UINTEGER"),
        ("UBIGINT", "UINT64", "uint64", "UBIGINT"),
        ("UHUGEINT", "UINT128", "uint128", "UINT128"),
    ],
)
def test_parse_data_type_unsigned(native, alias, tag, output):
    for name in (native, alias):
        data_type = polyglot_sql.parse_data_type(name, dialect="duckdb")
        assert data_type.to_dict() == {"data_type": {"data_type": tag}}
        assert data_type.sql("duckdb") == output
        nested = polyglot_sql.parse_data_type(f"{name}[]", dialect="duckdb")
        assert nested.to_dict()["data_type"]["element_type"] == {"data_type": tag}
        assert nested.sql("duckdb") == f"{output}[]"


def test_parse_one_into_data_type_matches_sqlglot_compatibility_path():
    data_type = polyglot_sql.parse_one(
        "VARCHAR(255)", dialect="duckdb", into=polyglot_sql.DataType
    )

    assert isinstance(data_type, polyglot_sql.DataType)
    assert data_type.sql("duckdb") == "TEXT(255)"


def test_parse_one_into_only_supports_data_type():
    with pytest.raises(NotImplementedError):
        polyglot_sql.parse_one("a", into=polyglot_sql.Column)


def test_parse_data_type_rejects_trailing_sql():
    with pytest.raises(polyglot_sql.ParseError):
        polyglot_sql.parse_data_type("DECIMAL(10, 2) SELECT 1", dialect="duckdb")


def test_parse_invalid_sql_raises_parse_error():
    with pytest.raises(polyglot_sql.ParseError):
        polyglot_sql.parse("SELECT FROM", dialect="postgres")


def test_parse_calls_execute_concurrently_with_stable_results_and_errors():
    sql = " UNION ALL ".join(
        f"SELECT {value} AS id FROM events_{value}" for value in range(80)
    )

    def parse_valid(_):
        return polyglot_sql.parse_one(sql, dialect="postgres").sql("postgres")

    with ThreadPoolExecutor(max_workers=8) as executor:
        results = list(executor.map(parse_valid, range(32)))

    assert len(set(results)) == 1

    def parse_invalid(_):
        with pytest.raises(polyglot_sql.ParseError):
            polyglot_sql.parse("SELECT FROM", dialect="postgres")

    with ThreadPoolExecutor(max_workers=8) as executor:
        list(executor.map(parse_invalid, range(32)))
