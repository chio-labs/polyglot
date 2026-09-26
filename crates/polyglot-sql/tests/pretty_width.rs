//! Width formatting through the real dialect parser and generator.
use polyglot_sql::{Dialect, DialectType, Generator};

const DIALECTS: [DialectType; 5] = [
    DialectType::Snowflake,
    DialectType::DuckDB,
    DialectType::PostgreSQL,
    DialectType::BigQuery,
    DialectType::Generic,
];

fn pretty(sql: &str, dialect: DialectType, width: usize) -> String {
    let dialect = Dialect::get(dialect);
    let ast = dialect.parse(sql).unwrap();
    let mut config = dialect.generator_config().clone();
    config.pretty = true;
    config.max_text_width = width;
    let mut generator = Generator::with_config(config);
    assert_eq!(ast.len(), 1);
    let output = generator.generate(&ast[0]).unwrap();
    let reparsed = dialect.parse(&output).unwrap_or_else(|error| {
        panic!("{error}\n{output}");
    });
    assert_eq!(ast, reparsed, "AST changed:\n{sql}\n{output}");
    assert_eq!(
        output,
        generator.generate(&reparsed[0]).unwrap(),
        "not idempotent: {sql}"
    );
    output
}

fn fits(output: &str, width: usize) {
    for line in output.lines() {
        assert!(
            line.chars().count() <= width,
            "over {width}: {line}\n{output}"
        );
    }
}

#[test]
fn functions_and_nested_casts() {
    for dialect in DIALECTS {
        for width in [40, 80, 100, 120] {
            let target = match dialect {
                DialectType::PostgreSQL | DialectType::DuckDB => "TEXT",
                DialectType::BigQuery => "STRING",
                _ => "VARCHAR",
            };
            let sql = format!("SELECT COALESCE(CAST(orders.customer_reference AS {target}), CAST(orders.shipment_reference AS {target}), orders.order_reference) FROM orders");
            let output = pretty(&sql, dialect, width);
            fits(&output, width);
            let unwrapped = pretty(&sql, dialect, usize::MAX);
            if unwrapped.lines().any(|line| line.chars().count() > width) {
                assert!(output.contains("COALESCE(\n"), "{output}");
                assert!(output.contains("\n  )"), "{output}");
            } else {
                assert_eq!(output, unwrapped);
            }
        }
    }
}

#[test]
fn boolean_arithmetic_case_and_join_conditions() {
    let queries = [
        "SELECT * FROM orders WHERE customer_id = 1 AND shipment_id = 2 AND order_id = 3 OR customer_id = 4 AND shipment_id = 5",
        "SELECT (ordered_quantity + shipped_quantity * requested_quantity - returned_quantity) / total_quantity AS quantity FROM orders",
        "SELECT customer_reference || shipment_reference || order_reference || customer_reference AS reference FROM orders",
        "SELECT CASE WHEN customer_id = 1 AND shipment_id = 2 AND order_id = 3 THEN customer_reference ELSE shipment_reference END FROM orders",
        "SELECT * FROM orders JOIN shipments ON orders.order_id = shipments.order_id AND orders.customer_id = shipments.customer_id",
    ];
    for dialect in DIALECTS {
        for sql in queries {
            fits(&pretty(sql, dialect, 40), 40);
        }
    }
}

#[test]
fn windows_and_lists() {
    let queries = [
        "SELECT ROW_NUMBER() OVER (PARTITION BY customer_id, shipment_id ORDER BY order_id, customer_reference, shipment_reference) AS position FROM orders",
        "SELECT customer_reference, shipment_reference, order_reference FROM orders GROUP BY customer_reference, shipment_reference, order_reference ORDER BY customer_reference, shipment_reference, order_reference",
        "SELECT * FROM orders WHERE order_reference IN ('customer', 'shipment', 'order', 'inventory', 'fulfillment')",
        "VALUES ('customer', 'shipment', 'order', 'inventory', 'fulfillment'), ('order', 'shipment', 'customer', 'inventory', 'fulfillment')",
    ];
    for dialect in DIALECTS {
        for sql in queries {
            fits(&pretty(sql, dialect, 40), 40);
        }
    }
    for sql in [
        "SELECT * FROM orders QUALIFY ROW_NUMBER() OVER (PARTITION BY customer_id, shipment_id ORDER BY order_id, customer_reference) = 1",
        "SELECT SUM(order_id) OVER orders_window FROM orders WINDOW orders_window AS (PARTITION BY customer_id, shipment_id ORDER BY order_id, customer_reference)",
    ] {
        fits(&pretty(sql, DialectType::DuckDB, 40), 40);
    }
    let window = pretty("SELECT ROW_NUMBER() OVER (PARTITION BY customer_reference, shipment_reference ORDER BY order_reference, customer_reference) FROM orders", DialectType::DuckDB, 40);
    assert!(window.contains("PARTITION BY\n"), "{window}");
    assert!(window.contains("ORDER BY\n"), "{window}");
    assert!(!window.contains("ROW_NUMBER(\n"), "{window}");
    fits(&window, 40);
}

#[test]
fn array_and_object_literals() {
    for (dialect, sql) in [
        (DialectType::Snowflake, "SELECT {'customer': customer_reference, 'shipment': shipment_reference, 'order': order_reference} FROM orders"),
        (DialectType::Snowflake, "SELECT ARRAY_CONSTRUCT(customer_reference, shipment_reference, order_reference) FROM orders"),
        (DialectType::DuckDB, "SELECT [customer_reference, shipment_reference, order_reference] FROM orders"),
        (DialectType::DuckDB, "SELECT {'customer': customer_reference, 'shipment': shipment_reference, 'order': order_reference} FROM orders"),
        (DialectType::BigQuery, "SELECT [customer_reference, shipment_reference, order_reference] FROM orders"),
    ] {
        fits(&pretty(sql, dialect, 40), 40);
    }
}

#[test]
fn unicode_width_and_protected_tokens() {
    for dialect in DIALECTS {
        let short = pretty("SELECT COALESCE('客户订单', '客户发货')", dialect, 40);
        assert!(!short.contains("COALESCE(\n"), "{short}");
        let sql = "SELECT COALESCE('customer, shipment (order) AND inventory || fulfillment', customer_reference, shipment_reference) FROM orders";
        let output = pretty(sql, dialect, 40);
        assert!(output.contains("'customer, shipment (order) AND inventory || fulfillment'"));
        assert_eq!(
            output
                .lines()
                .filter(|line| line.chars().count() > 40)
                .count(),
            1
        );
    }
    for (dialect, sql) in [
        (DialectType::PostgreSQL, "SELECT COALESCE($orders$customer, shipment (order) AND inventory || fulfillment$orders$, customer_reference, shipment_reference) FROM orders"),
        (DialectType::Snowflake, "SELECT COALESCE(\"customer, shipment (order) AND inventory || fulfillment\", customer_reference, shipment_reference) FROM orders"),
        (DialectType::BigQuery, "SELECT COALESCE(r'customer, shipment (order) AND inventory || fulfillment', customer_reference, shipment_reference) FROM orders"),
    ] {
        // These dialect generators already normalize literal kinds (e.g.
        // PostgreSQL dollar strings to ordinary strings) independently of layout.
        let language = Dialect::get(dialect);
        let canonical = language.generate(&language.parse(sql).unwrap()[0]).unwrap();
        pretty(&canonical, dialect, 40);
    }
}

#[test]
fn compact_output_ignores_width_and_short_pretty_is_unchanged() {
    let dialect = Dialect::get(DialectType::Generic);
    let ast = dialect
        .parse("SELECT COALESCE(customer_id, shipment_id) FROM orders")
        .unwrap();
    let mut config = dialect.generator_config().clone();
    config.max_text_width = 1;
    assert_eq!(
        Generator::with_config(config).generate(&ast[0]).unwrap(),
        dialect.generate(&ast[0]).unwrap()
    );
    assert_eq!(
        pretty(
            "SELECT COALESCE(customer_id, shipment_id) FROM orders",
            DialectType::Generic,
            80
        ),
        "SELECT\n  COALESCE(customer_id, shipment_id)\nFROM orders"
    );
}

#[test]
fn dialect_generate_pretty_uses_default_width() {
    for kind in DIALECTS {
        let dialect = Dialect::get(kind);
        let ast = dialect.parse("SELECT COALESCE(orders.customer_reference, orders.shipment_reference, orders.order_reference, orders.customer_reference) FROM orders").unwrap();
        let output = dialect.generate_pretty(&ast[0]).unwrap();
        fits(&output, 80);
        assert!(output.contains("COALESCE(\n"));
        let reparsed = dialect.parse(&output).unwrap();
        assert_eq!(ast, reparsed);
        assert_eq!(dialect.generate_pretty(&reparsed[0]).unwrap(), output);
    }
}

#[test]
fn single_keyword_parser_fallbacks_do_not_disable_layout() {
    let dialect = Dialect::get(DialectType::MySQL);
    let ast = dialect.parse("INSERT INTO orders SET customer_id = DEFAULT, shipment_id = 2 AS new ON DUPLICATE KEY UPDATE customer_id = new.customer_id + 1").unwrap();
    let mut config = dialect.generator_config().clone();
    config.pretty = true;
    config.max_text_width = 40;
    let mut generator = Generator::with_config(config);
    let output = generator.generate(&ast[0]).unwrap();
    fits(&output, 40);
    let reparsed = dialect.parse(&output).unwrap();
    // INSERT ... SET is an existing parser/generator normalization to VALUES.
    let canonical = dialect.parse(&dialect.generate(&ast[0]).unwrap()).unwrap();
    assert_eq!(canonical, reparsed);
    assert_eq!(output, generator.generate(&reparsed[0]).unwrap());
}

#[test]
fn wide_deep_generation_scales() {
    use std::time::{Duration, Instant};
    fn measure(count: usize) -> Duration {
        let dialect = Dialect::get(DialectType::Generic);
        let sql = format!(
            "SELECT {}{}{} FROM orders",
            "COALESCE(".repeat(32),
            vec!["customer_reference"; count].join(", "),
            ")".repeat(32)
        );
        let ast = dialect.parse(&sql).unwrap();
        let mut best = Duration::MAX;
        for _ in 0..3 {
            let start = Instant::now();
            let output = dialect.generate_pretty(&ast[0]).unwrap();
            best = best.min(start.elapsed());
            fits(&output, 80);
        }
        best
    }
    let small = measure(500);
    let large = measure(2_000);
    eprintln!("pretty generation 500={small:?}, 2000={large:?}");
    assert!(large < small * 10 + Duration::from_millis(20));
}
