import pytest

import polyglot_sql


@pytest.mark.parametrize(
    "name, identifier",
    [
        ("field name", '"field name"'),
        ('a"b', '"a""b"'),
        ("select", '"select"'),
        ("a INT, b", '"a INT, b"'),
    ],
)
def test_generate_constructed_struct_quotes_field_names(name, identifier):
    ast = {
        "data_type": {
            "data_type": "struct",
            "nested": False,
            "fields": [{"name": name, "data_type": {"data_type": "text"}}],
        }
    }
    sql = polyglot_sql.generate(ast, dialect="duckdb")[0]
    assert sql == f"STRUCT({identifier} TEXT)"
    parsed = polyglot_sql.parse_data_type(sql, dialect="duckdb")
    fields = parsed.to_dict()["data_type"]["fields"]
    assert len(fields) == 1
    assert fields[0]["name"] == identifier
    assert parsed.sql("duckdb") == sql


def test_generate_parsed_struct_preserves_quotes_across_dialects():
    sql = 'STRUCT("a""b" INT, "field name" INT)'
    parsed = polyglot_sql.parse_data_type(sql, dialect="duckdb")
    assert parsed.sql("duckdb") == sql
    assert parsed.sql("spark") == 'STRUCT<`a"b`: INT, `field name`: INT>'


def test_generate_roundtrip_from_parse_one():
    ast = polyglot_sql.parse_one("SELECT 1", dialect="postgres")
    out = polyglot_sql.generate(ast, dialect="postgres")
    assert isinstance(out, list)
    assert len(out) == 1
    assert "SELECT 1" in out[0]


def test_generate_invalid_ast_raises_generate_error():
    with pytest.raises(polyglot_sql.GenerateError):
        polyglot_sql.generate({"bad": "ast"}, dialect="postgres")


def test_generate_accepts_legacy_empty_object_as_null():
    assert polyglot_sql.generate([{}], dialect="postgres") == ["NULL"]


def test_generate_accepts_is_null_array_shorthand():
    ast = {
        "is_null": [
            {
                "column": {
                    "name": {
                        "name": "deleted_at",
                        "quoted": False,
                        "trailing_comments": [],
                        "span": None,
                    },
                    "table": None,
                    "join_mark": False,
                    "trailing_comments": [],
                    "span": None,
                    "inferred_type": None,
                }
            }
        ]
    }
    assert polyglot_sql.generate(ast, dialect="postgres") == ["deleted_at IS NULL"]


def test_generate_list_of_asts_returns_list():
    ast1 = polyglot_sql.parse_one("SELECT 1", dialect="postgres")
    ast2 = polyglot_sql.parse_one("SELECT 2", dialect="postgres")
    out = polyglot_sql.generate([ast1, ast2], dialect="postgres")
    assert len(out) == 2


def test_generate_with_different_target_dialect_transforms_output():
    ast = polyglot_sql.parse_one("SELECT x::TEXT FROM t", dialect="postgres")
    out = polyglot_sql.generate(ast, dialect="mysql")
    assert out == ["SELECT CAST(x AS CHAR) FROM t"]


def test_generate_pretty_contains_newlines():
    ast = polyglot_sql.parse_one("SELECT a,b FROM t WHERE x=1", dialect="postgres")
    out = polyglot_sql.generate(ast, dialect="postgres", pretty=True)
    assert len(out) == 1
    assert "\n" in out[0]


def test_generate_unknown_dialect_raises_value_error():
    ast = polyglot_sql.parse_one("SELECT 1", dialect="postgres")
    with pytest.raises(ValueError):
        polyglot_sql.generate(ast, dialect="not_a_dialect")
