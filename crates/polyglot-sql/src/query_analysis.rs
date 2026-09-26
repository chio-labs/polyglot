//! Compact query analysis facts.
//!
//! This module intentionally builds on the existing parser, scope builder, type
//! annotator, and lineage implementation. It is a convenience API: callers that
//! need the full AST or full lineage graph should continue using those lower
//! level APIs directly.

use crate::ast_transforms::get_output_column_names_for_dialect;
use crate::dialects::{Dialect, DialectType};
use crate::expressions::{DataType, Expression, Identifier, JoinKind, TableRef, With};
use crate::lineage::{LineageNode, ScopedLineage};
use crate::optimizer::annotate_types::annotate_types;
use crate::optimizer::qualify_schema_aware_expression;
use crate::schema::{MappingSchema, Schema};
use crate::scope::{build_scope, Scope, SourceInfo, SourceKind};
use crate::traversal::{contains_aggregate, ExpressionWalk};
use crate::validation::{mapping_schema_from_validation_schema_with_dialect, ValidationSchema};
use crate::{parse_one_with_options, ComplexityGuardOptions, Error, ParseOptions, Result};
use serde::{Deserialize, Serialize};
use std::cell::RefCell;
use std::collections::{HashMap, HashSet};

mod column_uses;

const WIDE_PROJECTION_LINEAGE_CACHE_THRESHOLD: usize = 64;

/// Options for [`analyze_query`].
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase", default, deny_unknown_fields)]
pub struct AnalyzeQueryOptions {
    /// Per-call limits used for parsing and rendering analysis facts.
    pub complexity_guard: Option<ComplexityGuardOptions>,
    /// SQL dialect used for parsing and dialect-aware rendering.
    pub dialect: DialectType,
    /// Optional validation schema used for qualification and type annotation.
    pub schema: Option<ValidationSchema>,
}

/// Compact facts about a query's output shape and data dependencies.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct QueryAnalysis {
    /// Whether the root query syntactically projects a star. This compact flag
    /// is retained even when project-only analysis omits standalone star facts.
    #[serde(skip)]
    pub has_root_star: bool,
    pub shape: QueryShape,
    pub ctes: Vec<String>,
    pub cte_facts: Vec<CteFact>,
    pub projections: Vec<ProjectionFact>,
    pub relations: Vec<RelationFact>,
    pub base_tables: Vec<RelationFact>,
    pub star_projections: Vec<StarProjectionFact>,
    pub set_operations: Vec<SetOperationFact>,
    /// Clause-specific uses, separate from output projection lineage.
    #[serde(default)]
    pub column_uses: Vec<ColumnUseFact>,
}

/// A half-open range in the original SQL, measured in Unicode characters.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct QuerySourceSpan {
    pub start: usize,
    pub end: usize,
}

/// The syntactic role of a column-containing expression.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ColumnUseContext {
    Join,
    Filter,
    Group,
    Having,
    Qualify,
    WindowPartition,
    WindowOrder,
    WindowFrame,
    Order,
    AggregateOrder,
    SetOperationFilter,
}

/// One resolved dependency of an original column occurrence. Several terminal
/// dependencies can share a span (for example, a reference to a computed CTE
/// column). Spans identify the use, not the upstream column's definition.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ColumnUseReferenceFact {
    #[serde(flatten)]
    pub reference: ColumnReferenceFact,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub span: Option<QuerySourceSpan>,
}

/// One containing expression, with references in occurrence order. Paths are
/// deterministic within an analysis, not persistent IDs across SQL edits.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ColumnUseFact {
    pub context: ColumnUseContext,
    pub scope_path: String,
    pub expression_path: String,
    /// Dialect-rendered SQL; not necessarily the original source substring.
    pub expression_sql: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub span: Option<QuerySourceSpan>,
    pub references: Vec<ColumnUseReferenceFact>,
}

/// Top-level query shape.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QueryShape {
    Select,
    SetOperation,
}

/// Compact fact about one output projection.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectionFact {
    pub index: usize,
    pub name: Option<String>,
    pub is_star: bool,
    pub star_table: Option<String>,
    pub transform_kind: TransformKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub transform_function: Option<TransformFunctionFact>,
    pub cast_type: Option<String>,
    pub type_hint: Option<String>,
    pub nullability: ProjectionNullability,
    pub upstream: Vec<ColumnReferenceFact>,
    #[serde(skip, default)]
    pub type_column_args: Vec<ColumnReferenceFact>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub passthrough_source: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub passthrough_column: Option<String>,
}

/// Compact fact about a function-like projection transform.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TransformFunctionFact {
    pub name: String,
    pub literal_args: Vec<String>,
    pub column_args: Vec<ColumnReferenceFact>,
    #[serde(skip, default)]
    pub type_column_args: Vec<ColumnReferenceFact>,
}

/// Compact fact about one top-level CTE definition.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CteFact {
    pub name: String,
    pub columns: Vec<String>,
    pub body_sql: String,
    pub output_columns: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shape: Option<QueryShape>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub projections: Vec<ProjectionFact>,
}

/// Compact fact about one original star projection.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StarProjectionFact {
    pub index: usize,
    pub table: Option<String>,
    pub expanded_columns: Vec<String>,
}

/// Compact fact about an upstream column reference.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ColumnReferenceFact {
    pub source_name: Option<String>,
    pub source_alias: Option<String>,
    pub source_kind: SourceKind,
    pub table: Option<String>,
    pub column: String,
    pub unqualified: bool,
    pub confidence: ReferenceConfidence,
}

/// Compact fact about a relation visible in the root scope.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RelationFact {
    pub name: String,
    pub alias: Option<String>,
    pub kind: SourceKind,
    pub columns: Vec<String>,
    pub catalog: Option<String>,
    pub schema: Option<String>,
    pub table: Option<String>,
}

/// Compact fact about a set operation.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SetOperationFact {
    pub kind: String,
    pub all: bool,
    pub distinct: bool,
    pub output_columns: Vec<String>,
    pub branches: Vec<SetOperationBranchFact>,
}

/// Compact facts for one immediate set-operation branch.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SetOperationBranchFact {
    pub index: usize,
    pub role: SetOperationBranchRole,
    pub projections: Vec<ProjectionFact>,
}

/// Semantic contribution of one set-operation branch.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SetOperationBranchRole {
    Value,
    Filter,
}

/// High-level kind of transformation performed by a projection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TransformKind {
    Direct,
    Cast,
    Aggregation,
    Constant,
    Expression,
    Star,
}

/// Confidence level for a compact upstream column reference.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReferenceConfidence {
    Resolved,
    Ambiguous,
    Unknown,
}

/// Conservative nullability classification for one output projection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProjectionNullability {
    NonNull,
    Nullable,
    Unknown,
}

/// Analyze a single SELECT or set-operation query.
pub fn analyze_query(sql: &str, options: AnalyzeQueryOptions) -> Result<QueryAnalysis> {
    analyze_query_inner(sql, options, QueryAnalysisMode::Default)
}

/// Analyze a query and include output facts for each top-level CTE.
///
/// This compact path retains original relation bindings instead of rewriting a
/// fully qualified AST. The supplied schema still drives star expansion, type
/// annotation, nullability, and conservative reference resolution.
pub fn analyze_query_with_cte_projections(
    sql: &str,
    options: AnalyzeQueryOptions,
) -> Result<QueryAnalysis> {
    analyze_query_inner(sql, options, QueryAnalysisMode::CteProjections)
}

/// Analyze only the projection and CTE facts required by a project compiler.
///
/// Relation inventories, standalone star facts, set-operation reports, and
/// clause-level column uses are omitted to avoid constructing fact graphs that
/// the project projection does not consume.
pub fn analyze_query_for_project_projections(
    sql: &str,
    options: AnalyzeQueryOptions,
) -> Result<QueryAnalysis> {
    analyze_query_inner(sql, options, QueryAnalysisMode::ProjectProjections)
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum QueryAnalysisMode {
    Default,
    CteProjections,
    ProjectProjections,
}

/// Results from one parsed query consumed by project analysis and validation.
pub struct CompiledQueryAnalysis {
    pub analysis: Result<QueryAnalysis>,
    pub validation: crate::ValidationResult,
    /// Original statement for consumer-specific checks requiring authored spans.
    pub expression: Option<Expression>,
}

/// Parse a project query once before applying independent analysis and binding semantics.
/// Invalid or multi-statement inputs preserve the standalone entry points' diagnostics.
pub fn compile_query_analysis(
    sql: &str,
    options: AnalyzeQueryOptions,
    schema: &ValidationSchema,
    validation_options: &crate::SchemaValidationOptions,
    project_projections: bool,
) -> CompiledQueryAnalysis {
    compile_query_analysis_inner(
        sql,
        options,
        schema,
        validation_options,
        project_projections,
        false,
    )
}

/// Compile dependency facts for project consumers that bind direct CTE projections.
/// Direct CTE edges are retained as passthrough facts instead of repeatedly tracing
/// their physical terminal columns; standalone analysis retains complete lineage.
pub fn compile_required_query_analysis(
    sql: &str,
    options: AnalyzeQueryOptions,
    schema: &ValidationSchema,
    validation_options: &crate::SchemaValidationOptions,
    project_projections: bool,
) -> CompiledQueryAnalysis {
    compile_query_analysis_inner(
        sql,
        options,
        schema,
        validation_options,
        project_projections,
        true,
    )
}

fn compile_query_analysis_inner(
    sql: &str,
    options: AnalyzeQueryOptions,
    schema: &ValidationSchema,
    validation_options: &crate::SchemaValidationOptions,
    project_projections: bool,
    required_cte_facts: bool,
) -> CompiledQueryAnalysis {
    let dialect = options.dialect;
    profile_query_analysis_stage("compile_start");
    let mode = if project_projections {
        QueryAnalysisMode::ProjectProjections
    } else {
        QueryAnalysisMode::Default
    };
    let parsed = crate::parse_for_validation(
        sql,
        &Dialect::get(dialect),
        &crate::ValidationOptions {
            complexity_guard: validation_options.complexity_guard,
            strict_syntax: validation_options.strict_syntax,
            semantic: validation_options.semantic,
        },
    );
    profile_query_analysis_stage("compile_parsed");
    match parsed {
        Ok(mut statements) if statements.len() == 1 => {
            let expression = statements.pop().unwrap();
            let share_scope = project_projections
                && !validation_options.check_types
                && !validation_options.semantic
                && matches!(
                    expression,
                    Expression::Select(_)
                        | Expression::Union(_)
                        | Expression::Intersect(_)
                        | Expression::Except(_)
                )
                && !expression
                    .dfs()
                    .any(|node| matches!(node, Expression::Select(select)
                         if select.expressions.iter().any(|projection| projection_is_star(unwrap_projection_alias(projection)))));
            let mut shared_validation = None;
            let mut validate_scope = |scope: &Scope| {
                shared_validation = Some(crate::validation::validate_project_query_scope(
                    scope,
                    dialect,
                    schema,
                    validation_options,
                ));
            };
            let analysis = analyze_query_expression(
                expression.clone(),
                options,
                mode,
                share_scope.then_some(&mut validate_scope as &mut dyn FnMut(&Scope)),
                required_cte_facts && project_projections,
            );
            profile_query_analysis_stage("compile_analyzed");
            let validation = shared_validation.unwrap_or_else(|| {
                crate::validation::validate_parsed_with_schema(
                    vec![expression.clone()],
                    dialect,
                    schema,
                    validation_options,
                )
            });
            profile_query_analysis_stage("compile_validated");
            CompiledQueryAnalysis {
                analysis,
                validation,
                expression: Some(expression),
            }
        }
        parsed => {
            let validation = match parsed {
                Ok(statements) => crate::validation::validate_parsed_with_schema(
                    statements,
                    dialect,
                    schema,
                    validation_options,
                ),
                Err(validation) => validation,
            };
            CompiledQueryAnalysis {
                analysis: analyze_query_inner(sql, options, mode),
                validation,
                expression: None,
            }
        }
    }
}

fn analyze_query_inner(
    sql: &str,
    options: AnalyzeQueryOptions,
    mode: QueryAnalysisMode,
) -> Result<QueryAnalysis> {
    profile_query_analysis_stage("start");
    let expression = parse_one_with_options(
        sql,
        options.dialect,
        &ParseOptions {
            complexity_guard: options.complexity_guard,
        },
    )?;
    profile_query_analysis_stage("parsed");
    analyze_query_expression(expression, options, mode, None, false)
}

fn analyze_query_expression(
    mut expression: Expression,
    options: AnalyzeQueryOptions,
    mode: QueryAnalysisMode,
    on_scope: Option<&mut dyn FnMut(&Scope)>,
    required_cte_facts: bool,
) -> Result<QueryAnalysis> {
    let include_extended_facts = mode != QueryAnalysisMode::ProjectProjections;
    expression = effective_query(expression);
    ensure_query(&expression)?;
    expression = crate::binding::bind_lambdas(expression, options.dialect);
    let include_cte_projections = mode != QueryAnalysisMode::Default;
    let has_root_star = select_expressions_for_query(&expression)
        .iter()
        .any(|projection| projection_is_star(unwrap_projection_alias(projection)));
    let original_expression = include_extended_facts.then(|| expression.clone());

    let mapping_schema = options
        .schema
        .as_ref()
        .map(|schema| analysis_mapping_schema(schema, options.dialect));
    let schema_info = options.schema.as_ref().map(AnalysisSchemaInfo::from_schema);
    let mut cte_facts = top_level_cte_facts(
        original_expression.as_ref().unwrap_or(&expression),
        options.dialect,
        options.complexity_guard,
        mode != QueryAnalysisMode::ProjectProjections,
    )?;
    profile_query_analysis_stage("cte_facts");
    let star_projections = if include_extended_facts {
        star_projection_facts(
            original_expression.as_ref().unwrap_or(&expression),
            mapping_schema.as_ref(),
            options.dialect,
        )
    } else {
        Vec::new()
    };

    if mode == QueryAnalysisMode::Default {
        if let Some(schema) = mapping_schema.as_ref() {
            use crate::optimizer::qualify_columns::QualifyColumnsError;
            expression = match qualify_schema_aware_expression(
                expression.clone(),
                schema,
                Some(options.dialect),
            ) {
                Ok(qualified) => qualified,
                // Analysis is not validation. Incomplete schemas and unresolved
                // lexical references must still yield conservative usage facts.
                Err(
                    QualifyColumnsError::UnknownTable(_)
                    | QualifyColumnsError::UnknownColumn(_)
                    | QualifyColumnsError::AmbiguousColumn(_)
                    | QualifyColumnsError::ColumnNotResolved { .. },
                ) => expression,
                Err(error) => {
                    return Err(Error::internal(format!(
                        "query analysis qualification failed: {error}"
                    )))
                }
            };
        }
    }

    crate::lineage::expand_cte_stars(
        &mut expression,
        mapping_schema.as_ref().map(|schema| schema as &dyn Schema),
    );
    profile_query_analysis_stage("stars_expanded");
    annotate_types(
        &mut expression,
        mapping_schema.as_ref().map(|schema| schema as &dyn Schema),
        Some(options.dialect),
    );
    profile_query_analysis_stage("types_annotated");

    let scope = build_scope(&expression);
    if let Some(on_scope) = on_scope {
        on_scope(&scope);
    }
    profile_query_analysis_stage("scope_built");
    let original_scope = original_expression.as_ref().map(build_scope);
    let empty_schema = MappingSchema::with_dialect(options.dialect);
    let mut uncertain_columns = HashMap::new();
    let selected_scopes =
        (mode == QueryAnalysisMode::ProjectProjections).then(|| selected_reference_scopes(&scope));
    if !can_skip_uncertain_occurrences(mode, &scope, options.schema.as_ref()) {
        if let Some(selected_scopes) = selected_scopes.as_ref() {
            column_uses::collect_uncertain_occurrences_with_selected_scopes(
                &scope,
                selected_scopes,
                mapping_schema.as_ref().unwrap_or(&empty_schema),
                &mut uncertain_columns,
            );
        } else {
            column_uses::collect_uncertain_occurrences(
                original_scope.as_ref().unwrap_or(&scope),
                mapping_schema.as_ref().unwrap_or(&empty_schema),
                &mut uncertain_columns,
            );
        }
    }
    profile_query_analysis_stage("uncertain_occurrences");
    let mut nullability_context = NullabilityContext::new(
        &scope,
        schema_info.as_ref(),
        mapping_schema.as_ref(),
        options.dialect,
        &uncertain_columns,
        selected_scopes,
    );
    if mode == QueryAnalysisMode::ProjectProjections
        && !scope.cte_scopes.is_empty()
        && !with_clause(&expression).is_some_and(|with| with.recursive)
    {
        nullability_context.project_root_lineage = Some(ScopedLineage::with_schema(
            &scope,
            &[],
            options.dialect,
            mapping_schema.as_ref().map(|schema| schema as &dyn Schema),
        ));
    }
    if required_cte_facts && !with_clause(&expression).is_some_and(|with| with.recursive) {
        let names: HashSet<String> = cte_facts
            .iter()
            .map(|fact| fact.name.to_lowercase())
            .collect();
        if names.len() == cte_facts.len() {
            nullability_context.project_cte_dependencies = names;
        }
    }
    profile_query_analysis_stage("nullability_context");
    let shape = if is_set_operation(&expression) {
        QueryShape::SetOperation
    } else {
        QueryShape::Select
    };
    let mut projections =
        projection_facts_for_query(&expression, &scope, options.dialect, &nullability_context);
    profile_query_analysis_stage("projection_facts");
    if include_cte_projections {
        enrich_projection_passthrough_sources(
            &scope,
            &mut projections,
            mapping_schema.as_ref(),
            options.dialect,
            Some(&nullability_context),
        );
        refine_filtered_projection_nullability(&expression, &mut projections);
        profile_query_analysis_stage("projections_enriched");
    }
    let set_operations = if include_extended_facts {
        set_operation_facts(&expression, &scope, options.dialect, &nullability_context)
    } else {
        Vec::new()
    };
    if include_cte_projections {
        if required_cte_facts && !with_clause(&expression).is_some_and(|with| with.recursive) {
            enrich_required_cte_facts(
                &mut cte_facts,
                &projections,
                &scope,
                options.dialect,
                &nullability_context,
            );
        } else {
            enrich_cte_facts(
                &mut cte_facts,
                &scope,
                options.dialect,
                &nullability_context,
            );
        }
        profile_query_analysis_stage("cte_facts_enriched");
    }

    Ok(QueryAnalysis {
        has_root_star,
        shape,
        ctes: collect_cte_names(&expression),
        cte_facts,
        projections,
        relations: include_extended_facts
            .then(|| relation_facts(&scope, mapping_schema.as_ref(), options.dialect))
            .unwrap_or_default(),
        base_tables: include_extended_facts
            .then(|| base_table_facts(&scope, mapping_schema.as_ref(), options.dialect))
            .unwrap_or_default(),
        star_projections,
        set_operations,
        column_uses: if let Some(original_scope) = original_scope.as_ref() {
            column_uses::collect(
                original_scope,
                &scope,
                mapping_schema.as_ref(),
                options.dialect,
                &uncertain_columns,
                options.complexity_guard,
            )
        } else {
            Vec::new()
        },
    })
}

fn profile_query_analysis_stage(stage: &str) {
    if std::env::var_os("POLYGLOT_PROFILE_QUERY_ANALYSIS").is_some() {
        thread_local! {
            static LAST_STAGE: RefCell<std::time::Instant> = RefCell::new(std::time::Instant::now());
        }
        LAST_STAGE.with(|last| {
            let now = std::time::Instant::now();
            let elapsed = now.duration_since(*last.borrow());
            *last.borrow_mut() = now;
            eprintln!(
                "polyglot query analysis stage: {stage} elapsed_us={}",
                elapsed.as_micros()
            );
        });
    }
}

fn can_skip_uncertain_occurrences(
    mode: QueryAnalysisMode,
    scope: &Scope,
    schema: Option<&ValidationSchema>,
) -> bool {
    if mode != QueryAnalysisMode::ProjectProjections
        || scope.sources.len() != 1
        || !scope
            .sources
            .values()
            .all(|source| source.kind == SourceKind::Table)
        || !scope.lateral_sources.is_empty()
        || !scope.cte_sources.is_empty()
        || !scope.subquery_scopes.is_empty()
        || !scope.derived_table_scopes.is_empty()
        || !scope.cte_scopes.is_empty()
        || !scope.udtf_scopes.is_empty()
        || !scope.union_scopes.is_empty()
    {
        return false;
    }
    let Some(schema) = schema else {
        return false;
    };
    if schema.tables.len() != 1 || schema.tables[0].columns.is_empty() {
        return false;
    }
    let schema_table = &schema.tables[0];
    let Some(source) = scope.sources.values().next() else {
        return false;
    };
    let Expression::Table(source_table) = source.expression.as_ref() else {
        return false;
    };
    let source_name_matches = source_table
        .name
        .name
        .eq_ignore_ascii_case(&schema_table.name)
        || schema_table
            .aliases
            .iter()
            .any(|alias| source_table.name.name.eq_ignore_ascii_case(alias));
    let source_schema_matches = source_table.schema.as_ref().is_none_or(|source_schema| {
        schema_table
            .schema
            .as_ref()
            .is_some_and(|schema| source_schema.name.eq_ignore_ascii_case(schema))
    });
    if !source_name_matches || !source_schema_matches {
        return false;
    }
    let known_columns: HashSet<&str> = schema_table
        .columns
        .iter()
        .map(|column| column.name.as_str())
        .collect();
    !known_columns.contains("*")
        && scope.expression.dfs().all(|expression| match expression {
            Expression::Column(column) => known_columns.contains(column.name.name.as_str()),
            _ => true,
        })
}

fn selected_reference_scopes(scope: &Scope) -> HashMap<*const Scope, Scope> {
    fn collect(scope: &Scope, selected: &mut HashMap<*const Scope, Scope>) {
        selected.insert(
            scope as *const Scope,
            crate::scope::selected_reference_scope(scope),
        );
        for child in scope
            .cte_scopes
            .iter()
            .chain(&scope.derived_table_scopes)
            .chain(&scope.subquery_scopes)
            .chain(&scope.udtf_scopes)
            .chain(&scope.union_scopes)
        {
            collect(child, selected);
        }
    }

    let mut selected = HashMap::new();
    collect(scope, &mut selected);
    selected
}

fn analysis_mapping_schema(schema: &ValidationSchema, dialect: DialectType) -> MappingSchema {
    mapping_schema_from_validation_schema_with_dialect(schema, dialect)
}

fn validation_table_names(table: &crate::validation::SchemaTable) -> Vec<String> {
    let mut names = Vec::new();

    names.push(table.name.to_ascii_lowercase());
    if let Some(schema_name) = &table.schema {
        names.push(format!(
            "{}.{}",
            schema_name.to_ascii_lowercase(),
            table.name.to_ascii_lowercase()
        ));
    }
    for alias in &table.aliases {
        names.push(alias.to_ascii_lowercase());
    }

    names.sort();
    names.dedup();
    names
}

#[derive(Debug, Clone)]
struct AnalysisColumnInfo {
    nullable: Option<bool>,
    primary_key: bool,
}

#[derive(Debug, Clone)]
struct AnalysisSchemaInfo {
    columns: HashMap<(String, String), AnalysisColumnInfo>,
}

impl AnalysisSchemaInfo {
    fn from_schema(schema: &ValidationSchema) -> Self {
        let mut columns = HashMap::new();

        for table in &schema.tables {
            let table_names = validation_table_names(table);
            let primary_keys: HashSet<String> = table
                .primary_key
                .iter()
                .map(|column| column.to_ascii_lowercase())
                .collect();

            for column in &table.columns {
                let info = AnalysisColumnInfo {
                    nullable: column.nullable,
                    primary_key: column.primary_key
                        || primary_keys.contains(&column.name.to_ascii_lowercase()),
                };

                for table_name in &table_names {
                    columns.insert(
                        (
                            normalize_lookup_name(table_name),
                            normalize_lookup_name(&column.name),
                        ),
                        info.clone(),
                    );
                }
            }
        }

        Self { columns }
    }

    fn column(&self, table: &str, column: &str) -> Option<&AnalysisColumnInfo> {
        self.columns
            .get(&(normalize_lookup_name(table), normalize_lookup_name(column)))
    }
}

struct NullabilityContext<'a> {
    project_root_lineage: Option<ScopedLineage<'a>>,
    project_cte_dependencies: HashSet<String>,
    source_column_cache:
        RefCell<HashMap<(*const Scope, String), Option<std::rc::Rc<Vec<Identifier>>>>>,
    lineage: RefCell<HashMap<*const Scope, ScopedLineage<'a>>>,
    terminal_lineage: RefCell<HashMap<(*const Scope, String, String), Vec<ColumnReferenceFact>>>,
    schema_columns: RefCell<HashMap<String, HashSet<String>>>,
    output_types: RefCell<crate::optimizer::set_operation_types::OutputResolver>,
    schema: Option<&'a AnalysisSchemaInfo>,
    mapping_schema: Option<&'a MappingSchema>,
    empty_schema: MappingSchema,
    dialect: DialectType,
    scopes: Vec<NullabilityScope<'a>>,
    scope_ids: HashMap<*const Scope, usize>,
    outputs: RefCell<HashMap<(usize, usize), ProjectionNullability>>,
    resolving: RefCell<HashSet<(usize, usize)>>,
    uncertain_columns: &'a HashMap<(usize, usize), ReferenceConfidence>,
}

struct NullabilityScope<'a> {
    scope: &'a Scope,
    selected: Scope,
    bindings: HashMap<String, &'a Expression>,
    ctes: HashMap<String, usize>,
    derived: Vec<usize>,
    branches: Vec<usize>,
    nullable_sources: HashSet<String>,
}

impl<'a> NullabilityContext<'a> {
    fn new(
        scope: &'a Scope,
        schema: Option<&'a AnalysisSchemaInfo>,
        mapping_schema: Option<&'a MappingSchema>,
        dialect: DialectType,
        uncertain_columns: &'a HashMap<(usize, usize), ReferenceConfidence>,
        mut selected_scopes: Option<HashMap<*const Scope, Scope>>,
    ) -> Self {
        let mut context = Self {
            source_column_cache: RefCell::new(HashMap::new()),
            project_cte_dependencies: HashSet::new(),
            project_root_lineage: None,
            lineage: RefCell::new(HashMap::new()),
            terminal_lineage: RefCell::new(HashMap::new()),
            schema_columns: RefCell::new(HashMap::new()),
            output_types: RefCell::new(crate::optimizer::set_operation_types::OutputResolver::new(
                dialect,
            )),
            schema,
            mapping_schema,
            empty_schema: MappingSchema::with_dialect(dialect),
            dialect,
            scopes: Vec::new(),
            scope_ids: HashMap::new(),
            outputs: RefCell::new(HashMap::new()),
            resolving: RefCell::new(HashSet::new()),
            uncertain_columns,
        };
        context.index_scope(scope, HashMap::new(), None, selected_scopes.as_mut());
        context
    }

    fn selected_scope(&self, scope: &Scope) -> Option<&Scope> {
        self.scope_ids
            .get(&(scope as *const Scope))
            .map(|id| &self.scopes[*id].selected)
    }

    // Keep each CTE's definition environment, rather than resolving it in the
    // environment of a later consumer that may shadow the same names.
    fn index_scope(
        &mut self,
        scope: &'a Scope,
        mut ctes: HashMap<String, usize>,
        recursive_name: Option<&Identifier>,
        mut selected_scopes: Option<&mut HashMap<*const Scope, Scope>>,
    ) -> usize {
        let id = self.scopes.len();
        self.scope_ids.insert(scope, id);
        if let Some(name) = recursive_name {
            ctes.insert(
                crate::set_operation::identifier_key(name, Some(self.dialect)),
                id,
            );
        }
        let query = crate::scope::scope_query(&scope.expression);
        let bindings: HashMap<_, _> = if let Expression::Select(select) = query {
            select
                .from
                .iter()
                .flat_map(|from| &from.expressions)
                .chain(select.joins.iter().map(|join| &join.this))
                .filter_map(|expression| {
                    expression_source_name(expression).map(|name| (name, expression))
                })
                .collect()
        } else {
            HashMap::new()
        };
        let selected = selected_scopes
            .as_deref_mut()
            .and_then(|scopes| scopes.remove(&(scope as *const Scope)))
            .unwrap_or_else(|| crate::scope::selected_reference_scope(scope));
        self.scopes.push(NullabilityScope {
            scope,
            selected,
            bindings,
            ctes: HashMap::new(),
            derived: Vec::new(),
            branches: Vec::new(),
            nullable_sources: nullable_source_names(query, self.dialect),
        });
        let recursive = with_clause(crate::scope::scope_query(&scope.expression))
            .is_some_and(|with| with.recursive);
        for child in &scope.cte_scopes {
            if let Expression::Cte(cte) = &child.expression {
                let name = &cte.alias;
                let child_id = self.index_scope(
                    child,
                    ctes.clone(),
                    recursive.then_some(name),
                    selected_scopes.as_deref_mut(),
                );
                ctes.insert(
                    crate::set_operation::identifier_key(name, Some(self.dialect)),
                    child_id,
                );
            }
        }
        self.scopes[id].ctes = ctes.clone();
        for child in &scope.derived_table_scopes {
            let child_id =
                self.index_scope(child, ctes.clone(), None, selected_scopes.as_deref_mut());
            self.scopes[id].derived.push(child_id);
        }
        for child in &scope.union_scopes {
            let child_id =
                self.index_scope(child, ctes.clone(), None, selected_scopes.as_deref_mut());
            self.scopes[id].branches.push(child_id);
        }
        id
    }
}

fn top_level_cte_facts(
    expression: &Expression,
    dialect: DialectType,
    guard: Option<ComplexityGuardOptions>,
    include_rendered_facts: bool,
) -> Result<Vec<CteFact>> {
    let Some(with_clause) = with_clause(expression) else {
        return Ok(Vec::new());
    };

    with_clause
        .ctes
        .iter()
        .map(|cte| {
            Ok(CteFact {
                name: cte.alias.name.clone(),
                columns: cte
                    .columns
                    .iter()
                    .map(|column| column.name.clone())
                    .collect(),
                body_sql: if include_rendered_facts {
                    Dialect::get(dialect).generate_with_guard(&cte.this, guard)?
                } else {
                    String::new()
                },
                output_columns: if include_rendered_facts {
                    get_output_column_names_for_dialect(&cte.this, Some(dialect))
                } else {
                    Vec::new()
                },
                shape: None,
                projections: Vec::new(),
            })
        })
        .collect()
}

fn enrich_cte_facts<'a>(
    facts: &mut [CteFact],
    scope: &'a Scope,
    dialect: DialectType,
    nullability_context: &NullabilityContext<'a>,
) {
    let scopes_by_name: HashMap<&str, &Scope> = scope
        .cte_scopes
        .iter()
        .filter_map(|cte_scope| match &cte_scope.expression {
            Expression::Cte(cte) => Some((cte.alias.name.as_str(), cte_scope)),
            _ => None,
        })
        .collect();
    for fact in facts {
        let Some(cte_scope) = scopes_by_name.get(fact.name.as_str()).copied() else {
            continue;
        };
        let query = crate::scope::scope_query(&cte_scope.expression);
        fact.shape = Some(if is_set_operation(query) {
            QueryShape::SetOperation
        } else {
            QueryShape::Select
        });
        fact.projections =
            projection_facts_for_query(query, cte_scope, dialect, nullability_context);
        enrich_projection_passthrough_sources(
            cte_scope,
            &mut fact.projections,
            nullability_context.mapping_schema,
            dialect,
            Some(nullability_context),
        );
        refine_filtered_projection_nullability(query, &mut fact.projections);
        for (projection, alias) in fact.projections.iter_mut().zip(&fact.columns) {
            projection.name = Some(alias.clone());
        }
    }
}

fn enrich_required_cte_facts<'a>(
    facts: &mut [CteFact],
    outputs: &[ProjectionFact],
    scope: &'a Scope,
    dialect: DialectType,
    context: &NullabilityContext<'a>,
) {
    let names: HashSet<String> = facts.iter().map(|fact| fact.name.to_lowercase()).collect();
    if names.len() != facts.len() {
        enrich_cte_facts(facts, scope, dialect, context);
        return;
    }
    let scopes: HashMap<&str, &Scope> = scope
        .cte_scopes
        .iter()
        .filter_map(|scope| match &scope.expression {
            Expression::Cte(cte) => Some((cte.alias.name.as_str(), scope)),
            _ => None,
        })
        .collect();
    let mut demanded: HashMap<String, HashSet<String>> = HashMap::new();
    let mut processed: HashMap<String, HashSet<String>> = HashMap::new();
    add_cte_projection_demands(outputs, &names, &mut demanded);
    while !demanded.is_empty() {
        for fact in facts.iter_mut().rev() {
            let name = fact.name.to_lowercase();
            let Some(mut columns) = demanded.remove(&name) else {
                continue;
            };
            let completed = processed.entry(name).or_default();
            columns.retain(|column| !completed.contains(column));
            if columns.is_empty() {
                continue;
            }
            completed.extend(columns.iter().cloned());
            let Some(cte_scope) = scopes.get(fact.name.as_str()).copied() else {
                continue;
            };
            let query = crate::scope::scope_query(&cte_scope.expression);
            fact.shape = Some(if is_set_operation(query) {
                QueryShape::SetOperation
            } else {
                QueryShape::Select
            });
            let mut projections = selected_projection_facts_for_query(
                query,
                cte_scope,
                dialect,
                context,
                Some((&columns, &fact.columns)),
            );
            enrich_projection_passthrough_sources(
                cte_scope,
                &mut projections,
                context.mapping_schema,
                dialect,
                Some(context),
            );
            refine_filtered_projection_nullability(query, &mut projections);
            add_cte_projection_demands(&projections, &names, &mut demanded);
            for projection in &mut projections {
                if let Some(alias) = fact.columns.get(projection.index) {
                    projection.name = Some(alias.clone());
                }
            }
            fact.projections.extend(projections);
        }
    }
    for fact in facts {
        fact.projections.sort_by_key(|projection| projection.index);
    }
}

fn add_cte_projection_demands(
    projections: &[ProjectionFact],
    names: &HashSet<String>,
    demanded: &mut HashMap<String, HashSet<String>>,
) {
    for projection in projections {
        if !matches!(
            projection.transform_kind,
            TransformKind::Direct | TransformKind::Cast
        ) {
            continue;
        }
        let mut add = |source: &str, column: &str| {
            let source = source.to_lowercase();
            if names.contains(&source) {
                demanded
                    .entry(source)
                    .or_default()
                    .insert(column.to_lowercase());
            }
        };
        if let (Some(source), Some(column)) = (
            &projection.passthrough_source,
            &projection.passthrough_column,
        ) {
            add(source, column);
        }
        for column in &projection.type_column_args {
            if let Some(source) = column.source_name.as_ref().or(column.table.as_ref()) {
                add(source, &column.column);
            }
        }
    }
}

fn enrich_projection_passthrough_sources(
    scope: &Scope,
    projections: &mut [ProjectionFact],
    mapping_schema: Option<&MappingSchema>,
    dialect: DialectType,
    nullability_context: Option<&NullabilityContext<'_>>,
) {
    let Some(projection_scope) = representative_projection_scope(scope) else {
        return;
    };
    let fallback_selected;
    let selected = if let Some(selected) =
        nullability_context.and_then(|context| context.selected_scope(projection_scope))
    {
        selected
    } else {
        fallback_selected = crate::scope::selected_reference_scope(projection_scope);
        &fallback_selected
    };
    let query = unwrap_query_annotations(crate::scope::scope_query(&projection_scope.expression));
    let Expression::Select(select) = query else {
        return;
    };
    for projection in projections {
        let Some(column) = select
            .expressions
            .get(projection.index)
            .and_then(direct_projected_column)
        else {
            continue;
        };
        let source = match &column.table {
            Some(table) if table.quoted => selected.sources.get_key_value(&table.name),
            Some(table) => selected
                .sources
                .iter()
                .find(|(name, _)| name.eq_ignore_ascii_case(&table.name))
                .map(|(name, source)| (name, source)),
            None if selected.sources.len() == 1 => selected.sources.iter().next(),
            None => unique_source_for_column(&selected, &column.name, mapping_schema, dialect),
        };
        let Some((source_name, source)) = source else {
            if let Some(source) = projection_cte_passthrough(projection, &column.name.name) {
                projection.passthrough_source = source.source_name.clone().or(source.table.clone());
                projection.passthrough_column = Some(column.name.name.clone());
            }
            continue;
        };
        if source.kind != SourceKind::Cte {
            continue;
        }
        let cte_name = source
            .lineage_name
            .clone()
            .unwrap_or_else(|| source_name.clone());
        projection.passthrough_source = Some(cte_name);
        projection.passthrough_column = Some(column.name.name.clone());
    }
}

fn unique_source_for_column<'a>(
    scope: &'a Scope,
    column: &Identifier,
    mapping_schema: Option<&MappingSchema>,
    dialect: DialectType,
) -> Option<(&'a String, &'a SourceInfo)> {
    let mut matching = None;
    for (name, source) in &scope.sources {
        let columns = source_columns(source, mapping_schema, dialect);
        if columns.is_empty() {
            return None;
        }
        let exposes_column = columns.iter().any(|candidate| {
            if column.quoted {
                candidate == &column.name
            } else {
                candidate.eq_ignore_ascii_case(&column.name)
            }
        });
        if !exposes_column {
            continue;
        }
        if matching.is_some() {
            return None;
        }
        matching = Some((name, source));
    }
    matching
}

fn projection_cte_passthrough<'a>(
    projection: &'a ProjectionFact,
    column: &str,
) -> Option<&'a ColumnReferenceFact> {
    let mut matching = projection.upstream.iter().filter(|reference| {
        reference.source_kind == SourceKind::Cte && reference.column.eq_ignore_ascii_case(column)
    });
    let first = matching.next()?;
    matching
        .all(|reference| {
            reference.source_name == first.source_name
                && reference.table == first.table
                && reference.column.eq_ignore_ascii_case(&first.column)
        })
        .then_some(first)
}

fn representative_projection_scope(scope: &Scope) -> Option<&Scope> {
    match unwrap_query_annotations(crate::scope::scope_query(&scope.expression)) {
        Expression::Select(_) => Some(scope),
        Expression::Union(_) | Expression::Intersect(_) | Expression::Except(_) => scope
            .union_scopes
            .first()
            .and_then(representative_projection_scope),
        _ => None,
    }
}

fn refine_filtered_projection_nullability(
    expression: &Expression,
    projections: &mut [ProjectionFact],
) {
    let Expression::Select(select) = crate::scope::scope_query(expression) else {
        return;
    };
    let Some(where_clause) = &select.where_clause else {
        return;
    };
    let mut filtered_columns = Vec::new();
    collect_non_null_conjunct_columns(&where_clause.this, &mut filtered_columns);
    if filtered_columns.is_empty() {
        return;
    }
    for projection in projections {
        let projected_column = select
            .expressions
            .get(projection.index)
            .and_then(direct_projected_column);
        if projected_column.is_some_and(|projected| {
            filtered_columns
                .iter()
                .any(|filtered| columns_identical_in_scope(projected, filtered))
        }) {
            projection.nullability = ProjectionNullability::NonNull;
        }
    }
}

fn collect_non_null_conjunct_columns<'a>(
    expression: &'a Expression,
    columns: &mut Vec<&'a crate::expressions::Column>,
) {
    match expression {
        Expression::Paren(paren) => collect_non_null_conjunct_columns(&paren.this, columns),
        Expression::And(and) => {
            collect_non_null_conjunct_columns(&and.left, columns);
            collect_non_null_conjunct_columns(&and.right, columns);
        }
        Expression::IsNull(is_null) if is_null.not => {
            if let Some(column) = direct_filter_column(&is_null.this) {
                columns.push(column);
            }
        }
        Expression::Not(not) => {
            if let Expression::IsNull(is_null) = unwrap_parentheses(&not.this) {
                if !is_null.not {
                    if let Some(column) = direct_filter_column(&is_null.this) {
                        columns.push(column);
                    }
                }
            }
        }
        _ => {}
    }
}

fn direct_filter_column(expression: &Expression) -> Option<&crate::expressions::Column> {
    match unwrap_parentheses(expression) {
        Expression::Column(column) => Some(column),
        _ => None,
    }
}

fn direct_projected_column(expression: &Expression) -> Option<&crate::expressions::Column> {
    match unwrap_parentheses(expression) {
        Expression::Alias(alias) => direct_projected_column(&alias.this),
        Expression::Column(column) => Some(column),
        _ => None,
    }
}

fn unwrap_parentheses(mut expression: &Expression) -> &Expression {
    while let Expression::Paren(paren) = expression {
        expression = &paren.this;
    }
    expression
}

fn columns_identical_in_scope(
    projected: &crate::expressions::Column,
    filtered: &crate::expressions::Column,
) -> bool {
    if !identifiers_match(&projected.name, &filtered.name) {
        return false;
    }
    match (&projected.table, &filtered.table) {
        (Some(left), Some(right)) => identifiers_match(left, right),
        (None, None) => true,
        _ => false,
    }
}

fn identifiers_match(
    left: &crate::expressions::Identifier,
    right: &crate::expressions::Identifier,
) -> bool {
    match (left.quoted, right.quoted) {
        (false, false) => left.name.eq_ignore_ascii_case(&right.name),
        (true, true) => left.name == right.name,
        _ => false,
    }
}

fn star_projection_facts(
    expression: &Expression,
    mapping_schema: Option<&MappingSchema>,
    dialect: DialectType,
) -> Vec<StarProjectionFact> {
    let projections = select_expressions_for_query(expression);
    if !projections
        .iter()
        .any(|projection| projection_is_star(unwrap_projection_alias(projection)))
    {
        return Vec::new();
    }
    let scope = build_scope(expression);
    let ordered_sources = ordered_source_names_for_query(expression);

    projections
        .iter()
        .enumerate()
        .filter_map(|(index, projection)| {
            let inner = unwrap_projection_alias(projection);
            if !projection_is_star(inner) {
                return None;
            }

            let table = projection_star_table(inner);
            let expanded_columns = expanded_star_columns(
                table.as_deref(),
                &scope,
                &ordered_sources,
                mapping_schema,
                dialect,
            );

            Some(StarProjectionFact {
                index,
                table,
                expanded_columns,
            })
        })
        .collect()
}

fn expanded_star_columns(
    star_table: Option<&str>,
    scope: &Scope,
    ordered_sources: &[String],
    mapping_schema: Option<&MappingSchema>,
    dialect: DialectType,
) -> Vec<String> {
    let mut columns = Vec::new();
    let mut source_names: Vec<String> = if ordered_sources.is_empty() {
        let mut names: Vec<_> = scope.sources.keys().cloned().collect();
        names.sort();
        names
    } else {
        ordered_sources.to_vec()
    };

    source_names.dedup();

    for source_name in source_names {
        let Some(source) = scope.sources.get(&source_name) else {
            continue;
        };

        if let Some(star_table) = star_table {
            let matches = source_name.eq_ignore_ascii_case(star_table)
                || source
                    .alias
                    .as_deref()
                    .is_some_and(|alias| alias.eq_ignore_ascii_case(star_table))
                || source_table_name(source)
                    .is_some_and(|table| table.eq_ignore_ascii_case(star_table));

            if !matches {
                continue;
            }
        }

        columns.extend(source_columns(source, mapping_schema, dialect));
    }

    columns
}

fn ordered_source_names_for_query(expression: &Expression) -> Vec<String> {
    match expression {
        Expression::Select(select) => ordered_source_names_for_select(select),
        Expression::Union(union) => ordered_source_names_for_query(&union.left),
        Expression::Intersect(intersect) => ordered_source_names_for_query(&intersect.left),
        Expression::Except(except) => ordered_source_names_for_query(&except.left),
        Expression::Subquery(subquery) => ordered_source_names_for_query(&subquery.this),
        _ => Vec::new(),
    }
}

fn ordered_source_names_for_select(select: &crate::expressions::Select) -> Vec<String> {
    let mut sources = Vec::new();

    if let Some(from) = &select.from {
        for expression in &from.expressions {
            if let Some(source_name) = expression_source_name(expression) {
                sources.push(source_name);
            }
        }
    }

    for join in &select.joins {
        if let Some(source_name) = expression_source_name(&join.this) {
            sources.push(source_name);
        }
    }

    sources
}

fn nullable_source_names(expression: &Expression, dialect: DialectType) -> HashSet<String> {
    match expression {
        Expression::Select(select) => nullable_source_names_for_select(select, dialect),
        Expression::Union(union) => nullable_source_names(&union.left, dialect),
        Expression::Intersect(intersect) => nullable_source_names(&intersect.left, dialect),
        Expression::Except(except) => nullable_source_names(&except.left, dialect),
        Expression::Subquery(subquery) => nullable_source_names(&subquery.this, dialect),
        _ => HashSet::new(),
    }
}

fn nullable_source_names_for_select(
    select: &crate::expressions::Select,
    dialect: DialectType,
) -> HashSet<String> {
    let mut nullable = HashSet::new();
    let mut left_sources = Vec::new();

    if let Some(from) = &select.from {
        for expression in &from.expressions {
            if let Some(identifier) = expression_source_identifier(expression) {
                left_sources.push(crate::set_operation::identifier_key(
                    identifier,
                    Some(dialect),
                ));
            }
        }
    }

    for join in &select.joins {
        let right_source = expression_source_identifier(&join.this)
            .map(|identifier| crate::set_operation::identifier_key(identifier, Some(dialect)));

        if join_nullable_left(join.kind) {
            for source_name in &left_sources {
                nullable.insert(source_name.clone());
            }
        }

        if join_nullable_right(join.kind) {
            if let Some(source_name) = &right_source {
                nullable.insert(source_name.clone());
            }
        }

        if let Some(source_name) = right_source {
            left_sources.push(source_name);
        }
    }

    nullable
}

fn join_nullable_left(kind: JoinKind) -> bool {
    matches!(
        kind,
        JoinKind::Right
            | JoinKind::NaturalRight
            | JoinKind::AsOfRight
            | JoinKind::Full
            | JoinKind::NaturalFull
            | JoinKind::Outer
    )
}

fn join_nullable_right(kind: JoinKind) -> bool {
    matches!(
        kind,
        JoinKind::Left
            | JoinKind::NaturalLeft
            | JoinKind::AsOfLeft
            | JoinKind::LeftLateral
            | JoinKind::OuterApply
            | JoinKind::LeftArray
            | JoinKind::Full
            | JoinKind::NaturalFull
            | JoinKind::Outer
    )
}

fn expression_source_name(expression: &Expression) -> Option<String> {
    expression_source_identifier(expression).map(|identifier| identifier.name.clone())
}

fn expression_source_identifier(expression: &Expression) -> Option<&Identifier> {
    match expression {
        Expression::Table(table) => Some(table.alias.as_ref().unwrap_or(&table.name)),
        Expression::Subquery(subquery) => subquery.alias.as_ref(),
        Expression::Alias(alias) => Some(&alias.alias),
        Expression::Cte(cte) => Some(&cte.alias),
        _ => None,
    }
}

fn normalize_lookup_name(name: &str) -> String {
    name.to_ascii_lowercase()
}

fn effective_query(expression: Expression) -> Expression {
    match expression {
        Expression::Prepare(prepare) => prepare.statement,
        Expression::Subquery(subquery) if subquery.alias.is_none() => subquery.this,
        other => other,
    }
}

fn ensure_query(expression: &Expression) -> Result<()> {
    if matches!(
        expression,
        Expression::Select(_)
            | Expression::Union(_)
            | Expression::Intersect(_)
            | Expression::Except(_)
    ) {
        Ok(())
    } else {
        Err(Error::internal(
            "analyze_query requires a SELECT or set operation query",
        ))
    }
}

fn is_set_operation(expression: &Expression) -> bool {
    matches!(
        expression,
        Expression::Union(_) | Expression::Intersect(_) | Expression::Except(_)
    )
}

fn collect_cte_names(expression: &Expression) -> Vec<String> {
    let mut names = Vec::new();
    let mut seen = HashSet::new();
    collect_cte_names_inner(expression, &mut names, &mut seen);
    names
}

fn collect_cte_names_inner(
    expression: &Expression,
    names: &mut Vec<String>,
    seen: &mut HashSet<String>,
) {
    if let Some(with_clause) = with_clause(expression) {
        collect_with_names(with_clause, names, seen);
    }

    match expression {
        Expression::Union(union) => {
            collect_cte_names_inner(&union.left, names, seen);
            collect_cte_names_inner(&union.right, names, seen);
        }
        Expression::Intersect(intersect) => {
            collect_cte_names_inner(&intersect.left, names, seen);
            collect_cte_names_inner(&intersect.right, names, seen);
        }
        Expression::Except(except) => {
            collect_cte_names_inner(&except.left, names, seen);
            collect_cte_names_inner(&except.right, names, seen);
        }
        Expression::Subquery(subquery) => collect_cte_names_inner(&subquery.this, names, seen),
        _ => {}
    }
}

fn collect_with_names(with_clause: &With, names: &mut Vec<String>, seen: &mut HashSet<String>) {
    for cte in &with_clause.ctes {
        if seen.insert(cte.alias.name.clone()) {
            names.push(cte.alias.name.clone());
        }
        collect_cte_names_inner(&cte.this, names, seen);
    }
}

fn with_clause(expression: &Expression) -> Option<&With> {
    match expression {
        Expression::Select(select) => select.with.as_ref(),
        Expression::Union(union) => union.with.as_ref(),
        Expression::Intersect(intersect) => intersect.with.as_ref(),
        Expression::Except(except) => except.with.as_ref(),
        _ => None,
    }
}

fn projection_facts_for_query<'a>(
    expression: &Expression,
    scope: &'a Scope,
    dialect: DialectType,
    nullability_context: &NullabilityContext<'a>,
) -> Vec<ProjectionFact> {
    selected_projection_facts_for_query(expression, scope, dialect, nullability_context, None)
}

fn selected_projection_facts_for_query<'a>(
    expression: &Expression,
    scope: &'a Scope,
    dialect: DialectType,
    nullability_context: &NullabilityContext<'a>,
    selection: Option<(&HashSet<String>, &[String])>,
) -> Vec<ProjectionFact> {
    let expressions = projection_sources_for_query(expression, dialect);
    profile_query_analysis_stage("projection_sources");
    let names = get_output_column_names_for_dialect(expression, Some(dialect));
    profile_query_analysis_stage("projection_names");
    let cached_lineage = cached_lineage_context(
        expression,
        expressions.len(),
        dialect,
        scope,
        nullability_context,
    );
    profile_query_analysis_stage("cached_lineage");
    let mut type_query = expression;
    loop {
        type_query = match type_query {
            Expression::Subquery(subquery) => &subquery.this,
            Expression::Paren(paren) => &paren.this,
            Expression::Annotated(annotated) => &annotated.this,
            _ => break,
        };
    }
    let resolved_outputs = is_set_operation(type_query).then(|| {
        nullability_context
            .output_types
            .borrow_mut()
            .resolve(expression)
    });

    expressions
        .iter()
        .enumerate()
        .filter(|(index, _)| {
            selection.is_none_or(|(columns, aliases)| {
                aliases
                    .get(*index)
                    .or_else(|| names.get(*index))
                    .is_some_and(|name| columns.contains(&name.to_lowercase()))
            })
        })
        .map(|(index, (projection, null_padded))| {
            if index % 10 == 0 {
                profile_query_analysis_stage(&format!("projection_fact_{index}"));
            }
            let mut fact = projection_fact(
                index,
                names
                    .get(index)
                    .cloned()
                    .or_else(|| projection_name(projection)),
                projection,
                expression,
                scope,
                dialect,
                nullability_context,
                cached_lineage.as_ref(),
            );
            if *null_padded {
                fact.nullability = ProjectionNullability::Nullable;
            }
            if let Some(outputs) = &resolved_outputs {
                let output = outputs.get(index);
                fact.type_hint = output
                    .and_then(|output| output.data_type())
                    .and_then(|data_type| render_data_type(data_type, dialect));
                fact.cast_type = output
                    .and_then(|output| output.cast_type.as_ref())
                    .and_then(|data_type| render_data_type(data_type, dialect));
            }
            fact
        })
        .collect()
}

/// Return one representative projection for each result ordinal together with
/// whether any immediate name-aligned branch contributes a synthetic NULL.
fn projection_sources_for_query(
    expression: &Expression,
    dialect: DialectType,
) -> Vec<(&Expression, bool)> {
    match expression {
        Expression::Subquery(subquery) => {
            return projection_sources_for_query(&subquery.this, dialect)
        }
        Expression::Paren(paren) => return projection_sources_for_query(&paren.this, dialect),
        Expression::Annotated(annotated) => {
            return projection_sources_for_query(&annotated.this, dialect)
        }
        _ => {}
    }
    match crate::set_operation::set_operation_layout(expression, Some(dialect)) {
        Ok(Some(layout)) => layout
            .outputs
            .iter()
            .filter_map(|output| {
                let null_padded = output.left_ordinal.is_none() || output.right_ordinal.is_none();
                output
                    .left_ordinal
                    .and_then(|ordinal| {
                        projection_source_for_ordinal(
                            set_operation_left(expression)?,
                            ordinal,
                            dialect,
                        )
                    })
                    .or_else(|| {
                        output.right_ordinal.and_then(|ordinal| {
                            projection_source_for_ordinal(
                                set_operation_right(expression)?,
                                ordinal,
                                dialect,
                            )
                        })
                    })
                    .map(|(projection, nested_null_padded)| {
                        (projection, null_padded || nested_null_padded)
                    })
            })
            .collect(),
        _ => select_expressions_for_query(expression)
            .into_iter()
            .map(|projection| (projection, false))
            .collect(),
    }
}

fn projection_source_for_ordinal(
    expression: &Expression,
    ordinal: usize,
    dialect: DialectType,
) -> Option<(&Expression, bool)> {
    let expression = unwrap_query_annotations(expression);
    match crate::set_operation::set_operation_layout(expression, Some(dialect)) {
        Ok(Some(layout)) => {
            let output = layout.outputs.get(ordinal)?;
            let null_padded = output.left_ordinal.is_none() || output.right_ordinal.is_none();
            output
                .left_ordinal
                .and_then(|child_ordinal| {
                    projection_source_for_ordinal(
                        set_operation_left(expression)?,
                        child_ordinal,
                        dialect,
                    )
                })
                .or_else(|| {
                    output.right_ordinal.and_then(|child_ordinal| {
                        projection_source_for_ordinal(
                            set_operation_right(expression)?,
                            child_ordinal,
                            dialect,
                        )
                    })
                })
                .map(|(projection, nested_null_padded)| {
                    (projection, null_padded || nested_null_padded)
                })
        }
        _ => match expression {
            Expression::Select(select) => select
                .expressions
                .get(ordinal)
                .map(|projection| (projection, false)),
            Expression::Union(union) => {
                projection_source_for_ordinal(&union.left, ordinal, dialect)
            }
            Expression::Intersect(intersect) => {
                projection_source_for_ordinal(&intersect.left, ordinal, dialect)
            }
            Expression::Except(except) => {
                projection_source_for_ordinal(&except.left, ordinal, dialect)
            }
            Expression::Subquery(subquery) => {
                projection_source_for_ordinal(&subquery.this, ordinal, dialect)
            }
            Expression::Paren(paren) => {
                projection_source_for_ordinal(&paren.this, ordinal, dialect)
            }
            _ => None,
        },
    }
}

fn set_operation_left(expression: &Expression) -> Option<&Expression> {
    match unwrap_query_annotations(expression) {
        Expression::Union(set_op) => Some(&set_op.left),
        Expression::Intersect(set_op) => Some(&set_op.left),
        Expression::Except(set_op) => Some(&set_op.left),
        _ => None,
    }
}

fn set_operation_right(expression: &Expression) -> Option<&Expression> {
    match unwrap_query_annotations(expression) {
        Expression::Union(set_op) => Some(&set_op.right),
        Expression::Intersect(set_op) => Some(&set_op.right),
        Expression::Except(set_op) => Some(&set_op.right),
        _ => None,
    }
}

fn select_expressions_for_query(expression: &Expression) -> Vec<&Expression> {
    match unwrap_query_annotations(expression) {
        Expression::Select(select) => select.expressions.iter().collect(),
        Expression::Union(union) => select_expressions_for_query(&union.left),
        Expression::Intersect(intersect) => select_expressions_for_query(&intersect.left),
        Expression::Except(except) => select_expressions_for_query(&except.left),
        Expression::Subquery(subquery) => select_expressions_for_query(&subquery.this),
        _ => Vec::new(),
    }
}

fn unwrap_query_annotations(expression: &Expression) -> &Expression {
    match expression {
        Expression::Annotated(annotated) => unwrap_query_annotations(&annotated.this),
        _ => expression,
    }
}

fn projection_fact<'a>(
    index: usize,
    name: Option<String>,
    projection: &Expression,
    _query: &Expression,
    scope: &'a Scope,
    dialect: DialectType,
    nullability_context: &NullabilityContext<'a>,
    cached_lineage: Option<&CachedLineageContext>,
) -> ProjectionFact {
    let inner = unwrap_projection_alias(projection);
    let is_star = projection_is_star(inner);
    let transform_kind = transform_kind(inner);
    let use_cached_lineage = cached_lineage
        .is_some_and(|cached| expression_supports_cached_column_lineage(inner, cached));
    if index == 0 {
        profile_query_analysis_stage(if use_cached_lineage {
            "projection_0_cached_lineage"
        } else {
            "projection_0_scoped_lineage"
        });
    }
    let deferred_source = direct_project_cte_dependency(inner, scope, nullability_context);
    let mut upstream = if deferred_source.is_some() {
        Vec::new()
    } else if use_cached_lineage {
        cached_terminal_references_for_expression(inner, scope, dialect, nullability_context)
    } else {
        let mut lineage = nullability_context.lineage.borrow_mut();
        if index == 0 {
            profile_query_analysis_stage("projection_0_before_scoped_lineage_new");
        }
        let prepared = lineage.entry(scope as *const Scope).or_insert_with(|| {
            nullability_context
                .project_root_lineage
                .as_ref()
                .and_then(|root| root.for_scope(scope))
                .unwrap_or_else(|| {
                    ScopedLineage::with_schema(
                        scope,
                        &[],
                        dialect,
                        nullability_context
                            .mapping_schema
                            .map(|schema| schema as &dyn Schema),
                    )
                })
        });
        if index == 0 {
            profile_query_analysis_stage("projection_0_after_scoped_lineage_new");
        }
        prepared
            .output(index)
            .map(|node| {
                if index == 0 {
                    profile_query_analysis_stage("projection_0_lineage_output");
                }
                let uncertain = lineage_uncertainty(&node, nullability_context.uncertain_columns);
                let mut references = terminal_references_from_lineage(&node);
                if let Some(confidence) = uncertain {
                    for reference in &mut references {
                        reference.confidence = confidence;
                    }
                }
                references
            })
            .ok()
            .filter(|refs| !refs.is_empty())
            .unwrap_or_else(|| fallback_column_references(inner, scope))
    };
    if let Some(confidence) = expression_uncertainty(inner, nullability_context.uncertain_columns) {
        for reference in &mut upstream {
            reference.confidence = confidence;
        }
    }
    if nullability_context.mapping_schema.is_some() {
        for reference in &mut upstream {
            if let Some(table) = &reference.table {
                if !schema_contains_column(nullability_context, table, &reference.column) {
                    reference.confidence = ReferenceConfidence::Unknown;
                }
            }
        }
    }

    let mut transform_function = transform_function_fact(inner, scope, dialect);
    if let Some(function) = &mut transform_function {
        for reference in &mut function.column_args {
            if let Some(upstream) = upstream.iter().find(|upstream| {
                upstream.column == reference.column && upstream.table == reference.table
            }) {
                reference.confidence = upstream.confidence;
            } else if upstream
                .iter()
                .any(|upstream| upstream.confidence != ReferenceConfidence::Resolved)
            {
                reference.confidence = ReferenceConfidence::Unknown;
            }
        }
    }
    let annotated_type = projection
        .inferred_type()
        .or_else(|| inner.inferred_type())
        .filter(|data_type| **data_type != DataType::Unknown)
        .and_then(|data_type| render_data_type(data_type, dialect));
    ProjectionFact {
        index,
        name,
        is_star,
        star_table: projection_star_table(inner),
        transform_kind,
        transform_function,
        cast_type: cast_type(inner, dialect),
        type_hint: annotated_type,
        nullability: nullability_context
            .scope_ids
            .get(&(scope as *const Scope))
            .map(|id| nullability_context.output(*id, index, 0))
            .unwrap_or(ProjectionNullability::Unknown),
        upstream,
        type_column_args: type_column_references(inner, scope),
        passthrough_source: deferred_source.as_ref().map(|(source, _)| source.clone()),
        passthrough_column: deferred_source.map(|(_, column)| column),
    }
}

fn direct_project_cte_dependency(
    expression: &Expression,
    scope: &Scope,
    context: &NullabilityContext<'_>,
) -> Option<(String, String)> {
    if context.project_cte_dependencies.is_empty()
        || is_set_operation(crate::scope::scope_query(&scope.expression))
    {
        return None;
    }
    let input = match expression {
        Expression::Cast(cast) | Expression::TryCast(cast) | Expression::SafeCast(cast) => {
            &cast.this
        }
        _ => expression,
    };
    let column = direct_projected_column(input)?;
    let selected = context.selected_scope(scope)?;
    let (name, source) = match &column.table {
        Some(table) if table.quoted => selected.sources.get_key_value(&table.name)?,
        Some(table) => selected
            .sources
            .iter()
            .find(|(name, _)| name.eq_ignore_ascii_case(&table.name))?,
        None if selected.sources.len() == 1 => selected.sources.iter().next()?,
        None => return None,
    };
    if source.kind != SourceKind::Cte {
        return None;
    }
    let frame = &context.scopes[*context.scope_ids.get(&(scope as *const Scope))?];
    let columns = context.source_columns(frame, name, source)?;
    if columns
        .iter()
        .filter(|candidate| candidate.name.eq_ignore_ascii_case(&column.name.name))
        .count()
        != 1
        || !columns
            .iter()
            .any(|candidate| identifiers_match(candidate, &column.name))
    {
        return None;
    }
    let name = source.lineage_name.as_ref().unwrap_or(name);
    context
        .project_cte_dependencies
        .contains(&name.to_lowercase())
        .then(|| (name.clone(), column.name.name.clone()))
}

fn schema_contains_column(context: &NullabilityContext<'_>, table: &str, column: &str) -> bool {
    let normalized_column =
        crate::schema::normalize_name(column, Some(context.dialect), false, true);
    if let Some(columns) = context.schema_columns.borrow().get(table) {
        return columns.is_empty() || columns.contains("*") || columns.contains(&normalized_column);
    }
    let columns: HashSet<String> = context
        .mapping_schema
        .and_then(|schema| schema.column_names(table).ok())
        .unwrap_or_default()
        .into_iter()
        .map(|name| crate::schema::normalize_name(&name, Some(context.dialect), false, true))
        .collect();
    let contains =
        columns.is_empty() || columns.contains("*") || columns.contains(&normalized_column);
    context
        .schema_columns
        .borrow_mut()
        .insert(table.to_string(), columns);
    contains
}

struct CachedLineageContext {
    dialect: DialectType,
    output_aliases: HashSet<String>,
    source_name: String,
    source_alias: Option<String>,
    source_columns_exact: HashSet<String>,
    source_columns_folded: HashSet<String>,
}

fn cached_lineage_context(
    query: &Expression,
    output_count: usize,
    dialect: DialectType,
    scope: &Scope,
    context: &NullabilityContext<'_>,
) -> Option<CachedLineageContext> {
    if output_count <= WIDE_PROJECTION_LINEAGE_CACHE_THRESHOLD {
        return None;
    }
    let Expression::Select(select) = unwrap_query_annotations(query) else {
        return None;
    };
    let output_aliases = select
        .expressions
        .iter()
        .filter_map(|expression| match expression {
            Expression::Alias(alias) => Some(crate::set_operation::identifier_key(
                &alias.alias,
                Some(dialect),
            )),
            _ => None,
        })
        .collect();
    let scope_id = context.scope_ids.get(&(scope as *const Scope))?;
    let selected = &context.scopes[*scope_id].selected;
    let mut sources = selected.sources.iter();
    let (source_name, source) = sources.next()?;
    if sources.next().is_some() {
        return None;
    }
    let source_columns = source_columns(source, context.mapping_schema, context.dialect);
    Some(CachedLineageContext {
        dialect,
        output_aliases,
        source_name: source_name.clone(),
        source_alias: source.alias.clone(),
        source_columns_exact: source_columns.iter().cloned().collect(),
        source_columns_folded: source_columns
            .into_iter()
            .map(|column| column.to_ascii_lowercase())
            .collect(),
    })
}

fn expression_supports_cached_column_lineage(
    expression: &Expression,
    context: &CachedLineageContext,
) -> bool {
    if expression
        .dfs()
        .any(|node| matches!(node, Expression::Subquery(_)))
    {
        return false;
    }
    !expression
        .find_all(|candidate| matches!(candidate, Expression::Column(_)))
        .into_iter()
        .any(|candidate| match candidate {
            Expression::Column(column) => {
                if let Some(table) = &column.table {
                    let matches = if table.quoted {
                        table.name == context.source_name
                    } else {
                        table.name.eq_ignore_ascii_case(&context.source_name)
                            || context
                                .source_alias
                                .as_deref()
                                .is_some_and(|alias| alias.eq_ignore_ascii_case(&table.name))
                    };
                    return !matches;
                }
                let key = crate::set_operation::identifier_key(&column.name, Some(context.dialect));
                context.output_aliases.contains(&key)
                    && if column.name.quoted {
                        !context.source_columns_exact.contains(&column.name.name)
                    } else {
                        !context
                            .source_columns_folded
                            .contains(&column.name.name.to_ascii_lowercase())
                    }
            }
            _ => false,
        })
}

fn cached_terminal_references_for_expression<'a>(
    expression: &Expression,
    scope: &'a Scope,
    dialect: DialectType,
    context: &NullabilityContext<'a>,
) -> Vec<ColumnReferenceFact> {
    let immediate_scope = context
        .scope_ids
        .get(&(scope as *const Scope))
        .map(|scope_id| &context.scopes[*scope_id].selected)
        .unwrap_or(scope);
    let immediate = fallback_column_references(expression, immediate_scope);
    let mut terminal = Vec::new();
    for reference in immediate {
        let Some(source) = reference.source_name.as_ref() else {
            terminal.push(reference);
            continue;
        };
        if reference.source_kind == SourceKind::Unknown {
            terminal.push(reference);
            continue;
        }
        let key = (
            scope as *const Scope,
            source.clone(),
            reference.column.clone(),
        );
        let cached = context.terminal_lineage.borrow().get(&key).cloned();
        let resolved = cached.unwrap_or_else(|| {
            let node = {
                let mut lineage = context.lineage.borrow_mut();
                lineage
                    .entry(scope as *const Scope)
                    .or_insert_with(|| {
                        context
                            .project_root_lineage
                            .as_ref()
                            .and_then(|root| root.for_scope(scope))
                            .unwrap_or_else(|| {
                                ScopedLineage::with_schema(
                                    scope,
                                    &[],
                                    dialect,
                                    context.mapping_schema.map(|schema| schema as &dyn Schema),
                                )
                            })
                    })
                    .column(source, &reference.column)
            };
            let uncertain = lineage_uncertainty(&node, context.uncertain_columns);
            let mut references = terminal_references_from_lineage(&node);
            if let Some(confidence) = uncertain {
                for reference in &mut references {
                    reference.confidence = confidence;
                }
            }
            context
                .terminal_lineage
                .borrow_mut()
                .insert(key, references.clone());
            references
        });
        if resolved.is_empty() {
            terminal.push(reference);
        } else {
            terminal.extend(resolved);
        }
    }
    dedupe_column_refs(terminal)
}

fn expression_uncertainty(
    expression: &Expression,
    uncertain: &HashMap<(usize, usize), ReferenceConfidence>,
) -> Option<ReferenceConfidence> {
    combine_uncertainty(
        crate::scope::walk_in_scope(expression, false).filter_map(|node| match node {
            Expression::Column(column) => column
                .span
                .or(column.name.span)
                .and_then(|span| uncertain.get(&(span.start, span.end)))
                .copied(),
            _ => None,
        }),
    )
}

fn combine_uncertainty(
    confidences: impl Iterator<Item = ReferenceConfidence>,
) -> Option<ReferenceConfidence> {
    confidences.fold(None, |current, confidence| {
        Some(
            if current == Some(ReferenceConfidence::Ambiguous)
                || confidence == ReferenceConfidence::Ambiguous
            {
                ReferenceConfidence::Ambiguous
            } else {
                confidence
            },
        )
    })
}

fn lineage_uncertainty(
    node: &LineageNode,
    uncertain: &HashMap<(usize, usize), ReferenceConfidence>,
) -> Option<ReferenceConfidence> {
    combine_uncertainty(
        node.walk()
            .filter_map(|node| expression_uncertainty(&node.expression, uncertain)),
    )
}

fn transform_function_fact(
    expression: &Expression,
    scope: &Scope,
    dialect: DialectType,
) -> Option<TransformFunctionFact> {
    if let Some(function) = transform_function_fact_for_node(expression, scope, dialect) {
        return Some(function);
    }
    let mut matches = expression
        .find_all(|candidate| transform_function_fact_for_node(candidate, scope, dialect).is_some())
        .into_iter();

    let first = matches.next()?;
    if matches.next().is_some() {
        return None;
    }

    transform_function_fact_for_node(first, scope, dialect)
}

fn transform_function_fact_for_node(
    expression: &Expression,
    scope: &Scope,
    dialect: DialectType,
) -> Option<TransformFunctionFact> {
    match expression {
        Expression::Function(function) => Some(transform_function_from_args(
            &function.name,
            &function.args,
            scope,
            dialect,
        )),
        Expression::AggregateFunction(function) => Some(transform_function_from_args(
            &function.name,
            &function.args,
            scope,
            dialect,
        )),
        Expression::ObjectAgg(function) => Some(transform_function_from_parts(
            "OBJECT_AGG",
            Vec::new(),
            vec![&function.this, &function.expression],
            scope,
            dialect,
        )),
        Expression::StringAgg(function) => {
            let mut args = vec![&function.this];
            if let Some(separator) = &function.separator {
                args.push(separator);
            }
            Some(transform_function_from_parts(
                "STRING_AGG",
                Vec::new(),
                args,
                scope,
                dialect,
            ))
        }
        Expression::ListAgg(function) => {
            let mut args = vec![&function.this];
            if let Some(separator) = &function.separator {
                args.push(separator);
            }
            Some(transform_function_from_parts(
                "LISTAGG",
                Vec::new(),
                args,
                scope,
                dialect,
            ))
        }
        Expression::Min(function) => Some(type_preserving_aggregate_function(
            "MIN",
            &function.this,
            scope,
            dialect,
        )),
        Expression::Max(function) => Some(type_preserving_aggregate_function(
            "MAX",
            &function.this,
            scope,
            dialect,
        )),
        Expression::Sum(function) => Some(type_preserving_aggregate_function(
            "SUM",
            &function.this,
            scope,
            dialect,
        )),
        Expression::Avg(function) => Some(type_preserving_aggregate_function(
            "AVG",
            &function.this,
            scope,
            dialect,
        )),
        Expression::DateTrunc(function) => Some(transform_function_from_parts(
            "DATE_TRUNC",
            vec![datetime_field_name(&function.unit)],
            vec![&function.this],
            scope,
            dialect,
        )),
        Expression::TimestampTrunc(function) => Some(transform_function_from_parts(
            "TIMESTAMP_TRUNC",
            vec![datetime_field_name(&function.unit)],
            vec![&function.this],
            scope,
            dialect,
        )),
        Expression::TimeTrunc(function) => {
            let mut args = vec![function.this.as_ref()];
            if let Some(zone) = function.zone.as_deref() {
                args.push(zone);
            }
            Some(transform_function_from_parts(
                "TIME_TRUNC",
                vec![function.unit.clone()],
                args,
                scope,
                dialect,
            ))
        }
        Expression::Extract(function) => Some(transform_function_from_parts(
            "EXTRACT",
            vec![datetime_field_name(&function.field)],
            vec![&function.this],
            scope,
            dialect,
        )),
        Expression::DateAdd(function) => Some(transform_function_from_parts(
            "DATE_ADD",
            Vec::new(),
            vec![&function.this, &function.interval],
            scope,
            dialect,
        )),
        Expression::DateSub(function) => Some(transform_function_from_parts(
            "DATE_SUB",
            Vec::new(),
            vec![&function.this, &function.interval],
            scope,
            dialect,
        )),
        Expression::DateDiff(function) => Some(transform_function_from_parts(
            "DATE_DIFF",
            Vec::new(),
            vec![&function.this, &function.expression],
            scope,
            dialect,
        )),
        _ => None,
    }
}

fn transform_function_from_args(
    name: &str,
    args: &[Expression],
    scope: &Scope,
    dialect: DialectType,
) -> TransformFunctionFact {
    let literal_args = args
        .iter()
        .filter_map(|arg| literal_argument(arg, dialect))
        .collect();
    transform_function_from_parts(name, literal_args, args.iter().collect(), scope, dialect)
}

fn transform_function_from_parts(
    name: &str,
    literal_args: Vec<String>,
    args: Vec<&Expression>,
    scope: &Scope,
    _dialect: DialectType,
) -> TransformFunctionFact {
    let column_args = dedupe_column_refs(
        args.into_iter()
            .flat_map(|arg| fallback_column_references(arg, scope))
            .collect(),
    );

    TransformFunctionFact {
        name: name.to_string(),
        literal_args,
        type_column_args: column_args.clone(),
        column_args,
    }
}

fn type_preserving_aggregate_function(
    name: &str,
    argument: &Expression,
    scope: &Scope,
    dialect: DialectType,
) -> TransformFunctionFact {
    let mut fact = transform_function_from_parts(name, Vec::new(), vec![argument], scope, dialect);
    fact.type_column_args = type_column_references(argument, scope);
    fact
}

fn type_column_references(expression: &Expression, scope: &Scope) -> Vec<ColumnReferenceFact> {
    let references = match expression {
        Expression::Alias(alias) => type_column_references(&alias.this, scope),
        Expression::Annotated(annotated) => type_column_references(&annotated.this, scope),
        Expression::Paren(paren) => type_column_references(&paren.this, scope),
        Expression::Column(_) => fallback_column_references(expression, scope),
        _ => Vec::new(),
    };
    dedupe_column_refs(references)
}

fn literal_argument(expression: &Expression, dialect: DialectType) -> Option<String> {
    match expression {
        Expression::Literal(literal) => Some(literal.value_str().to_string()),
        Expression::Boolean(boolean) => Some(boolean.value.to_string()),
        Expression::Null(_) => Some("NULL".to_string()),
        Expression::Identifier(identifier) => Some(identifier.name.clone()),
        Expression::Var(var) => Some(var.this.clone()),
        Expression::DataType(data_type) => render_data_type(data_type, dialect),
        _ => None,
    }
}

fn datetime_field_name(field: &crate::expressions::DateTimeField) -> String {
    match field {
        crate::expressions::DateTimeField::Year => "year".to_string(),
        crate::expressions::DateTimeField::Month => "month".to_string(),
        crate::expressions::DateTimeField::Day => "day".to_string(),
        crate::expressions::DateTimeField::Hour => "hour".to_string(),
        crate::expressions::DateTimeField::Minute => "minute".to_string(),
        crate::expressions::DateTimeField::Second => "second".to_string(),
        crate::expressions::DateTimeField::Millisecond => "millisecond".to_string(),
        crate::expressions::DateTimeField::Microsecond => "microsecond".to_string(),
        crate::expressions::DateTimeField::DayOfWeek => "day_of_week".to_string(),
        crate::expressions::DateTimeField::DayOfYear => "day_of_year".to_string(),
        crate::expressions::DateTimeField::Week => "week".to_string(),
        crate::expressions::DateTimeField::WeekWithModifier(modifier) => {
            format!("week({modifier})")
        }
        crate::expressions::DateTimeField::Quarter => "quarter".to_string(),
        crate::expressions::DateTimeField::Epoch => "epoch".to_string(),
        crate::expressions::DateTimeField::Timezone => "timezone".to_string(),
        crate::expressions::DateTimeField::TimezoneHour => "timezone_hour".to_string(),
        crate::expressions::DateTimeField::TimezoneMinute => "timezone_minute".to_string(),
        crate::expressions::DateTimeField::Date => "date".to_string(),
        crate::expressions::DateTimeField::Time => "time".to_string(),
        crate::expressions::DateTimeField::Custom(name) => name.clone(),
    }
}

fn unwrap_projection_alias(expression: &Expression) -> &Expression {
    match expression {
        Expression::Alias(alias) => unwrap_projection_alias(&alias.this),
        Expression::Annotated(annotated) => unwrap_projection_alias(&annotated.this),
        Expression::Paren(paren) => unwrap_projection_alias(&paren.this),
        _ => expression,
    }
}

fn projection_name(expression: &Expression) -> Option<String> {
    match expression {
        Expression::Alias(alias) => Some(alias.alias.name.clone()),
        Expression::Column(column) => Some(column.name.name.clone()),
        Expression::Identifier(identifier) => Some(identifier.name.clone()),
        Expression::Star(_) => Some("*".to_string()),
        Expression::Annotated(annotated) => projection_name(&annotated.this),
        _ => None,
    }
}

fn projection_is_star(expression: &Expression) -> bool {
    matches!(expression, Expression::Star(_))
        || matches!(expression, Expression::Column(column) if column.name.name == "*")
}

fn projection_star_table(expression: &Expression) -> Option<String> {
    match expression {
        Expression::Star(star) => star
            .table
            .as_ref()
            .map(|identifier| identifier.name.clone()),
        Expression::Column(column) if column.name.name == "*" => column
            .table
            .as_ref()
            .map(|identifier| identifier.name.clone()),
        _ => None,
    }
}

fn transform_kind(expression: &Expression) -> TransformKind {
    if projection_is_star(expression) {
        TransformKind::Star
    } else if is_cast_expression(expression) {
        TransformKind::Cast
    } else if contains_aggregate(expression) {
        TransformKind::Aggregation
    } else if matches!(
        expression,
        Expression::Column(_) | Expression::Identifier(_)
    ) {
        TransformKind::Direct
    } else if is_simple_constant(expression) {
        TransformKind::Constant
    } else {
        TransformKind::Expression
    }
}

fn is_cast_expression(expression: &Expression) -> bool {
    matches!(
        expression,
        Expression::Cast(_) | Expression::TryCast(_) | Expression::SafeCast(_)
    )
}

fn cast_type(expression: &Expression, dialect: DialectType) -> Option<String> {
    match expression {
        Expression::Cast(cast) | Expression::TryCast(cast) | Expression::SafeCast(cast) => {
            render_data_type(&cast.to, dialect)
        }
        _ => None,
    }
}

fn render_data_type(data_type: &DataType, dialect: DialectType) -> Option<String> {
    if dialect == DialectType::ClickHouse {
        return crate::generator::Generator::with_config(
            Dialect::get(dialect).generator_config().clone(),
        )
        .generate_type_hint(data_type)
        .ok();
    }
    Dialect::get(dialect)
        .generate(&Expression::DataType(data_type.clone()))
        .ok()
}

fn is_simple_constant(expression: &Expression) -> bool {
    match expression {
        Expression::Literal(_) | Expression::Boolean(_) | Expression::Null(_) => true,
        Expression::Cast(cast) | Expression::TryCast(cast) | Expression::SafeCast(cast) => {
            is_simple_constant(&cast.this)
        }
        Expression::Neg(unary) | Expression::BitwiseNot(unary) => is_simple_constant(&unary.this),
        _ => false,
    }
}

impl NullabilityContext<'_> {
    // Inference is optional metadata: cycles and excessive dependency depth
    // must yield Unknown, not recurse indefinitely or claim non-nullability.
    const MAX_DEPTH: usize = 128;

    fn output(&self, scope_id: usize, ordinal: usize, depth: usize) -> ProjectionNullability {
        let key = (scope_id, ordinal);
        if let Some(value) = self.outputs.borrow().get(&key) {
            return *value;
        }
        if depth >= Self::MAX_DEPTH || !self.resolving.borrow_mut().insert(key) {
            return ProjectionNullability::Unknown;
        }
        let value = self.output_inner(scope_id, ordinal, depth + 1);
        self.resolving.borrow_mut().remove(&key);
        self.outputs.borrow_mut().insert(key, value);
        value
    }

    fn output_inner(&self, scope_id: usize, ordinal: usize, depth: usize) -> ProjectionNullability {
        use ProjectionNullability::*;
        let frame = &self.scopes[scope_id];
        let query = crate::scope::scope_query(&frame.scope.expression);
        if let Expression::Select(select) = query {
            return select
                .expressions
                .get(ordinal)
                .map(|expression| self.expression(scope_id, expression, depth))
                .unwrap_or(Unknown);
        }
        if !is_set_operation(query) || frame.branches.len() != 2 {
            return Unknown;
        }
        let (left, right) =
            match crate::set_operation::set_operation_layout(query, Some(self.dialect)) {
                Ok(Some(layout)) => match layout.outputs.get(ordinal) {
                    Some(output) => (output.left_ordinal, output.right_ordinal),
                    None => return Unknown,
                },
                Ok(None) => (Some(ordinal), Some(ordinal)),
                Err(_) => return Unknown,
            };
        let branch = |id, ordinal: Option<usize>| {
            ordinal
                .map(|ordinal| self.output(id, ordinal, depth))
                .unwrap_or(Nullable)
        };
        let left = branch(frame.branches[0], left);
        if matches!(query, Expression::Except(_)) {
            return left;
        }
        let right = branch(frame.branches[1], right);
        if matches!(query, Expression::Intersect(_)) {
            return match (left, right) {
                (NonNull, _) | (_, NonNull) => NonNull,
                (Nullable, Nullable) => Nullable,
                _ => Unknown,
            };
        }
        match (left, right) {
            (NonNull, NonNull) => NonNull,
            (Nullable, _) | (_, Nullable) => Nullable,
            _ => Unknown,
        }
    }

    fn expression(
        &self,
        scope_id: usize,
        expression: &Expression,
        depth: usize,
    ) -> ProjectionNullability {
        use ProjectionNullability::*;
        if depth >= Self::MAX_DEPTH {
            return Unknown;
        }
        let depth = depth + 1;
        match expression {
            Expression::Alias(alias) => self.expression(scope_id, &alias.this, depth),
            Expression::Annotated(annotated) => self.expression(scope_id, &annotated.this, depth),
            Expression::Paren(paren) => self.expression(scope_id, &paren.this, depth),
            Expression::Literal(_)
            | Expression::Boolean(_)
            | Expression::Count(_)
            | Expression::CountIf(_) => NonNull,
            Expression::Null(_) => Nullable,
            Expression::Cast(cast) => self.expression(scope_id, &cast.this, depth),
            Expression::Column(column) => {
                if column.span.or(column.name.span).is_some_and(|span| {
                    self.uncertain_columns.contains_key(&(span.start, span.end))
                }) {
                    Unknown
                } else {
                    self.column(scope_id, &column.name, column.table.as_ref(), depth)
                }
            }
            Expression::Identifier(identifier) => self.column(scope_id, identifier, None, depth),
            Expression::Coalesce(func) => {
                let mut all_nullable = !func.expressions.is_empty();
                for expression in &func.expressions {
                    match self.expression(scope_id, expression, depth) {
                        NonNull => return NonNull,
                        Nullable => {}
                        Unknown => all_nullable = false,
                    }
                }
                if all_nullable {
                    Nullable
                } else {
                    Unknown
                }
            }
            _ => Unknown,
        }
    }

    fn source_scope(
        &self,
        frame: &NullabilityScope<'_>,
        name: &str,
        source: &SourceInfo,
    ) -> Option<usize> {
        if let Some(Expression::Table(table)) = frame.bindings.get(name).copied() {
            if table.schema.is_none() && table.catalog.is_none() {
                return frame
                    .ctes
                    .get(&crate::set_operation::identifier_key(
                        &table.name,
                        Some(self.dialect),
                    ))
                    .copied();
            }
        }
        if source.kind == SourceKind::DerivedTable && source.is_scope {
            return frame.derived.iter().copied().find(|id| {
                crate::scope::scope_query(&self.scopes[*id].scope.expression)
                    == crate::scope::scope_query(&source.expression)
            });
        }
        None
    }

    fn source_columns(
        &self,
        frame: &NullabilityScope<'_>,
        name: &str,
        source: &SourceInfo,
    ) -> Option<std::rc::Rc<Vec<Identifier>>> {
        let key = (frame.scope as *const Scope, name.to_string());
        if let Some(columns) = self.source_column_cache.borrow().get(&key) {
            return columns.clone();
        }
        let columns = self
            .source_columns_uncached(frame, name, source)
            .map(std::rc::Rc::new);
        self.source_column_cache
            .borrow_mut()
            .insert(key, columns.clone());
        columns
    }

    fn source_columns_uncached(
        &self,
        frame: &NullabilityScope<'_>,
        name: &str,
        source: &SourceInfo,
    ) -> Option<Vec<Identifier>> {
        if let Some(id) = self.source_scope(frame, name, source) {
            let expression = if matches!(
                frame.bindings.get(name).copied(),
                Some(Expression::Table(_))
            ) {
                &self.scopes[id].scope.expression
            } else {
                &source.expression
            };
            return crate::set_operation::query_output_identifiers(expression, Some(self.dialect))
                .ok();
        }
        if source.kind == SourceKind::Table {
            let mut resolver = crate::resolver::Resolver::new(
                &frame.selected,
                self.mapping_schema.unwrap_or(&self.empty_schema),
                false,
            );
            let columns = resolver.get_source_columns(name).ok()?;
            if columns.is_empty() || columns.iter().any(|name| name == "*") {
                return None;
            }
            Some(columns.into_iter().map(Identifier::new).collect())
        } else {
            crate::set_operation::query_output_identifiers(&source.expression, Some(self.dialect))
                .ok()
        }
    }

    fn column(
        &self,
        scope_id: usize,
        column: &Identifier,
        qualifier: Option<&Identifier>,
        depth: usize,
    ) -> ProjectionNullability {
        use ProjectionNullability::*;
        let frame = &self.scopes[scope_id];
        let column_key = crate::set_operation::identifier_key(column, Some(self.dialect));
        let same_column = |name: &Identifier| {
            crate::set_operation::identifier_key(name, Some(self.dialect)) == column_key
        };
        let binding = if let Some(qualifier) = qualifier {
            let key = crate::set_operation::identifier_key(qualifier, Some(self.dialect));
            let mut matches = frame
                .bindings
                .iter()
                .filter(|(_, expression)| {
                    expression_source_identifier(expression).is_some_and(|identifier| {
                        crate::set_operation::identifier_key(identifier, Some(self.dialect)) == key
                    })
                })
                .filter_map(|(name, _)| frame.selected.sources.get_key_value(name));
            let binding = matches.next();
            if matches.next().is_some() {
                return Unknown;
            }
            binding
        } else {
            let mut binding = None;
            for (name, source) in &frame.selected.sources {
                // An open source can also provide this column. Do not pick a
                // known source just because the other source lacks metadata.
                let Some(columns) = self.source_columns(frame, name, source) else {
                    return Unknown;
                };
                if columns.iter().any(&same_column) {
                    if binding.is_some() {
                        return Unknown;
                    }
                    binding = Some((name, source));
                }
            }
            binding
        };
        let Some((name, source)) = binding else {
            return Unknown;
        };
        if frame
            .bindings
            .get(name)
            .and_then(|expression| expression_source_identifier(expression))
            .is_some_and(|identifier| {
                frame
                    .nullable_sources
                    .contains(&crate::set_operation::identifier_key(
                        identifier,
                        Some(self.dialect),
                    ))
            })
        {
            return Nullable;
        }
        let target = self.source_scope(frame, name, source);
        if target.is_none() && source.kind == SourceKind::Table {
            let info = self.schema.and_then(|schema| {
                source_table_name(source).and_then(|table| schema.column(&table, &column.name))
            });
            return match info {
                Some(info) if info.primary_key || info.nullable == Some(false) => NonNull,
                Some(info) if info.nullable == Some(true) => Nullable,
                _ => Unknown,
            };
        }
        let Some(columns) = self.source_columns(frame, name, source) else {
            return Unknown;
        };
        let mut ordinals = columns
            .iter()
            .enumerate()
            .filter(|(_, name)| same_column(name))
            .map(|(ordinal, _)| ordinal);
        let Some(ordinal) = ordinals.next() else {
            return Unknown;
        };
        if ordinals.next().is_some() {
            return Unknown;
        }
        target
            .map(|id| self.output(id, ordinal, depth))
            .unwrap_or(Unknown)
    }
}

fn terminal_references_from_lineage(node: &LineageNode) -> Vec<ColumnReferenceFact> {
    let mut refs = Vec::new();
    let mut seen = HashSet::new();
    collect_terminal_references(node, &mut seen, &mut refs);
    refs
}

fn collect_terminal_references(
    node: &LineageNode,
    seen: &mut HashSet<ColumnReferenceFact>,
    refs: &mut Vec<ColumnReferenceFact>,
) {
    if node.downstream.is_empty() {
        if let Some(reference) = column_reference_from_lineage_node(node) {
            if seen.insert(reference.clone()) {
                refs.push(reference);
            }
        }
        return;
    }

    for child in &node.downstream {
        collect_terminal_references(child, seen, refs);
    }
}

fn column_reference_from_lineage_node(node: &LineageNode) -> Option<ColumnReferenceFact> {
    match &node.expression {
        Expression::Column(column) => {
            let source_name = non_empty_string(node.source_name.clone());
            let table =
                lineage_node_table(node).or_else(|| column.table.as_ref().map(|t| t.name.clone()));
            let confidence = if node.source_kind == SourceKind::Unknown && source_name.is_none() {
                ReferenceConfidence::Unknown
            } else {
                ReferenceConfidence::Resolved
            };
            Some(ColumnReferenceFact {
                source_name,
                source_alias: node.source_alias.clone(),
                source_kind: node.source_kind,
                table,
                column: column.name.name.clone(),
                unqualified: column.table.is_none(),
                confidence,
            })
        }
        Expression::Star(_) => Some(ColumnReferenceFact {
            source_name: non_empty_string(node.source_name.clone()),
            source_alias: node.source_alias.clone(),
            source_kind: node.source_kind,
            table: lineage_node_table(node),
            column: "*".to_string(),
            unqualified: true,
            confidence: if node.source_kind == SourceKind::Unknown {
                ReferenceConfidence::Unknown
            } else {
                ReferenceConfidence::Resolved
            },
        }),
        _ => None,
    }
}

fn lineage_node_table(node: &LineageNode) -> Option<String> {
    match &node.source {
        Expression::Table(table) => Some(table_name(table)),
        _ => None,
    }
}

fn fallback_column_references(expression: &Expression, scope: &Scope) -> Vec<ColumnReferenceFact> {
    let mut refs = Vec::new();
    let source_count = scope.sources.len();
    let single_source = if source_count == 1 {
        scope.sources.iter().next()
    } else {
        None
    };

    for column_expr in expression.find_all(|candidate| matches!(candidate, Expression::Column(_))) {
        if let Expression::Column(column) = column_expr {
            if column.name.name == "*" {
                continue;
            }
            let source = column
                .table
                .as_ref()
                .and_then(|table| scope.sources.get(&table.name));
            let (source_name, source_alias, source_kind, table, confidence) =
                if let Some(table_identifier) = &column.table {
                    if let Some(source) = source {
                        (
                            Some(table_identifier.name.clone()),
                            source.alias.clone(),
                            source.kind,
                            source_table_name(source)
                                .or_else(|| Some(table_identifier.name.clone())),
                            ReferenceConfidence::Resolved,
                        )
                    } else {
                        (
                            Some(table_identifier.name.clone()),
                            None,
                            SourceKind::Unknown,
                            Some(table_identifier.name.clone()),
                            ReferenceConfidence::Unknown,
                        )
                    }
                } else if let Some((name, source)) = single_source {
                    (
                        Some(name.clone()),
                        source.alias.clone(),
                        source.kind,
                        source_table_name(source).or_else(|| Some(name.clone())),
                        ReferenceConfidence::Resolved,
                    )
                } else if source_count > 1 {
                    (
                        None,
                        None,
                        SourceKind::Unknown,
                        None,
                        ReferenceConfidence::Ambiguous,
                    )
                } else {
                    (
                        None,
                        None,
                        SourceKind::Unknown,
                        None,
                        ReferenceConfidence::Unknown,
                    )
                };

            refs.push(ColumnReferenceFact {
                source_name,
                source_alias,
                source_kind,
                table,
                column: column.name.name.clone(),
                unqualified: column.table.is_none(),
                confidence,
            });
        }
    }

    dedupe_column_refs(refs)
}

fn dedupe_column_refs(refs: Vec<ColumnReferenceFact>) -> Vec<ColumnReferenceFact> {
    let mut seen = HashSet::new();
    let mut deduped = Vec::new();

    for reference in refs {
        if seen.insert(reference.clone()) {
            deduped.push(reference);
        }
    }

    deduped
}

fn relation_facts(
    scope: &Scope,
    mapping_schema: Option<&crate::schema::MappingSchema>,
    dialect: DialectType,
) -> Vec<RelationFact> {
    let mut relations = Vec::new();
    let mut seen = HashSet::new();
    collect_relation_facts(scope, mapping_schema, dialect, &mut seen, &mut relations);

    relations.sort_by(|left, right| {
        left.name
            .cmp(&right.name)
            .then_with(|| left.alias.cmp(&right.alias))
    });
    relations
}

fn collect_relation_facts(
    scope: &Scope,
    mapping_schema: Option<&crate::schema::MappingSchema>,
    dialect: DialectType,
    seen: &mut HashSet<String>,
    relations: &mut Vec<RelationFact>,
) {
    for relation in scope.sources.iter().map(|(source_name, source)| {
        let identity = source_table_identity(source);
        RelationFact {
            name: source
                .lineage_name
                .clone()
                .or_else(|| identity.as_ref().map(|identity| identity.name.clone()))
                .unwrap_or_else(|| source_name.clone()),
            alias: source.alias.clone().or_else(|| source_alias(source)),
            kind: source.kind,
            columns: source_columns(source, mapping_schema, dialect),
            catalog: identity
                .as_ref()
                .and_then(|identity| identity.catalog.clone()),
            schema: identity
                .as_ref()
                .and_then(|identity| identity.schema.clone()),
            table: identity
                .as_ref()
                .and_then(|identity| identity.table.clone()),
        }
    }) {
        let key = format!("{:?}|{}|{:?}", relation.kind, relation.name, relation.alias);
        if seen.insert(key) {
            relations.push(relation);
        }
    }

    for branch_scope in &scope.union_scopes {
        collect_relation_facts(branch_scope, mapping_schema, dialect, seen, relations);
    }
}

fn base_table_facts(
    scope: &Scope,
    mapping_schema: Option<&crate::schema::MappingSchema>,
    dialect: DialectType,
) -> Vec<RelationFact> {
    let mut relations = Vec::new();
    let mut seen = HashSet::new();

    collect_base_table_facts(scope, mapping_schema, dialect, &mut seen, &mut relations);

    relations.sort_by(|left, right| left.name.cmp(&right.name));
    relations
}

fn collect_base_table_facts(
    scope: &Scope,
    mapping_schema: Option<&crate::schema::MappingSchema>,
    dialect: DialectType,
    seen: &mut HashSet<String>,
    relations: &mut Vec<RelationFact>,
) {
    for source in scope.sources.values() {
        if source.kind != SourceKind::Table {
            continue;
        }

        let Some(identity) = source_table_identity(source) else {
            continue;
        };

        if seen.insert(identity.name.clone()) {
            relations.push(RelationFact {
                name: identity.name,
                alias: source.alias.clone().or_else(|| source_alias(source)),
                kind: SourceKind::Table,
                columns: source_columns(source, mapping_schema, dialect),
                catalog: identity.catalog,
                schema: identity.schema,
                table: identity.table,
            });
        }
    }

    for child_scope in scope
        .cte_scopes
        .iter()
        .chain(scope.union_scopes.iter())
        .chain(scope.table_scopes.iter())
        .chain(scope.derived_table_scopes.iter())
        .chain(scope.subquery_scopes.iter())
    {
        collect_base_table_facts(child_scope, mapping_schema, dialect, seen, relations);
    }
}

fn source_columns(
    source: &SourceInfo,
    mapping_schema: Option<&crate::schema::MappingSchema>,
    dialect: DialectType,
) -> Vec<String> {
    match source.expression.as_ref() {
        Expression::Table(table) => mapping_schema
            .and_then(|schema| schema.column_names(&table_name(table)).ok())
            .unwrap_or_default(),
        Expression::Select(_)
        | Expression::Union(_)
        | Expression::Intersect(_)
        | Expression::Except(_) => {
            get_output_column_names_for_dialect(&source.expression, Some(dialect))
        }
        Expression::Subquery(subquery) => {
            get_output_column_names_for_dialect(&subquery.this, Some(dialect))
        }
        Expression::Cte(cte) if !cte.columns.is_empty() => cte
            .columns
            .iter()
            .map(|column| column.name.clone())
            .collect(),
        Expression::Cte(cte) => get_output_column_names_for_dialect(&cte.this, Some(dialect)),
        _ => Vec::new(),
    }
}

fn source_table_name(source: &SourceInfo) -> Option<String> {
    source_table_identity(source).map(|identity| identity.name)
}

fn source_alias(source: &SourceInfo) -> Option<String> {
    match source.expression.as_ref() {
        Expression::Table(table) => table.alias.as_ref().map(|alias| alias.name.clone()),
        Expression::Subquery(subquery) => subquery.alias.as_ref().map(|alias| alias.name.clone()),
        _ => None,
    }
}

fn table_name(table: &TableRef) -> String {
    let mut parts = Vec::new();
    if let Some(catalog) = &table.catalog {
        parts.push(catalog.name.clone());
    }
    if let Some(schema) = &table.schema {
        parts.push(schema.name.clone());
    }
    parts.push(table.name.name.clone());
    parts.join(".")
}

#[derive(Debug, Clone)]
struct RelationIdentity {
    name: String,
    catalog: Option<String>,
    schema: Option<String>,
    table: Option<String>,
}

fn source_table_identity(source: &SourceInfo) -> Option<RelationIdentity> {
    match source.expression.as_ref() {
        Expression::Table(table) => Some(table_identity(table)),
        _ => None,
    }
}

fn table_identity(table: &TableRef) -> RelationIdentity {
    RelationIdentity {
        name: table_name(table),
        catalog: table.catalog.as_ref().map(|catalog| catalog.name.clone()),
        schema: table.schema.as_ref().map(|schema| schema.name.clone()),
        table: Some(table.name.name.clone()),
    }
}

fn set_operation_facts<'a>(
    expression: &Expression,
    scope: &'a Scope,
    dialect: DialectType,
    nullability: &NullabilityContext<'a>,
) -> Vec<SetOperationFact> {
    let mut facts = Vec::new();
    collect_set_operation_facts(expression, scope, dialect, nullability, &mut facts);
    facts
}

fn collect_set_operation_facts<'a>(
    expression: &Expression,
    scope: &'a Scope,
    dialect: DialectType,
    nullability: &NullabilityContext<'a>,
    facts: &mut Vec<SetOperationFact>,
) {
    match expression {
        Expression::Union(union) => {
            facts.push(SetOperationFact {
                kind: "union".to_string(),
                all: union.all,
                distinct: union.distinct,
                output_columns: get_output_column_names_for_dialect(expression, Some(dialect)),
                branches: set_operation_branches(
                    &union.left,
                    &union.right,
                    scope,
                    dialect,
                    SetOperationBranchRole::Value,
                    nullability,
                ),
            });
            collect_set_operation_facts(
                &union.left,
                scope.union_scopes.first().unwrap_or(scope),
                dialect,
                nullability,
                facts,
            );
            collect_set_operation_facts(
                &union.right,
                scope.union_scopes.get(1).unwrap_or(scope),
                dialect,
                nullability,
                facts,
            );
        }
        Expression::Intersect(intersect) => {
            facts.push(SetOperationFact {
                kind: "intersect".to_string(),
                all: intersect.all,
                distinct: intersect.distinct,
                output_columns: get_output_column_names_for_dialect(expression, Some(dialect)),
                branches: set_operation_branches(
                    &intersect.left,
                    &intersect.right,
                    scope,
                    dialect,
                    SetOperationBranchRole::Filter,
                    nullability,
                ),
            });
            collect_set_operation_facts(
                &intersect.left,
                scope.union_scopes.first().unwrap_or(scope),
                dialect,
                nullability,
                facts,
            );
            collect_set_operation_facts(
                &intersect.right,
                scope.union_scopes.get(1).unwrap_or(scope),
                dialect,
                nullability,
                facts,
            );
        }
        Expression::Except(except) => {
            facts.push(SetOperationFact {
                kind: "except".to_string(),
                all: except.all,
                distinct: except.distinct,
                output_columns: get_output_column_names_for_dialect(expression, Some(dialect)),
                branches: set_operation_branches(
                    &except.left,
                    &except.right,
                    scope,
                    dialect,
                    SetOperationBranchRole::Filter,
                    nullability,
                ),
            });
            collect_set_operation_facts(
                &except.left,
                scope.union_scopes.first().unwrap_or(scope),
                dialect,
                nullability,
                facts,
            );
            collect_set_operation_facts(
                &except.right,
                scope.union_scopes.get(1).unwrap_or(scope),
                dialect,
                nullability,
                facts,
            );
        }
        Expression::Subquery(subquery) => {
            collect_set_operation_facts(&subquery.this, scope, dialect, nullability, facts);
        }
        _ => {}
    }
}

fn set_operation_branches<'a>(
    left: &Expression,
    right: &Expression,
    scope: &'a Scope,
    dialect: DialectType,
    right_role: SetOperationBranchRole,
    nullability: &NullabilityContext<'a>,
) -> Vec<SetOperationBranchFact> {
    vec![
        SetOperationBranchFact {
            index: 0,
            role: SetOperationBranchRole::Value,
            projections: projection_facts_for_query(
                left,
                scope.union_scopes.first().unwrap_or(scope),
                dialect,
                nullability,
            ),
        },
        SetOperationBranchFact {
            index: 1,
            role: right_role,
            projections: projection_facts_for_query(
                right,
                scope.union_scopes.get(1).unwrap_or(scope),
                dialect,
                nullability,
            ),
        },
    ]
}

fn non_empty_string(value: String) -> Option<String> {
    if value.is_empty() {
        None
    } else {
        Some(value)
    }
}
