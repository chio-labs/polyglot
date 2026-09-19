//! Shared result-layout semantics for name-aligned set operations.
//!
//! A set-operation result ordinal is not necessarily the same ordinal in each
//! input branch. DuckDB and Snowflake also allow an output to be absent from a
//! branch, in which case the database contributes `NULL`. BigQuery exposes the
//! same building blocks through its STRICT, INNER, LEFT, FULL, and ON variants.

use crate::dialects::DialectType;
use crate::expressions::{Expression, Identifier};
use std::collections::{HashMap, HashSet};
use std::fmt;

/// One output of an immediate name-aligned set operation.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct SetOperationOutput {
    /// The result name, preserving the spelling from the defining input.
    pub identifier: Identifier,
    /// Matching output ordinal in the immediate left input, if present.
    pub left_ordinal: Option<usize>,
    /// Matching output ordinal in the immediate right input, if present.
    pub right_ordinal: Option<usize>,
}

/// Ordered outputs of an immediate name-aligned set operation.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct SetOperationLayout {
    pub outputs: Vec<SetOperationOutput>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum SetOperationLayoutError {
    Indeterminate(String),
    DuplicateColumn(String),
    StrictColumnMismatch,
    MissingRequiredColumn(String),
}

#[cfg(feature = "semantic")]
impl SetOperationLayoutError {
    pub(crate) fn is_indeterminate(&self) -> bool {
        matches!(self, Self::Indeterminate(_))
    }
}

impl fmt::Display for SetOperationLayoutError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Indeterminate(message) => write!(f, "{message}"),
            Self::DuplicateColumn(name) => {
                write!(
                    f,
                    "duplicate output column '{name}' in name-aligned set operation"
                )
            }
            Self::StrictColumnMismatch => {
                f.write_str("BY NAME requires both inputs to expose the same set of output columns")
            }
            Self::MissingRequiredColumn(name) => write!(
                f,
                "column '{name}' required by the name-aligned set operation is missing"
            ),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SetOperationType {
    Union,
    Intersect,
    Except,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AlignmentMode {
    Strict,
    Inner,
    Left,
    Full,
}

struct SetOperationRef<'a> {
    operation_type: SetOperationType,
    left: &'a Expression,
    right: &'a Expression,
    by_name: bool,
    side: Option<&'a str>,
    kind: Option<&'a str>,
    corresponding: bool,
    strict: bool,
    on_columns: &'a [Expression],
}

/// Return the immediate result layout when `expression` is a name-aligned set
/// operation supported by the selected dialect. Positional and unsupported
/// forms return `None`.
pub(crate) fn set_operation_layout(
    expression: &Expression,
    dialect: Option<DialectType>,
) -> Result<Option<SetOperationLayout>, SetOperationLayoutError> {
    let Some(set_op) = set_operation_ref(expression) else {
        return Ok(None);
    };
    let Some(mode) = alignment_mode(&set_op, dialect) else {
        return Ok(None);
    };

    let left = query_output_identifiers(set_op.left, dialect)?;
    let right = query_output_identifiers(set_op.right, dialect)?;
    let left_index = identifier_index(&left, dialect)?;
    let right_index = identifier_index(&right, dialect)?;

    let outputs = if set_op.on_columns.is_empty() {
        layout_without_explicit_columns(mode, &left, &right, &left_index, &right_index, dialect)?
    } else {
        layout_with_explicit_columns(
            mode,
            set_op.on_columns,
            &left,
            &right,
            &left_index,
            &right_index,
            dialect,
        )?
    };

    Ok(Some(SetOperationLayout { outputs }))
}

/// Resolve all statically known result identifiers for a query expression.
///
/// This intentionally returns an indeterminate error for wildcards and unnamed
/// expressions. Callers that expose partial output information can fall back to
/// their existing projection-layout representation in that case.
pub(crate) fn query_output_identifiers(
    expression: &Expression,
    dialect: Option<DialectType>,
) -> Result<Vec<Identifier>, SetOperationLayoutError> {
    match expression {
        Expression::Select(select) => {
            let mut identifiers = Vec::new();
            for projection in &select.expressions {
                collect_projection_identifiers(projection, &mut identifiers)?;
            }
            Ok(identifiers)
        }
        Expression::Union(_) | Expression::Intersect(_) | Expression::Except(_) => {
            if let Some(layout) = set_operation_layout(expression, dialect)? {
                Ok(layout
                    .outputs
                    .into_iter()
                    .map(|output| output.identifier)
                    .collect())
            } else {
                let set_op = set_operation_ref(expression)
                    .expect("set-operation expression must expose set-operation fields");
                query_output_identifiers(set_op.left, dialect)
            }
        }
        Expression::Subquery(subquery) => {
            let identifiers = query_output_identifiers(&subquery.this, dialect)?;
            Ok(apply_column_aliases(identifiers, &subquery.column_aliases))
        }
        Expression::Alias(alias) => {
            let identifiers = query_output_identifiers(&alias.this, dialect)?;
            Ok(apply_column_aliases(identifiers, &alias.column_aliases))
        }
        Expression::Cte(cte) => {
            let identifiers = query_output_identifiers(&cte.this, dialect)?;
            Ok(apply_column_aliases(identifiers, &cte.columns))
        }
        Expression::Paren(paren) => query_output_identifiers(&paren.this, dialect),
        Expression::Annotated(annotated) => query_output_identifiers(&annotated.this, dialect),
        _ => Err(SetOperationLayoutError::Indeterminate(
            "query output columns cannot be determined statically".to_string(),
        )),
    }
}

/// Compare two identifiers using the column-name rules of the selected
/// dialect. The result spelling is kept separately from this comparison key.
pub(crate) fn identifier_key(identifier: &Identifier, dialect: Option<DialectType>) -> String {
    match dialect {
        // DuckDB treats even quoted identifiers case-insensitively. BigQuery
        // column names and aliases are likewise case-insensitive.
        Some(DialectType::DuckDB | DialectType::BigQuery) => identifier.name.to_ascii_lowercase(),
        // Snowflake folds unquoted identifiers to uppercase but preserves the
        // exact case of quoted identifiers.
        Some(DialectType::Snowflake) if !identifier.quoted => identifier.name.to_ascii_uppercase(),
        Some(DialectType::Snowflake) => identifier.name.clone(),
        _ if identifier.quoted => identifier.name.clone(),
        _ => identifier.name.to_ascii_lowercase(),
    }
}

fn set_operation_ref(expression: &Expression) -> Option<SetOperationRef<'_>> {
    match expression {
        Expression::Union(set_op) => Some(SetOperationRef {
            operation_type: SetOperationType::Union,
            left: &set_op.left,
            right: &set_op.right,
            by_name: set_op.by_name,
            side: set_op.side.as_deref(),
            kind: set_op.kind.as_deref(),
            corresponding: set_op.corresponding,
            strict: set_op.strict,
            on_columns: &set_op.on_columns,
        }),
        Expression::Intersect(set_op) => Some(SetOperationRef {
            operation_type: SetOperationType::Intersect,
            left: &set_op.left,
            right: &set_op.right,
            by_name: set_op.by_name,
            side: set_op.side.as_deref(),
            kind: set_op.kind.as_deref(),
            corresponding: set_op.corresponding,
            strict: set_op.strict,
            on_columns: &set_op.on_columns,
        }),
        Expression::Except(set_op) => Some(SetOperationRef {
            operation_type: SetOperationType::Except,
            left: &set_op.left,
            right: &set_op.right,
            by_name: set_op.by_name,
            side: set_op.side.as_deref(),
            kind: set_op.kind.as_deref(),
            corresponding: set_op.corresponding,
            strict: set_op.strict,
            on_columns: &set_op.on_columns,
        }),
        _ => None,
    }
}

fn alignment_mode(
    set_op: &SetOperationRef<'_>,
    dialect: Option<DialectType>,
) -> Option<AlignmentMode> {
    if !set_op.by_name && !set_op.corresponding {
        return None;
    }

    match dialect {
        Some(DialectType::DuckDB | DialectType::Snowflake)
            if set_op.operation_type == SetOperationType::Union && set_op.by_name =>
        {
            Some(AlignmentMode::Full)
        }
        Some(DialectType::BigQuery) => {
            let side = set_op.side.map(str::to_ascii_uppercase);
            let kind = set_op.kind.map(str::to_ascii_uppercase);

            if side.as_deref() == Some("FULL")
                || (side.is_none() && kind.as_deref() == Some("OUTER"))
            {
                Some(AlignmentMode::Full)
            } else if side.as_deref() == Some("LEFT") {
                Some(AlignmentMode::Left)
            } else if kind.as_deref() == Some("INNER") || (set_op.corresponding && !set_op.strict) {
                Some(AlignmentMode::Inner)
            } else {
                Some(AlignmentMode::Strict)
            }
        }
        _ => None,
    }
}

fn layout_without_explicit_columns(
    mode: AlignmentMode,
    left: &[Identifier],
    right: &[Identifier],
    left_index: &HashMap<String, usize>,
    right_index: &HashMap<String, usize>,
    dialect: Option<DialectType>,
) -> Result<Vec<SetOperationOutput>, SetOperationLayoutError> {
    match mode {
        AlignmentMode::Strict => {
            if left_index.len() != right_index.len()
                || left_index.keys().any(|key| !right_index.contains_key(key))
            {
                return Err(SetOperationLayoutError::StrictColumnMismatch);
            }
            Ok(left
                .iter()
                .enumerate()
                .map(|(left_ordinal, identifier)| SetOperationOutput {
                    identifier: identifier.clone(),
                    left_ordinal: Some(left_ordinal),
                    right_ordinal: right_index
                        .get(&identifier_key(identifier, dialect))
                        .copied(),
                })
                .collect())
        }
        AlignmentMode::Inner => Ok(left
            .iter()
            .enumerate()
            .filter_map(|(left_ordinal, identifier)| {
                right_index
                    .get(&identifier_key(identifier, dialect))
                    .copied()
                    .map(|right_ordinal| SetOperationOutput {
                        identifier: identifier.clone(),
                        left_ordinal: Some(left_ordinal),
                        right_ordinal: Some(right_ordinal),
                    })
            })
            .collect()),
        AlignmentMode::Left => Ok(left
            .iter()
            .enumerate()
            .map(|(left_ordinal, identifier)| SetOperationOutput {
                identifier: identifier.clone(),
                left_ordinal: Some(left_ordinal),
                right_ordinal: right_index
                    .get(&identifier_key(identifier, dialect))
                    .copied(),
            })
            .collect()),
        AlignmentMode::Full => {
            let mut outputs: Vec<_> = left
                .iter()
                .enumerate()
                .map(|(left_ordinal, identifier)| SetOperationOutput {
                    identifier: identifier.clone(),
                    left_ordinal: Some(left_ordinal),
                    right_ordinal: right_index
                        .get(&identifier_key(identifier, dialect))
                        .copied(),
                })
                .collect();
            for (right_ordinal, identifier) in right.iter().enumerate() {
                if !left_index.contains_key(&identifier_key(identifier, dialect)) {
                    outputs.push(SetOperationOutput {
                        identifier: identifier.clone(),
                        left_ordinal: None,
                        right_ordinal: Some(right_ordinal),
                    });
                }
            }
            Ok(outputs)
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn layout_with_explicit_columns(
    mode: AlignmentMode,
    columns: &[Expression],
    left: &[Identifier],
    right: &[Identifier],
    left_index: &HashMap<String, usize>,
    right_index: &HashMap<String, usize>,
    dialect: Option<DialectType>,
) -> Result<Vec<SetOperationOutput>, SetOperationLayoutError> {
    let mut outputs = Vec::new();
    let mut seen = HashSet::new();

    for column in columns {
        let identifier = expression_identifier(column).ok_or_else(|| {
            SetOperationLayoutError::Indeterminate(
                "BY NAME ON columns must be statically named".to_string(),
            )
        })?;
        let key = identifier_key(&identifier, dialect);
        if !seen.insert(key.clone()) {
            return Err(SetOperationLayoutError::DuplicateColumn(identifier.name));
        }

        let left_ordinal = left_index.get(&key).copied();
        let right_ordinal = right_index.get(&key).copied();
        let include = match mode {
            AlignmentMode::Strict => left_ordinal.is_some() && right_ordinal.is_some(),
            AlignmentMode::Inner => left_ordinal.is_some() && right_ordinal.is_some(),
            AlignmentMode::Left => left_ordinal.is_some(),
            AlignmentMode::Full => left_ordinal.is_some() || right_ordinal.is_some(),
        };

        if !include {
            if mode == AlignmentMode::Inner {
                continue;
            }
            return Err(SetOperationLayoutError::MissingRequiredColumn(
                identifier.name,
            ));
        }

        let result_identifier = left_ordinal
            .and_then(|ordinal| left.get(ordinal))
            .or_else(|| right_ordinal.and_then(|ordinal| right.get(ordinal)))
            .cloned()
            .unwrap_or(identifier);
        outputs.push(SetOperationOutput {
            identifier: result_identifier,
            left_ordinal,
            right_ordinal,
        });
    }

    Ok(outputs)
}

fn identifier_index(
    identifiers: &[Identifier],
    dialect: Option<DialectType>,
) -> Result<HashMap<String, usize>, SetOperationLayoutError> {
    let mut index = HashMap::new();
    for (ordinal, identifier) in identifiers.iter().enumerate() {
        let key = identifier_key(identifier, dialect);
        if index.insert(key, ordinal).is_some() {
            return Err(SetOperationLayoutError::DuplicateColumn(
                identifier.name.clone(),
            ));
        }
    }
    Ok(index)
}

fn collect_projection_identifiers(
    expression: &Expression,
    identifiers: &mut Vec<Identifier>,
) -> Result<(), SetOperationLayoutError> {
    match expression {
        Expression::Aliases(aliases) if !aliases.expressions.is_empty() => {
            for alias in &aliases.expressions {
                identifiers.push(expression_identifier(alias).ok_or_else(|| {
                    SetOperationLayoutError::Indeterminate(
                        "query output alias cannot be determined statically".to_string(),
                    )
                })?);
            }
            Ok(())
        }
        Expression::Annotated(annotated) => {
            collect_projection_identifiers(&annotated.this, identifiers)
        }
        Expression::Star(_) => Err(SetOperationLayoutError::Indeterminate(
            "name-aligned set operation contains an unresolved wildcard".to_string(),
        )),
        Expression::Column(column) if column.name.name == "*" => {
            Err(SetOperationLayoutError::Indeterminate(
                "name-aligned set operation contains an unresolved wildcard".to_string(),
            ))
        }
        _ => {
            identifiers.push(expression_identifier(expression).ok_or_else(|| {
                SetOperationLayoutError::Indeterminate(
                    "name-aligned set operation contains an unnamed output".to_string(),
                )
            })?);
            Ok(())
        }
    }
}

fn expression_identifier(expression: &Expression) -> Option<Identifier> {
    match expression {
        Expression::Alias(alias) => Some(alias.alias.clone()),
        Expression::Column(column) => Some(column.name.clone()),
        Expression::Identifier(identifier) => Some(identifier.clone()),
        Expression::Annotated(annotated) => expression_identifier(&annotated.this),
        _ => None,
    }
}

fn apply_column_aliases(
    mut identifiers: Vec<Identifier>,
    aliases: &[Identifier],
) -> Vec<Identifier> {
    for (ordinal, alias) in aliases.iter().enumerate() {
        if let Some(identifier) = identifiers.get_mut(ordinal) {
            *identifier = alias.clone();
        }
    }
    identifiers
}
