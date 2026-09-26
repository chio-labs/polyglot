//! Width layout over the generated dialect's token boundaries. Only whitespace is
//! changed: the generator remains the sole owner of SQL syntax and precedence.
//!
//! Original short lines are retained verbatim. On long lines, lower-precedence
//! operators and outer brackets are expanded before their contents. A range-min
//! tree avoids repeatedly scanning deep expressions (O(n log n), O(n) storage).

use crate::dialects::{Dialect, DialectType};
use crate::error::Result;
use crate::expressions::{AlterTableAction, Expression, TriggerBody};
use crate::generator::GeneratorConfig;
use crate::traversal::ExpressionWalk;

/// Raw fragments belong to languages the SQL AST does not model. Their exact
/// text (including whitespace) is part of the AST contract. Do not interpret
/// their punctuation as SQL layout. This also covers raw fragments nested in
/// otherwise parsed statements.
pub(crate) fn has_opaque_sql(expression: &Expression) -> bool {
    expression.dfs().any(|node| match node {
        Expression::Raw(raw) => !raw.sql.chars().all(|c| c.is_alphanumeric() || c == '_'),
        Expression::Command(command) => !command
            .this
            .chars()
            .all(|c| c.is_alphanumeric() || c == '_'),
        Expression::CreateTrigger(trigger) => matches!(trigger.body, TriggerBody::Block(_)),
        Expression::CreateView(view) => view.row_access_policy.is_some(),
        Expression::CreateTable(table) => table.columns.iter().any(|column| column.codec.is_some()),
        Expression::AlterTable(table) => table
            .actions
            .iter()
            .any(|action| matches!(action, AlterTableAction::Raw { .. })),
        _ => false,
    })
}

#[derive(Clone, Copy, Debug)]
struct Break {
    start: usize,
    end: usize,
    rank: usize,
    indent: usize,
}

const FALLBACK: usize = usize::MAX - 1;

struct MinTree {
    size: usize,
    ranks: Vec<usize>,
}

impl MinTree {
    fn new(breaks: &[Break]) -> Self {
        let size = breaks.len().next_power_of_two();
        let mut ranks = vec![usize::MAX; 2 * size];
        for (i, point) in breaks.iter().enumerate() {
            ranks[size + i] = point.rank;
        }
        for i in (1..size).rev() {
            ranks[i] = ranks[2 * i].min(ranks[2 * i + 1]);
        }
        Self { size, ranks }
    }

    fn min(&self, mut left: usize, mut right: usize) -> usize {
        left += self.size;
        right += self.size;
        let mut result = usize::MAX;
        while left < right {
            if left % 2 == 1 {
                result = result.min(self.ranks[left]);
                left += 1;
            }
            if right % 2 == 1 {
                right -= 1;
                result = result.min(self.ranks[right]);
            }
            left /= 2;
            right /= 2;
        }
        result
    }

    fn first(&self, left: usize, right: usize, rank: usize) -> usize {
        self.find(1, 0, self.size, left, right, rank).unwrap()
    }

    fn find(
        &self,
        node: usize,
        start: usize,
        end: usize,
        left: usize,
        right: usize,
        rank: usize,
    ) -> Option<usize> {
        if end <= left || start >= right || self.ranks[node] > rank {
            return None;
        }
        if end - start == 1 {
            return (self.ranks[node] == rank).then_some(start);
        }
        let mid = (start + end) / 2;
        self.find(node * 2, start, mid, left, right, rank)
            .or_else(|| self.find(node * 2 + 1, mid, end, left, right, rank))
    }
}

pub(crate) fn wrap(sql: String, config: &GeneratorConfig) -> Result<String> {
    let width = config.max_text_width.max(1);
    if sql.lines().all(|line| line.chars().count() <= width) {
        return Ok(sql);
    }
    let tokens = Dialect::get(config.dialect.unwrap_or(DialectType::Generic)).tokenize(&sql)?;
    let chars: Vec<char> = sql.chars().collect();
    // The tokenizer reports Unicode scalar offsets, as does the width budget.
    let mut bytes: Vec<usize> = sql.char_indices().map(|(i, _)| i).collect();
    bytes.push(sql.len());
    let mut lines = vec![0];
    let mut line_at = Vec::with_capacity(chars.len() + 1);
    for (i, c) in chars.iter().enumerate() {
        line_at.push(lines.len() - 1);
        if *c == '\n' {
            lines.push(i + 1);
        }
    }
    line_at.push(lines.len() - 1);
    let indents: Vec<usize> = lines
        .iter()
        .map(|&start| {
            chars[start..]
                .iter()
                .take_while(|c| matches!(c, ' ' | '\t'))
                .count()
        })
        .collect();
    let mut points = vec![Vec::new(); lines.len()];
    let indent_width = config.indent.chars().count();
    let mut stack: Vec<(usize, bool)> = Vec::new();
    let mut line_depth = vec![None; lines.len()];
    for (i, token) in tokens.iter().enumerate() {
        let start = token.span.start;
        let end = token.span.end;
        if start >= chars.len() || end > chars.len() {
            continue;
        }
        let line = line_at[start];
        let initial_depth = *line_depth[line].get_or_insert(stack.len());
        let depth = stack.len().saturating_sub(initial_depth);
        let text = &sql[bytes[start]..bytes[end]];
        let clause = matches!(
            text.to_ascii_uppercase().as_str(),
            "PARTITION" | "ORDER" | "GROUP"
        ) && tokens
            .get(i + 1)
            .is_some_and(|next| next.text.eq_ignore_ascii_case("BY"));
        if clause {
            if let Some((_, window)) = stack.last_mut() {
                *window = true;
            }
        }
        let opening = matches!(text, "(" | "[" | "{");
        let closing = matches!(text, ")" | "]" | "}");
        let base = indents[line];
        let indent = |level: usize| (base + level * indent_width).min(width / 2);
        if i == 0
            && start > lines[line]
            && chars[lines[line]..start].iter().any(|c| !c.is_whitespace())
        {
            let mut gap_start = start;
            while gap_start > lines[line] && matches!(chars[gap_start - 1], ' ' | '\t') {
                gap_start -= 1;
            }
            if gap_start < start {
                points[line].push(Break {
                    start: gap_start,
                    end: start,
                    rank: FALLBACK,
                    indent: base,
                });
            }
        }
        if i > 0 {
            let previous = &tokens[i - 1];
            let mut gap_start = previous.span.end;
            // Comments live in gaps, not tokens. Preserve their text and all
            // existing newlines (including those inside multiline literals).
            if gap_start <= start && line_at[gap_start] == line {
                // Keep inline comments attached to the preceding token, but
                // permit a break after the complete comment, before this token.
                if chars[gap_start..start]
                    .iter()
                    .any(|c| *c != ' ' && *c != '\t')
                {
                    gap_start = start;
                    while gap_start > previous.span.end
                        && matches!(chars[gap_start - 1], ' ' | '\t')
                    {
                        gap_start -= 1;
                    }
                }
                let prev = &sql[bytes[previous.span.start]..bytes[previous.span.end]];
                let mut rank = FALLBACK;
                let mut level = 1;
                if matches!(prev, "(" | "[" | "{") {
                    rank = depth.saturating_sub(1) * 16 + 8;
                    level = depth;
                } else if closing {
                    rank = depth.saturating_sub(1) * 16 + 8;
                    level = depth.saturating_sub(1);
                } else if prev == "," {
                    let (opened_line, window) =
                        stack.last().copied().unwrap_or((usize::MAX, false));
                    rank = if opened_line == line && !window {
                        depth.saturating_sub(1) * 16 + 8
                    } else {
                        depth * 16
                    };
                    level = if window { depth + 1 } else { depth.max(1) };
                } else if prev.eq_ignore_ascii_case("BY")
                    && i >= 2
                    && matches!(
                        tokens[i - 2].text.to_ascii_uppercase().as_str(),
                        "PARTITION" | "ORDER" | "GROUP"
                    )
                {
                    rank = depth * 16;
                    level = depth + 1;
                } else {
                    let operator = match text.to_ascii_uppercase().as_str() {
                        "PARTITION" | "ORDER" | "GROUP" if clause => Some(1),
                        "QUALIFY" | "WINDOW" => Some(1),
                        "OR" => Some(2),
                        "AND" => Some(3),
                        "||" => Some(4),
                        "+" | "-" => Some(5),
                        "*" | "/" | "%" => Some(6),
                        _ => None,
                    };
                    // Unspaced punctuation can belong to a dialect's name or
                    // path syntax (for example BigQuery project-name.orders).
                    // Expression operators emitted by the generator are spaced.
                    if let Some(priority) = operator.filter(|_| gap_start < start) {
                        rank = depth * 16 + priority;
                        level = depth.max(1);
                        if stack
                            .last()
                            .is_some_and(|(opened, window)| *opened == line && *window)
                            && priority == 1
                        {
                            rank = depth.saturating_sub(1) * 16 + 8;
                        }
                    }
                }
                // Fallback breaks require existing whitespace. In particular we
                // must not split dotted names, numeric literals or operators.
                let empty_brackets = matches!(prev, "(" | "[" | "{") && closing;
                let clause_tail = text.eq_ignore_ascii_case("BY")
                    && matches!(
                        prev.to_ascii_uppercase().as_str(),
                        "PARTITION" | "ORDER" | "GROUP"
                    );
                if !empty_brackets
                    && !clause_tail
                    && (rank != FALLBACK || gap_start < start || prev == "=")
                {
                    points[line].push(Break {
                        start: gap_start,
                        end: start,
                        rank,
                        indent: indent(level),
                    });
                }
            }
        }
        if opening {
            let window = i > 0 && tokens[i - 1].text.eq_ignore_ascii_case("OVER");
            stack.push((line, window));
        } else if closing {
            stack.pop();
        }
    }
    let mut output = String::with_capacity(sql.len());
    for (line, &start) in lines.iter().enumerate() {
        let end = lines.get(line + 1).map_or(chars.len(), |end| end - 1);
        if line > 0 {
            output.push('\n');
        }
        if end - start <= width || points[line].is_empty() {
            output.push_str(&sql[bytes[start]..bytes[end]]);
            continue;
        }
        let breaks = &points[line];
        let original_start = start;
        let original_indent = indents[line];
        let tree = MinTree::new(breaks);
        // Tasks are disjoint intervals; use an explicit stack for deep SQL.
        let mut tasks = vec![(start, end, 0, breaks.len(), 0)];
        let mut first = true;
        while let Some((start, end, left, right, indent)) = tasks.pop() {
            if indent + end - start <= width || left == right {
                if !first {
                    output.push('\n');
                }
                first = false;
                let prefix = indent.min(original_indent);
                output.extend(&chars[original_start..original_start + prefix]);
                output.extend(config.indent.chars().cycle().take(indent - prefix));
                output.push_str(&sql[bytes[start]..bytes[end]]);
                continue;
            }
            let rank = tree.min(left, right);
            let mut cuts = Vec::new();
            if rank == FALLBACK {
                // Last-resort whitespace packing, for aliases, CAST ... AS,
                // frame bounds and other syntax without an expression operator.
                let budget = start + width.saturating_sub(indent);
                let fit = breaks[left..right].partition_point(|point| point.start <= budget);
                cuts.push(left + fit.saturating_sub(1));
            } else {
                let mut cursor = left;
                while cursor < right && tree.min(cursor, right) == rank {
                    let cut = tree.first(cursor, right, rank);
                    cuts.push(cut);
                    cursor = cut + 1;
                }
            }
            let mut pieces = Vec::with_capacity(cuts.len() + 1);
            let (mut from, mut lo, mut padding) = (start, left, indent);
            for cut in cuts {
                let point = breaks[cut];
                if point.start > from && point.end < end {
                    pieces.push((from, point.start, lo, cut, padding));
                    from = point.end;
                    lo = cut + 1;
                    padding = point.indent;
                }
            }
            pieces.push((from, end, lo, right, padding));
            tasks.extend(pieces.into_iter().rev());
        }
    }
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::generator::Generator;
    use serde_json::Value;
    use std::path::Path;
    use std::time::{Duration, Instant};

    #[test]
    fn protects_comments_and_multiline_tokens() {
        let config = GeneratorConfig {
            pretty: true,
            max_text_width: 30,
            ..Default::default()
        };
        for sql in [
            "SELECT COALESCE(customer_id, /* shipment, (order) AND inventory */ shipment_id, order_id)",
            "SELECT COALESCE('customer\nshipment (order), AND inventory', customer_id, shipment_id)",
            "SELECT customer_id -- shipment, (order) AND inventory\nFROM orders",
            "SELECT COALESCE(\"customer (shipment), AND inventory\", customer_id, shipment_id)",
        ] {
            let output = wrap(sql.into(), &config).unwrap();
            let dialect = Dialect::get(DialectType::Generic);
            let original = dialect.tokenize(sql).unwrap();
            let formatted = dialect.tokenize(&output).unwrap();
            assert_eq!(original.len(), formatted.len());
            for (before, after) in original.iter().zip(&formatted) {
                assert_eq!(before.token_type, after.token_type, "{output}");
                assert_eq!(before.text, after.text, "{output}");
            }
            assert_eq!(wrap(output.clone(), &config).unwrap(), output);
        }
    }

    fn fixture_queries(
        value: &Value,
        dialect: DialectType,
        queries: &mut Vec<(DialectType, String)>,
    ) {
        match value {
            Value::Array(values) => {
                for value in values {
                    fixture_queries(value, dialect, queries);
                }
            }
            Value::Object(values) => {
                let dialect = values
                    .get("dialect")
                    .and_then(Value::as_str)
                    .and_then(|name| name.parse().ok())
                    .unwrap_or(dialect);
                let read = values
                    .get("read")
                    .and_then(Value::as_str)
                    .and_then(|name| name.parse().ok())
                    .unwrap_or(dialect);
                let write = values
                    .get("write")
                    .and_then(Value::as_str)
                    .and_then(|name| name.parse().ok())
                    .unwrap_or(dialect);
                for (key, value) in values {
                    if let Some(sql) = value.as_str() {
                        match key.as_str() {
                            "sql" | "input" => queries.push((read, sql.into())),
                            "expected" => queries.push((write, sql.into())),
                            _ => {}
                        }
                    } else if matches!(key.as_str(), "read" | "write") && value.is_object() {
                        for (name, sql) in value.as_object().unwrap() {
                            if let (Ok(dialect), Some(sql)) = (name.parse(), sql.as_str()) {
                                queries.push((dialect, sql.into()));
                            }
                        }
                    } else {
                        fixture_queries(value, dialect, queries);
                    }
                }
            }
            _ => {}
        }
    }

    fn read_fixtures(path: &Path, queries: &mut Vec<(DialectType, String)>) {
        for entry in std::fs::read_dir(path).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                read_fixtures(&path, queries);
            } else if path.extension().is_some_and(|ext| ext == "json") {
                let data: Value =
                    serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
                fixture_queries(&data, DialectType::Generic, queries);
            }
        }
    }

    fn assert_width(sql: &str, dialect: &Dialect, width: usize) {
        let tokens = dialect.tokenize(sql).unwrap();
        let chars: Vec<char> = sql.chars().collect();
        let mut atoms = Vec::new();
        let mut previous = 0;
        let mut previous_start = 0;
        for token in tokens {
            if chars[previous..token.span.start]
                .iter()
                .any(|c| !c.is_whitespace())
            {
                // Comments are deliberately indivisible, including whitespace.
                let attached = chars[previous..token.span.start]
                    .iter()
                    .take_while(|c| **c != '\n')
                    .any(|c| !c.is_whitespace());
                atoms.push((
                    if attached { previous_start } else { previous },
                    token.span.start,
                ));
            }
            atoms.push((token.span.start, token.span.end));
            previous = token.span.end;
            previous_start = token.span.start;
        }
        if chars[previous..].iter().any(|c| !c.is_whitespace()) {
            atoms.push((previous_start, chars.len()));
        }
        let mut start = 0;
        for line in sql.split('\n') {
            let length = line.chars().count();
            if length > width {
                let padding = line.chars().take_while(|c| c.is_whitespace()).count();
                let end = start + length;
                let indivisible = atoms
                    .iter()
                    .any(|&(a, b)| b.min(end).saturating_sub(a.max(start)) + padding + 1 >= width);
                // Dotted names and other unspaced dialect syntax also cannot be
                // split safely by a whitespace-only layout pass.
                let unspaced = line
                    .split_whitespace()
                    .any(|part| part.chars().count() + padding + 1 >= width);
                assert!(
                    indivisible || unspaced,
                    "splittable line exceeds {width}:\n{sql}"
                );
            }
            start += length + 1;
        }
    }

    /// Differential properties isolate layout from existing parser/generator
    /// normalization. Every extracted query is attempted, including negative
    /// parser cases; baseline failures are counted explicitly, never attributed
    /// to width handling. The focused integration tests also require equality
    /// with the original input AST, not just the generated baseline AST.
    #[test]
    #[ignore = "requires make extract-fixtures; run make test-rust-width"]
    fn sqlglot_corpus_width_properties() {
        std::thread::Builder::new().stack_size(16 * 1024 * 1024).spawn(|| {
            let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/sqlglot_fixtures");
            assert!(root.exists(), "run make extract-fixtures before corpus properties");
            let mut queries = Vec::new();
            read_fixtures(&root, &mut queries);
            queries.sort_by(|a, b| (a.0.to_string(), &a.1).cmp(&(b.0.to_string(), &b.1)));
            queries.dedup();
            let mut checked = 0;
            let mut unsupported_input = 0;
            let mut baseline_failures = 0;
            let mut baseline_ast_changes = 0;
            let mut baseline_unstable = 0;
            let mut opaque_commands = 0;
            let mut failures = 0;
            for (kind, sql) in &queries {
                let dialect = Dialect::get(*kind);
                let Ok(expressions) = dialect.parse(sql) else {
                    unsupported_input += 1;
                    continue;
                };
                let mut config = dialect.generator_config().clone();
                config.pretty = true;
                for expression in expressions {
                    let opaque = has_opaque_sql(&expression);
                    if opaque {
                        opaque_commands += 1;
                    }
                    for width in [40, 80, 100, 120] {
                        config.max_text_width = width;
                        let mut generator = Generator::with_config(config.clone())
                            .with_preserved_null_ordering().with_preserved_variant_paths();
                        let Ok(baseline) = generator.generate_unwrapped(&expression) else {
                            baseline_failures += 1;
                            continue;
                        };
                        let Ok(expected) = dialect.parse(&baseline) else {
                            baseline_failures += 1;
                            continue;
                        };
                        if expected.as_slice() != std::slice::from_ref(&expression) {
                            baseline_ast_changes += 1;
                        }
                        let stable = expected.len() == 1 && generator.generate_unwrapped(&expected[0]).is_ok_and(|again| again == baseline);
                        if !stable {
                            baseline_unstable += 1;
                        }
                        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        let output = generator.generate(&expression).unwrap_or_else(|error| {
                            panic!("{kind} width {width}: {error}\n{baseline}")
                        });
                        let actual = dialect.parse(&output).unwrap_or_else(|error| {
                            panic!("{kind} width {width}: {error}\n{baseline}\n{output}")
                        });
                        assert!(actual == expected, "AST changed: {kind} width {width}\n{baseline}\n{output}");
                        if opaque {
                            assert_eq!(output, baseline, "opaque payload changed");
                        } else {
                            assert_eq!(wrap(output.clone(), &config).unwrap(), output,
                                "layout not idempotent: {kind} width {width}\n{baseline}");
                            assert_width(&output, &dialect, width);
                        }
                        if stable {
                            assert_eq!(generator.generate(&actual[0]).unwrap(), output,
                                "generator not idempotent: {kind} width {width}\n{baseline}");
                        }
                        checked += 1;
                        }));
                        if result.is_err() {
                            failures += 1;
                        }
                    }
                }
            }
            eprintln!("Width corpus: {} queries, {checked} width checks, {unsupported_input} unsupported inputs, {baseline_failures} baseline generation/parse failures, {baseline_ast_changes} baseline AST normalizations, {baseline_unstable} baseline non-idempotent outputs, {opaque_commands} opaque passthrough commands", queries.len());
            assert!(checked > 10_000, "fixture corpus was incomplete");
            assert_eq!(failures, 0, "width property failures");
        }).unwrap().join().unwrap();
    }

    #[test]
    fn wide_and_deep_layout_scales() {
        fn measure(size: usize) -> Duration {
            let sql = format!(
                "SELECT {}{}{} FROM orders",
                "COALESCE(".repeat(size),
                vec!["customer_reference"; size].join(", "),
                ")".repeat(size)
            );
            let config = GeneratorConfig {
                pretty: true,
                max_text_width: 80,
                ..Default::default()
            };
            let mut best = Duration::MAX;
            for _ in 0..3 {
                let start = Instant::now();
                let output = wrap(sql.clone(), &config).unwrap();
                best = best.min(start.elapsed());
                assert!(output.lines().all(|line| line.chars().count() <= 80));
            }
            best
        }
        let small = measure(500);
        let large = measure(2_000);
        eprintln!("width layout 500={small:?}, 2000={large:?}");
        // Generous shared-CI allowance; quadratic growth would be 16x.
        assert!(large < small * 10 + Duration::from_millis(20));
    }
}
