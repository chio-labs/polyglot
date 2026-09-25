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
    Timestamp,
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
            TypeFamily::Timestamp => Self::Timestamp,
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

    /// NULL contributes no target type; unknown metadata must remain unknown.
    fn target(self, later: Self) -> Self {
        if self == Self::Null {
            later
        } else {
            self
        }
    }

    fn temporal(self) -> bool {
        matches!(
            self,
            Self::Date | Self::Timestamp | Self::TimestampNtz | Self::TimestampTz | Self::Time
        )
    }
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
        (Number | Timestamp | TimestampNtz | TimestampTz, Variant) => Clean,
        (Variant, Boolean | Number | Array) | (Array, Variant) => Clean,
        (Date, Timestamp | TimestampNtz | TimestampTz)
        | (Timestamp | TimestampNtz | TimestampTz, Date)
        | (TimestampTz, TimestampNtz)
        | (Timestamp, TimestampNtz | TimestampTz)
        | (TimestampNtz | TimestampTz, Timestamp) => Clean,
        // All other pairs in the measured scalar/VARIANT/ARRAY domain reject.
        _ => Rejected,
    }
}

#[derive(Default)]
pub(super) struct SetResolver {
    outputs: HashMap<*const Expression, Option<Vec<Kind>>>,
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
    fn aligned(&mut self, query: &Expression) -> Option<(Vec<Kind>, Vec<Kind>)> {
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
                                .unwrap_or(Kind::Null),
                            output
                                .right_ordinal
                                .and_then(|i| right.get(i).copied())
                                .unwrap_or(Kind::Null),
                        )
                    })
                    .unzip(),
            ),
            None => Some((left, right)),
        }
    }

    fn resolve(&mut self, query: &Expression) -> Option<Vec<Kind>> {
        let key = query as *const Expression;
        if let Some(value) = self.outputs.get(&key) {
            return value.clone();
        }
        let result = if operands(query).is_some() {
            self.aligned(query).and_then(|(left, right)| {
                (left.len() == right.len()).then(|| {
                    left.into_iter()
                        .zip(right)
                        .map(|(a, b)| a.target(b))
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
                    Some(select.expressions.iter().map(Kind::expression).collect())
                }
                Expression::Subquery(query) => self.resolve(&query.this),
                Expression::Paren(paren) => self.resolve(&paren.this),
                Expression::Annotated(annotated) => self.resolve(&annotated.this),
                Expression::Values(values) => values
                    .expressions
                    .first()
                    .map(|row| row.expressions.iter().map(Kind::expression).collect()),
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
            let verdict = compatibility(first, later);
            if verdict != Compatibility::Clean {
                errors.push(type_issue(
                    strict && verdict == Compatibility::Rejected,
                    validation_codes::E_SETOP_TYPE_MISMATCH,
                    validation_codes::W_SETOP_IMPLICIT_COERCION,
                    format!(
                        "Set-operation column {} {}: first branch {}, later branch {}",
                        i + 1,
                        if verdict == Compatibility::Rejected {
                            "has incompatible types"
                        } else {
                            "may fail during runtime conversion"
                        },
                        kind_name(first),
                        kind_name(later)
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
        Kind::Timestamp => "TIMESTAMP",
        Kind::TimestampNtz => "TIMESTAMP_NTZ",
        Kind::TimestampTz => "TIMESTAMP_TZ",
        Kind::Time => "TIME",
        Kind::Variant => "VARIANT",
        Kind::Array => "ARRAY",
    }
}
