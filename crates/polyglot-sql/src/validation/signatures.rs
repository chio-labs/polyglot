use super::*;
use polyglot_sql_function_catalogs::types::{type_signature, ArgumentType};

pub(crate) fn dialect_key(dialect: DialectType) -> &'static str {
    match dialect {
        DialectType::DuckDB => "duckdb",
        DialectType::PostgreSQL => "postgres",
        DialectType::Snowflake => "snowflake",
        DialectType::BigQuery => "bigquery",
        _ => "",
    }
}

fn call(expr: &Expression) -> Option<(String, Vec<&Expression>)> {
    let args = match expr {
        Expression::Function(f) => {
            return Some((function_dispatch_name(&f.name), f.args.iter().collect()))
        }
        Expression::AggregateFunction(f) => {
            return Some((function_dispatch_name(&f.name), f.args.iter().collect()))
        }
        Expression::Sum(f)
        | Expression::Avg(f)
        | Expression::Min(f)
        | Expression::Max(f)
        | Expression::Stddev(f)
        | Expression::StddevPop(f)
        | Expression::StddevSamp(f)
        | Expression::Variance(f)
        | Expression::VarPop(f)
        | Expression::VarSamp(f) => vec![&f.this],
        Expression::Lag(f) | Expression::Lead(f) => {
            let mut args = vec![&f.this];
            args.extend(f.offset.iter());
            args.extend(f.default.iter());
            args
        }
        Expression::NTile(f) => return Some(("ntile".to_owned(), f.num_buckets.iter().collect())),
        Expression::NullIf(f) => return Some(("nullif".to_owned(), vec![&f.this, &f.expression])),
        Expression::IfNull(f) => return Some(("ifnull".to_owned(), vec![&f.this, &f.expression])),
        Expression::FirstValue(f) | Expression::LastValue(f) => vec![&f.this],
        Expression::Coalesce(f) | Expression::Greatest(f) | Expression::Least(f) => {
            f.expressions.iter().collect()
        }
        Expression::StringAgg(f) => {
            let mut args = vec![&f.this];
            args.extend(f.separator.iter());
            args
        }
        Expression::ListAgg(f) => {
            let mut args = vec![&f.this];
            args.extend(f.separator.iter());
            return Some(("listagg".to_owned(), args));
        }
        Expression::Upper(f)
        | Expression::Lower(f)
        | Expression::LTrim(f)
        | Expression::RTrim(f)
        | Expression::Abs(f)
        | Expression::Sqrt(f)
        | Expression::Length(f) => vec![&f.this],
        Expression::Substring(f) => {
            let mut args = vec![&f.this, &f.start];
            args.extend(f.length.iter());
            args
        }
        Expression::Replace(f) => vec![&f.this, &f.old, &f.new],
        Expression::Left(f) | Expression::Right(f) => vec![&f.this, &f.length],
        Expression::Round(f) => {
            let mut args = vec![&f.this];
            args.extend(f.decimals.iter());
            args
        }
        Expression::Floor(f) => {
            let mut args = vec![&f.this];
            args.extend(f.scale.iter());
            args
        }
        Expression::Ceil(f) => {
            let mut args = vec![&f.this];
            args.extend(f.decimals.iter());
            args
        }
        Expression::Power(f) => vec![&f.this, &f.expression],
        Expression::Trim(f) => {
            let mut args = vec![&f.this];
            args.extend(f.characters.iter());
            args
        }
        _ => return None,
    };
    Some((expr.variant_name().to_owned(), args))
}

pub(super) fn check(
    node: &Expression,
    dialect: DialectType,
    schema: &HashMap<String, TableSchemaEntry>,
    context: &TypeCheckContext,
    strict: bool,
    errors: &mut Vec<ValidationError>,
    catalog: Option<&dyn FunctionCatalog>,
) -> bool {
    let Some((name, mut args)) = call(node) else {
        return false;
    };
    let Some(signature) = type_signature(dialect_key(dialect), &name) else {
        return false;
    };
    if !matches!(node, Expression::Function(_))
        && catalog.is_some_and(|catalog| catalog.lookup(dialect, &name, &name).is_some())
    {
        check_named_function_catalog(&name, args.len(), dialect, catalog, strict, errors);
    }
    if name == "strftime"
        && dialect == DialectType::DuckDB
        && args.len() == 2
        && infer_expression_type_family(args[1], schema, context).is_temporal()
    {
        args.swap(0, 1);
    }
    for (index, (arg, expected)) in args.iter().zip(signature.arguments).enumerate() {
        let family = infer_expression_type_family(arg, schema, context);
        let valid = match expected {
            ArgumentType::Any => true,
            ArgumentType::Numeric => family.is_numeric(),
            ArgumentType::Integer => {
                family == TypeFamily::Integer
                    || (family.is_numeric()
                        && matches!(
                            dialect,
                            DialectType::DuckDB | DialectType::Snowflake | DialectType::PostgreSQL
                        ))
            }
            ArgumentType::String => {
                family == TypeFamily::String
                    || (family == TypeFamily::Binary
                        && matches!(name.as_str(), "substring" | "substr")
                        && matches!(dialect, DialectType::BigQuery | DialectType::PostgreSQL))
            }
            ArgumentType::Sequence => matches!(
                family,
                TypeFamily::String | TypeFamily::Binary | TypeFamily::Array
            ),
            ArgumentType::Temporal => coercion::temporal_argument(dialect, arg, family),
            ArgumentType::Boolean => predicate_compatible(family, dialect),
        };
        // Snowflake's string signatures permit scalar implicit conversion.
        let implicit = dialect == DialectType::Snowflake
            && (*expected == ArgumentType::String
                || (family == TypeFamily::String
                    && matches!(
                        expected,
                        ArgumentType::Numeric | ArgumentType::Integer | ArgumentType::Temporal
                    )));
        let literal = match expected {
            ArgumentType::Integer | ArgumentType::Numeric => {
                coercion::literal_coerces(dialect, arg, TypeFamily::Numeric)
            }
            ArgumentType::Boolean => coercion::literal_coerces(dialect, arg, TypeFamily::Boolean),
            _ => false,
        };
        let interval_aggregate = family == TypeFamily::Interval
            && matches!(name.as_str(), "sum" | "avg")
            && dialect != DialectType::DuckDB;
        let runtime_argument = dialect == DialectType::DuckDB
            && family == TypeFamily::String
            && *expected == ArgumentType::Integer
            && matches!(name.as_str(), "lag" | "lead" | "ntile");
        let target = match expected {
            ArgumentType::Numeric | ArgumentType::Integer => Some(TypeFamily::Numeric),
            ArgumentType::Temporal => Some(TypeFamily::Timestamp),
            ArgumentType::Boolean => Some(TypeFamily::Boolean),
            _ => None,
        };
        if family == TypeFamily::String
            && (valid || implicit || literal)
            && target.is_some_and(|target| {
                !coercion::string_literal(arg) || expressions::invalid_literal_for(arg, target)
            })
        {
            errors.push(type_issue(
                false,
                validation_codes::E_INVALID_FUNCTION_ARGUMENT_TYPE,
                validation_codes::W_FUNCTION_ARGUMENT_COERCION,
                format!(
                    "Function '{name}' argument {} uses a runtime conversion",
                    index + 1
                ),
            ));
        }
        check_function_argument(
            errors,
            strict && !runtime_argument,
            &name,
            index,
            family,
            &format!("{expected:?}"),
            valid || implicit || literal || interval_aggregate,
        );
    }
    if name == "avg" && dialect == DialectType::DuckDB {
        if let Some(arg) = args.first() {
            let family = infer_expression_type_family(arg, schema, context);
            check_function_argument(
                errors,
                strict,
                &name,
                0,
                family,
                "numeric or temporal",
                family.is_numeric() || family.is_temporal(),
            );
        }
    }
    if name == "sum" && dialect == DialectType::DuckDB {
        if let Some(arg) = args.first() {
            let family = infer_expression_type_family(arg, schema, context);
            check_function_argument(
                errors,
                strict,
                &name,
                0,
                family,
                "numeric or boolean",
                family.is_numeric() || family == TypeFamily::Boolean,
            );
        }
    }
    if matches!(
        name.as_str(),
        "coalesce" | "ifnull" | "nvl" | "greatest" | "least" | "nullif"
    ) {
        unify(
            &args,
            dialect,
            schema,
            context,
            strict
                && !(name == "nullif"
                    && matches!(dialect, DialectType::DuckDB | DialectType::Snowflake)),
            errors,
        );
    }
    if matches!(node, Expression::Coalesce(_)) && args.is_empty() {
        errors.push(type_issue(
            strict,
            validation_codes::E_INVALID_FUNCTION_ARITY,
            validation_codes::E_INVALID_FUNCTION_ARITY,
            "COALESCE requires at least one argument".to_owned(),
        ));
    }
    true
}

pub(super) fn unify(
    args: &[&Expression],
    dialect: DialectType,
    schema: &HashMap<String, TableSchemaEntry>,
    context: &TypeCheckContext,
    strict: bool,
    errors: &mut Vec<ValidationError>,
) {
    // Compare against a typed nonliteral, not the first NULL/string literal.
    let base = args.iter().find(|arg| {
        !coercion::string_literal(arg)
            && infer_expression_type_family(arg, schema, context) != TypeFamily::Unknown
    });
    if let Some(base) = base {
        let family = infer_expression_type_family(base, schema, context);
        for arg in args {
            let other = infer_expression_type_family(arg, schema, context);
            let boolean_numeric = dialect == DialectType::DuckDB
                && ((family == TypeFamily::Boolean && other.is_numeric())
                    || (other == TypeFamily::Boolean && family.is_numeric()));
            if !boolean_numeric && !coercion::comparable(dialect, base, arg, family, other) {
                errors.push(type_issue(
                    strict,
                    validation_codes::E_INVALID_FUNCTION_ARGUMENT_TYPE,
                    validation_codes::W_FUNCTION_ARGUMENT_COERCION,
                    format!(
                        "Cannot unify {} and {}",
                        type_family_name(family),
                        type_family_name(other)
                    ),
                ));
                break;
            }
        }
    }
}
