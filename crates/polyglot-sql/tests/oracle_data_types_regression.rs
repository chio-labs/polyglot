use polyglot_sql::dialects::{Dialect, DialectType, TranspileOptions};
use polyglot_sql::{generate_data_type, parse_data_type};

const ISSUE_367_SQL: &str = r#"CREATE TABLE TEST_TYPES_MAPPING
(
  -- NUMERIC (DECIMAL / FIXED POINT)
  C_NUMBER         NUMBER             /*M:DECIMAL(38,30)/DOUBLE,S:DECIMAL(38,10)/FLOAT,P:NUMERIC*/,
  C_NUMBER_P       NUMBER(9)          /*M:INT,S:INT,P:INTEGER*/,
  C_NUMBER_P_L     NUMBER(18)         /*M:BIGINT,S:BIGINT,P:BIGINT*/,
  C_NUMBER_P_S     NUMBER(15,2)       /*M:DECIMAL(15,2),S:DECIMAL(15,2),P:NUMERIC(15,2)*/,
  C_NUMBER_NEG_S   NUMBER(5,-2)       /*M:DECIMAL(5,0),S:DECIMAL(5,0),P:NUMERIC(5,0)*/,

  -- NUMERIC (BINARY FLOATING POINT)
  C_BINARY_FLOAT   BINARY_FLOAT       /*M:FLOAT,S:REAL,P:REAL*/,
  C_BINARY_DOUBLE  BINARY_DOUBLE      /*M:DOUBLE,S:FLOAT,P:DOUBLE PRECISION*/,
  C_FLOAT          FLOAT              /*M:DOUBLE,S:FLOAT,P:DOUBLE PRECISION*/,
  C_FLOAT_P        FLOAT(53)          /*M:FLOAT/DOUBLE,S:FLOAT(53),P:DOUBLE PRECISION*/,

  -- CHARACTER (CHAR / VARCHAR)
  C_VARCHAR2_BYTE  VARCHAR2(100 BYTE) /*M:VARCHAR(100),S:VARCHAR(100),P:VARCHAR(100)*/,
  C_VARCHAR2_CHAR  VARCHAR2(100 CHAR) /*M:VARCHAR(100),S:NVARCHAR(100),P:VARCHAR(100)*/,
  C_CHAR           CHAR               /*M:CHAR(1),S:CHAR(1),P:CHAR(1)*/,
  C_CHAR_P         CHAR(10 CHAR)      /*M:CHAR(10),S:NCHAR(10),P:CHAR(10)*/,
  C_NVARCHAR2      NVARCHAR2(100)     /*M:VARCHAR(100),S:NVARCHAR(100),P:VARCHAR(100)*/,
  C_NCHAR          NCHAR(10)          /*M:CHAR(10),S:NCHAR(10),P:CHAR(10)*/,

  -- DATE & TIME
  C_DATE           DATE               /*M:DATETIME,S:DATETIME2(0),P:TIMESTAMP(0)*/,
  C_TIMESTAMP      TIMESTAMP          /*M:DATETIME(6),S:DATETIME2(6),P:TIMESTAMP(6)*/,
  C_TIMESTAMP_P    TIMESTAMP(3)       /*M:DATETIME(3),S:DATETIME2(3),P:TIMESTAMP(3)*/,
  C_TIMESTAMP_TZ   TIMESTAMP WITH TIME ZONE       /*M:DATETIME(6),S:DATETIMEOFFSET(6),P:TIMESTAMPTZ(6)*/,
  C_TIMESTAMP_LTZ  TIMESTAMP WITH LOCAL TIME ZONE /*M:TIMESTAMP(6),S:DATETIME2(6),P:TIMESTAMPTZ(6)*/,
  C_INTERVAL_YM    INTERVAL YEAR(2) TO MONTH      /*M:VARCHAR(30),S:VARCHAR(30),P:INTERVAL YEAR TO MONTH*/,
  C_INTERVAL_DS    INTERVAL DAY(2) TO SECOND(6)   /*M:TIME(6),S:TIME(6),P:INTERVAL DAY TO SECOND*/,

  -- LARGE OBJECTS & BINARY
  C_CLOB           CLOB               /*M:LONGTEXT,S:VARCHAR(MAX),P:TEXT*/,
  C_NCLOB          NCLOB              /*M:LONGTEXT,S:NVARCHAR(MAX),P:TEXT*/,
  C_BLOB           BLOB               /*M:LONGBLOB,S:VARBINARY(MAX),P:BYTEA*/,
  C_RAW            RAW(2000)          /*M:VARBINARY(2000),S:VARBINARY(2000),P:BYTEA*/,
  C_LONG           LONG               /*M:LONGTEXT,S:VARCHAR(MAX),P:TEXT*/,
  C_LONG_RAW       LONG RAW           /*M:LONGBLOB,S:VARBINARY(MAX),P:BYTEA*/,
  
  -- SYSTEM
  C_ROWID          ROWID              /*M:CHAR(18),S:CHAR(18),P:CHAR(18)*/
);"#;

fn render(data_type: &str, target: DialectType) -> String {
    let parsed = parse_data_type(data_type, DialectType::Oracle)
        .unwrap_or_else(|error| panic!("failed to parse {data_type}: {error}"));
    generate_data_type(&parsed, target)
        .unwrap_or_else(|error| panic!("failed to render {data_type} for {target}: {error}"))
}

fn transpile_issue(target: DialectType) -> String {
    Dialect::get(DialectType::Oracle)
        .transpile(ISSUE_367_SQL, target)
        .unwrap_or_else(|error| panic!("failed to transpile issue #367 for {target}: {error}"))
        .remove(0)
}

#[test]
fn issue_367_oracle_types_round_trip() {
    let cases = [
        ("CHAR(10 CHAR)", "CHAR(10 CHAR)"),
        ("VARCHAR2(100 BYTE)", "VARCHAR2(100 BYTE)"),
        ("NUMBER", "NUMBER"),
        ("NUMBER(15,2)", "NUMBER(15, 2)"),
        ("NUMBER(5,-2)", "NUMBER(5, -2)"),
        ("INTERVAL YEAR(2) TO MONTH", "INTERVAL YEAR(2) TO MONTH"),
        (
            "INTERVAL DAY(2) TO SECOND(6)",
            "INTERVAL DAY(2) TO SECOND(6)",
        ),
        ("LONG RAW", "LONG RAW"),
        ("ROWID", "ROWID"),
    ];

    for (input, expected) in cases {
        assert_eq!(render(input, DialectType::Oracle), expected, "{input}");
    }
}

#[test]
fn issue_367_numeric_mappings() {
    let cases = [
        ("NUMBER", "DECIMAL(65, 30)", "DECIMAL(38, 10)", "NUMERIC"),
        ("NUMBER(9)", "INT", "INT", "INT"),
        ("NUMBER(18)", "BIGINT", "BIGINT", "BIGINT"),
        (
            "NUMBER(15,2)",
            "DECIMAL(15, 2)",
            "DECIMAL(15, 2)",
            "DECIMAL(15, 2)",
        ),
        (
            "NUMBER(5,-2)",
            "DECIMAL(7, 0)",
            "DECIMAL(7, 0)",
            "NUMERIC(5, -2)",
        ),
        ("BINARY_FLOAT", "FLOAT", "REAL", "REAL"),
        ("BINARY_DOUBLE", "DOUBLE", "FLOAT", "DOUBLE PRECISION"),
        ("FLOAT", "DOUBLE", "FLOAT(53)", "DOUBLE PRECISION"),
        ("FLOAT(53)", "DOUBLE", "FLOAT(53)", "DOUBLE PRECISION"),
    ];

    for (input, mysql, tsql, postgres) in cases {
        assert_eq!(render(input, DialectType::MySQL), mysql, "{input}");
        assert_eq!(render(input, DialectType::TSQL), tsql, "{input}");
        assert_eq!(render(input, DialectType::PostgreSQL), postgres, "{input}");
    }
}

#[test]
fn issue_367_character_and_temporal_mappings() {
    let cases = [
        (
            "VARCHAR2(100 BYTE)",
            "VARCHAR(100)",
            "VARCHAR(100)",
            "VARCHAR(100)",
        ),
        (
            "VARCHAR2(100 CHAR)",
            "VARCHAR(100)",
            "NVARCHAR(100)",
            "VARCHAR(100)",
        ),
        ("CHAR", "CHAR(1)", "CHAR(1)", "CHAR(1)"),
        ("CHAR(10 CHAR)", "CHAR(10)", "NCHAR(10)", "CHAR(10)"),
        (
            "NVARCHAR2(100)",
            "NVARCHAR(100)",
            "NVARCHAR(100)",
            "VARCHAR(100)",
        ),
        ("NCHAR(10)", "NCHAR(10)", "NCHAR(10)", "CHAR(10)"),
        ("DATE", "DATETIME", "DATETIME2(0)", "TIMESTAMP(0)"),
        ("TIMESTAMP", "DATETIME(6)", "DATETIME2(6)", "TIMESTAMP(6)"),
        (
            "TIMESTAMP(3)",
            "DATETIME(3)",
            "DATETIME2(3)",
            "TIMESTAMP(3)",
        ),
        (
            "TIMESTAMP WITH TIME ZONE",
            "DATETIME(6)",
            "DATETIMEOFFSET(6)",
            "TIMESTAMPTZ(6)",
        ),
        (
            "TIMESTAMP WITH LOCAL TIME ZONE",
            "TIMESTAMP(6)",
            "DATETIME2(6)",
            "TIMESTAMPTZ(6)",
        ),
    ];

    for (input, mysql, tsql, postgres) in cases {
        assert_eq!(render(input, DialectType::MySQL), mysql, "{input}");
        assert_eq!(render(input, DialectType::TSQL), tsql, "{input}");
        assert_eq!(render(input, DialectType::PostgreSQL), postgres, "{input}");
    }
}

#[test]
fn issue_367_interval_lob_binary_and_rowid_mappings() {
    let cases = [
        (
            "INTERVAL YEAR(2) TO MONTH",
            "VARCHAR(30)",
            "VARCHAR(30)",
            "INTERVAL YEAR TO MONTH",
        ),
        (
            "INTERVAL DAY(2) TO SECOND(6)",
            "VARCHAR(30)",
            "VARCHAR(30)",
            "INTERVAL(6) DAY TO SECOND",
        ),
        ("CLOB", "LONGTEXT", "VARCHAR(MAX)", "TEXT"),
        ("NCLOB", "LONGTEXT", "NVARCHAR(MAX)", "TEXT"),
        ("BLOB", "LONGBLOB", "VARBINARY(MAX)", "BYTEA"),
        ("RAW(2000)", "VARBINARY(2000)", "VARBINARY(2000)", "BYTEA"),
        ("LONG", "LONGTEXT", "VARCHAR(MAX)", "TEXT"),
        ("LONG RAW", "LONGBLOB", "VARBINARY(MAX)", "BYTEA"),
        ("ROWID", "CHAR(18)", "CHAR(18)", "CHAR(18)"),
    ];

    for (input, mysql, tsql, postgres) in cases {
        assert_eq!(render(input, DialectType::MySQL), mysql, "{input}");
        assert_eq!(render(input, DialectType::TSQL), tsql, "{input}");
        assert_eq!(render(input, DialectType::PostgreSQL), postgres, "{input}");
    }
}

#[test]
fn issue_367_full_example_transpiles_for_all_reported_targets() {
    let mysql = transpile_issue(DialectType::MySQL);
    assert!(mysql.contains("C_CHAR_P CHAR(10)"));
    assert!(mysql.contains("C_INTERVAL_YM VARCHAR(30)"));
    assert!(mysql.contains("C_LONG_RAW LONGBLOB"));

    let tsql = transpile_issue(DialectType::TSQL);
    assert!(tsql.contains("C_CHAR_P NCHAR(10)"));
    assert!(tsql.contains("C_TIMESTAMP_TZ DATETIMEOFFSET(6)"));
    assert!(tsql.contains("C_NCLOB NVARCHAR(MAX)"));

    let postgres = transpile_issue(DialectType::PostgreSQL);
    assert!(postgres.contains("C_NUMBER_NEG_S NUMERIC(5, -2)"));
    assert!(postgres.contains("C_INTERVAL_DS INTERVAL(6) DAY TO SECOND"));
    assert!(postgres.contains("C_RAW BYTEA"));
}

#[test]
fn issue_367_strict_mode_rejects_lossy_mappings() {
    let source = Dialect::get(DialectType::Oracle);
    let cases = [
        (
            "CREATE TABLE t (value NUMBER)",
            DialectType::MySQL,
            "NUMBER without precision or scale",
        ),
        (
            "CREATE TABLE t (value NUMBER(5,-2))",
            DialectType::TSQL,
            "negative-scale rounding",
        ),
        (
            "CREATE TABLE t (value INTERVAL YEAR(2) TO MONTH)",
            DialectType::MySQL,
            "no native target column type",
        ),
        (
            "CREATE TABLE t (value ROWID)",
            DialectType::PostgreSQL,
            "physical row identity",
        ),
        (
            "CREATE TABLE t (value TIMESTAMP WITH TIME ZONE)",
            DialectType::MySQL,
            "loses its time zone",
        ),
    ];

    for (sql, target, expected) in cases {
        let error = source
            .transpile_with(sql, target, TranspileOptions::strict())
            .expect_err("strict mode should reject a lossy Oracle type mapping");
        assert!(
            error.to_string().contains(expected),
            "unexpected error for {sql}: {error}"
        );
    }

    let postgres = source
        .transpile_with(
            "CREATE TABLE t (value NUMBER(5,-2))",
            DialectType::PostgreSQL,
            TranspileOptions::strict(),
        )
        .expect("PostgreSQL 15+ can preserve an Oracle negative numeric scale");
    assert_eq!(
        postgres,
        vec!["CREATE TABLE t (value NUMERIC(5, -2))".to_string()]
    );
}

#[test]
fn issue_367_oracle_names_are_source_gated() {
    assert_eq!(
        render("LONG RAW", DialectType::Oracle),
        "LONG RAW",
        "Oracle parsing should consume the multi-token type"
    );

    let generic = parse_data_type("RAW(10)", DialectType::Generic)
        .expect("RAW should remain a generic custom type");
    assert_eq!(
        generate_data_type(&generic, DialectType::PostgreSQL).expect("generic RAW render"),
        "RAW(10)"
    );
}
