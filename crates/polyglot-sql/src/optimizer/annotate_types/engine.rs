//! DuckDB and PostgreSQL result-type rules.
//!
//! These rules are replayed against engine-recorded result types in
//! `tests/engine_type_truth.rs`. Where the engine type depends on values or on
//! overloads that are not modelled, the rules return no type rather than a
//! plausible concrete type.

use super::{double_type, duckdb_unsigned_integer_coercion, integer_shape, TypeAnnotator};
use crate::dialects::DialectType;
use crate::expressions::{DataType, Expression, Function, Literal};
use crate::optimizer::set_operation_types::common_type;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Engine {
    DuckDb,
    Postgres,
}

impl Engine {
    pub(super) fn of(dialect: Option<DialectType>) -> Option<Self> {
        match dialect {
            Some(DialectType::DuckDB) => Some(Self::DuckDb),
            Some(DialectType::PostgreSQL) => Some(Self::Postgres),
            _ => None,
        }
    }

    fn dialect(self) -> DialectType {
        match self {
            Self::DuckDb => DialectType::DuckDB,
            Self::Postgres => DialectType::PostgreSQL,
        }
    }

    fn string(self) -> DataType {
        match self {
            Self::DuckDb => DataType::VarChar {
                length: None,
                parenthesized_length: false,
            },
            Self::Postgres => DataType::Text,
        }
    }

    /// Result type of the session-identity functions (`CURRENT_USER`, ...).
    fn identity(self) -> DataType {
        match self {
            Self::DuckDb => self.string(),
            Self::Postgres => DataType::Custom {
                name: "NAME".to_string(),
            },
        }
    }

    /// Resolve declared-type shorthands to the type the engine stores.
    pub(super) fn declared(self, data_type: DataType) -> DataType {
        match (self, data_type) {
            (Self::DuckDb, DataType::Decimal { precision, scale }) => DataType::Decimal {
                precision: Some(precision.unwrap_or(18)),
                scale: Some(scale.unwrap_or(if precision.is_some() { 0 } else { 3 })),
            },
            (
                Self::Postgres,
                DataType::Float {
                    precision,
                    scale: None,
                    real_spelling,
                },
            ) => {
                if real_spelling || precision.is_some_and(|p| p <= 24) {
                    real()
                } else {
                    double_type()
                }
            }
            (_, other) => other,
        }
    }
}

/// Numeric operand shape shared by arithmetic and unification rules.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Num {
    Int(u16, bool),
    Dec(u32, u32),
    Real,
    Double,
}

fn real() -> DataType {
    DataType::Float {
        precision: None,
        scale: None,
        real_spelling: true,
    }
}

fn numeric() -> DataType {
    DataType::Decimal {
        precision: None,
        scale: None,
    }
}

fn decimal(precision: u32, scale: u32) -> DataType {
    DataType::Decimal {
        precision: Some(precision),
        scale: Some(scale),
    }
}

fn bigint() -> DataType {
    DataType::BigInt { length: None }
}

fn int() -> DataType {
    DataType::Int {
        length: None,
        integer_spelling: false,
    }
}

fn timestamp(timezone: bool) -> DataType {
    DataType::Timestamp {
        precision: None,
        timezone,
    }
}

fn time(timezone: bool) -> DataType {
    DataType::Time {
        precision: None,
        timezone,
    }
}

fn interval() -> DataType {
    DataType::Interval {
        unit: None,
        to: None,
    }
}

fn int_type(bits: u16, unsigned: bool) -> DataType {
    match (bits, unsigned) {
        (8, false) => DataType::TinyInt { length: None },
        (16, false) => DataType::SmallInt { length: None },
        (32, false) => int(),
        (64, false) => bigint(),
        (128, false) => DataType::Int128,
        (8, true) => DataType::UInt8,
        (16, true) => DataType::UInt16,
        (32, true) => DataType::UInt32,
        (64, true) => DataType::UInt64,
        _ => DataType::UInt128,
    }
}

fn num(engine: Engine, data_type: &DataType) -> Option<Num> {
    if let Some((bits, unsigned)) = integer_shape(data_type) {
        return (bits > 0 && (engine == Engine::DuckDb || !unsigned))
            .then_some(Num::Int(bits, unsigned));
    }
    match (engine, engine.declared(data_type.clone())) {
        (_, DataType::Decimal { precision, scale }) => {
            Some(Num::Dec(precision.unwrap_or(0), scale.unwrap_or(0)))
        }
        (
            Engine::DuckDb,
            DataType::Float {
                precision: None, ..
            },
        ) => Some(Num::Real),
        (Engine::Postgres, DataType::Float { .. }) => Some(Num::Real),
        (_, DataType::Double { .. }) => Some(Num::Double),
        _ => None,
    }
}

fn is_binary(data_type: &DataType) -> bool {
    matches!(
        data_type,
        DataType::Binary { .. } | DataType::VarBinary { .. } | DataType::Blob
    )
}

fn is_string(data_type: &DataType) -> bool {
    matches!(
        data_type,
        DataType::Char { .. }
            | DataType::VarChar { .. }
            | DataType::String { .. }
            | DataType::Text
            | DataType::TextWithLength { .. }
    )
}

fn unwrap(mut expr: &Expression) -> &Expression {
    loop {
        expr = match expr {
            Expression::Paren(p) => &p.this,
            Expression::Annotated(a) => &a.this,
            _ => return expr,
        };
    }
}

/// Value of an integer literal, including a negated or parenthesized one.
fn integer_literal(expr: &Expression) -> Option<i128> {
    match unwrap(expr) {
        Expression::Literal(lit) => match lit.as_ref() {
            Literal::Number(n) if !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()) => {
                n.parse().ok()
            }
            _ => None,
        },
        Expression::Neg(op) => integer_literal(&op.this).map(|v| -v),
        _ => None,
    }
}

fn is_string_literal(expr: &Expression) -> bool {
    matches!(unwrap(expr), Expression::Literal(lit) if lit.is_string())
}

fn is_null(expr: &Expression) -> bool {
    matches!(unwrap(expr), Expression::Null(_))
}

fn fits(value: i128, bits: u16, unsigned: bool) -> bool {
    if unsigned {
        value >= 0 && (bits >= 128 || value < (1i128 << bits))
    } else if bits >= 128 {
        true
    } else {
        let limit = 1i128 << (bits - 1);
        value >= -limit && value < limit
    }
}

/// Type of a literal as bound by the engine.
pub(super) fn literal_type(engine: Engine, lit: &Literal) -> Option<DataType> {
    let Literal::Number(text) = lit else {
        return match lit {
            Literal::String(_)
            | Literal::NationalString(_)
            | Literal::EscapeString(_)
            | Literal::DollarString(_)
            | Literal::TripleQuotedString(_, _)
            | Literal::RawString(_) => Some(engine.string()),
            _ => TypeAnnotator::annotate_literal(lit),
        };
    };
    if text.contains(['e', 'E']) {
        return Some(match engine {
            Engine::DuckDb => double_type(),
            Engine::Postgres => numeric(),
        });
    }
    if let Some((whole, fraction)) = text.split_once('.') {
        if !whole
            .bytes()
            .chain(fraction.bytes())
            .all(|b| b.is_ascii_digit())
        {
            return None;
        }
        return match engine {
            Engine::Postgres => Some(numeric()),
            Engine::DuckDb => {
                let scale = fraction.len() as u32;
                let precision = (whole.len() + fraction.len()) as u32;
                (precision <= 38).then(|| decimal(precision.max(scale).max(1), scale))
            }
        };
    }
    let value: i128 = text.parse().ok()?;
    Some(if fits(value, 32, false) {
        int()
    } else if fits(value, 64, false) {
        bigint()
    } else {
        match engine {
            Engine::DuckDb => DataType::Int128,
            Engine::Postgres => numeric(),
        }
    })
}

/// DuckDB converts integers to DECIMAL(width, 0) before decimal arithmetic.
fn duckdb_decimal_shape(value: Num) -> Option<(u32, u32)> {
    match value {
        Num::Dec(p, s) => Some((p, s)),
        Num::Int(8, _) => Some((3, 0)),
        Num::Int(16, _) => Some((5, 0)),
        Num::Int(32, _) => Some((10, 0)),
        Num::Int(64, false) => Some((19, 0)),
        Num::Int(64, true) => Some((20, 0)),
        Num::Int(_, _) => Some((38, 0)),
        _ => None,
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum ArithOp {
    Add,
    Sub,
    Mul,
    Div,
    Mod,
    IntDiv,
}

/// Numeric arithmetic. `None` means the engine result is not modelled.
fn numeric_arithmetic(
    engine: Engine,
    op: ArithOp,
    (left, left_type, left_literal): (Num, &DataType, Option<i128>),
    (right, right_type, right_literal): (Num, &DataType, Option<i128>),
) -> Option<DataType> {
    if matches!(left, Num::Real | Num::Double) || matches!(right, Num::Real | Num::Double) {
        return match engine {
            Engine::Postgres if op == ArithOp::Mod => None,
            Engine::Postgres if left == Num::Real && right == Num::Real => Some(real()),
            Engine::Postgres => Some(double_type()),
            Engine::DuckDb if left == Num::Double || right == Num::Double => Some(double_type()),
            Engine::DuckDb => Some(real()),
        };
    }
    if engine == Engine::Postgres {
        return match (left, right) {
            (Num::Int(l, _), Num::Int(r, _)) if op != ArithOp::IntDiv => {
                Some(int_type(l.max(r), false))
            }
            _ if op != ArithOp::IntDiv => Some(numeric()),
            _ => None,
        };
    }
    // DuckDB `/` always binds the DOUBLE overload for exact numeric inputs.
    if op == ArithOp::Div {
        return Some(double_type());
    }
    if let (Num::Int(lb, lu), Num::Int(rb, ru)) = (left, right) {
        // Integer literals adopt the other operand's integer type when they fit.
        match (left_literal, right_literal) {
            (None, Some(value)) if fits(value, lb, lu) => return Some(left_type.clone()),
            (Some(value), None) if fits(value, rb, ru) => return Some(right_type.clone()),
            _ => {}
        }
        return duckdb_unsigned_integer_coercion(left_type, right_type, true)
            .or_else(|| Some(int_type(lb.max(rb), false)));
    }
    if op == ArithOp::IntDiv {
        return Some(double_type());
    }
    let (lp, ls) = duckdb_decimal_shape(left)?;
    let (rp, rs) = duckdb_decimal_shape(right)?;
    let max_width = lp.max(rp);
    let max_scale = ls.max(rs);
    let over_scale = (lp - ls).max(rp - rs);
    match op {
        ArithOp::Add | ArithOp::Sub => {
            let mut width = (max_scale + over_scale).max(max_width) + 1;
            if width > 18 && max_width <= 18 {
                width = 18;
            }
            Some(decimal(width.min(38), max_scale))
        }
        ArithOp::Mul => {
            let scale = ls + rs;
            if scale > 38 {
                return None;
            }
            let mut width = lp + rp;
            if width > 18 && max_width <= 18 && scale < 18 {
                width = 18;
            }
            Some(decimal(width.min(38), scale))
        }
        ArithOp::Mod => {
            let width = (max_scale + over_scale).max(max_width);
            Some(if width > 38 {
                double_type()
            } else {
                decimal(width, max_scale)
            })
        }
        ArithOp::Div | ArithOp::IntDiv => None,
    }
}

/// Date/time arithmetic overloads verified against both engines.
fn temporal_arithmetic(
    engine: Engine,
    op: ArithOp,
    left: &DataType,
    right: &DataType,
) -> Option<DataType> {
    use DataType::{Date, Interval, Time, Timestamp};
    let duck = engine == Engine::DuckDb;
    let date_offset = |t: &DataType| match num(engine, t) {
        Some(Num::Int(bits, _)) => duck || bits <= 32,
        _ => false,
    };
    let numeric_value = |t: &DataType| num(engine, t).is_some();
    match op {
        ArithOp::Add => match (left, right) {
            (Date, other) | (other, Date) if date_offset(other) => Some(Date),
            (Date, Interval { .. }) | (Interval { .. }, Date) => Some(timestamp(false)),
            (Timestamp { timezone, .. }, Interval { .. })
            | (Interval { .. }, Timestamp { timezone, .. }) => Some(timestamp(*timezone)),
            (Time { timezone, .. }, Interval { .. }) | (Interval { .. }, Time { timezone, .. })
                if duck || !*timezone =>
            {
                Some(time(*timezone))
            }
            (
                Date,
                Time {
                    timezone: false, ..
                },
            )
            | (
                Time {
                    timezone: false, ..
                },
                Date,
            ) => Some(timestamp(false)),
            (Interval { .. }, Interval { .. }) => Some(interval()),
            _ => None,
        },
        ArithOp::Sub => match (left, right) {
            (Date, other) if date_offset(other) => Some(Date),
            (Date, Date) => Some(if duck { bigint() } else { int() }),
            (Date, Interval { .. }) => Some(timestamp(false)),
            (Timestamp { timezone, .. }, Interval { .. }) => Some(timestamp(*timezone)),
            (Timestamp { timezone: l, .. }, Timestamp { timezone: r, .. }) if l == r => {
                Some(interval())
            }
            (
                Date,
                Timestamp {
                    timezone: false, ..
                },
            )
            | (
                Timestamp {
                    timezone: false, ..
                },
                Date,
            ) => Some(interval()),
            (
                Time {
                    timezone: false, ..
                },
                Time {
                    timezone: false, ..
                },
            ) if !duck => Some(interval()),
            (Time { timezone, .. }, Interval { .. }) if duck || !*timezone => Some(time(*timezone)),
            (Interval { .. }, Interval { .. }) => Some(interval()),
            _ => None,
        },
        ArithOp::Mul => match (left, right) {
            (Interval { .. }, other) | (other, Interval { .. }) if numeric_value(other) => {
                Some(interval())
            }
            _ => None,
        },
        ArithOp::Div => match (left, right) {
            (Interval { .. }, other) if numeric_value(other) => Some(interval()),
            _ => None,
        },
        _ => None,
    }
}

impl TypeAnnotator<'_> {
    fn arithmetic(
        &mut self,
        engine: Engine,
        op: ArithOp,
        left: &Expression,
        right: &Expression,
    ) -> Option<DataType> {
        let left_type = self.annotate(left)?;
        let right_type = self.annotate(right)?;
        let left_type = engine.declared(left_type);
        let right_type = engine.declared(right_type);
        match (num(engine, &left_type), num(engine, &right_type)) {
            (Some(l), Some(r)) => numeric_arithmetic(
                engine,
                op,
                (l, &left_type, integer_literal(left)),
                (r, &right_type, integer_literal(right)),
            ),
            _ => temporal_arithmetic(engine, op, &left_type, &right_type),
        }
    }

    /// Common type of CASE/COALESCE-style branches. NULL is ignored, untyped
    /// string literals adopt the other branches' type, and DuckDB integer
    /// literals adopt an integer type they fit.
    fn unify<'b>(
        &mut self,
        engine: Engine,
        values: impl IntoIterator<Item = &'b Expression>,
    ) -> Option<DataType> {
        let mut typed: Option<DataType> = None;
        let mut literals = Vec::new();
        let mut string_literal = false;
        for value in values {
            if is_null(value) {
                continue;
            }
            if is_string_literal(value) {
                string_literal = true;
                continue;
            }
            if engine == Engine::DuckDb {
                if let Some(literal) = integer_literal(value) {
                    literals.push(literal);
                    continue;
                }
            }
            let data_type = engine.declared(self.annotate(value)?);
            typed = Some(match typed {
                None => data_type,
                Some(current) => common_type(&current, &data_type, engine.dialect(), 0)?,
            });
        }
        for literal in literals {
            let literal_type =
                literal_type(engine, &Literal::Number(literal.unsigned_abs().to_string()))?;
            typed = Some(match typed {
                None => literal_type,
                Some(current) => match integer_shape(&current) {
                    Some((bits, unsigned)) if bits > 0 && fits(literal, bits, unsigned) => current,
                    _ => common_type(&current, &literal_type, engine.dialect(), 0)?,
                },
            });
        }
        match typed {
            Some(data_type) => Some(data_type),
            None if string_literal => Some(engine.string()),
            None => None,
        }
    }

    fn nullif(
        &mut self,
        engine: Engine,
        left: &Expression,
        right: &Expression,
    ) -> Option<DataType> {
        let left_type = engine.declared(self.annotate(left)?);
        if engine == Engine::DuckDb {
            return Some(left_type);
        }
        // PostgreSQL resolves the equality operator; the first argument is
        // cast when that operator promotes it.
        let right_type = engine.declared(self.annotate(right)?);
        match (num(engine, &left_type), num(engine, &right_type)) {
            (Some(Num::Real | Num::Double), _) => Some(left_type),
            (Some(_), Some(Num::Real | Num::Double)) => Some(double_type()),
            (Some(Num::Int(..)), Some(Num::Dec(..))) => Some(numeric()),
            (Some(_), Some(_)) => Some(left_type),
            _ if left_type == right_type => Some(left_type),
            _ => None,
        }
    }

    fn engine_sum(&mut self, engine: Engine, arg: &Expression) -> Option<DataType> {
        if engine == Engine::DuckDb {
            return self.annotate_sum(arg);
        }
        let arg_type = engine.declared(self.annotate(arg)?);
        match num(engine, &arg_type) {
            Some(Num::Int(bits, _)) if bits <= 32 => Some(bigint()),
            Some(Num::Int(..) | Num::Dec(..)) => Some(numeric()),
            Some(Num::Real) => Some(real()),
            Some(Num::Double) => Some(double_type()),
            None if matches!(arg_type, DataType::Interval { .. }) => Some(interval()),
            None => None,
        }
    }

    fn engine_avg(&mut self, engine: Engine, arg: &Expression) -> Option<DataType> {
        let arg_type = engine.declared(self.annotate(arg)?);
        match (engine, num(engine, &arg_type)) {
            (Engine::DuckDb, Some(_)) => Some(double_type()),
            (Engine::Postgres, Some(Num::Int(..) | Num::Dec(..))) => Some(numeric()),
            (Engine::Postgres, Some(_)) => Some(double_type()),
            (_, None) => match arg_type {
                DataType::Interval { .. } => Some(interval()),
                DataType::Date if engine == Engine::DuckDb => Some(timestamp(false)),
                DataType::Timestamp { .. } | DataType::Time { .. } if engine == Engine::DuckDb => {
                    Some(arg_type)
                }
                _ => None,
            },
        }
    }

    /// STDDEV/VARIANCE family.
    fn engine_statistic(&mut self, engine: Engine, arg: &Expression) -> Option<DataType> {
        let arg_type = engine.declared(self.annotate(arg)?);
        match (engine, num(engine, &arg_type)?) {
            (Engine::Postgres, Num::Int(..) | Num::Dec(..)) => Some(numeric()),
            _ => Some(double_type()),
        }
    }

    /// MIN/MAX. PostgreSQL aggregates drop the input's type modifier.
    fn engine_min_max(&mut self, engine: Engine, arg: &Expression) -> Option<DataType> {
        let arg_type = engine.declared(self.annotate(arg)?);
        if engine == Engine::DuckDb {
            return Some(arg_type);
        }
        Some(match arg_type {
            DataType::Decimal { .. } => numeric(),
            DataType::VarChar { .. } => DataType::VarChar {
                length: None,
                parenthesized_length: false,
            },
            DataType::Char { .. } => return None,
            DataType::Timestamp { timezone, .. } => timestamp(timezone),
            DataType::Time { timezone, .. } => time(timezone),
            other => other,
        })
    }

    fn engine_round(
        &mut self,
        engine: Engine,
        arg: &Expression,
        digits: Option<&Expression>,
    ) -> Option<DataType> {
        let arg_type = engine.declared(self.annotate(arg)?);
        let value = num(engine, &arg_type)?;
        if engine == Engine::Postgres {
            return match (value, digits) {
                (Num::Int(..) | Num::Real | Num::Double, None) => Some(double_type()),
                (Num::Int(..) | Num::Dec(..), _) => Some(numeric()),
                _ => None,
            };
        }
        match value {
            Num::Int(bits, false) => Some(int_type(bits, false)),
            Num::Int(bits, true) if bits <= 32 => Some(bigint()),
            Num::Int(64, true) => Some(DataType::Int128),
            Num::Int(..) | Num::Double => Some(double_type()),
            Num::Real => Some(real()),
            Num::Dec(p, s) => match digits {
                None => Some(decimal(p, 0)),
                Some(digits) => {
                    let digits = u32::try_from(integer_literal(digits)?).ok()?;
                    Some(decimal(p, s.min(digits)))
                }
            },
        }
    }

    fn engine_floor_ceil(&mut self, engine: Engine, arg: &Expression) -> Option<DataType> {
        let arg_type = engine.declared(self.annotate(arg)?);
        match (engine, num(engine, &arg_type)?) {
            (_, Num::Int(..)) | (_, Num::Double) | (Engine::Postgres, Num::Real) => {
                Some(double_type())
            }
            (Engine::DuckDb, Num::Real) => Some(real()),
            (Engine::DuckDb, Num::Dec(p, _)) => Some(decimal(p, 0)),
            (Engine::Postgres, Num::Dec(..)) => Some(numeric()),
        }
    }

    fn engine_sign(&mut self, engine: Engine, arg: &Expression) -> Option<DataType> {
        let arg_type = engine.declared(self.annotate(arg)?);
        match (engine, num(engine, &arg_type)?) {
            (Engine::DuckDb, _) => Some(DataType::TinyInt { length: None }),
            (Engine::Postgres, Num::Dec(..)) => Some(numeric()),
            (Engine::Postgres, _) => Some(double_type()),
        }
    }

    fn engine_abs(&mut self, engine: Engine, arg: &Expression) -> Option<DataType> {
        let arg_type = engine.declared(self.annotate(arg)?);
        match num(engine, &arg_type)? {
            Num::Dec(..) if engine == Engine::Postgres => Some(numeric()),
            _ => Some(arg_type),
        }
    }

    /// SQRT/LN/EXP/POWER: PostgreSQL keeps NUMERIC inputs exact.
    fn engine_transcendental(&mut self, engine: Engine, args: &[&Expression]) -> Option<DataType> {
        let mut decimal_input = false;
        for arg in args {
            let arg_type = engine.declared(self.annotate(arg)?);
            match num(engine, &arg_type)? {
                Num::Real | Num::Double => return Some(double_type()),
                Num::Dec(..) => decimal_input = true,
                Num::Int(..) => {}
            }
        }
        Some(if engine == Engine::Postgres && decimal_input {
            numeric()
        } else {
            double_type()
        })
    }

    fn engine_string_result(
        &mut self,
        engine: Engine,
        input: Option<&Expression>,
    ) -> Option<DataType> {
        if let Some(input) = input {
            if let Some(input_type) = self.annotate(input) {
                if is_binary(&input_type) || matches!(input_type, DataType::Array { .. }) {
                    return None;
                }
            }
        }
        Some(engine.string())
    }

    fn engine_concat(&mut self, engine: Engine, args: &[&Expression]) -> Option<DataType> {
        let mut has_string = false;
        let mut all_known = true;
        for arg in args {
            if is_string_literal(arg) {
                has_string = true;
                continue;
            }
            match self.annotate(arg) {
                Some(t)
                    if is_binary(&t)
                        || matches!(
                            t,
                            DataType::Array { .. }
                                | DataType::List { .. }
                                | DataType::JsonB
                                | DataType::Json
                        ) =>
                {
                    return None
                }
                Some(t) => has_string |= is_string(&t),
                None => all_known &= is_null(arg),
            }
        }
        (has_string || (engine == Engine::DuckDb && all_known)).then(|| engine.string())
    }

    fn engine_date_trunc(&mut self, engine: Engine, value: &Expression) -> Option<DataType> {
        match engine.declared(self.annotate(value)?) {
            DataType::Date => Some(timestamp(engine == Engine::Postgres)),
            DataType::Timestamp { timezone, .. } => Some(timestamp(timezone)),
            DataType::Interval { .. } => Some(interval()),
            _ => None,
        }
    }

    fn engine_date_part(&mut self, engine: Engine, field: &Expression) -> Option<DataType> {
        if engine == Engine::Postgres {
            return Some(double_type());
        }
        let Expression::Literal(lit) = unwrap(field) else {
            return None;
        };
        let Literal::String(name) = lit.as_ref() else {
            return None;
        };
        Some(
            if name.eq_ignore_ascii_case("epoch") || name.eq_ignore_ascii_case("julian") {
                double_type()
            } else {
                bigint()
            },
        )
    }
}

impl TypeAnnotator<'_> {
    /// Returns `Some(result)` when an engine rule owns `expr`; `result` may be
    /// `None` when the engine type is not modelled.
    pub(super) fn annotate_engine(
        &mut self,
        engine: Engine,
        expr: &Expression,
    ) -> Option<Option<DataType>> {
        let duck = engine == Engine::DuckDb;
        Some(match expr {
            Expression::Literal(lit) => literal_type(engine, lit),
            Expression::Add(op) => self.arithmetic(engine, ArithOp::Add, &op.left, &op.right),
            Expression::Sub(op) => self.arithmetic(engine, ArithOp::Sub, &op.left, &op.right),
            Expression::Mul(op) => self.arithmetic(engine, ArithOp::Mul, &op.left, &op.right),
            Expression::Div(op) => self.arithmetic(engine, ArithOp::Div, &op.left, &op.right),
            Expression::Mod(op) => self.arithmetic(engine, ArithOp::Mod, &op.left, &op.right),
            Expression::ModFunc(f) => self.arithmetic(engine, ArithOp::Mod, &f.this, &f.expression),
            Expression::IntDiv(f) if duck => {
                self.arithmetic(engine, ArithOp::IntDiv, &f.this, &f.expression)
            }
            Expression::Neg(op) => {
                let data_type = engine.declared(self.annotate(&op.this)?);
                match (engine, &data_type) {
                    (Engine::Postgres, DataType::Decimal { .. }) => Some(numeric()),
                    (_, DataType::Interval { .. }) => Some(data_type),
                    _ if num(engine, &data_type).is_some() => Some(data_type),
                    _ => None,
                }
            }
            Expression::Concat(op) => self.engine_concat(engine, &[&op.left, &op.right]),
            Expression::NullSafeEq(_) | Expression::NullSafeNeq(_) => Some(DataType::Boolean),
            Expression::Column(_) => self.annotate_default(expr).map(|t| engine.declared(t)),
            Expression::Cast(c) | Expression::TryCast(c) | Expression::SafeCast(c) => {
                Some(engine.declared(c.to.clone()))
            }
            Expression::Count(_) => Some(bigint()),
            Expression::Sum(agg) => self.engine_sum(engine, &agg.this),
            Expression::Avg(agg) => self.engine_avg(engine, &agg.this),
            Expression::Min(agg) | Expression::Max(agg) => self.engine_min_max(engine, &agg.this),
            Expression::Stddev(agg)
            | Expression::StddevPop(agg)
            | Expression::StddevSamp(agg)
            | Expression::Variance(agg)
            | Expression::VarPop(agg)
            | Expression::VarSamp(agg) => self.engine_statistic(engine, &agg.this),
            Expression::LogicalAnd(_) | Expression::LogicalOr(_) => Some(DataType::Boolean),
            Expression::StringAgg(agg) => self.engine_string_result(engine, Some(&agg.this)),
            Expression::Case(case) => self.unify(
                engine,
                case.whens
                    .iter()
                    .map(|(_, result)| result)
                    .chain(case.else_.iter()),
            ),
            Expression::Coalesce(f) | Expression::Greatest(f) | Expression::Least(f) => {
                self.unify(engine, f.expressions.iter())
            }
            Expression::IfNull(f) | Expression::Nvl(f) => {
                self.unify(engine, [&f.this, &f.expression])
            }
            Expression::NullIf(f) => self.nullif(engine, &f.this, &f.expression),
            Expression::Round(f) => self.engine_round(engine, &f.this, f.decimals.as_ref()),
            Expression::Floor(f) if f.scale.is_none() && f.to.is_none() => {
                self.engine_floor_ceil(engine, &f.this)
            }
            Expression::Ceil(f) if f.decimals.is_none() && f.to.is_none() => {
                self.engine_floor_ceil(engine, &f.this)
            }
            Expression::Floor(_) | Expression::Ceil(_) => None,
            Expression::Abs(f) => self.engine_abs(engine, &f.this),
            Expression::Sign(f) => self.engine_sign(engine, &f.this),
            Expression::Sqrt(f) | Expression::Ln(f) | Expression::Exp(f) => {
                self.engine_transcendental(engine, &[&f.this])
            }
            Expression::Power(f) => self.engine_transcendental(engine, &[&f.this, &f.expression]),
            Expression::Length(_) => Some(if duck { bigint() } else { int() }),
            Expression::StrPosition(_) => Some(if duck { bigint() } else { int() }),
            Expression::Upper(f)
            | Expression::Lower(f)
            | Expression::LTrim(f)
            | Expression::RTrim(f)
            | Expression::Reverse(f)
            | Expression::Initcap(f) => self.engine_string_result(engine, Some(&f.this)),
            Expression::Trim(f) => self.engine_string_result(engine, Some(&f.this)),
            Expression::Substring(f) => self.engine_string_result(engine, Some(&f.this)),
            Expression::Replace(f) => self.engine_string_result(engine, Some(&f.this)),
            Expression::Left(_)
            | Expression::Right(_)
            | Expression::Repeat(_)
            | Expression::Lpad(_)
            | Expression::Rpad(_)
            | Expression::ConcatWs(_) => Some(engine.string()),
            Expression::ToChar(_) if !duck => Some(DataType::Text),
            Expression::Random(_) => Some(double_type()),
            Expression::Extract(_) if !duck => Some(numeric()),
            Expression::DateTrunc(f) | Expression::TimestampTrunc(f) => {
                self.engine_date_trunc(engine, &f.this)
            }
            Expression::CurrentDate(_) => Some(DataType::Date),
            Expression::CurrentTime(_) => Some(time(true)),
            Expression::CurrentTimestamp(_) => Some(timestamp(true)),
            Expression::Localtime(_) => Some(time(false)),
            Expression::Localtimestamp(_) => Some(timestamp(false)),
            Expression::CurrentUser(_)
            | Expression::SessionUser(_)
            | Expression::CurrentSchema(_) => Some(engine.identity()),
            Expression::RowNumber(_) | Expression::Rank(_) | Expression::DenseRank(_) => {
                Some(bigint())
            }
            Expression::Lag(f) | Expression::Lead(f) => {
                self.annotate(&f.this).map(|t| engine.declared(t))
            }
            Expression::Epoch(_) if duck => Some(double_type()),
            Expression::Year(_)
            | Expression::Month(_)
            | Expression::Day(_)
            | Expression::Hour(_)
            | Expression::Minute(_)
            | Expression::Second(_)
                if duck =>
            {
                Some(bigint())
            }
            Expression::Uuid(_) => Some(DataType::Uuid),
            Expression::Function(func) => self.annotate_engine_function(engine, func)?,
            _ => return None,
        })
    }

    /// Named-function rules. Recognized names return `Some` even when the
    /// arguments are missing, so relation outputs treat them as modelled.
    pub(super) fn annotate_engine_function(
        &mut self,
        engine: Engine,
        func: &Function,
    ) -> Option<Option<DataType>> {
        if func.quoted {
            return None;
        }
        let duck = engine == Engine::DuckDb;
        let args = &func.args;
        let first = args.first();
        let name = func.name.to_ascii_uppercase();
        Some(match name.as_str() {
            "COUNT" => Some(bigint()),
            "SUM" => first.and_then(|a| self.engine_sum(engine, a)),
            "AVG" => first.and_then(|a| self.engine_avg(engine, a)),
            "MIN" | "MAX" if args.len() == 1 => first.and_then(|a| self.engine_min_max(engine, a)),
            "STDDEV" | "STDDEV_SAMP" | "STDDEV_POP" | "VARIANCE" | "VAR_SAMP" | "VAR_POP" => {
                first.and_then(|a| self.engine_statistic(engine, a))
            }
            "BOOL_AND" | "BOOL_OR" | "EVERY" => Some(DataType::Boolean),
            "ROUND" if args.len() <= 2 => {
                first.and_then(|a| self.engine_round(engine, a, args.get(1)))
            }
            "ABS" => first.and_then(|a| self.engine_abs(engine, a)),
            "FLOOR" | "CEIL" | "CEILING" if args.len() <= 1 => {
                first.and_then(|a| self.engine_floor_ceil(engine, a))
            }
            "SIGN" => first.and_then(|a| self.engine_sign(engine, a)),
            "SQRT" | "LN" | "EXP" | "POWER" | "POW" if !args.is_empty() => {
                let args: Vec<_> = args.iter().collect();
                self.engine_transcendental(engine, &args)
            }
            "MOD" if args.len() == 2 => self.arithmetic(engine, ArithOp::Mod, &args[0], &args[1]),
            "LENGTH" | "CHAR_LENGTH" | "CHARACTER_LENGTH" | "STRPOS" => {
                Some(if duck { bigint() } else { int() })
            }
            "UPPER" | "LOWER" | "TRIM" | "LTRIM" | "RTRIM" | "REPLACE" | "SUBSTRING" | "SUBSTR"
            | "LEFT" | "RIGHT" | "REVERSE" | "REPEAT" | "LPAD" | "RPAD" | "INITCAP" | "MD5" => {
                self.engine_string_result(engine, first)
            }
            "STRING_AGG" => self.engine_string_result(engine, first),
            "CONCAT" | "CONCAT_WS" if !duck => Some(DataType::Text),
            "CONCAT" => {
                let args: Vec<_> = args.iter().collect();
                self.engine_concat(engine, &args)
            }
            "CONCAT_WS" => Some(engine.string()),
            "TO_CHAR" if !duck => Some(DataType::Text),
            "STRFTIME" | "DAYNAME" | "MONTHNAME" if duck => Some(engine.string()),
            "NOW" | "CURRENT_TIMESTAMP" | "TRANSACTION_TIMESTAMP" => Some(timestamp(true)),
            "GET_CURRENT_TIMESTAMP" if duck => Some(timestamp(true)),
            "STATEMENT_TIMESTAMP" | "CLOCK_TIMESTAMP" if !duck => Some(timestamp(true)),
            "CURRENT_TIME" => Some(time(true)),
            "LOCALTIME" => Some(time(false)),
            "LOCALTIMESTAMP" => Some(timestamp(false)),
            "CURRENT_DATE" => Some(DataType::Date),
            "TODAY" if duck => Some(DataType::Date),
            "USER" | "CURRENT_USER" | "SESSION_USER" | "CURRENT_ROLE" | "CURRENT_SCHEMA"
            | "CURRENT_CATALOG" | "CURRENT_DATABASE"
                if args.is_empty() =>
            {
                Some(engine.identity())
            }
            "GEN_RANDOM_UUID" => Some(DataType::Uuid),
            "UUID" if duck => Some(DataType::Uuid),
            "RANDOM" => Some(double_type()),
            "DATE_TRUNC" if args.len() == 2 => self.engine_date_trunc(engine, &args[1]),
            "DATE_PART" if args.len() == 2 => self.engine_date_part(engine, &args[0]),
            "AGE" if !duck => Some(interval()),
            "EPOCH" if duck => Some(double_type()),
            "YEAR" | "MONTH" | "DAY" | "HOUR" | "MINUTE" | "SECOND" if duck => Some(bigint()),
            "NULLIF" if args.len() == 2 => self.nullif(engine, &args[0], &args[1]),
            "COALESCE" | "GREATEST" | "LEAST" => self.unify(engine, args.iter()),
            "IFNULL" if duck => self.unify(engine, args.iter()),
            "LIST_VALUE" | "LIST_PACK" if duck => {
                self.unify(engine, args.iter())
                    .map(|element_type| DataType::Array {
                        element_type: Box::new(element_type),
                        dimension: None,
                    })
            }
            "ROW_NUMBER" | "RANK" | "DENSE_RANK" => Some(bigint()),
            "LAG" | "LEAD" | "FIRST_VALUE" | "LAST_VALUE" => first
                .and_then(|a| self.annotate(a))
                .map(|t| engine.declared(t)),
            _ => return None,
        })
    }
}

#[cfg(test)]
mod tests {
    use crate::expressions::{DataType, Expression};
    use crate::optimizer::annotate_types::annotate_types;
    use crate::{parse_one, DialectType, MappingSchema, Schema};

    fn projection_type(sql: &str, dialect: DialectType) -> Option<DataType> {
        let mut schema = MappingSchema::with_dialect(dialect);
        let columns = [
            ("quantity", "INTEGER"),
            ("line_count", "BIGINT"),
            ("unit_price", "DECIMAL(9,2)"),
            ("weight", "DOUBLE PRECISION"),
        ];
        let columns: Vec<_> = columns
            .iter()
            .map(|(name, ty)| {
                let Expression::Cast(cast) =
                    parse_one(&format!("CAST(NULL AS {ty})"), dialect).unwrap()
                else {
                    panic!("cast")
                };
                (name.to_string(), cast.to)
            })
            .collect();
        schema.add_table("orders", &columns, Some(dialect)).unwrap();
        let mut expression = parse_one(sql, dialect).unwrap();
        annotate_types(&mut expression, Some(&schema), Some(dialect));
        let Expression::Select(select) = expression else {
            panic!("select")
        };
        select.expressions[0].inferred_type().cloned()
    }

    fn double() -> DataType {
        DataType::Double {
            precision: None,
            scale: None,
        }
    }

    #[test]
    fn duckdb_division_always_binds_double() {
        for expr in [
            "SUM(quantity) / COUNT(*)",
            "7 / 2",
            "quantity / line_count",
            "unit_price / unit_price",
            "CAST(10 AS HUGEINT) / 2",
            "line_count / 2",
        ] {
            let sql = format!("SELECT {expr} AS v FROM orders");
            assert_eq!(
                projection_type(&sql, DialectType::DuckDB),
                Some(double()),
                "{expr}"
            );
        }
    }

    #[test]
    fn duckdb_integer_division_and_postgres_division_stay_integral() {
        let sql = "SELECT quantity // 2 AS v FROM orders";
        assert!(matches!(
            projection_type(sql, DialectType::DuckDB),
            Some(DataType::Int { .. })
        ));
        let sql = "SELECT line_count // quantity AS v FROM orders";
        assert!(matches!(
            projection_type(sql, DialectType::DuckDB),
            Some(DataType::BigInt { .. })
        ));
        let sql = "SELECT SUM(quantity) / COUNT(*) AS v FROM orders";
        assert!(matches!(
            projection_type(sql, DialectType::PostgreSQL),
            Some(DataType::BigInt { .. })
        ));
        let sql = "SELECT unit_price / 2 AS v FROM orders";
        assert_eq!(
            projection_type(sql, DialectType::PostgreSQL),
            Some(DataType::Decimal {
                precision: None,
                scale: None
            })
        );
    }

    #[test]
    fn duckdb_integer_division_round_trips() {
        let expression =
            parse_one("SELECT quantity // 2 FROM orders", DialectType::DuckDB).unwrap();
        assert_eq!(
            crate::generate(&expression, DialectType::DuckDB).unwrap(),
            "SELECT quantity // 2 FROM orders"
        );
    }

    #[test]
    fn bare_niladic_keywords_are_typed_functions() {
        let varchar = DataType::VarChar {
            length: None,
            parenthesized_length: false,
        };
        let name = DataType::Custom {
            name: "NAME".into(),
        };
        for keyword in [
            "USER",
            "CURRENT_USER",
            "SESSION_USER",
            "CURRENT_ROLE",
            "CURRENT_SCHEMA",
            "CURRENT_CATALOG",
        ] {
            let sql = format!("SELECT {keyword} AS v FROM orders");
            assert_eq!(
                projection_type(&sql, DialectType::DuckDB),
                Some(varchar.clone()),
                "{keyword}"
            );
            assert_eq!(
                projection_type(&sql, DialectType::PostgreSQL),
                Some(name.clone()),
                "{keyword}"
            );
        }
        for dialect in [DialectType::DuckDB, DialectType::PostgreSQL] {
            let typed = |keyword: &str| {
                projection_type(&format!("SELECT {keyword} AS v FROM orders"), dialect)
            };
            assert_eq!(typed("CURRENT_DATE"), Some(DataType::Date));
            assert_eq!(
                typed("CURRENT_TIMESTAMP"),
                Some(DataType::Timestamp {
                    precision: None,
                    timezone: true
                })
            );
            assert_eq!(
                typed("LOCALTIMESTAMP"),
                Some(DataType::Timestamp {
                    precision: None,
                    timezone: false
                })
            );
            assert_eq!(
                typed("LOCALTIME"),
                Some(DataType::Time {
                    precision: None,
                    timezone: false
                })
            );
        }
    }

    #[test]
    fn bare_user_stays_a_column_where_it_is_not_a_keyword() {
        for dialect in [DialectType::DuckDB, DialectType::PostgreSQL] {
            for sql in [
                "SELECT orders.user FROM orders",
                "SELECT \"user\" FROM orders",
            ] {
                let Expression::Select(select) = parse_one(sql, dialect).unwrap() else {
                    panic!("select")
                };
                assert!(
                    matches!(select.expressions[0], Expression::Column(_)),
                    "{sql}"
                );
            }
        }
        let Expression::Select(select) =
            parse_one("SELECT user FROM orders", DialectType::MySQL).unwrap()
        else {
            panic!("select")
        };
        assert!(matches!(select.expressions[0], Expression::Column(_)));
    }

    #[test]
    fn unmodelled_values_are_unknown_not_guessed() {
        for sql in [
            "SELECT unknown_udf(quantity) AS v FROM orders",
            "SELECT quantity + missing_column AS v FROM orders",
            "SELECT unit_price % CAST(1 AS HUGEINT) + unknown_udf(1) AS v FROM orders",
        ] {
            assert_eq!(projection_type(sql, DialectType::DuckDB), None, "{sql}");
        }
    }
}
