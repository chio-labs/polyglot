//! Optional observations from reference validation. This module indexes identities;
//! decisions about visibility and resolution remain in the validation pass.

use crate::expressions::{Column, Expression};
use crate::schema::Schema;
use crate::scope::{Scope, SourceKind};
use crate::tokens::Span;
use crate::{DialectType, ExpressionWalk, MappingSchema, Resolver, ValidationResult};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[cfg(test)]
thread_local! { pub(super) static OBSERVERS_CREATED: std::cell::Cell<usize> = const { std::cell::Cell::new(0) }; }

/// Identity of a lexical query scope in one extraction result.
pub type BindingScopeId = usize;

/// An output position, independent of its physical-column lineage.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OutputSlotIdentity {
    pub scope_id: BindingScopeId,
    /// Position in the interface, where each unknown-width expansion is one entry.
    pub ordinal: usize,
    /// Physical result position, absent after an unknown-width expansion.
    pub physical_ordinal: Option<usize>,
    pub name: String,
}

/// Ordered outputs. An open entry explicitly denotes an unknown-width expansion.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum BindingOutput {
    Slot { slot: OutputSlotIdentity },
    Open { start_ordinal: usize },
}

/// A query scope and its ordered output interface.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BindingScopeFact {
    pub id: BindingScopeId,
    pub parent: Option<BindingScopeId>,
    pub path: String,
    pub kind: String,
    pub name: Option<String>,
    pub outputs: Vec<BindingOutput>,
    /// At least one output expansion has unknown width. Named outputs remain checked.
    pub partially_checked: bool,
}

/// The binding chosen by the validator for a reference occurrence.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum OccurrenceBinding {
    /// Some grammar nodes use Column syntax to spell a relation, not a value.
    RelationName {
        name: String,
    },
    /// An option-assignment key represented by Column syntax in the parser AST.
    OptionName {
        name: String,
    },
    /// Incomplete interfaces leave several possible inputs; no unique binding is asserted.
    Partial {
        candidates: Vec<OccurrenceBinding>,
    },
    /// A named read from a relation whose complete column interface is unknown.
    OpenSourceColumn {
        scope_id: BindingScopeId,
        source: String,
        column: String,
    },
    OutputSlot {
        slot: OutputSlotIdentity,
    },
    SourceColumn {
        scope_id: BindingScopeId,
        source: String,
        relation: Option<String>,
        column: String,
        source_kind: SourceKind,
    },
    Merged {
        inputs: Vec<OccurrenceBinding>,
    },
    LambdaParameter {
        lambda_id: usize,
        declaration: Option<Span>,
        name: String,
    },
    Pseudocolumn {
        name: String,
    },
    Open {
        sources: Vec<String>,
    },
    Unresolved {
        reason: String,
    },
}

/// One column occurrence, including unresolved and open-schema occurrences.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BindingOccurrence {
    pub span: Option<Span>,
    pub clause: String,
    pub scope_id: BindingScopeId,
    pub name: String,
    pub binding: OccurrenceBinding,
}

/// Opt-in validation result with binding observations.
#[derive(Debug, Serialize, Deserialize)]
pub struct ValidationBindingFacts {
    pub validation: ValidationResult,
    pub scopes: Vec<BindingScopeFact>,
    pub occurrences: Vec<BindingOccurrence>,
}

pub(super) struct BindingObserver {
    pub scopes: Vec<BindingScopeFact>,
    pub occurrences: Vec<BindingOccurrence>,
    identities: HashMap<usize, BindingScopeId>,
    sources: HashMap<(BindingScopeId, String), BindingScopeId>,
    declarations: Vec<(Expression, BindingScopeId)>,
    paths: HashMap<String, BindingScopeId>,
    occurrence_indexes: HashMap<(usize, usize), usize>,
    statement_root: usize,
    merged_columns: HashMap<(usize, String), OccurrenceBinding>,
    pub(super) source_interfaces: HashMap<usize, Vec<String>>,
    raw_outputs: HashMap<usize, Vec<String>>,
    query_roots: usize,
}

impl BindingObserver {
    pub fn new() -> Self {
        #[cfg(test)]
        OBSERVERS_CREATED.with(|count| count.set(count.get() + 1));
        Self {
            scopes: Vec::new(),
            occurrences: Vec::new(),
            identities: HashMap::new(),
            sources: HashMap::new(),
            declarations: Vec::new(),
            paths: HashMap::new(),
            occurrence_indexes: HashMap::new(),
            statement_root: 0,
            merged_columns: HashMap::new(),
            source_interfaces: HashMap::new(),
            raw_outputs: HashMap::new(),
            query_roots: 0,
        }
    }

    pub fn prepare(&mut self, statement: &Expression, schema: &MappingSchema) {
        self.statement_root = self.scopes.len();
        self.query_roots = 0;
        self.occurrence_indexes.clear();
        let scope = crate::scope::build_scope_for_binding_facts(statement);
        self.register(&scope, schema);
        for node in statement.dfs() {
            if let Expression::Column(column) = node {
                if let Some(span) = column.span.or(column.name.span) {
                    if !self
                        .occurrence_indexes
                        .contains_key(&(span.start, span.end))
                    {
                        self.occurrence_indexes
                            .insert((span.start, span.end), self.occurrences.len());
                        self.occurrences.push(BindingOccurrence {
                            span: Some(span),
                            clause: "expression".to_owned(),
                            scope_id: self.statement_root,
                            name: column.name.name.clone(),
                            binding: OccurrenceBinding::Unresolved {
                                reason: "occurrence was not resolved by reference validation"
                                    .to_owned(),
                            },
                        });
                    }
                }
            }
        }
        self.query_roots = 0;
    }

    fn collect_occurrences(&mut self, scope: &Scope) {
        let scope_id = self.identities[&(scope as *const Scope as usize)];
        for node in complete_scope_nodes(crate::scope::scope_query(&scope.expression)) {
            if let Expression::Identifier(identifier) = node {
                if let Some(index) = identifier
                    .span
                    .and_then(|span| self.occurrence_indexes.get(&(span.start, span.end)))
                {
                    self.occurrences[*index].scope_id = scope_id;
                }
            }
            if let Expression::Column(column) = node {
                let span = column.span.or(column.name.span);
                if span.is_none() {
                    continue;
                }
                if let Some(span) = span {
                    if let Some(index) = self.occurrence_indexes.get(&(span.start, span.end)) {
                        self.occurrences[*index].scope_id = scope_id;
                        continue;
                    }
                    self.occurrence_indexes
                        .insert((span.start, span.end), self.occurrences.len());
                }
                self.occurrences.push(BindingOccurrence {
                    span,
                    clause: "expression".to_owned(),
                    scope_id,
                    name: column.name.name.clone(),
                    binding: OccurrenceBinding::Unresolved {
                        reason: "occurrence was not resolved by reference validation".to_owned(),
                    },
                });
            }
        }
        self.label_clauses(crate::scope::scope_query(&scope.expression));
        for child in scope
            .cte_scopes
            .iter()
            .chain(&scope.derived_table_scopes)
            .chain(&scope.subquery_scopes)
            .chain(&scope.udtf_scopes)
            .chain(&scope.union_scopes)
        {
            self.collect_occurrences(child);
        }
    }

    fn label_clauses(&mut self, expression: &Expression) {
        let mut pending = vec![(expression, "projection")];
        while let Some((node, clause)) = pending.pop() {
            if let Expression::Identifier(identifier) = node {
                if let Some(index) = identifier
                    .span
                    .and_then(|span| self.occurrence_indexes.get(&(span.start, span.end)))
                {
                    self.occurrences[*index].clause = clause.to_owned();
                }
            }
            if let Expression::Column(column) = node {
                if let Some(index) = column
                    .span
                    .or(column.name.span)
                    .and_then(|span| self.occurrence_indexes.get(&(span.start, span.end)))
                {
                    self.occurrences[*index].clause = clause.to_owned();
                }
            }
            crate::ast_children::for_each_child(node, |path, child| {
                if path.contains(&crate::ast_children::ChildPathSegment::Field("ctes"))
                    || matches!(
                        child,
                        Expression::Select(_)
                            | Expression::Subquery(_)
                            | Expression::Union(_)
                            | Expression::Intersect(_)
                            | Expression::Except(_)
                            | Expression::Cte(_)
                    )
                {
                    return;
                }
                let mut next = clause;
                for field in path {
                    if let crate::ast_children::ChildPathSegment::Field(field) = field {
                        next = match *field {
                            "where_clause" => "where",
                            "on" | "match_condition" => "join_on",
                            "group_by" => "group_by",
                            "having" => "having",
                            "qualify" => "qualify",
                            "windows" => "window",
                            "filter" => "aggregate_filter",
                            "order_by"
                                if next.starts_with("window")
                                    || matches!(node, Expression::WindowFunction(_)) =>
                            {
                                "window_order"
                            }
                            "order_by" => "order_by",
                            "partition_by" => "window_partition",
                            "frame" => "window_frame",
                            "connect" => "connect_by",
                            _ => next,
                        };
                    }
                }
                pending.push((child, next));
            });
        }
    }

    pub fn record_lambda(
        &mut self,
        column: &Column,
        lambda_id: usize,
        declaration: &crate::expressions::Identifier,
    ) {
        if let Some(index) = column
            .span
            .or(column.name.span)
            .and_then(|span| self.occurrence_indexes.get(&(span.start, span.end)))
            .copied()
        {
            self.occurrences[index].binding = OccurrenceBinding::LambdaParameter {
                lambda_id,
                declaration: declaration.span,
                name: declaration.name.clone(),
            };
            if let Some(span) = column.table.as_ref().unwrap_or(&column.name).span {
                self.occurrence_indexes
                    .insert((span.start, span.end), index);
            }
        }
    }

    pub fn record_standalone(&mut self, column: &Column) {
        if let Some(index) = column
            .span
            .or(column.name.span)
            .and_then(|span| self.occurrence_indexes.get(&(span.start, span.end)))
        {
            self.occurrences[*index].binding = OccurrenceBinding::Open {
                sources: Vec::new(),
            };
        }
    }

    pub fn record_alias(&mut self, column: &Column, dialect: DialectType) {
        let Some(index) = column
            .span
            .or(column.name.span)
            .and_then(|span| self.occurrence_indexes.get(&(span.start, span.end)))
            .copied()
        else {
            return;
        };
        let id = self.occurrences[index].scope_id;
        let strategy =
            crate::optimizer::normalize_identifiers::get_normalization_strategy(Some(dialect));
        let name = crate::optimizer::normalize_identifiers::normalize_identifier(
            column.name.clone(),
            strategy,
        )
        .name;
        let Some((expression, _)) = self.declarations.iter().find(|(_, target)| *target == id)
        else {
            return;
        };
        let Expression::Select(select) = crate::scope::scope_query(expression) else {
            return;
        };
        for (ordinal, projection) in select.expressions.iter().enumerate() {
            let Expression::Alias(alias) = projection else {
                continue;
            };
            if crate::optimizer::normalize_identifiers::normalize_identifier(
                alias.alias.clone(),
                strategy,
            )
            .name
                == name
            {
                if let Some(BindingOutput::Slot { slot }) = self.scopes[id].outputs.get(ordinal) {
                    self.occurrences[index].binding =
                        OccurrenceBinding::OutputSlot { slot: slot.clone() };
                }
                return;
            }
        }
    }

    pub fn register(&mut self, scope: &Scope, schema: &MappingSchema) {
        let root = self.statement_root;
        let path = if self.query_roots == 0 {
            format!("statement[{root}]")
        } else {
            format!("statement[{root}].queries[{}]", self.query_roots)
        };
        self.query_roots += 1;
        self.register_tree(scope, schema, None, path);
        if self.query_roots == 1 {
            self.source_interfaces.clear();
        }
        self.compute_interfaces(scope, schema);
        self.register_sources(scope, schema);
        self.collect_occurrences(scope);
    }

    fn compute_interfaces(&mut self, scope: &Scope, schema: &MappingSchema) {
        for child in scope
            .cte_scopes
            .iter()
            .chain(&scope.derived_table_scopes)
            .chain(&scope.subquery_scopes)
            .chain(&scope.udtf_scopes)
            .chain(&scope.union_scopes)
        {
            self.compute_interfaces(child, schema);
            let child_id = self.identities[&(child as *const Scope as usize)];
            for source in scope.sources.values().chain(scope.cte_sources.values()) {
                let matches = match (source.expression.as_ref(), &child.expression) {
                    (Expression::Subquery(source), Expression::Subquery(target)) => {
                        source == target
                            && span_signature(&source.this) == span_signature(&target.this)
                    }
                    (Expression::Cte(source), Expression::Cte(target)) => {
                        source.alias.span == target.alias.span && source.alias == target.alias
                    }
                    (Expression::Subquery(source), target) => {
                        source.this == *target
                            && span_signature(&source.this) == span_signature(target)
                    }
                    (Expression::Alias(source), target) => {
                        source.this == *target
                            && span_signature(&source.this) == span_signature(target)
                    }
                    _ => false,
                };
                if !matches {
                    continue;
                }
                let mut columns = self.raw_outputs[&child_id].clone();
                let aliases = match source.expression.as_ref() {
                    Expression::Cte(cte) => cte.columns.as_slice(),
                    Expression::Subquery(query) => query.column_aliases.as_slice(),
                    Expression::Alias(alias) => alias.column_aliases.as_slice(),
                    _ => &[],
                };
                for (column, alias) in columns.iter_mut().zip(aliases) {
                    *column = alias.name.clone();
                }
                self.source_interfaces.insert(
                    std::sync::Arc::as_ptr(&source.expression) as usize,
                    columns.clone(),
                );
                if !aliases.is_empty() && source.kind != SourceKind::Cte {
                    self.set_outputs(child_id, &columns, &source.expression, schema);
                }
            }
        }
        let id = self.identities[&(scope as *const Scope as usize)];
        let resolver =
            Resolver::new(scope, schema, true).with_source_interfaces(&self.source_interfaces);
        let mut columns =
            resolver.get_source_output_columns(crate::scope::scope_query(&scope.expression));
        if let Expression::Cte(cte) = &scope.expression {
            for (column, alias) in columns.iter_mut().zip(&cte.columns) {
                *column = alias.name.clone();
            }
        }
        self.set_outputs(id, &columns, &scope.expression, schema);
        self.raw_outputs.insert(id, columns);
    }

    fn set_outputs(
        &mut self,
        id: usize,
        names: &[String],
        expression: &Expression,
        schema: &MappingSchema,
    ) {
        let identifiers = super::source_output_identifiers(expression);
        let strategy =
            crate::optimizer::normalize_identifiers::get_normalization_strategy(schema.dialect());
        let mut outputs = Vec::new();
        for (ordinal, name) in names.iter().enumerate() {
            if name == "*" {
                outputs.push(BindingOutput::Open {
                    start_ordinal: ordinal,
                });
                continue;
            }
            let identifier = identifiers
                .iter()
                .find(|identifier| identifier.name == *name)
                .copied()
                .cloned()
                .unwrap_or_else(|| crate::binding::schema_identifier(name));
            outputs.push(BindingOutput::Slot {
                slot: OutputSlotIdentity {
                    scope_id: id,
                    ordinal,
                    physical_ordinal: if outputs
                        .iter()
                        .any(|output| matches!(output, BindingOutput::Open { .. }))
                    {
                        None
                    } else {
                        Some(ordinal)
                    },
                    name: crate::optimizer::normalize_identifiers::normalize_identifier(
                        identifier, strategy,
                    )
                    .name,
                },
            });
        }
        if outputs.is_empty() {
            outputs.push(BindingOutput::Open { start_ordinal: 0 });
        }
        self.scopes[id].outputs = outputs;
        self.scopes[id].partially_checked = self.scopes[id]
            .outputs
            .iter()
            .any(|output| matches!(output, BindingOutput::Open { .. }));
    }

    fn register_tree(
        &mut self,
        scope: &Scope,
        schema: &MappingSchema,
        parent: Option<usize>,
        path: String,
    ) {
        let id = self.paths.get(&path).copied().unwrap_or(self.scopes.len());
        self.paths.insert(path.clone(), id);
        self.identities.insert(scope as *const Scope as usize, id);
        if !self
            .declarations
            .iter()
            .any(|(expression, target)| *target == id && *expression == scope.expression)
        {
            self.declarations.push((scope.expression.clone(), id));
        }
        let resolver = Resolver::new(scope, schema, true);
        let names = resolver.get_source_output_columns(&scope.expression);
        let identifiers = super::source_output_identifiers(&scope.expression);
        let strategy =
            crate::optimizer::normalize_identifiers::get_normalization_strategy(schema.dialect());
        let mut outputs = Vec::new();
        for (ordinal, name) in names.into_iter().enumerate() {
            if name == "*" {
                outputs.push(BindingOutput::Open {
                    start_ordinal: ordinal,
                });
                continue;
            }
            outputs.push(BindingOutput::Slot {
                slot: OutputSlotIdentity {
                    scope_id: id,
                    ordinal,
                    physical_ordinal: if outputs
                        .iter()
                        .any(|output| matches!(output, BindingOutput::Open { .. }))
                    {
                        None
                    } else {
                        Some(ordinal)
                    },
                    name: crate::optimizer::normalize_identifiers::normalize_identifier(
                        identifiers
                            .iter()
                            .find(|identifier| identifier.name == name)
                            .copied()
                            .cloned()
                            .unwrap_or_else(|| crate::binding::schema_identifier(&name)),
                        strategy,
                    )
                    .name,
                },
            });
        }
        if outputs.is_empty() {
            outputs.push(BindingOutput::Open { start_ordinal: 0 });
        }
        let name = match &scope.expression {
            Expression::Cte(cte) => Some(cte.alias.name.clone()),
            _ => None,
        };
        let fact = BindingScopeFact {
            id,
            parent,
            path: path.clone(),
            kind: format!("{:?}", scope.scope_type),
            name,
            partially_checked: outputs
                .iter()
                .any(|output| matches!(output, BindingOutput::Open { .. })),
            outputs,
        };
        if id == self.scopes.len() {
            self.scopes.push(fact);
        } else {
            self.scopes[id] = fact;
        }
        for (label, children) in [
            ("ctes", &scope.cte_scopes),
            ("derived", &scope.derived_table_scopes),
            ("subqueries", &scope.subquery_scopes),
            ("lateral", &scope.udtf_scopes),
            ("branches", &scope.union_scopes),
        ] {
            for (index, child) in children.iter().enumerate() {
                self.register_tree(child, schema, Some(id), format!("{path}.{label}[{index}]"));
            }
        }
    }

    fn register_sources(&mut self, scope: &Scope, schema: &MappingSchema) {
        let id = self.identities[&(scope as *const Scope as usize)];
        let mut resolver =
            Resolver::new(scope, schema, true).with_source_interfaces(&self.source_interfaces);
        for (name, source) in &scope.sources {
            let mut matches: Vec<_> = self
                .declarations
                .iter()
                .filter(|(_, target)| *target >= self.statement_root)
                .filter(
                    |(expression, target_id)| match (source.expression.as_ref(), expression) {
                        (Expression::Subquery(source), Expression::Subquery(target)) => {
                            source == target
                                && span_signature(&source.this) == span_signature(&target.this)
                                && self.scopes[*target_id].parent == Some(id)
                        }
                        (Expression::Cte(source), Expression::Cte(target)) => {
                            source.alias.span == target.alias.span && source.alias == target.alias
                        }
                        (Expression::Subquery(source), target) => {
                            source.this == *target
                                && span_signature(&source.this) == span_signature(target)
                                && self.scopes[*target_id].parent == Some(id)
                        }
                        (Expression::Alias(source), target) => {
                            source.this == *target
                                && span_signature(&source.this) == span_signature(target)
                                && self.scopes[*target_id].parent == Some(id)
                        }
                        (source, target) => source == target,
                    },
                )
                .map(|(_, target)| *target)
                .collect();
            matches.sort_unstable();
            matches.dedup();
            if let [target] = matches.as_slice() {
                self.sources.insert((id, name.clone()), *target);
                if source.kind == SourceKind::DerivedTable {
                    let identifiers = super::source_output_identifiers(&source.expression);
                    let strategy =
                        crate::optimizer::normalize_identifiers::get_normalization_strategy(
                            schema.dialect(),
                        );
                    let columns = resolver.get_source_columns(name).unwrap_or_default();
                    let mut outputs = Vec::new();
                    for (ordinal, column) in columns.into_iter().enumerate() {
                        if column == "*" {
                            outputs.push(BindingOutput::Open {
                                start_ordinal: ordinal,
                            });
                            continue;
                        }
                        let identifier = identifiers
                            .iter()
                            .find(|identifier| identifier.name == column)
                            .copied()
                            .cloned()
                            .unwrap_or_else(|| crate::binding::schema_identifier(&column));
                        outputs.push(BindingOutput::Slot {
                            slot: OutputSlotIdentity {
                                scope_id: *target,
                                ordinal,
                                physical_ordinal: if outputs
                                    .iter()
                                    .any(|output| matches!(output, BindingOutput::Open { .. }))
                                {
                                    None
                                } else {
                                    Some(ordinal)
                                },
                                name:
                                    crate::optimizer::normalize_identifiers::normalize_identifier(
                                        identifier, strategy,
                                    )
                                    .name,
                            },
                        });
                    }
                    if outputs.is_empty() {
                        outputs.push(BindingOutput::Open { start_ordinal: 0 });
                    }
                    self.scopes[*target].partially_checked = outputs
                        .iter()
                        .any(|output| matches!(output, BindingOutput::Open { .. }));
                    self.scopes[*target].outputs = outputs;
                }
            }
        }
        for child in scope
            .cte_scopes
            .iter()
            .chain(&scope.derived_table_scopes)
            .chain(&scope.subquery_scopes)
            .chain(&scope.udtf_scopes)
            .chain(&scope.union_scopes)
        {
            self.register_sources(child, schema);
        }
    }

    pub fn alias_scope(&mut self, original: &Scope, selected: &Scope) {
        if let Some(id) = self
            .identities
            .get(&(original as *const Scope as usize))
            .copied()
        {
            self.identities
                .insert(selected as *const Scope as usize, id);
        }
    }

    pub fn output_projection_names(&self, scope: &Scope) -> Vec<crate::expressions::Identifier> {
        let Some(id) = self.identities.get(&(scope as *const Scope as usize)) else {
            return Vec::new();
        };
        self.scopes[*id]
            .outputs
            .iter()
            .filter_map(|output| match output {
                BindingOutput::Slot { slot } => {
                    Some(crate::expressions::Identifier::quoted(&slot.name))
                }
                _ => None,
            })
            .collect()
    }

    pub fn source_binding(
        &self,
        scope: &Scope,
        source: &str,
        column: &Column,
        dialect: DialectType,
    ) -> OccurrenceBinding {
        let id = self
            .identities
            .get(&(scope as *const Scope as usize))
            .copied();
        if let Some(target) = id.and_then(|id| self.sources.get(&(id, source.to_owned()))) {
            let strategy =
                crate::optimizer::normalize_identifiers::get_normalization_strategy(Some(dialect));
            let name = crate::optimizer::normalize_identifiers::normalize_identifier(
                column.name.clone(),
                strategy,
            )
            .name;
            let matches: Vec<_> = self.scopes[*target]
                .outputs
                .iter()
                .filter_map(|output| match output {
                    BindingOutput::Slot { slot } if slot.name == name => Some(slot),
                    _ => None,
                })
                .collect();
            if let Some(source_info) = scope.sources.get(source) {
                if let Expression::Cte(cte) = source_info.expression.as_ref() {
                    let alias_ordinals: Vec<_> = cte
                        .columns
                        .iter()
                        .enumerate()
                        .filter(|(_, alias)| {
                            crate::optimizer::normalize_identifiers::normalize_identifier(
                                (*alias).clone(),
                                strategy,
                            )
                            .name
                                == name
                        })
                        .map(|(index, _)| index)
                        .collect();
                    if let [ordinal] = alias_ordinals.as_slice() {
                        if let Some(slot) =
                            self.scopes[*target]
                                .outputs
                                .iter()
                                .find_map(|output| match output {
                                    BindingOutput::Slot { slot }
                                        if slot.physical_ordinal == Some(*ordinal) =>
                                    {
                                        Some(slot)
                                    }
                                    _ => None,
                                })
                        {
                            return OccurrenceBinding::OutputSlot { slot: slot.clone() };
                        }
                        if self.scopes[*target].partially_checked {
                            return OccurrenceBinding::OpenSourceColumn {
                                scope_id: id.unwrap_or(0),
                                source: source.to_owned(),
                                column: column.name.name.clone(),
                            };
                        }
                    } else if alias_ordinals.len() > 1 {
                        return OccurrenceBinding::Unresolved {
                            reason: "ambiguous relation column alias".to_owned(),
                        };
                    }
                }
            }
            if let [slot] = matches.as_slice() {
                return OccurrenceBinding::OutputSlot {
                    slot: (*slot).clone(),
                };
            }
            if matches.is_empty() && self.scopes[*target].partially_checked {
                return OccurrenceBinding::OpenSourceColumn {
                    scope_id: id.unwrap_or(0),
                    source: source.to_owned(),
                    column: column.name.name.clone(),
                };
            }
            return OccurrenceBinding::Unresolved {
                reason: "resolved source has no unique output ordinal".to_owned(),
            };
        }
        if scope
            .sources
            .get(source)
            .is_some_and(|source| source.kind == SourceKind::Cte)
        {
            return OccurrenceBinding::Unresolved {
                reason: "CTE declaration identity is unavailable".to_owned(),
            };
        }
        let relation =
            scope
                .sources
                .get(source)
                .and_then(|source| match source.expression.as_ref() {
                    Expression::Table(table) => Some(
                        table
                            .catalog
                            .iter()
                            .chain(table.schema.iter())
                            .map(|identifier| identifier.name.as_str())
                            .chain(std::iter::once(table.name.name.as_str()))
                            .collect::<Vec<_>>()
                            .join("."),
                    ),
                    _ => None,
                });
        OccurrenceBinding::SourceColumn {
            scope_id: id.unwrap_or(0),
            source: source.to_owned(),
            relation,
            column: column.name.name.clone(),
            source_kind: scope
                .sources
                .get(source)
                .map_or(SourceKind::Unknown, |source| source.kind),
        }
    }

    pub fn open_source_binding(
        &self,
        scope: &Scope,
        source: &str,
        column: &Column,
        dialect: DialectType,
    ) -> OccurrenceBinding {
        match self.source_binding(scope, source, column, dialect) {
            OccurrenceBinding::SourceColumn {
                scope_id,
                source,
                column,
                ..
            } => OccurrenceBinding::OpenSourceColumn {
                scope_id,
                source,
                column,
            },
            binding => binding,
        }
    }

    pub fn output_binding(
        &self,
        scope: &Scope,
        column: &Column,
        dialect: DialectType,
    ) -> OccurrenceBinding {
        let Some(id) = self
            .identities
            .get(&(scope as *const Scope as usize))
            .copied()
        else {
            return OccurrenceBinding::Unresolved {
                reason: "output scope is unavailable".to_owned(),
            };
        };
        let strategy =
            crate::optimizer::normalize_identifiers::get_normalization_strategy(Some(dialect));
        let name = crate::optimizer::normalize_identifiers::normalize_identifier(
            column.name.clone(),
            strategy,
        )
        .name;
        let Expression::Select(select) = crate::scope::scope_query(&scope.expression) else {
            return OccurrenceBinding::Unresolved {
                reason: "output is not a select scope".to_owned(),
            };
        };
        for (ordinal, projection) in select.expressions.iter().enumerate() {
            let identifier = match projection {
                Expression::Alias(alias) => &alias.alias,
                Expression::Column(column) => &column.name,
                _ => continue,
            };
            if crate::optimizer::normalize_identifiers::normalize_identifier(
                identifier.clone(),
                strategy,
            )
            .name
                == name
            {
                if let Some(BindingOutput::Slot { slot }) = self.scopes[id].outputs.get(ordinal) {
                    return OccurrenceBinding::OutputSlot { slot: slot.clone() };
                }
            }
        }
        OccurrenceBinding::Open {
            sources: Vec::new(),
        }
    }

    pub fn observe_stars(&mut self, scope: &Scope, schema: &MappingSchema, dialect: DialectType) {
        let Expression::Select(select) = crate::scope::scope_query(&scope.expression) else {
            return;
        };
        for projection in &select.expressions {
            let star = match crate::scope::scope_query(projection) {
                Expression::Star(star) => star.clone(),
                Expression::Column(column) if column.name.name == "*" => crate::expressions::Star {
                    table: column.table.clone(),
                    except: None,
                    replace: None,
                    rename: None,
                    trailing_comments: Vec::new(),
                    span: column.span,
                },
                _ => continue,
            };
            let mut resolver =
                Resolver::new(scope, schema, true).with_source_interfaces(&self.source_interfaces);
            let mut sources: Vec<_> = scope
                .sources
                .keys()
                .filter(|source| {
                    star.table.as_ref().is_none_or(|table| {
                        super::resolve_scope_source_name(scope, &table.name).as_ref()
                            == Some(*source)
                    })
                })
                .collect();
            sources.sort();
            let mut inputs = Vec::new();
            for source in sources {
                let columns = resolver.get_source_columns(source).unwrap_or_default();
                if columns.is_empty() || columns.iter().any(|name| name == "*") {
                    inputs.push(OccurrenceBinding::Open {
                        sources: vec![source.clone()],
                    });
                }
                let identifiers =
                    super::source_output_identifiers(&scope.sources[source].expression);
                for name in columns {
                    if name == "*" {
                        continue;
                    }
                    let identifier = identifiers
                        .iter()
                        .find(|identifier| identifier.name == name)
                        .copied()
                        .cloned()
                        .unwrap_or_else(|| crate::binding::schema_identifier(&name));
                    if star
                        .except
                        .as_ref()
                        .is_some_and(|columns| columns.contains(&identifier))
                    {
                        continue;
                    }
                    let column = Column {
                        name: identifier,
                        table: None,
                        join_mark: false,
                        trailing_comments: Vec::new(),
                        span: star.span,
                        inferred_type: None,
                    };
                    inputs.push(self.source_binding(scope, source, &column, dialect));
                }
            }
            let scope_id = self
                .identities
                .get(&(scope as *const Scope as usize))
                .copied()
                .unwrap_or(0);
            if inputs.is_empty() {
                inputs.push(OccurrenceBinding::Open {
                    sources: Vec::new(),
                });
            }
            if let Some(index) = star
                .span
                .and_then(|span| self.occurrence_indexes.get(&(span.start, span.end)))
            {
                self.occurrences[*index].binding = OccurrenceBinding::Merged { inputs };
                continue;
            }
            self.occurrences.push(BindingOccurrence {
                span: star.span,
                clause: "projection".to_owned(),
                scope_id,
                name: "*".to_owned(),
                binding: OccurrenceBinding::Merged { inputs },
            });
        }
    }

    pub fn observe_using(&mut self, scope: &Scope, schema: &MappingSchema, dialect: DialectType) {
        let Expression::Select(select) = crate::scope::scope_query(&scope.expression) else {
            return;
        };
        if select.joins.is_empty() {
            return;
        }
        let scope_id = self
            .identities
            .get(&(scope as *const Scope as usize))
            .copied()
            .unwrap_or(0);
        let mut expanded = select.clone();
        let mut resolver =
            Resolver::new(scope, schema, true).with_source_interfaces(&self.source_interfaces);
        let Ok(merged) =
            crate::optimizer::qualify_columns::expand_using(&mut expanded, scope, &mut resolver)
        else {
            return;
        };
        let mut merged: Vec<_> = merged.into_iter().collect();
        merged.sort_by(|left, right| left.0.cmp(&right.0));
        for (name, sources) in merged {
            let authored = select
                .joins
                .iter()
                .flat_map(|join| &join.using)
                .find(|identifier| {
                    crate::schema::normalize_name(
                        &identifier.to_type_field_name(),
                        Some(dialect),
                        false,
                        true,
                    ) == name
                });
            let identifier = authored
                .cloned()
                .unwrap_or_else(|| crate::binding::schema_identifier(&name));
            let column = Column {
                name: identifier.clone(),
                table: None,
                join_mark: false,
                trailing_comments: Vec::new(),
                span: identifier.span,
                inferred_type: None,
            };
            let inputs = sources
                .iter()
                .map(|source| self.source_binding(scope, source, &column, dialect))
                .collect();
            let binding = OccurrenceBinding::Merged { inputs };
            self.merged_columns
                .insert((scope_id, name.clone()), binding.clone());
            self.occurrences.push(BindingOccurrence {
                span: identifier.span,
                clause: if authored.is_some() {
                    "join_using"
                } else {
                    "natural_join"
                }
                .to_owned(),
                scope_id,
                name,
                binding,
            });
        }
    }

    pub fn merged_binding(
        &self,
        scope: &Scope,
        column: &Column,
        dialect: DialectType,
    ) -> Option<OccurrenceBinding> {
        let id = self.identities.get(&(scope as *const Scope as usize))?;
        let name = crate::schema::normalize_name(
            &column.name.to_type_field_name(),
            Some(dialect),
            false,
            true,
        );
        self.merged_columns.get(&(*id, name)).cloned()
    }

    pub fn record(&mut self, scope: &Scope, column: &Column, binding: OccurrenceBinding) {
        let span = column.span.or(column.name.span);
        let index = span
            .and_then(|span| self.occurrence_indexes.get(&(span.start, span.end)))
            .copied()
            .or_else(|| {
                let span = span?;
                let candidates: Vec<_> = self
                    .occurrence_indexes
                    .iter()
                    .filter(|((start, end), _)| *start <= span.start && *end >= span.end)
                    .map(|(_, index)| *index)
                    .collect();
                if let [index] = candidates.as_slice() {
                    Some(*index)
                } else {
                    None
                }
            });
        if let Some(index) = index {
            self.occurrences[index].binding = binding;
            if let Some(id) = self.identities.get(&(scope as *const Scope as usize)) {
                self.occurrences[index].scope_id = *id;
            }
            return;
        }
        let scope_id = self
            .identities
            .get(&(scope as *const Scope as usize))
            .copied()
            .unwrap_or(0);
        self.occurrences.push(BindingOccurrence {
            span: column.span.or(column.name.span),
            clause: "expression".to_owned(),
            scope_id,
            name: column.name.name.clone(),
            binding,
        });
    }
}

fn span_signature(expression: &Expression) -> Vec<(usize, usize)> {
    expression
        .dfs()
        .filter_map(|node| match node {
            Expression::Column(column) => column.span.or(column.name.span),
            Expression::Alias(alias) => alias.alias.span,
            Expression::Table(table) => table.name.span,
            Expression::Identifier(identifier) => identifier.span,
            _ => None,
        })
        .map(|span| (span.start, span.end))
        .collect()
}

/// Enumerate clause expressions without entering another lexical query scope.
/// The opt-in validator uses the generated child metadata so filters, frames and
/// named windows follow the same reference-resolution code as projections.
pub(super) fn complete_scope_nodes(expression: &Expression) -> Vec<&Expression> {
    let mut nodes = Vec::new();
    let mut pending = vec![expression];
    while let Some(node) = pending.pop() {
        nodes.push(node);
        crate::ast_children::for_each_child(node, |_, child| {
            if !matches!(
                child,
                Expression::Select(_)
                    | Expression::Subquery(_)
                    | Expression::Cte(_)
                    | Expression::Union(_)
                    | Expression::Intersect(_)
                    | Expression::Except(_)
            ) {
                pending.push(child);
            }
        });
    }
    nodes
}

pub(super) fn output_orders(mut expression: &Expression) -> Vec<crate::expressions::OrderBy> {
    let mut orders = Vec::new();
    loop {
        let (order, columns) = match expression {
            Expression::Subquery(query) => {
                if let Some(order) = &query.order_by {
                    orders.push(order.clone());
                }
                expression = &query.this;
                continue;
            }
            Expression::Cte(cte) => {
                expression = &cte.this;
                continue;
            }
            Expression::Paren(paren) => {
                expression = &paren.this;
                continue;
            }
            Expression::Alias(alias) => {
                expression = &alias.this;
                continue;
            }
            Expression::Annotated(annotated) => {
                expression = &annotated.this;
                continue;
            }
            Expression::Union(query) => (&query.order_by, &query.on_columns),
            Expression::Intersect(query) => (&query.order_by, &query.on_columns),
            Expression::Except(query) => (&query.order_by, &query.on_columns),
            _ => break,
        };
        if let Some(order) = order {
            orders.push(order.clone());
        }
        if !columns.is_empty() {
            orders.push(crate::expressions::OrderBy {
                expressions: columns
                    .iter()
                    .cloned()
                    .map(crate::expressions::Ordered::asc)
                    .collect(),
                comments: Vec::new(),
                siblings: false,
            });
        }
        break;
    }
    orders
}

pub(super) fn ddl_option_keys(expression: &Expression) -> Vec<&Column> {
    let mut keys = Vec::new();
    let mut pending = vec![expression];
    while let Some(node) = pending.pop() {
        crate::ast_children::for_each_child(node, |path, child| {
            if path.contains(&crate::ast_children::ChildPathSegment::Field("options")) {
                if let Expression::Eq(assignment) = child {
                    if let Expression::Column(key) = &assignment.left {
                        keys.push(key.as_ref());
                    }
                }
            }
            pending.push(child);
        });
    }
    keys
}
