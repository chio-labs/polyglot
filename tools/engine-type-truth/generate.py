# /// script
# requires-python = ">=3.10"
# dependencies = ["duckdb==1.5.2", "psycopg[binary]>=3.2"]
# ///
"""Record DuckDB and PostgreSQL result types for a synthetic expression matrix.

Each case is bound by the real engine against a one-row synthetic table `t`.
DuckDB types come from DESCRIBE (cross-checked with typeof()); PostgreSQL types
come from a temporary view's column type (cross-checked with pg_typeof()), so
declared numeric typmods are retained. Expressions the engine rejects are kept
with their error class so replays can confirm they are skipped deliberately.

Usage:
    docker run -d --rm --name pg-type-truth -e POSTGRES_PASSWORD=pw -p 55432:5432 postgres:17
    uv run tools/engine-type-truth/generate.py --postgres-dsn postgresql://postgres:pw@localhost:55432/postgres
"""

from __future__ import annotations

import argparse
import json
import pathlib
from itertools import product

OUT_DIR = pathlib.Path(__file__).resolve().parents[2] / "crates/polyglot-sql/tests/fixtures/engine_types"

# (column name, DuckDB type, sample literal)
DUCKDB_COLUMNS = [
    ("c_tinyint", "TINYINT", "1"),
    ("c_smallint", "SMALLINT", "2"),
    ("c_integer", "INTEGER", "3"),
    ("c_bigint", "BIGINT", "4"),
    ("c_hugeint", "HUGEINT", "5"),
    ("c_utinyint", "UTINYINT", "6"),
    ("c_usmallint", "USMALLINT", "7"),
    ("c_uinteger", "UINTEGER", "8"),
    ("c_ubigint", "UBIGINT", "9"),
    ("c_uhugeint", "UHUGEINT", "10"),
    ("c_dec4_1", "DECIMAL(4,1)", "1.5"),
    ("c_dec9_2", "DECIMAL(9,2)", "2.25"),
    ("c_dec18_4", "DECIMAL(18,4)", "3.125"),
    ("c_dec38_10", "DECIMAL(38,10)", "4.5"),
    ("c_real", "REAL", "1.5"),
    ("c_double", "DOUBLE", "2.5"),
    ("c_varchar", "VARCHAR", "'abc'"),
    ("c_boolean", "BOOLEAN", "TRUE"),
    ("c_date", "DATE", "DATE '2026-01-02'"),
    ("c_timestamp", "TIMESTAMP", "TIMESTAMP '2026-01-02 03:04:05'"),
    ("c_timestamptz", "TIMESTAMPTZ", "TIMESTAMPTZ '2026-01-02 03:04:05+00'"),
    ("c_time", "TIME", "TIME '03:04:05'"),
    ("c_interval", "INTERVAL", "INTERVAL 1 DAY"),
]

POSTGRES_COLUMNS = [
    ("c_smallint", "smallint", "1"),
    ("c_integer", "integer", "2"),
    ("c_bigint", "bigint", "3"),
    ("c_num10_2", "numeric(10,2)", "1.25"),
    ("c_num20_5", "numeric(20,5)", "2.5"),
    ("c_numeric", "numeric", "3.75"),
    ("c_real", "real", "1.5"),
    ("c_double", "double precision", "2.5"),
    ("c_text", "text", "'abc'"),
    ("c_varchar", "varchar(20)", "'def'"),
    ("c_boolean", "boolean", "true"),
    ("c_date", "date", "DATE '2026-01-02'"),
    ("c_timestamp", "timestamp", "TIMESTAMP '2026-01-02 03:04:05'"),
    ("c_timestamptz", "timestamptz", "TIMESTAMPTZ '2026-01-02 03:04:05+00'"),
    ("c_time", "time", "TIME '03:04:05'"),
    ("c_interval", "interval", "INTERVAL '1 day'"),
]

DUCKDB_NUMERIC = [c for c, _, _ in DUCKDB_COLUMNS[:16]]
POSTGRES_NUMERIC = [c for c, _, _ in POSTGRES_COLUMNS[:8]]


def expression_cases(dialect: str) -> list[tuple[str, str, str]]:
    """Return (id, category, sql) triples."""
    duck = dialect == "duckdb"
    numeric = DUCKDB_NUMERIC if duck else POSTGRES_NUMERIC
    text = "c_varchar" if duck else "c_text"
    cases: list[tuple[str, str, str]] = []

    def add(category: str, expr: str, sql: str | None = None) -> None:
        cases.append((f"{category}: {expr}", category, sql or f"SELECT {expr} AS v FROM t"))

    ops = ["+", "-", "*", "/", "%"] + (["//"] if duck else [])
    for op, left, right in product(ops, numeric, numeric):
        add("arithmetic", f"{left} {op} {right}")
    literals = ["7", "2", "3000000000", "1.5", "2.25", "1e3"]
    for op, left, right in product(ops, literals, literals):
        add("arithmetic_literal", f"{left} {op} {right}")
    for op, column, literal in product(ops, ["c_integer", "c_bigint", numeric[-3], numeric[-1]], ["2", "1.5"]):
        add("arithmetic_literal", f"{column} {op} {literal}")
    add("arithmetic_literal", "SUM(c_integer) / COUNT(*)")
    add("arithmetic_literal", "-c_integer")
    add("arithmetic_literal", "-c_double")
    for column in numeric:
        add("arithmetic_literal", f"-{column}")

    for left, right in [
        (text, text),
        (text, "c_integer"),
        (text, "c_date"),
        ("'a'", "'b'"),
        ("'a'", text),
        (text, "c_boolean"),
        (text, numeric[-1]),
    ] + ([("c_integer", "c_integer")] if duck else [("c_varchar", "c_varchar"), (text, "c_varchar")]):
        add("concat", f"{left} || {right}")
    add("concat", f"CONCAT({text}, c_integer)")
    add("concat", f"CONCAT({text}, {text})")
    add("concat", f"CONCAT_WS(',', {text}, c_integer)")

    for expr in [
        "c_integer = c_bigint",
        "c_integer < c_double",
        f"{text} <> 'x'",
        "c_date >= c_timestamp",
        "c_boolean AND c_integer > 1",
        "c_boolean OR c_boolean",
        "NOT c_boolean",
        "c_integer IS NULL",
        "c_integer IS NOT NULL",
        "c_integer BETWEEN 1 AND 3",
        "c_integer IN (1, 2)",
        f"{text} LIKE 'a%'",
        f"{text} ILIKE 'a%'",
        f"{text} NOT LIKE 'a%'",
        "c_boolean IS TRUE",
        "c_integer IS DISTINCT FROM c_bigint",
        "EXISTS (SELECT 1)",
    ]:
        add("comparison", expr)

    aggregate_inputs = numeric + [text, "c_boolean", "c_date", "c_timestamp", "c_interval"]
    for function, column in product(["SUM", "AVG", "MIN", "MAX", "COUNT", "STDDEV"], aggregate_inputs):
        add("aggregate", f"{function}({column})")
    add("aggregate", "COUNT(*)")
    for function in ["VARIANCE", "STDDEV_POP", "VAR_POP"]:
        for column in ["c_integer", numeric[-3], "c_double"]:
            add("aggregate", f"{function}({column})")
    for function in ["BOOL_AND", "BOOL_OR"]:
        add("aggregate", f"{function}(c_boolean)")
    add("aggregate", f"STRING_AGG({text}, ',')")

    unify = (
        ["c_smallint", "c_integer", "c_bigint", numeric[3] if duck else "c_num10_2"]
        + ([numeric[10], numeric[11]] if duck else ["c_num20_5", "c_numeric"])
        + ["c_real", "c_double", text, "c_date", "c_timestamp"]
    )
    for left, right in product(unify, unify):
        add("conditional", f"CASE WHEN c_boolean THEN {left} ELSE {right} END")
        add("conditional", f"COALESCE({left}, {right})")
        if left == right or (left in numeric and right in numeric):
            add("conditional", f"NULLIF({left}, {right})")
        if duck:
            add("conditional", f"IFNULL({left}, {right})")
    for literal in ["1", "1.5", "'x'", "NULL"]:
        add("conditional", f"COALESCE(c_integer, {literal})")
        add("conditional", f"CASE WHEN c_boolean THEN c_integer ELSE {literal} END")
    add("conditional", "CASE WHEN c_boolean THEN 1 END")
    add("conditional", "CASE WHEN c_boolean THEN 1 ELSE 2.5 END")

    for left, right in product(unify, unify):
        add(
            "set_operation",
            f"{left} UNION ALL {right}",
            f"SELECT {left} AS v FROM t UNION ALL SELECT {right} AS v FROM t",
        )
    for left, right in [("1", "2.5"), ("1", "3000000000"), ("'a'", "'bc'"), ("NULL", "c_integer")]:
        add(
            "set_operation",
            f"{left} UNION ALL {right}",
            f"SELECT {left} AS v FROM t UNION ALL SELECT {right} AS v FROM t",
        )

    interval = "INTERVAL 1 DAY" if duck else "INTERVAL '1 day'"
    for expr in [
        "c_date + 1",
        "1 + c_date",
        "c_date - 1",
        "c_date - c_date",
        f"c_date + {interval}",
        f"c_date - {interval}",
        f"c_timestamp + {interval}",
        f"c_timestamp - {interval}",
        f"c_timestamptz + {interval}",
        "c_timestamp - c_timestamp",
        "c_timestamptz - c_timestamptz",
        "c_date - c_timestamp",
        f"c_time + {interval}",
        "c_time - c_time",
        "c_date + c_time",
        "c_interval + c_interval",
        "c_interval * 2",
        "c_interval / 2",
        "-c_interval",
        interval,
        "c_timestamp - c_date",
    ]:
        add("datetime", expr)

    casts = (
        ["TINYINT", "SMALLINT", "INTEGER", "BIGINT", "HUGEINT", "UBIGINT", "DECIMAL", "DECIMAL(10,2)", "DECIMAL(5)",
         "NUMERIC(12,4)", "REAL", "FLOAT", "DOUBLE", "VARCHAR", "VARCHAR(10)", "TEXT", "BOOLEAN", "DATE",
         "TIMESTAMP", "TIMESTAMPTZ", "TIME", "INTERVAL", "BLOB", "UUID", "JSON"]
        if duck
        else ["SMALLINT", "INTEGER", "INT", "BIGINT", "NUMERIC", "NUMERIC(10,2)", "DECIMAL(12,4)", "REAL",
              "DOUBLE PRECISION", "FLOAT", "TEXT", "VARCHAR", "VARCHAR(10)", "CHAR(3)", "BOOLEAN", "DATE",
              "TIMESTAMP", "TIMESTAMPTZ", "TIME", "INTERVAL", "BYTEA", "UUID", "JSON", "JSONB"]
    )
    for target in casts:
        source = "'00000000-0000-0000-0000-000000000000'" if target == "UUID" else (
            "'{}'" if target.startswith("JSON") else ("'1 day'" if target == "INTERVAL" else (
                "'2026-01-02'" if target in {"DATE", "TIMESTAMP", "TIMESTAMPTZ"} else (
                    "'03:04:05'" if target == "TIME" else (
                        "'abc'" if target in {"BLOB", "BYTEA"} else (
                            "'true'" if target == "BOOLEAN" else "'1'"))))))
        add("cast", f"CAST({source} AS {target})")
    add("cast", "c_integer::BIGINT")
    add("cast", f"CAST(c_integer AS {'VARCHAR' if duck else 'TEXT'})")

    for function, column in product(["ROUND", "ABS", "FLOOR", "CEIL", "SIGN"], numeric):
        add("scalar", f"{function}({column})")
    for column in numeric:
        add("scalar", f"ROUND({column}, 2)")
    for expr in [
        f"LENGTH({text})",
        f"CHAR_LENGTH({text})",
        f"SUBSTRING({text}, 1, 2)",
        f"SUBSTR({text}, 1, 2)",
        f"UPPER({text})",
        f"LOWER({text})",
        f"TRIM({text})",
        f"REPLACE({text}, 'a', 'b')",
        f"LEFT({text}, 2)",
        f"POSITION('a' IN {text})",
        "DATE_TRUNC('month', c_date)",
        "DATE_TRUNC('month', c_timestamp)",
        "DATE_TRUNC('month', c_timestamptz)",
        "EXTRACT(YEAR FROM c_date)",
        "EXTRACT(YEAR FROM c_timestamp)",
        "EXTRACT(EPOCH FROM c_timestamp)",
        "EXTRACT(DOW FROM c_date)",
        "DATE_PART('year', c_date)",
        "NOW()",
        "SQRT(c_double)",
        "POWER(c_integer, 2)",
        "LN(c_double)",
        "EXP(c_integer)",
        "MOD(c_integer, 2)",
        "GREATEST(c_integer, c_bigint)",
        "LEAST(c_integer, c_double)",
        "RANDOM()",
        f"MD5({text})",
        "ROW_NUMBER() OVER (ORDER BY c_integer)",
        "RANK() OVER (ORDER BY c_integer)",
        "LAG(c_integer) OVER (ORDER BY c_integer)",
        "SUM(c_integer) OVER ()",
        "COUNT(*) OVER ()",
    ]:
        add("scalar", expr)
    for expr in (["TODAY()", "CURRENT_DATABASE()", "GEN_RANDOM_UUID()", "EPOCH(c_timestamp)", "YEAR(c_date)",
                  "DAYNAME(c_date)", "STRFTIME(c_date, '%Y')", "LIST_VALUE(1, 2)"]
                 if duck else ["CURRENT_DATABASE()", "GEN_RANDOM_UUID()", "TO_CHAR(c_date, 'YYYY')", "AGE(c_timestamp)",
                               "CLOCK_TIMESTAMP()", "STATEMENT_TIMESTAMP()"]):
        add("scalar", expr)

    niladic = ["USER", "CURRENT_USER", "SESSION_USER", "CURRENT_ROLE", "CURRENT_SCHEMA", "CURRENT_CATALOG",
               "CURRENT_DATE", "CURRENT_TIME", "CURRENT_TIMESTAMP", "LOCALTIME", "LOCALTIMESTAMP"]
    for name in niladic:
        add("niladic", name)
        add("niladic", f"{name} without FROM", f"SELECT {name} AS v")
        add("niladic", f"{name} in WHERE", f"SELECT c_integer AS v FROM t WHERE {name} IS NOT NULL")
    return cases


def table_ddl(columns) -> tuple[str, str]:
    ddl = "CREATE TABLE t (" + ", ".join(f"{name} {ty}" for name, ty, _ in columns) + ")"
    insert = "INSERT INTO t VALUES (" + ", ".join(value for _, _, value in columns) + ")"
    return ddl, insert


def error_class(message: str) -> str:
    return message.split(":", 1)[0].strip().split("\n")[0][:80]


def run_duckdb() -> dict:
    import duckdb

    conn = duckdb.connect()
    ddl, insert = table_ddl(DUCKDB_COLUMNS)
    conn.execute(ddl)
    conn.execute(insert)
    cases = []
    for case_id, category, sql in expression_cases("duckdb"):
        record = {"id": case_id, "category": category, "sql": sql}
        try:
            described = conn.execute(f"DESCRIBE {sql}").fetchall()[0][1]
            runtime = conn.execute(f"SELECT typeof(v) FROM ({sql}) LIMIT 1").fetchall()
            if runtime and runtime[0][0] != described:
                raise AssertionError(f"{case_id}: DESCRIBE {described} != typeof {runtime[0][0]}")
            record["type"] = described
        except duckdb.Error as error:
            record["error"] = error_class(str(error))
        cases.append(record)
    return {
        "engine": "duckdb",
        "engine_version": duckdb.__version__,
        "method": "DESCRIBE of SELECT over one-row synthetic table t, cross-checked with typeof()",
        "schema": {"tables": [{"name": "t", "columns": [{"name": n, "type": ty} for n, ty, _ in DUCKDB_COLUMNS]}]},
        "cases": cases,
    }


def run_postgres(dsn: str) -> dict:
    import psycopg

    conn = psycopg.connect(dsn, autocommit=False)
    cur = conn.cursor()
    cur.execute("DROP TABLE IF EXISTS t")
    ddl, insert = table_ddl(POSTGRES_COLUMNS)
    cur.execute(ddl)
    cur.execute(insert)
    cur.execute("SET TIME ZONE 'UTC'")
    conn.commit()
    cur.execute("SHOW server_version")
    version = cur.fetchone()[0]
    cases = []
    for case_id, category, sql in expression_cases("postgres"):
        record = {"id": case_id, "category": category, "sql": sql}
        try:
            cur.execute(f"CREATE TEMP VIEW type_probe AS {sql}")
            cur.execute(
                "SELECT format_type(atttypid, atttypmod) FROM pg_attribute "
                "WHERE attrelid = 'type_probe'::regclass AND attnum = 1"
            )
            described = cur.fetchone()[0]
            cur.execute(f"SELECT pg_typeof(v)::text FROM ({sql}) AS probe LIMIT 1")
            row = cur.fetchone()
            base = described.split("(")[0]
            if row and row[0] != base and not (row[0] == "unknown" and described == "text"):
                raise AssertionError(f"{case_id}: view {described} != pg_typeof {row[0]}")
            record["type"] = described
        except psycopg.Error as error:
            record["error"] = type(error).__name__
        conn.rollback()
        cases.append(record)
    cur.execute("DROP TABLE t")
    conn.commit()
    return {
        "engine": "postgres",
        "engine_version": version,
        "method": "temporary view column type over one-row synthetic table t, cross-checked with pg_typeof()",
        "schema": {"tables": [{"name": "t", "columns": [{"name": n, "type": ty} for n, ty, _ in POSTGRES_COLUMNS]}]},
        "cases": cases,
    }


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--postgres-dsn")
    args = parser.parse_args()
    OUT_DIR.mkdir(parents=True, exist_ok=True)
    outputs = {"duckdb": run_duckdb()}
    if args.postgres_dsn:
        outputs["postgres"] = run_postgres(args.postgres_dsn)
    for name, data in outputs.items():
        path = OUT_DIR / f"{name}.json"
        records = data.pop("cases")
        cases = ",\n".join("  " + json.dumps(case) for case in records)
        header = json.dumps(data, indent=1)[:-2]
        path.write_text(f'{header},\n "cases": [\n{cases}\n ]\n}}\n')
        typed = sum("type" in case for case in records)
        print(f"{name}: {len(records)} cases, {typed} typed, {len(records) - typed} engine errors -> {path}")


if __name__ == "__main__":
    main()
