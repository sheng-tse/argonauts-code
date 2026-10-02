//! Starlark values and the operators on them.

use std::cell::RefCell;
use std::cmp::Ordering;
use std::collections::HashMap;
use std::rc::Rc;

use starlark_syntax::codemap::Span;
use starlark_syntax::syntax::ast::{AstExpr, AstStmt, BinOp};

use argonauts_core::workflow::StepKind;

use super::{Eval, MAX_LEN, fail};

#[derive(Clone)]
pub enum Value {
    None,
    Bool(bool),
    Int(i64),
    Float(f64),
    Str(Rc<str>),
    List(Rc<RefCell<Vec<Value>>>),
    Tuple(Rc<[Value]>),
    Dict(Rc<RefCell<Dict>>),
    Range(Range),
    Function(Rc<Function>),
    Builtin(Builtin),
    /// A method looked up on a value, such as `steps.append`, waiting for its call.
    Method(Box<Value>, Rc<str>),
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub struct Range {
    pub start: i64,
    pub stop: i64,
    pub step: i64,
}

impl Range {
    pub fn len(&self) -> i64 {
        let (start, stop, step) = (
            i128::from(self.start),
            i128::from(self.stop),
            i128::from(self.step),
        );
        let len = if step > 0 && stop > start {
            (stop - start + step - 1) / step
        } else if step < 0 && start > stop {
            (start - stop - step - 1) / -step
        } else {
            0
        };
        i64::try_from(len).unwrap_or(i64::MAX)
    }

    pub fn get(&self, index: i64) -> i64 {
        self.start + index * self.step
    }
}

/// Insertion-ordered, as Starlark's dicts are, so a script always builds the same workflow.
#[derive(Default)]
pub struct Dict {
    pub entries: Vec<(Value, Value)>,
}

impl Dict {
    pub fn get(&self, key: &Value) -> Option<&Value> {
        self.entries
            .iter()
            .find(|(k, _)| equals(k, key))
            .map(|(_, v)| v)
    }

    pub fn insert(&mut self, key: Value, value: Value) {
        match self.entries.iter_mut().find(|(k, _)| equals(k, &key)) {
            Some(entry) => entry.1 = value,
            None => self.entries.push((key, value)),
        }
    }

    pub fn remove(&mut self, key: &Value) -> Option<Value> {
        let index = self.entries.iter().position(|(k, _)| equals(k, key))?;
        Some(self.entries.remove(index).1)
    }
}

pub struct Function {
    pub name: String,
    pub params: Vec<Param>,
    pub body: Body,
    /// The enclosing function's variables when this one was defined. Module globals are looked
    /// up when called, so functions can call ones defined after them.
    pub captured: Rc<HashMap<String, Value>>,
    pub id: usize,
}

pub enum Param {
    Required(String),
    Optional(String, Value),
    Args(String),
    Kwargs(String),
    /// A bare `*`: the parameters after it are keyword-only.
    KeywordOnly,
}

pub enum Body {
    Block(AstStmt),
    Lambda(AstExpr),
}

/// The globals a script gets: Starlark's basics, the step builders, `phase`, and `workflow`.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Builtin {
    Len,
    Range,
    Str,
    Int,
    Bool,
    List,
    Tuple,
    Dict,
    Enumerate,
    Zip,
    Min,
    Max,
    Sorted,
    Reversed,
    Any,
    All,
    Fail,
    Type,
    Step(StepKind),
    Phase,
    Workflow,
}

/// Builder names for each step kind. A `workflow` step is `workflow_step`, since `workflow`
/// builds the whole workflow.
pub const STEP_BUILDERS: &[(&str, StepKind)] = &[
    ("restate", StepKind::Restate),
    ("differ", StepKind::Differ),
    ("draft", StepKind::Draft),
    ("map", StepKind::Map),
    ("fan_out", StepKind::FanOut),
    ("for_each", StepKind::ForEach),
    ("review", StepKind::Review),
    ("judge", StepKind::Judge),
    ("refute", StepKind::Refute),
    ("synthesize", StepKind::Synthesize),
    ("build", StepKind::Build),
    ("gate", StepKind::Gate),
    ("loop_until", StepKind::LoopUntil),
    ("checkpoint", StepKind::Checkpoint),
    ("merge", StepKind::Merge),
    ("workflow_step", StepKind::Workflow),
];

impl Builtin {
    pub fn named(name: &str) -> Option<Self> {
        Some(match name {
            "len" => Self::Len,
            "range" => Self::Range,
            "str" => Self::Str,
            "int" => Self::Int,
            "bool" => Self::Bool,
            "list" => Self::List,
            "tuple" => Self::Tuple,
            "dict" => Self::Dict,
            "enumerate" => Self::Enumerate,
            "zip" => Self::Zip,
            "min" => Self::Min,
            "max" => Self::Max,
            "sorted" => Self::Sorted,
            "reversed" => Self::Reversed,
            "any" => Self::Any,
            "all" => Self::All,
            "fail" => Self::Fail,
            "type" => Self::Type,
            "phase" => Self::Phase,
            "workflow" => Self::Workflow,
            _ => {
                return STEP_BUILDERS
                    .iter()
                    .find(|(builder, _)| *builder == name)
                    .map(|(_, kind)| Self::Step(*kind));
            }
        })
    }
}

impl Value {
    pub fn str(text: &str) -> Self {
        Self::Str(Rc::from(text))
    }

    pub fn list(items: Vec<Value>) -> Self {
        Self::List(Rc::new(RefCell::new(items)))
    }

    pub fn tuple(items: Vec<Value>) -> Self {
        Self::Tuple(Rc::from(items))
    }

    pub fn dict(dict: Dict) -> Self {
        Self::Dict(Rc::new(RefCell::new(dict)))
    }

    pub fn type_name(&self) -> &'static str {
        match self {
            Self::None => "NoneType",
            Self::Bool(_) => "bool",
            Self::Int(_) => "int",
            Self::Float(_) => "float",
            Self::Str(_) => "string",
            Self::List(_) => "list",
            Self::Tuple(_) => "tuple",
            Self::Dict(_) => "dict",
            Self::Range(_) => "range",
            Self::Function(_) | Self::Builtin(_) | Self::Method(..) => "function",
        }
    }

    pub fn truthy(&self) -> bool {
        match self {
            Self::None => false,
            Self::Bool(flag) => *flag,
            Self::Int(n) => *n != 0,
            Self::Float(f) => *f != 0.0,
            Self::Str(s) => !s.is_empty(),
            Self::List(items) => !items.borrow().is_empty(),
            Self::Tuple(items) => !items.is_empty(),
            Self::Dict(dict) => !dict.borrow().entries.is_empty(),
            Self::Range(range) => range.len() > 0,
            Self::Function(_) | Self::Builtin(_) | Self::Method(..) => true,
        }
    }

    pub fn hashable(&self) -> bool {
        match self {
            Self::None | Self::Bool(_) | Self::Int(_) | Self::Float(_) | Self::Str(_) => true,
            Self::Tuple(items) => items.iter().all(Value::hashable),
            Self::Function(_) | Self::Builtin(_) => true,
            _ => false,
        }
    }

    /// What `str()` gives: a string as itself, anything else as its literal.
    pub fn to_text(&self) -> String {
        match self {
            Self::Str(s) => s.to_string(),
            other => other.repr(),
        }
    }

    pub fn repr(&self) -> String {
        let join = |items: &[Value]| -> String {
            items.iter().map(Value::repr).collect::<Vec<_>>().join(", ")
        };
        match self {
            Self::None => "None".to_owned(),
            Self::Bool(true) => "True".to_owned(),
            Self::Bool(false) => "False".to_owned(),
            Self::Int(n) => n.to_string(),
            Self::Float(f) => format!("{f:?}"),
            Self::Str(s) => format!("{s:?}"),
            Self::List(items) => format!("[{}]", join(&items.borrow())),
            Self::Tuple(items) if items.len() == 1 => format!("({},)", items[0].repr()),
            Self::Tuple(items) => format!("({})", join(items)),
            Self::Dict(dict) => {
                let entries: Vec<String> = dict
                    .borrow()
                    .entries
                    .iter()
                    .map(|(k, v)| format!("{}: {}", k.repr(), v.repr()))
                    .collect();
                format!("{{{}}}", entries.join(", "))
            }
            Self::Range(r) => format!("range({}, {}, {})", r.start, r.stop, r.step),
            Self::Function(f) => format!("<function {}>", f.name),
            Self::Builtin(_) => "<built-in function>".to_owned(),
            Self::Method(_, name) => format!("<method {name}>"),
        }
    }

    /// The value as the JSON a workflow is deserialized from.
    pub fn to_json(&self) -> std::result::Result<serde_json::Value, String> {
        use serde_json::Value as Json;
        Ok(match self {
            Self::None => Json::Null,
            Self::Bool(flag) => Json::Bool(*flag),
            Self::Int(n) => Json::from(*n),
            Self::Float(f) => serde_json::Number::from_f64(*f)
                .map(Json::Number)
                .ok_or_else(|| format!("{f} isn't a number a workflow can hold"))?,
            Self::Str(s) => Json::String(s.to_string()),
            Self::List(items) => Json::Array(
                items
                    .borrow()
                    .iter()
                    .map(Value::to_json)
                    .collect::<std::result::Result<_, _>>()?,
            ),
            Self::Tuple(items) => Json::Array(
                items
                    .iter()
                    .map(Value::to_json)
                    .collect::<std::result::Result<_, _>>()?,
            ),
            Self::Dict(dict) => {
                let mut map = serde_json::Map::new();
                for (key, value) in &dict.borrow().entries {
                    let Self::Str(key) = key else {
                        return Err(format!(
                            "a workflow's dict keys are strings, not {}",
                            key.type_name()
                        ));
                    };
                    map.insert(key.to_string(), value.to_json()?);
                }
                Json::Object(map)
            }
            other => {
                return Err(format!(
                    "a {} can't be part of a workflow",
                    other.type_name()
                ));
            }
        })
    }
}

pub fn equals(a: &Value, b: &Value) -> bool {
    use Value::*;
    match (a, b) {
        (None, None) => true,
        (Bool(x), Bool(y)) => x == y,
        (Int(x), Int(y)) => x == y,
        (Float(x), Float(y)) => x == y,
        (Int(x), Float(y)) | (Float(y), Int(x)) => (*x as f64) == *y,
        (Str(x), Str(y)) => x == y,
        (List(x), List(y)) => Rc::ptr_eq(x, y) || slices_equal(&x.borrow(), &y.borrow()),
        (Tuple(x), Tuple(y)) => slices_equal(x, y),
        (Dict(x), Dict(y)) => {
            if Rc::ptr_eq(x, y) {
                return true;
            }
            let (x, y) = (x.borrow(), y.borrow());
            x.entries.len() == y.entries.len()
                && x.entries
                    .iter()
                    .all(|(k, v)| y.get(k).is_some_and(|other| equals(v, other)))
        }
        (Range(x), Range(y)) => x == y,
        (Function(x), Function(y)) => Rc::ptr_eq(x, y),
        (Builtin(x), Builtin(y)) => x == y,
        _ => false,
    }
}

fn slices_equal(a: &[Value], b: &[Value]) -> bool {
    a.len() == b.len() && a.iter().zip(b).all(|(x, y)| equals(x, y))
}

pub fn compare(a: &Value, b: &Value) -> Option<Ordering> {
    use Value::*;
    match (a, b) {
        (Int(x), Int(y)) => Some(x.cmp(y)),
        (Float(x), Float(y)) => x.partial_cmp(y),
        (Int(x), Float(y)) => (*x as f64).partial_cmp(y),
        (Float(x), Int(y)) => x.partial_cmp(&(*y as f64)),
        (Str(x), Str(y)) => Some(x.cmp(y)),
        (Bool(x), Bool(y)) => Some(x.cmp(y)),
        (List(x), List(y)) => compare_slices(&x.borrow(), &y.borrow()),
        (Tuple(x), Tuple(y)) => compare_slices(x, y),
        _ => Option::None,
    }
}

fn compare_slices(a: &[Value], b: &[Value]) -> Option<Ordering> {
    for (x, y) in a.iter().zip(b) {
        match compare(x, y)? {
            Ordering::Equal => continue,
            other => return Some(other),
        }
    }
    Some(a.len().cmp(&b.len()))
}

/// The items `for` walks. A range stays lazy, so `range(10**9)` costs nothing until it's run,
/// and the step limit stops it then.
pub enum Items {
    Values(std::vec::IntoIter<Value>),
    Range { next: i64, left: i64, step: i64 },
}

impl Iterator for Items {
    type Item = Value;

    fn next(&mut self) -> Option<Value> {
        match self {
            Self::Values(values) => values.next(),
            Self::Range { next, left, step } => {
                if *left <= 0 {
                    return None;
                }
                let value = *next;
                *left -= 1;
                *next = next.wrapping_add(*step);
                Some(Value::Int(value))
            }
        }
    }
}

pub fn items(value: &Value, span: Span) -> Eval<Items> {
    Ok(match value {
        Value::List(items) => Items::Values(items.borrow().clone().into_iter()),
        Value::Tuple(items) => Items::Values(Vec::from(&items[..]).into_iter()),
        Value::Dict(dict) => Items::Values(
            dict.borrow()
                .entries
                .iter()
                .map(|(key, _)| key.clone())
                .collect::<Vec<_>>()
                .into_iter(),
        ),
        Value::Range(range) => Items::Range {
            next: range.start,
            left: range.len(),
            step: range.step,
        },
        other => return fail(span, format!("a {} isn't iterable", other.type_name())),
    })
}

/// Collects an iterable, for builtins that need all of it at once.
pub fn collect(value: &Value, span: Span) -> Eval<Vec<Value>> {
    if let Value::Range(range) = value
        && range.len() > MAX_LEN as i64
    {
        return fail(span, "that range is too long to collect");
    }
    Ok(items(value, span)?.collect())
}

pub fn binary(op: BinOp, left: Value, right: Value, span: Span) -> Eval<Value> {
    use Value::*;
    let overflow = || fail(span, "integer overflow");
    let mismatch = |l: &Value, r: &Value| {
        fail(
            span,
            format!(
                "can't apply `{}` to {} and {}",
                op_symbol(op),
                l.type_name(),
                r.type_name()
            ),
        )
    };
    match op {
        BinOp::Equal => Ok(Bool(equals(&left, &right))),
        BinOp::NotEqual => Ok(Bool(!equals(&left, &right))),
        BinOp::Less | BinOp::Greater | BinOp::LessOrEqual | BinOp::GreaterOrEqual => {
            let Some(order) = compare(&left, &right) else {
                return mismatch(&left, &right);
            };
            Ok(Bool(match op {
                BinOp::Less => order.is_lt(),
                BinOp::Greater => order.is_gt(),
                BinOp::LessOrEqual => order.is_le(),
                _ => order.is_ge(),
            }))
        }
        BinOp::In => Ok(Bool(contains(&right, &left, span)?)),
        BinOp::NotIn => Ok(Bool(!contains(&right, &left, span)?)),
        BinOp::Add => match (&left, &right) {
            (Int(x), Int(y)) => x.checked_add(*y).map(Int).map_or_else(overflow, Ok),
            (Str(x), Str(y)) => {
                check_len(x.len() + y.len(), span)?;
                Ok(Value::str(&format!("{x}{y}")))
            }
            (List(x), List(y)) => {
                let mut items = x.borrow().clone();
                items.extend(y.borrow().iter().cloned());
                check_len(items.len(), span)?;
                Ok(Value::list(items))
            }
            (Tuple(x), Tuple(y)) => {
                let items: Vec<Value> = x.iter().chain(y.iter()).cloned().collect();
                check_len(items.len(), span)?;
                Ok(Value::tuple(items))
            }
            _ => float_op(&left, &right, |x, y| x + y).map_or_else(|| mismatch(&left, &right), Ok),
        },
        BinOp::Subtract => match (&left, &right) {
            (Int(x), Int(y)) => x.checked_sub(*y).map(Int).map_or_else(overflow, Ok),
            _ => float_op(&left, &right, |x, y| x - y).map_or_else(|| mismatch(&left, &right), Ok),
        },
        BinOp::Multiply => match (&left, &right) {
            (Int(x), Int(y)) => x.checked_mul(*y).map(Int).map_or_else(overflow, Ok),
            (Str(s), Int(n)) | (Int(n), Str(s)) => {
                let times = usize::try_from(*n).unwrap_or(0);
                check_len(s.len().saturating_mul(times), span)?;
                Ok(Value::str(&s.repeat(times)))
            }
            (List(items), Int(n)) | (Int(n), List(items)) => {
                Ok(Value::list(repeat(&items.borrow(), *n, span)?))
            }
            (Tuple(items), Int(n)) | (Int(n), Tuple(items)) => {
                Ok(Value::tuple(repeat(items, *n, span)?))
            }
            _ => float_op(&left, &right, |x, y| x * y).map_or_else(|| mismatch(&left, &right), Ok),
        },
        BinOp::Divide => {
            let (Some(x), Some(y)) = (as_float(&left), as_float(&right)) else {
                return mismatch(&left, &right);
            };
            if y == 0.0 {
                return fail(span, "division by zero");
            }
            Ok(Float(x / y))
        }
        BinOp::FloorDivide => match (&left, &right) {
            (Int(_), Int(0)) => fail(span, "division by zero"),
            (Int(x), Int(y)) => {
                let Some(quotient) = x.checked_div(*y) else {
                    return overflow();
                };
                let floor = if x % y != 0 && ((*x < 0) != (*y < 0)) {
                    quotient - 1
                } else {
                    quotient
                };
                Ok(Int(floor))
            }
            _ => {
                let (Some(x), Some(y)) = (as_float(&left), as_float(&right)) else {
                    return mismatch(&left, &right);
                };
                if y == 0.0 {
                    return fail(span, "division by zero");
                }
                Ok(Float((x / y).floor()))
            }
        },
        BinOp::Percent => match (&left, &right) {
            (Int(_), Int(0)) => fail(span, "modulo by zero"),
            (Int(x), Int(y)) => {
                let Some(rem) = x.checked_rem(*y) else {
                    return overflow();
                };
                Ok(Int(if rem != 0 && ((rem < 0) != (*y < 0)) {
                    rem + y
                } else {
                    rem
                }))
            }
            (Str(format), args) => percent_format(format, args, span),
            _ => mismatch(&left, &right),
        },
        BinOp::BitAnd | BinOp::BitOr | BinOp::BitXor | BinOp::LeftShift | BinOp::RightShift => {
            let (Int(x), Int(y)) = (&left, &right) else {
                return mismatch(&left, &right);
            };
            let shift = || u32::try_from(*y).ok().filter(|s| *s < 64);
            match op {
                BinOp::BitAnd => Ok(Int(x & y)),
                BinOp::BitOr => Ok(Int(x | y)),
                BinOp::BitXor => Ok(Int(x ^ y)),
                BinOp::LeftShift => shift()
                    .and_then(|s| x.checked_shl(s))
                    .map(Int)
                    .map_or_else(overflow, Ok),
                _ => shift().map(|s| Int(x >> s)).map_or_else(overflow, Ok),
            }
        }
        BinOp::And | BinOp::Or => fail(span, "`and` and `or` are evaluated lazily"),
    }
}

fn op_symbol(op: BinOp) -> &'static str {
    match op {
        BinOp::Or => "or",
        BinOp::And => "and",
        BinOp::Equal => "==",
        BinOp::NotEqual => "!=",
        BinOp::Less => "<",
        BinOp::Greater => ">",
        BinOp::LessOrEqual => "<=",
        BinOp::GreaterOrEqual => ">=",
        BinOp::In => "in",
        BinOp::NotIn => "not in",
        BinOp::Subtract => "-",
        BinOp::Add => "+",
        BinOp::Multiply => "*",
        BinOp::Percent => "%",
        BinOp::Divide => "/",
        BinOp::FloorDivide => "//",
        BinOp::BitAnd => "&",
        BinOp::BitOr => "|",
        BinOp::BitXor => "^",
        BinOp::LeftShift => "<<",
        BinOp::RightShift => ">>",
    }
}

fn as_float(value: &Value) -> Option<f64> {
    match value {
        Value::Int(n) => Some(*n as f64),
        Value::Float(f) => Some(*f),
        _ => None,
    }
}

fn float_op(left: &Value, right: &Value, op: impl Fn(f64, f64) -> f64) -> Option<Value> {
    Some(Value::Float(op(as_float(left)?, as_float(right)?)))
}

fn repeat(items: &[Value], times: i64, span: Span) -> Eval<Vec<Value>> {
    let times = usize::try_from(times).unwrap_or(0);
    check_len(items.len().saturating_mul(times), span)?;
    Ok(items
        .iter()
        .cloned()
        .cycle()
        .take(items.len() * times)
        .collect())
}

pub fn check_len(len: usize, span: Span) -> Eval<()> {
    if len > MAX_LEN {
        fail(
            span,
            format!("a value of {len} items is past the limit of {MAX_LEN}"),
        )
    } else {
        Ok(())
    }
}

fn contains(container: &Value, item: &Value, span: Span) -> Eval<bool> {
    Ok(match container {
        Value::List(items) => items.borrow().iter().any(|x| equals(x, item)),
        Value::Tuple(items) => items.iter().any(|x| equals(x, item)),
        Value::Dict(dict) => dict.borrow().get(item).is_some(),
        Value::Str(text) => match item {
            Value::Str(part) => text.contains(&**part),
            other => {
                return fail(
                    span,
                    format!("`in` a string needs a string, not {}", other.type_name()),
                );
            }
        },
        Value::Range(range) => match item {
            Value::Int(n) => {
                let offset = i128::from(*n) - i128::from(range.start);
                let step = i128::from(range.step);
                offset % step == 0 && (0..i128::from(range.len())).contains(&(offset / step))
            }
            _ => false,
        },
        other => return fail(span, format!("a {} can't hold anything", other.type_name())),
    })
}

/// `"%s-%d" % (a, b)`, with `%s`, `%d`, `%r`, and `%%`.
fn percent_format(format: &str, args: &Value, span: Span) -> Eval<Value> {
    let args: Vec<Value> = match args {
        Value::Tuple(items) => items.to_vec(),
        other => vec![other.clone()],
    };
    let mut args = args.into_iter();
    let mut out = String::new();
    let mut chars = format.chars();
    while let Some(c) = chars.next() {
        if c != '%' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('%') => out.push('%'),
            Some(conversion @ ('s' | 'd' | 'r')) => {
                let Some(arg) = args.next() else {
                    return fail(span, "not enough values for the format");
                };
                match (conversion, &arg) {
                    ('s', arg) => out.push_str(&arg.to_text()),
                    ('r', arg) => out.push_str(&arg.repr()),
                    (_, Value::Int(n)) => out.push_str(&n.to_string()),
                    (_, Value::Float(f)) => out.push_str(&(f.trunc() as i64).to_string()),
                    (_, other) => {
                        return fail(
                            span,
                            format!("%d needs a number, not {}", other.type_name()),
                        );
                    }
                }
            }
            other => {
                return fail(
                    span,
                    format!("`%{}` isn't a supported format", other.unwrap_or(' ')),
                );
            }
        }
    }
    if args.next().is_some() {
        return fail(span, "too many values for the format");
    }
    check_len(out.len(), span)?;
    Ok(Value::str(&out))
}
