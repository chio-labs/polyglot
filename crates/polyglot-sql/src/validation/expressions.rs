use super::*;

// Validate the ISO calendar component without narrowing the engine's accepted
// alternative date formats or special values. Non-ISO forms stay unchecked.
fn invalid_iso_date(value: &str) -> bool {
    let bytes = value.as_bytes();
    if bytes.len() < 10 || bytes[4] != b'-' || bytes[7] != b'-' {
        return false;
    }
    let (Ok(year), Ok(month), Ok(day)) = (
        value.get(..4).unwrap_or("").parse::<u32>(),
        value.get(5..7).unwrap_or("").parse::<u32>(),
        value.get(8..10).unwrap_or("").parse::<u32>(),
    ) else {
        return false;
    };
    let days = match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if year % 4 == 0 && (year % 100 != 0 || year % 400 == 0) => 29,
        2 => 28,
        _ => 0,
    };
    day == 0 || day > days
}

fn invalid_temporal_literal(value: &str) -> bool {
    let bytes = value.as_bytes();
    let invalid_time = if bytes.len() >= 19 && bytes[13] == b':' && bytes[16] == b':' {
        match (
            value.get(11..13).unwrap_or("").parse::<u32>(),
            value.get(14..16).unwrap_or("").parse::<u32>(),
            value.get(17..19).unwrap_or("").parse::<u32>(),
        ) {
            (Ok(hour), Ok(minute), Ok(second)) => {
                hour > 24
                    || minute > 59
                    || second > 59
                    || (hour == 24 && (minute != 0 || second != 0))
            }
            _ => false,
        }
    } else {
        false
    };
    invalid_iso_date(value)
        || invalid_time
        || (!value.chars().any(|c| c.is_ascii_digit())
            && !matches!(
                value.to_ascii_lowercase().as_str(),
                "infinity" | "-infinity" | "epoch" | "today" | "tomorrow" | "yesterday" | "now"
            ))
}

pub(super) fn invalid_literal_for(expr: &Expression, target: TypeFamily) -> bool {
    match expr {
        Expression::Alias(alias) => invalid_literal_for(&alias.this, target),
        Expression::Paren(paren) => invalid_literal_for(&paren.this, target),
        Expression::Literal(literal) => match literal.as_ref() {
            crate::expressions::Literal::String(value) => match target {
                TypeFamily::Integer | TypeFamily::Numeric => value.trim().parse::<f64>().is_err(),
                TypeFamily::Date | TypeFamily::Timestamp => invalid_temporal_literal(value),
                TypeFamily::Boolean => !matches!(
                    value.to_ascii_lowercase().as_str(),
                    "true" | "false" | "t" | "f" | "yes" | "no" | "y" | "n" | "1" | "0"
                ),
                _ => false,
            },
            _ => false,
        },
        _ => false,
    }
}

fn scalar_query(expr: &Expression) -> Option<&Expression> {
    match expr {
        Expression::Subquery(query) => Some(&query.this),
        Expression::Alias(alias) => scalar_query(&alias.this),
        Expression::Paren(paren) => scalar_query(&paren.this),
        _ => None,
    }
}

fn relation_column(
    expr: &Expression,
    name: &str,
    schema: &HashMap<String, TableSchemaEntry>,
    dialect: DialectType,
) -> TypeFamily {
    match expr {
        Expression::Table(table) => table_ref_candidates(table)
            .iter()
            .find_map(|key| schema.get(key))
            .and_then(|table| table.columns.get(&lower(name)))
            .copied()
            .unwrap_or(TypeFamily::Unknown),
        Expression::Cte(cte) => relation_column(&cte.this, name, schema, dialect),
        _ => {
            let Ok(names) = crate::set_operation::query_output_identifiers(expr, Some(dialect))
            else {
                return TypeFamily::Unknown;
            };
            let Some(index) = names
                .iter()
                .position(|column| column.name.eq_ignore_ascii_case(name))
            else {
                return TypeFamily::Unknown;
            };
            projection_families(expr, schema, dialect)
                .and_then(|types| types.get(index).copied())
                .unwrap_or(TypeFamily::Unknown)
        }
    }
}

fn check_using(
    node: &Expression,
    select: &crate::expressions::Select,
    dialect: DialectType,
    schema: &HashMap<String, TableSchemaEntry>,
    strict: bool,
    errors: &mut Vec<ValidationError>,
) {
    // The two-source case is unambiguous; larger join trees require a merged
    // output schema and remain unchecked here rather than guessing ownership.
    if select
        .from
        .as_ref()
        .is_none_or(|from| from.expressions.len() != 1)
        || select.joins.len() != 1
        || select.joins[0].using.is_empty()
    {
        return;
    }
    let scope = selected_validation_scope(&build_scope(node));
    if scope.sources.len() != 2 {
        return;
    }
    let sources: Vec<_> = scope
        .sources
        .values()
        .map(|source| &source.expression)
        .collect();
    for column in &select.joins[0].using {
        let left = relation_column(sources[0], &column.name, schema, dialect);
        let right = relation_column(sources[1], &column.name, schema, dialect);
        if !coercion::comparable(dialect, sources[0], sources[1], left, right) {
            errors.push(type_issue(
                strict,
                validation_codes::E_INCOMPATIBLE_COMPARISON_TYPES,
                validation_codes::W_IMPLICIT_CAST_COMPARISON,
                format!("JOIN USING column '{}' has incompatible types", column.name),
            ));
        }
    }
}

pub(super) fn check(
    node: &Expression,
    dialect: DialectType,
    schema: &HashMap<String, TableSchemaEntry>,
    context: &TypeCheckContext,
    strict: bool,
    known_types: &[String],
    errors: &mut Vec<ValidationError>,
) {
    let scalar_children: Vec<_> = match node {
        Expression::Select(select) => select.expressions.iter().collect(),
        Expression::Exists(_)
        | Expression::In(_)
        | Expression::Subquery(_)
        | Expression::Union(_)
        | Expression::Intersect(_)
        | Expression::Except(_)
        | Expression::Cte(_)
        | Expression::From(_)
        | Expression::Lateral(_)
        | Expression::JoinedTable(_)
        | Expression::Alias(_)
        | Expression::Paren(_)
        | Expression::Join(_) => Vec::new(),
        _ => node.children(),
    };
    for child in scalar_children {
        if let Some(query) = scalar_query(child) {
            if projection_families(query, schema, dialect).is_some_and(|types| types.len() > 1) {
                errors.push(type_issue(
                    strict,
                    validation_codes::E_SETOP_ARITY_MISMATCH,
                    validation_codes::W_SETOP_IMPLICIT_COERCION,
                    "Scalar subquery must return one column".to_owned(),
                ));
            }
        }
    }
    let family = |expr: &Expression| infer_expression_type_family(expr, schema, context);
    let mut predicate = |expr: &Expression| {
        if !predicate_expression_compatible(expr, family(expr), dialect) {
            errors.push(type_issue(
                strict,
                validation_codes::E_INVALID_PREDICATE_TYPE,
                validation_codes::W_PREDICATE_NULLABILITY,
                format!(
                    "Expected boolean condition, found {}",
                    type_family_name(family(expr))
                ),
            ));
        }
    };
    match node {
        Expression::Select(select) => {
            check_using(node, select, dialect, schema, strict, errors);
            if dialect != DialectType::DuckDB {
                return;
            }
            if let Some(with) = select.with.as_ref().filter(|with| with.recursive) {
                for cte in &with.ctes {
                    if let Expression::Union(union) = &cte.this {
                        if let (Some(anchor), Some(recursive)) = (
                            projection_families(&union.left, schema, dialect),
                            projection_families(&union.right, schema, dialect),
                        ) {
                            for (index, (target, source)) in
                                anchor.into_iter().zip(recursive).enumerate()
                            {
                                let literal = coercion::projection_literal(&union.right, index);
                                let invalid = literal
                                    .is_some_and(|expr| invalid_literal_for(expr, target))
                                    || (source == TypeFamily::String
                                        && target != TypeFamily::String
                                        && target != TypeFamily::Unknown
                                        && literal.is_none())
                                    || !coercion::setop(dialect, target, source);
                                if invalid {
                                    errors.push(type_issue(
                                        strict,
                                        validation_codes::E_SETOP_TYPE_MISMATCH,
                                        validation_codes::W_SETOP_IMPLICIT_COERCION,
                                        "Recursive branch cannot convert to the anchor column type"
                                            .to_owned(),
                                    ));
                                }
                            }
                        }
                    }
                }
            }
        }
        Expression::WindowFunction(window) if dialect == DialectType::DuckDB => {
            use crate::expressions::{WindowFrameBound, WindowFrameKind};
            if let Some(frame) = &window.over.frame {
                let offset = |bound: &WindowFrameBound| {
                    matches!(
                        bound,
                        WindowFrameBound::Preceding(_) | WindowFrameBound::Following(_)
                    )
                };
                if frame.kind == WindowFrameKind::Range
                    && (offset(&frame.start) || frame.end.as_ref().is_some_and(offset))
                {
                    for ordered in &window.over.order_by {
                        let ty = family(&ordered.this);
                        if ty != TypeFamily::Unknown && !ty.is_numeric() && !ty.is_temporal() {
                            errors.push(type_issue(
                                strict,
                                "E232",
                                "E232",
                                "RANGE offset requires a numeric or temporal ordering expression"
                                    .to_owned(),
                            ));
                        }
                    }
                }
            }
        }
        Expression::Cast(expr) | Expression::TryCast(expr) | Expression::SafeCast(expr) if matches!(&expr.to, DataType::Custom { name } if canonical_type_family(name) == TypeFamily::Unknown && !known_types.iter().any(|known| known.eq_ignore_ascii_case(name))) => {
            if coercion::covered(dialect) {
                errors.push(type_issue(
                    strict,
                    validation_codes::E_INVALID_CAST,
                    validation_codes::W_LOSSY_CAST,
                    format!("Unknown cast target type {:?}", expr.to),
                ));
            }
        }
        Expression::Literal(literal) if dialect == DialectType::DuckDB => {
            if let crate::expressions::Literal::Date(value)
            | crate::expressions::Literal::Timestamp(value)
            | crate::expressions::Literal::Datetime(value) = literal.as_ref()
            {
                if invalid_temporal_literal(value) {
                    errors.push(type_issue(
                        strict,
                        validation_codes::E_INVALID_CAST,
                        validation_codes::W_LOSSY_CAST,
                        "Invalid temporal literal".to_owned(),
                    ));
                }
            }
        }
        Expression::Interval(interval) if dialect == DialectType::DuckDB => {
            if let Some(Expression::Literal(literal)) = &interval.this {
                if let crate::expressions::Literal::String(value) = literal.as_ref() {
                    if !value.chars().any(|c| c.is_ascii_digit()) {
                        errors.push(type_issue(
                            strict,
                            validation_codes::E_INVALID_CAST,
                            validation_codes::W_LOSSY_CAST,
                            "Invalid interval literal".to_owned(),
                        ));
                    }
                }
            }
        }
        Expression::DateTrunc(expr) | Expression::TimestampTrunc(expr) => {
            let ty = family(&expr.this);
            check_function_argument(
                errors,
                strict,
                "date_trunc",
                1,
                ty,
                "a temporal argument",
                ty.is_temporal(),
            );
        }
        Expression::Extract(expr) => {
            let ty = family(&expr.this);
            check_function_argument(
                errors,
                strict,
                "extract",
                1,
                ty,
                "a temporal argument",
                ty.is_temporal(),
            );
        }
        Expression::DateDiff(expr) => {
            for (index, arg) in [&expr.this, &expr.expression].iter().enumerate() {
                let ty = family(arg);
                check_function_argument(
                    errors,
                    strict,
                    "date_diff",
                    index,
                    ty,
                    "a temporal argument",
                    ty.is_temporal(),
                );
            }
        }
        Expression::IsTrue(expr) | Expression::IsFalse(expr) => predicate(&expr.this),
        Expression::Case(case) => {
            if case.operand.is_none() {
                for (condition, _) in &case.whens {
                    predicate(condition);
                }
            }
            let mut values: Vec<_> = case.whens.iter().map(|(_, value)| value).collect();
            values.extend(case.else_.iter());
            signatures::unify(&values, dialect, schema, context, strict, errors);
        }
        Expression::IfFunc(expr) => {
            predicate(&expr.condition);
            let mut values = vec![&expr.true_value];
            values.extend(expr.false_value.iter());
            signatures::unify(&values, dialect, schema, context, strict, errors);
        }
        Expression::Sum(expr)
        | Expression::Avg(expr)
        | Expression::Min(expr)
        | Expression::Max(expr)
        | Expression::Stddev(expr)
        | Expression::Variance(expr) => {
            if let Some(filter) = &expr.filter {
                predicate(filter);
            }
        }
        Expression::Count(expr) => {
            if let Some(filter) = &expr.filter {
                predicate(filter);
            }
        }
        Expression::Neg(expr) => {
            let ty = family(&expr.this);
            if coercion::covered(dialect)
                && ty != TypeFamily::Unknown
                && !ty.is_numeric()
                && ty != TypeFamily::Interval
            {
                errors.push(type_issue(
                    strict,
                    validation_codes::E_INVALID_ARITHMETIC_TYPE,
                    validation_codes::W_IMPLICIT_CAST_ARITHMETIC,
                    format!("Unary minus cannot operate on {}", type_family_name(ty)),
                ));
            }
        }
        Expression::Cast(expr) if dialect == DialectType::DuckDB => {
            let source = family(&expr.this);
            let target = data_type_family(&expr.to);
            // Only reject established impossible built-in conversions. Unknown
            // names may be application-defined types and remain unchecked.
            if (source == TypeFamily::Timestamp
                && matches!(target, TypeFamily::Boolean | TypeFamily::Integer))
                || (source == TypeFamily::Boolean && target == TypeFamily::Timestamp)
            {
                errors.push(type_issue(
                    strict,
                    validation_codes::E_INVALID_CAST,
                    validation_codes::W_LOSSY_CAST,
                    format!(
                        "Cannot cast {} to {}",
                        type_family_name(source),
                        type_family_name(target)
                    ),
                ));
            }
        }
        Expression::In(expr) => {
            if dialect == DialectType::DuckDB
                && expr
                    .expressions
                    .iter()
                    .any(|value| invalid_literal_for(value, family(&expr.this)))
            {
                errors.push(type_issue(
                    strict,
                    validation_codes::E_INVALID_CAST,
                    validation_codes::W_LOSSY_CAST,
                    "IN list contains an invalid coercible literal".to_owned(),
                ));
            }
            if let Some(query) = &expr.query {
                if let Some(types) = projection_families(query, schema, dialect) {
                    let expected = match &expr.this {
                        Expression::Tuple(tuple) => tuple.expressions.len(),
                        _ => 1,
                    };
                    if types.len() != expected {
                        errors.push(type_issue(
                            strict,
                            validation_codes::E_SETOP_ARITY_MISMATCH,
                            validation_codes::W_SETOP_IMPLICIT_COERCION,
                            format!(
                                "IN subquery returns {} columns, expected {expected}",
                                types.len()
                            ),
                        ));
                    } else if expected == 1
                        && !coercion::setop_literal(dialect, query, 0, family(&expr.this))
                        && !are_comparable(family(&expr.this), types[0])
                    {
                        errors.push(type_issue(
                            strict,
                            validation_codes::E_INCOMPATIBLE_COMPARISON_TYPES,
                            validation_codes::W_IMPLICIT_CAST_COMPARISON,
                            "IN subquery has incompatible output type".to_owned(),
                        ));
                    }
                }
            }
        }
        Expression::Eq(expr)
        | Expression::Neq(expr)
        | Expression::Lt(expr)
        | Expression::Gt(expr)
        | Expression::Lte(expr)
        | Expression::Gte(expr) => {
            if dialect == DialectType::DuckDB
                && (invalid_literal_for(&expr.left, family(&expr.right))
                    || invalid_literal_for(&expr.right, family(&expr.left)))
            {
                errors.push(type_issue(
                    strict,
                    validation_codes::E_INVALID_CAST,
                    validation_codes::W_LOSSY_CAST,
                    "Comparison contains an invalid coercible literal".to_owned(),
                ));
            }
            if let (Expression::Tuple(left), Expression::Tuple(right)) = (&expr.left, &expr.right) {
                if left.expressions.len() != right.expressions.len() {
                    errors.push(type_issue(
                        strict,
                        validation_codes::E_SETOP_ARITY_MISMATCH,
                        validation_codes::W_SETOP_IMPLICIT_COERCION,
                        "Row values have different column counts".to_owned(),
                    ));
                }
            }
        }
        Expression::Between(expr)
            if dialect == DialectType::DuckDB
                && (invalid_literal_for(&expr.low, family(&expr.this))
                    || invalid_literal_for(&expr.high, family(&expr.this))) =>
        {
            errors.push(type_issue(
                strict,
                validation_codes::E_INVALID_CAST,
                validation_codes::W_LOSSY_CAST,
                "BETWEEN contains an invalid coercible literal".to_owned(),
            ));
        }
        _ => {}
    }
}
