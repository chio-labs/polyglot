//! Conservative dialect coercion policy. See docs/semantic-types.md for sources.
use super::*;

struct Rules {
    dialect: DialectType,
    numeric_literal: bool,
    temporal_literal: bool,
    boolean_literal: bool,
    string_setop: bool,
    boolean_numeric_setop: bool,
    predicates: &'static [TypeFamily],
    comparison_pairs: &'static [(TypeFamily, TypeFamily)],
}

const SNOWFLAKE_COMPARISONS: &[(TypeFamily, TypeFamily)] = &[
    (TypeFamily::String, TypeFamily::Integer),
    (TypeFamily::String, TypeFamily::Numeric),
    (TypeFamily::String, TypeFamily::Boolean),
    (TypeFamily::String, TypeFamily::Date),
    (TypeFamily::String, TypeFamily::Time),
    (TypeFamily::String, TypeFamily::Timestamp),
    (TypeFamily::Integer, TypeFamily::Boolean),
    (TypeFamily::Numeric, TypeFamily::Boolean),
];

const RULES: &[Rules] = &[
    // Generic is the library's historical strict, non-coercing abstract dialect.
    Rules {
        dialect: DialectType::Generic,
        numeric_literal: false,
        temporal_literal: false,
        boolean_literal: false,
        string_setop: false,
        boolean_numeric_setop: false,
        predicates: &[],
        comparison_pairs: &[],
    },
    Rules {
        dialect: DialectType::DuckDB,
        numeric_literal: true,
        temporal_literal: true,
        boolean_literal: true,
        string_setop: true,
        boolean_numeric_setop: true,
        predicates: &[TypeFamily::Integer, TypeFamily::Numeric],
        comparison_pairs: &[
            (TypeFamily::Boolean, TypeFamily::Integer),
            (TypeFamily::Boolean, TypeFamily::Numeric),
        ],
    },
    Rules {
        dialect: DialectType::PostgreSQL,
        numeric_literal: true,
        temporal_literal: true,
        boolean_literal: true,
        string_setop: false,
        boolean_numeric_setop: false,
        predicates: &[],
        comparison_pairs: &[],
    },
    Rules {
        dialect: DialectType::Snowflake,
        numeric_literal: true,
        temporal_literal: true,
        boolean_literal: true,
        string_setop: true,
        boolean_numeric_setop: false,
        predicates: &[TypeFamily::Integer, TypeFamily::Numeric, TypeFamily::String],
        comparison_pairs: SNOWFLAKE_COMPARISONS,
    },
    Rules {
        dialect: DialectType::BigQuery,
        numeric_literal: false,
        temporal_literal: true,
        boolean_literal: false,
        string_setop: false,
        boolean_numeric_setop: false,
        predicates: &[],
        comparison_pairs: &[],
    },
];

fn rules(dialect: DialectType) -> Option<&'static Rules> {
    RULES.iter().find(|r| r.dialect == dialect)
}

pub(super) fn covered(dialect: DialectType) -> bool {
    rules(dialect).is_some()
}

pub(super) fn fully_modelled(family: TypeFamily) -> bool {
    !matches!(
        family,
        TypeFamily::Unknown
            | TypeFamily::Json
            | TypeFamily::Uuid
            | TypeFamily::Array
            | TypeFamily::Map
            | TypeFamily::Struct
    )
}

pub(super) fn predicate(dialect: DialectType, family: TypeFamily) -> bool {
    rules(dialect).is_some_and(|rule| rule.predicates.contains(&family))
}

pub(super) fn string_literal(expr: &Expression) -> bool {
    match expr {
        Expression::Literal(value) => {
            matches!(value.as_ref(), crate::expressions::Literal::String(_))
        }
        Expression::Alias(alias) => string_literal(&alias.this),
        Expression::Paren(paren) => string_literal(&paren.this),
        _ => false,
    }
}

pub(super) fn literal_coerces(dialect: DialectType, expr: &Expression, target: TypeFamily) -> bool {
    let Some(r) = rules(dialect) else {
        return true;
    };
    string_literal(expr)
        && ((target.is_numeric() && r.numeric_literal)
            || (matches!(
                target,
                TypeFamily::Date | TypeFamily::Time | TypeFamily::Timestamp
            ) && r.temporal_literal)
            || (target == TypeFamily::Boolean && r.boolean_literal))
}

pub(super) fn comparable(
    dialect: DialectType,
    left: &Expression,
    right: &Expression,
    lf: TypeFamily,
    rf: TypeFamily,
) -> bool {
    !covered(dialect)
        || (dialect != DialectType::Generic && (!fully_modelled(lf) || !fully_modelled(rf)))
        || are_comparable(lf, rf)
        || rules(dialect).is_some_and(|rule| {
            rule.comparison_pairs.contains(&(lf, rf)) || rule.comparison_pairs.contains(&(rf, lf))
        })
        || literal_coerces(dialect, left, rf)
        || literal_coerces(dialect, right, lf)
}

/// Accepted binder coercions whose success depends on the runtime value.
pub(super) fn runtime_comparison(
    dialect: DialectType,
    left: TypeFamily,
    right: TypeFamily,
) -> bool {
    matches!(dialect, DialectType::DuckDB | DialectType::Snowflake)
        && ((left == TypeFamily::String
            && (right.is_numeric() || right == TypeFamily::Boolean || right.is_temporal()))
            || (right == TypeFamily::String
                && (left.is_numeric() || left == TypeFamily::Boolean || left.is_temporal())))
}

/// Equality uses combination casts, whereas ordering has stricter binder rules.
pub(super) fn runtime_equality(dialect: DialectType, left: TypeFamily, right: TypeFamily) -> bool {
    dialect == DialectType::DuckDB
        && fully_modelled(left)
        && fully_modelled(right)
        && left != right
        && !(left.is_numeric() && right.is_numeric())
}

pub(super) fn temporal_argument(
    dialect: DialectType,
    expr: &Expression,
    family: TypeFamily,
) -> bool {
    family.is_temporal()
        || (dialect == DialectType::Snowflake && family == TypeFamily::String)
        || literal_coerces(dialect, expr, TypeFamily::Timestamp)
}

/// DATE_TRUNC/EXTRACT do not share DATEDIFF's Snowflake VARCHAR overloads.
pub(super) fn temporal_function_argument(
    name: &str,
    dialect: DialectType,
    expr: &Expression,
    family: TypeFamily,
) -> bool {
    if dialect == DialectType::Snowflake && matches!(name, "date_trunc" | "extract" | "date_part") {
        return family.is_temporal();
    }
    temporal_argument(dialect, expr, family)
}

pub(super) fn setop(dialect: DialectType, left: TypeFamily, right: TypeFamily) -> bool {
    if dialect != DialectType::Generic && (!fully_modelled(left) || !fully_modelled(right)) {
        return true;
    }
    let Some(r) = rules(dialect) else {
        return true;
    };
    are_setop_compatible(left, right)
        || (r.string_setop
            && (left == TypeFamily::String || right == TypeFamily::String)
            && !matches!(
                left,
                TypeFamily::Array | TypeFamily::Map | TypeFamily::Struct
            )
            && !matches!(
                right,
                TypeFamily::Array | TypeFamily::Map | TypeFamily::Struct
            ))
        || (r.boolean_numeric_setop
            && ((left == TypeFamily::Boolean && right.is_numeric())
                || (right == TypeFamily::Boolean && left.is_numeric())))
}

pub(super) fn projection_literal(query: &Expression, index: usize) -> Option<&Expression> {
    match query {
        Expression::Select(select) => select.expressions.get(index).filter(|e| string_literal(e)),
        Expression::Subquery(query) => projection_literal(&query.this, index),
        Expression::Paren(query) => projection_literal(&query.this, index),
        _ => None,
    }
}

pub(super) fn setop_literal(
    dialect: DialectType,
    query: &Expression,
    index: usize,
    target: TypeFamily,
) -> bool {
    projection_literal(query, index).is_some_and(|expr| literal_coerces(dialect, expr, target))
}

pub(super) fn arithmetic(
    dialect: DialectType,
    node: &Expression,
    left: TypeFamily,
    right: TypeFamily,
) -> bool {
    use TypeFamily::*;
    if !covered(dialect)
        || (dialect != DialectType::Generic && (!fully_modelled(left) || !fully_modelled(right)))
        || left == Unknown
        || right == Unknown
        || (left.is_numeric() && right.is_numeric())
    {
        return true;
    }
    let add = matches!(node, Expression::Add(_));
    let sub = matches!(node, Expression::Sub(_));
    if dialect == DialectType::Generic {
        return (add || sub)
            && ((left.is_temporal() && right.is_numeric())
                || (right.is_temporal() && left.is_numeric())
                || (sub && left.is_temporal() && right.is_temporal()));
    }
    if matches!(dialect, DialectType::DuckDB | DialectType::PostgreSQL)
        && ((matches!(node, Expression::Mul(_))
            && ((left == Interval && right.is_numeric())
                || (right == Interval && left.is_numeric())))
            || (matches!(node, Expression::Div(_)) && left == Interval && right.is_numeric()))
    {
        return true;
    }
    // Known date/integer and temporal/interval overloads; direction matters.
    const ADD: &[(TypeFamily, TypeFamily)] = &[
        (Date, Integer),
        (Integer, Date),
        (Date, Interval),
        (Interval, Date),
        (Timestamp, Interval),
        (Interval, Timestamp),
        (Time, Interval),
        (Interval, Time),
        (Interval, Interval),
    ];
    const SUB: &[(TypeFamily, TypeFamily)] = &[
        (Date, Integer),
        (Date, Date),
        (Date, Interval),
        (Timestamp, Interval),
        (Timestamp, Timestamp),
        (Time, Interval),
        (Interval, Interval),
    ];
    if (add && ADD.contains(&(left, right))) || (sub && SUB.contains(&(left, right))) {
        return true;
    }
    // Snowflake permits numeric VARCHAR arithmetic; do not reject uncertain
    // overloads on other engines based on DuckDB's binder.
    dialect == DialectType::Snowflake
        && ((left == String && right.is_numeric()) || (right == String && left.is_numeric()))
}
