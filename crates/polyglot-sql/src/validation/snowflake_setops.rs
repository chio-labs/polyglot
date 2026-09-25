//! Directional set-operation binding policy from synthetic Snowflake probes.
//! This is intentionally separate from scalar coercion and recursive-CTE rules.
use super::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    Unknown,
    Null,
    Boolean,
    Number,
    String,
    Date,
    TimestampLtz,
    TimestampNtz,
    TimestampTz,
    Time,
    Variant,
    Array,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Compatibility {
    Clean,
    Runtime,
    Rejected,
}

impl Kind {
    fn from_type(ty: &DataType) -> Self {
        match ty {
            DataType::Custom { name } => match name
                .split('(')
                .next()
                .unwrap_or(name)
                .trim()
                .replace('_', "")
                .to_ascii_uppercase()
                .as_str()
            {
                "TIMESTAMPNTZ" => Self::TimestampNtz,
                "TIMESTAMPTZ" => Self::TimestampTz,
                "TIMESTAMPLTZ" => Self::TimestampLtz,
                _ => Self::from_family(data_type_family(ty)),
            },
            DataType::Timestamp { timezone: true, .. } => Self::TimestampTz,
            _ => Self::from_family(data_type_family(ty)),
        }
    }

    fn from_family(family: TypeFamily) -> Self {
        match family {
            TypeFamily::Boolean => Self::Boolean,
            TypeFamily::Integer | TypeFamily::Numeric => Self::Number,
            TypeFamily::String => Self::String,
            TypeFamily::Date => Self::Date,
            TypeFamily::Timestamp => Self::TimestampNtz,
            TypeFamily::Time => Self::Time,
            TypeFamily::Json => Self::Variant,
            TypeFamily::Array => Self::Array,
            _ => Self::Unknown,
        }
    }

    fn expression(expr: &Expression) -> Self {
        match expr {
            Expression::Alias(alias) => Self::expression(&alias.this),
            Expression::Annotated(annotated) => Self::expression(&annotated.this),
            Expression::Paren(paren) => Self::expression(&paren.this),
            Expression::Null(_) => Self::Null,
            _ => expr
                .inferred_type()
                .map(Self::from_type)
                .unwrap_or_else(|| {
                    Self::from_family(infer_expression_type_family(
                        expr,
                        &HashMap::new(),
                        &TypeCheckContext {
                            dialect: Some(DialectType::Snowflake),
                            ..Default::default()
                        },
                    ))
                }),
        }
    }

    fn temporal(self) -> bool {
        matches!(
            self,
            Self::Date | Self::TimestampLtz | Self::TimestampNtz | Self::TimestampTz | Self::Time
        )
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Numeric {
    Fixed(u32, u32),
    Float,
}

impl Numeric {
    fn combine(self, other: Self) -> Self {
        match (self, other) {
            (Self::Float, _) | (_, Self::Float) => Self::Float,
            (Self::Fixed(p, s), Self::Fixed(q, t)) => {
                let scale = s.max(t).min(38);
                Self::Fixed(
                    (p.saturating_sub(s).max(q.saturating_sub(t)) + scale).min(38),
                    scale,
                )
            }
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct SetType {
    kind: Kind,
    numeric: Numeric,
    temporal_precision: u32,
}

impl SetType {
    fn new(kind: Kind) -> Self {
        Self {
            kind,
            numeric: Numeric::Fixed(38, 0),
            temporal_precision: 9,
        }
    }

    fn from_type(ty: &DataType) -> Self {
        let mut result = Self::new(Kind::from_type(ty));
        match ty {
            DataType::Float { .. } | DataType::Double { .. } => result.numeric = Numeric::Float,
            DataType::Decimal { precision, scale } => {
                result.numeric = Numeric::Fixed(precision.unwrap_or(38), scale.unwrap_or(0))
            }
            DataType::Time { precision, .. } | DataType::Timestamp { precision, .. } => {
                result.temporal_precision = precision.unwrap_or(9)
            }
            DataType::Custom { name } => {
                let base = name
                    .split('(')
                    .next()
                    .unwrap_or(name)
                    .trim()
                    .to_ascii_uppercase();
                let parameters: Vec<u32> = name
                    .split_once('(')
                    .map(|(_, args)| {
                        args.trim_end_matches(')')
                            .split(',')
                            .filter_map(|s| s.trim().parse().ok())
                            .collect()
                    })
                    .unwrap_or_default();
                if matches!(base.as_str(), "FLOAT" | "DOUBLE" | "REAL") {
                    result.numeric = Numeric::Float;
                } else if result.kind == Kind::Number {
                    result.numeric = Numeric::Fixed(
                        parameters.first().copied().unwrap_or(38),
                        parameters.get(1).copied().unwrap_or(0),
                    );
                } else if result.kind.temporal() {
                    result.temporal_precision = parameters.first().copied().unwrap_or(9);
                }
            }
            _ => {}
        }
        result
    }

    fn expression(expr: &Expression) -> Self {
        match expr {
            Expression::Alias(alias) => Self::expression(&alias.this),
            Expression::Annotated(annotated) => Self::expression(&annotated.this),
            Expression::Paren(paren) => Self::expression(&paren.this),
            Expression::Literal(literal)
                if matches!(literal.as_ref(), crate::expressions::Literal::Number(_)) =>
            {
                let crate::expressions::Literal::Number(number) = literal.as_ref() else {
                    unreachable!()
                };
                let mut result = Self::new(Kind::Number);
                result.numeric = if number.contains(['e', 'E']) {
                    Numeric::Float
                } else {
                    Numeric::Fixed(
                        number
                            .chars()
                            .filter(char::is_ascii_digit)
                            .count()
                            .max(1)
                            .min(38) as u32,
                        number
                            .split_once('.')
                            .map_or(0, |(_, fraction)| fraction.len().min(38) as u32),
                    )
                };
                result
            }
            _ => expr
                .inferred_type()
                .map(Self::from_type)
                .unwrap_or_else(|| Self::new(Kind::expression(expr))),
        }
    }

    /// Each operator consumes its child result types, including runtime-risk
    /// pairs. Rejected or unknown children cannot establish a result type.
    fn fold(self, later: Self) -> Self {
        use Kind::*;
        if matches!(self.kind, Unknown)
            || matches!(later.kind, Unknown)
            || compatibility(self.kind, later.kind) == Compatibility::Rejected
        {
            return Self::new(Unknown);
        }
        if self.kind == Null {
            return later;
        }
        if later.kind == Null {
            return self;
        }
        let mut result = match (self.kind, later.kind) {
            (Number, Number) => Self {
                numeric: self.numeric.combine(later.numeric),
                ..self
            },
            (Number, String) => Self {
                numeric: self.numeric.combine(Numeric::Fixed(18, 5)),
                ..self
            },
            (String, Number) => Self {
                numeric: later.numeric.combine(Numeric::Fixed(18, 5)),
                ..later
            },
            (Number, Variant) => later,
            (String, kind) if kind.temporal() => later,
            (Date, TimestampLtz | TimestampNtz | TimestampTz) => later,
            _ => self,
        };
        if self.kind.temporal() && later.kind.temporal() {
            result.temporal_precision = self.temporal_precision.max(later.temporal_precision);
        }
        result
    }

    fn data_type(self) -> Option<DataType> {
        Some(match self.kind {
            Kind::Unknown | Kind::Null => return None,
            Kind::Boolean => DataType::Boolean,
            Kind::Number => match self.numeric {
                Numeric::Fixed(p, s) => DataType::Decimal {
                    precision: Some(p),
                    scale: Some(s),
                },
                Numeric::Float => DataType::Float {
                    precision: None,
                    scale: None,
                    real_spelling: false,
                },
            },
            Kind::String => DataType::VarChar {
                length: None,
                parenthesized_length: false,
            },
            Kind::Date => DataType::Date,
            Kind::TimestampNtz => DataType::Custom {
                name: format!("TIMESTAMP_NTZ({})", self.temporal_precision),
            },
            Kind::TimestampLtz => DataType::Custom {
                name: format!("TIMESTAMP_LTZ({})", self.temporal_precision),
            },
            Kind::TimestampTz => DataType::Timestamp {
                precision: Some(self.temporal_precision),
                timezone: self.kind == Kind::TimestampTz,
            },
            Kind::Time => DataType::Time {
                precision: Some(self.temporal_precision),
                timezone: false,
            },
            Kind::Variant => DataType::Json,
            Kind::Array => DataType::Array {
                element_type: Box::new(DataType::Json),
                dimension: None,
            },
        })
    }
}

/// Shared with output inference so a CTE or derived table keeps the same folded
/// type as an inline set-operation operand. None denotes an untyped NULL.
pub(crate) fn result_type(left: Option<&DataType>, right: Option<&DataType>) -> Option<DataType> {
    left.map(SetType::from_type)
        .unwrap_or_else(|| SetType::new(Kind::Null))
        .fold(
            right
                .map(SetType::from_type)
                .unwrap_or_else(|| SetType::new(Kind::Null)),
        )
        .data_type()
}

/// Rows are the first branch; columns are the later branch. Runtime means some
/// values of these static types compile but fail during conversion.
fn compatibility(first: Kind, later: Kind) -> Compatibility {
    use Compatibility::*;
    use Kind::*;
    if matches!(first, Unknown | Null) || matches!(later, Unknown | Null) || first == later {
        return Clean;
    }
    match (first, later) {
        (Boolean, Number) => Clean,
        (Number, Boolean) => Rejected,
        (Boolean | Number, String) => Runtime,
        (String, Boolean | Variant) => Clean,
        (String, Number) => Runtime,
        (String, value) if value.temporal() => Runtime,
        (value, String) if value.temporal() => Runtime,
        (Boolean | Date | Time, Variant) => Runtime,
        (Number | TimestampLtz | TimestampNtz | TimestampTz, Variant) => Clean,
        (Variant, Boolean | Number | Array) | (Array, Variant) => Clean,
        (Date, TimestampLtz | TimestampNtz | TimestampTz)
        | (TimestampLtz | TimestampNtz | TimestampTz, Date)
        | (TimestampTz, TimestampNtz)
        // LTZ pairs retain the existing conservative compatibility policy;
        // the measured matrix distinguishes NTZ/TZ but does not contain LTZ.
        | (TimestampLtz, TimestampNtz | TimestampTz)
        | (TimestampNtz | TimestampTz, TimestampLtz) => Clean,
        // All other pairs in the measured scalar/VARIANT/ARRAY domain reject.
        _ => Rejected,
    }
}

#[derive(Default)]
pub(super) struct SetResolver {
    outputs: HashMap<*const Expression, Option<Vec<SetType>>>,
    layouts: crate::set_operation::LayoutResolver,
}

fn operands(query: &Expression) -> Option<(&Expression, &Expression)> {
    match query {
        Expression::Union(op) => Some((&op.left, &op.right)),
        Expression::Intersect(op) => Some((&op.left, &op.right)),
        Expression::Except(op) => Some((&op.left, &op.right)),
        _ => None,
    }
}

impl SetResolver {
    fn aligned(&mut self, query: &Expression) -> Option<(Vec<SetType>, Vec<SetType>)> {
        let (left, right) = operands(query)?;
        let left = self.resolve(left)?;
        let right = self.resolve(right)?;
        match self
            .layouts
            .layout(query, Some(DialectType::Snowflake))
            .ok()?
        {
            Some(layout) => Some(
                layout
                    .outputs
                    .iter()
                    .map(|output| {
                        (
                            output
                                .left_ordinal
                                .and_then(|i| left.get(i).copied())
                                .unwrap_or_else(|| SetType::new(Kind::Null)),
                            output
                                .right_ordinal
                                .and_then(|i| right.get(i).copied())
                                .unwrap_or_else(|| SetType::new(Kind::Null)),
                        )
                    })
                    .unzip(),
            ),
            None => Some((left, right)),
        }
    }

    fn resolve(&mut self, query: &Expression) -> Option<Vec<SetType>> {
        let key = query as *const Expression;
        if let Some(value) = self.outputs.get(&key) {
            return value.clone();
        }
        let result = if operands(query).is_some() {
            self.aligned(query).and_then(|(left, right)| {
                (left.len() == right.len()).then(|| {
                    left.into_iter()
                        .zip(right)
                        .map(|(a, b)| a.fold(b))
                        .collect()
                })
            })
        } else {
            match query {
                Expression::Select(select)
                    if !select.expressions.iter().any(|expr| {
                        matches!(expr, Expression::Star(_) | Expression::BracedWildcard(_))
                    }) =>
                {
                    Some(select.expressions.iter().map(SetType::expression).collect())
                }
                Expression::Subquery(query) => self.resolve(&query.this),
                Expression::Paren(paren) => self.resolve(&paren.this),
                Expression::Annotated(annotated) => self.resolve(&annotated.this),
                Expression::Values(values) => values
                    .expressions
                    .first()
                    .map(|row| row.expressions.iter().map(SetType::expression).collect()),
                _ => None,
            }
        };
        self.outputs.insert(key, result.clone());
        result
    }

    pub(super) fn check(
        &mut self,
        query: &Expression,
        strict: bool,
        errors: &mut Vec<ValidationError>,
    ) {
        if let Err(error) = self.layouts.layout(query, Some(DialectType::Snowflake)) {
            if !error.is_indeterminate() {
                errors.push(type_issue(
                    strict,
                    validation_codes::E_SETOP_ARITY_MISMATCH,
                    validation_codes::W_SETOP_IMPLICIT_COERCION,
                    error.to_string(),
                ));
            }
            return;
        }
        let Some((left, right)) = self.aligned(query) else {
            return;
        };
        if left.len() != right.len() {
            errors.push(type_issue(
                strict,
                validation_codes::E_SETOP_ARITY_MISMATCH,
                validation_codes::W_SETOP_IMPLICIT_COERCION,
                format!(
                    "Set-operation operands return different column counts: left {}, right {}",
                    left.len(),
                    right.len()
                ),
            ));
            return;
        }
        for (i, (first, later)) in left.into_iter().zip(right).enumerate() {
            let verdict = compatibility(first.kind, later.kind);
            if verdict != Compatibility::Clean {
                errors.push(type_issue(
                    strict && verdict == Compatibility::Rejected,
                    validation_codes::E_SETOP_TYPE_MISMATCH,
                    validation_codes::W_SETOP_IMPLICIT_COERCION,
                    format!(
                        "Set-operation column {} {}: accumulated type {}, next type {}",
                        i + 1,
                        if verdict == Compatibility::Rejected {
                            "has incompatible types"
                        } else {
                            "may fail during runtime conversion"
                        },
                        kind_name(first.kind),
                        kind_name(later.kind)
                    ),
                ));
            }
        }
    }
}

fn kind_name(kind: Kind) -> &'static str {
    match kind {
        Kind::Unknown => "unknown",
        Kind::Null => "NULL",
        Kind::Boolean => "BOOLEAN",
        Kind::Number => "NUMBER",
        Kind::String => "VARCHAR",
        Kind::Date => "DATE",
        Kind::TimestampLtz => "TIMESTAMP_LTZ",
        Kind::TimestampNtz => "TIMESTAMP_NTZ",
        Kind::TimestampTz => "TIMESTAMP_TZ",
        Kind::Time => "TIME",
        Kind::Variant => "VARIANT",
        Kind::Array => "ARRAY",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_type(name: &str) -> DataType {
        let query =
            crate::parse_one(&format!("SELECT NULL::{name}"), DialectType::Snowflake).unwrap();
        let Expression::Select(select) = query else {
            panic!()
        };
        let Expression::Cast(cast) = &select.expressions[0] else {
            panic!()
        };
        cast.to.clone()
    }

    #[test]
    fn measured_result_type_matrix() {
        let fixture = include_str!("../../tests/fixtures/snowflake_set_operation_results.txt");
        assert_eq!(fixture.lines().count(), 88);
        let operand = |name| match name {
            "NUMBER" => "NULL::NUMBER(38,0)",
            "DECIMAL" => "NULL::NUMBER(10,2)",
            "VARCHAR_NUM" | "VARCHAR_TXT" => "NULL::VARCHAR",
            "NULL" => "NULL",
            "BOOLEAN" => "NULL::BOOLEAN",
            "FLOAT" => "NULL::FLOAT",
            "DATE" => "NULL::DATE",
            "TIMESTAMP_NTZ" => "NULL::TIMESTAMP_NTZ",
            "TIMESTAMP_TZ" => "NULL::TIMESTAMP_TZ",
            "TIME" => "NULL::TIME",
            "ARRAY" => "NULL::ARRAY",
            "VARIANT" => "NULL::VARIANT",
            _ => panic!(),
        };
        for line in fixture.lines() {
            let (pair, expected) = line.split_once(' ').unwrap();
            let (first, later) = pair.split_once('|').unwrap();
            let expected = SetType::from_type(&parse_type(expected));
            for operator in [
                "UNION",
                "UNION ALL",
                "INTERSECT",
                "EXCEPT",
                "MINUS",
                "UNION BY NAME",
                "UNION ALL BY NAME",
            ] {
                let sql = format!(
                    "SELECT {} x {operator} SELECT {} x",
                    operand(first),
                    operand(later)
                );
                let mut query = crate::parse_one(&sql, DialectType::Snowflake).unwrap();
                annotate_types(&mut query, None, Some(DialectType::Snowflake));
                let actual = SetResolver::default().resolve(&query).unwrap()[0];
                assert_eq!(actual.kind, expected.kind, "{pair} {operator}");
                if actual.kind == Kind::Number {
                    assert_eq!(actual.numeric, expected.numeric, "{pair} {operator}");
                }
                if actual.kind.temporal() {
                    assert_eq!(
                        actual.temporal_precision, expected.temporal_precision,
                        "{pair}"
                    );
                }
                let mut derived = crate::parse_one(
                    &format!("WITH orders AS ({sql}) SELECT x FROM orders"),
                    DialectType::Snowflake,
                )
                .unwrap();
                annotate_types(&mut derived, None, Some(DialectType::Snowflake));
                let output = SetResolver::default().resolve(&derived).unwrap()[0];
                assert_eq!(output.kind, expected.kind, "CTE {pair} {operator}");
                if output.kind == Kind::Number {
                    assert_eq!(output.numeric, expected.numeric, "CTE {pair} {operator}");
                }
            }
        }
    }

    #[test]
    fn intersect_precedence_is_snowflake_specific() {
        for operator in ["UNION ALL", "EXCEPT"] {
            let sql = format!(
                "SELECT TRUE x {operator} SELECT 1 x INTERSECT SELECT TRUE x ORDER BY x LIMIT 1"
            );
            let query = crate::parse_one(&sql, DialectType::Snowflake).unwrap();
            let right = match &query {
                Expression::Union(op) => {
                    assert!(op.order_by.is_some());
                    assert!(op.limit.is_some());
                    &op.right
                }
                Expression::Except(op) => {
                    assert!(op.order_by.is_some());
                    assert!(op.limit.is_some());
                    &op.right
                }
                _ => panic!("{query:?}"),
            };
            assert!(matches!(right, Expression::Intersect(_)));
            let result = validate_with_schema(
                &sql,
                DialectType::Snowflake,
                &ValidationSchema {
                    tables: vec![],
                    strict: Some(true),
                },
                &SchemaValidationOptions {
                    semantic: true,
                    check_types: true,
                    ..Default::default()
                },
            );
            assert!(
                result.errors.iter().any(|e| e.code == "E215"),
                "{:?}",
                result.errors
            );
        }
        let query = crate::parse_one(
            "(SELECT TRUE UNION ALL SELECT 1) INTERSECT SELECT TRUE",
            DialectType::Snowflake,
        )
        .unwrap();
        assert!(matches!(query, Expression::Intersect(_)));
        let query = crate::parse_one(
            "SELECT TRUE UNION ALL SELECT 1 INTERSECT SELECT TRUE",
            DialectType::DuckDB,
        )
        .unwrap();
        assert!(matches!(query, Expression::Intersect(_)));
        let query = crate::parse_one(
            "SELECT TRUE UNION ALL SELECT 1\n-- orders\nINTERSECT SELECT TRUE",
            DialectType::Snowflake,
        )
        .unwrap();
        let Expression::Annotated(annotated) = query else {
            panic!()
        };
        assert!(matches!(annotated.this, Expression::Union(_)));
    }
}
