//! The globals a script can call, the methods on its values, and indexing.

use std::cmp::Ordering;
use std::rc::Rc;

use starlark_syntax::codemap::Span;

use argonauts_core::workflow::StepKind;

use super::interp::{Args, Interp};
pub use super::value::check_len;
use super::value::{Builtin, Dict, Range, Value, collect, compare, equals};
use super::{Eval, fail};

/// The fields a step builder takes, as core's `Step` names them.
const STEP_FIELDS: &[&str] = &[
    "name",
    "role",
    "roles",
    "workers",
    "count",
    "blind",
    "different_family_from",
    "when",
    "task",
    "input",
    "steps",
    "until",
    "max",
    "checkpoint",
    "workflow",
    "params",
];

const LIST_METHODS: &[&str] = &[
    "append", "extend", "insert", "pop", "remove", "index", "clear",
];
const DICT_METHODS: &[&str] = &[
    "get",
    "keys",
    "values",
    "items",
    "pop",
    "setdefault",
    "update",
    "clear",
];
const STRING_METHODS: &[&str] = &[
    "join",
    "upper",
    "lower",
    "strip",
    "lstrip",
    "rstrip",
    "startswith",
    "endswith",
    "replace",
    "split",
    "format",
    "elems",
];

pub fn call(interp: &mut Interp, builtin: Builtin, args: Args, span: Span) -> Eval<Value> {
    match builtin {
        Builtin::Len => {
            let [x] = bind::<1>(args, ["x"], 1, "len", span)?;
            let len = match x.unwrap_or(Value::None) {
                Value::Str(s) => s.chars().count() as i64,
                Value::List(items) => items.borrow().len() as i64,
                Value::Tuple(items) => items.len() as i64,
                Value::Dict(dict) => dict.borrow().entries.len() as i64,
                Value::Range(range) => range.len(),
                other => return fail(span, format!("a {} has no length", other.type_name())),
            };
            Ok(Value::Int(len))
        }
        Builtin::Range => {
            if !args.named.is_empty() {
                return fail(span, "`range` takes no keyword arguments");
            }
            let mut ints = Vec::new();
            for value in args.positional {
                let Value::Int(n) = value else {
                    return fail(
                        span,
                        format!("`range` needs ints, not {}", value.type_name()),
                    );
                };
                ints.push(n);
            }
            let (start, stop, step) = match ints.as_slice() {
                [stop] => (0, *stop, 1),
                [start, stop] => (*start, *stop, 1),
                [start, stop, step] => (*start, *stop, *step),
                _ => return fail(span, "`range` takes one to three ints"),
            };
            if step == 0 {
                return fail(span, "`range` can't step by 0");
            }
            Ok(Value::Range(Range { start, stop, step }))
        }
        Builtin::Str => {
            let [x] = bind::<1>(args, ["x"], 1, "str", span)?;
            let text = x.unwrap_or(Value::None).to_text();
            check_len(text.len(), span)?;
            Ok(Value::str(&text))
        }
        Builtin::Int => {
            let [x] = bind::<1>(args, ["x"], 1, "int", span)?;
            match x.unwrap_or(Value::None) {
                Value::Int(n) => Ok(Value::Int(n)),
                Value::Bool(flag) => Ok(Value::Int(i64::from(flag))),
                Value::Float(f) if f.is_finite() && f.abs() < 9.2e18 => {
                    Ok(Value::Int(f.trunc() as i64))
                }
                Value::Str(s) => s
                    .trim()
                    .parse()
                    .map(Value::Int)
                    .map_or_else(|_| fail(span, format!("{s:?} isn't an int")), Ok),
                other => fail(span, format!("`int` can't convert a {}", other.type_name())),
            }
        }
        Builtin::Bool => {
            let [x] = bind::<1>(args, ["x"], 0, "bool", span)?;
            Ok(Value::Bool(x.is_some_and(|x| x.truthy())))
        }
        Builtin::List | Builtin::Tuple => {
            let what = if builtin == Builtin::List {
                "list"
            } else {
                "tuple"
            };
            let [x] = bind::<1>(args, ["x"], 0, what, span)?;
            let items = match x {
                Some(x) => collect(&x, span)?,
                None => Vec::new(),
            };
            Ok(if builtin == Builtin::List {
                Value::list(items)
            } else {
                Value::tuple(items)
            })
        }
        Builtin::Dict => {
            if args.positional.len() > 1 {
                return fail(span, "`dict` takes at most one positional argument");
            }
            let mut dict = Dict::default();
            if let Some(source) = args.positional.into_iter().next() {
                update(&mut dict, &source, span)?;
            }
            for (key, value) in args.named {
                dict.insert(Value::str(&key), value);
            }
            Ok(Value::dict(dict))
        }
        Builtin::Enumerate => {
            let [x, start] = bind::<2>(args, ["x", "start"], 1, "enumerate", span)?;
            let start = match start {
                Some(Value::Int(n)) => n,
                None => 0,
                Some(other) => {
                    return fail(
                        span,
                        format!("`start` is an int, not {}", other.type_name()),
                    );
                }
            };
            let items = collect(&x.unwrap_or(Value::None), span)?;
            Ok(Value::list(
                items
                    .into_iter()
                    .enumerate()
                    .map(|(i, item)| Value::tuple(vec![Value::Int(start + i as i64), item]))
                    .collect(),
            ))
        }
        Builtin::Zip => {
            let lists = args
                .positional
                .iter()
                .map(|value| collect(value, span))
                .collect::<Eval<Vec<_>>>()?;
            let len = lists.iter().map(Vec::len).min().unwrap_or(0);
            Ok(Value::list(
                (0..len)
                    .map(|i| Value::tuple(lists.iter().map(|list| list[i].clone()).collect()))
                    .collect(),
            ))
        }
        Builtin::Min | Builtin::Max => {
            let items = match args.positional.as_slice() {
                [single] => collect(single, span)?,
                _ => args.positional,
            };
            let wanted = if builtin == Builtin::Min {
                Ordering::Less
            } else {
                Ordering::Greater
            };
            let mut best: Option<Value> = None;
            for item in items {
                best = Some(match best {
                    None => item,
                    Some(current) => match compare(&item, &current) {
                        Some(order) if order == wanted => item,
                        Some(_) => current,
                        None => return fail(span, "those values can't be compared"),
                    },
                });
            }
            best.map_or_else(|| fail(span, "`min` and `max` need at least one value"), Ok)
        }
        Builtin::Sorted => {
            let [x, reverse] = bind::<2>(args, ["x", "reverse"], 1, "sorted", span)?;
            let mut items = collect(&x.unwrap_or(Value::None), span)?;
            let mut incomparable = false;
            items.sort_by(|a, b| {
                compare(a, b).unwrap_or_else(|| {
                    incomparable = true;
                    Ordering::Equal
                })
            });
            if incomparable {
                return fail(span, "those values can't be compared");
            }
            if reverse.is_some_and(|r| r.truthy()) {
                items.reverse();
            }
            Ok(Value::list(items))
        }
        Builtin::Reversed => {
            let [x] = bind::<1>(args, ["x"], 1, "reversed", span)?;
            let mut items = collect(&x.unwrap_or(Value::None), span)?;
            items.reverse();
            Ok(Value::list(items))
        }
        Builtin::Any | Builtin::All => {
            let [x] = bind::<1>(args, ["x"], 1, "any", span)?;
            let items = collect(&x.unwrap_or(Value::None), span)?;
            Ok(Value::Bool(if builtin == Builtin::Any {
                items.iter().any(Value::truthy)
            } else {
                items.iter().all(Value::truthy)
            }))
        }
        Builtin::Fail => {
            let message: Vec<String> = args.positional.iter().map(Value::to_text).collect();
            fail(span, format!("fail: {}", message.join(" ")))
        }
        Builtin::Type => {
            let [x] = bind::<1>(args, ["x"], 1, "type", span)?;
            Ok(Value::str(x.unwrap_or(Value::None).type_name()))
        }
        Builtin::Phase => {
            let [name, standard, steps] =
                bind::<3>(args, ["name", "standard", "steps"], 2, "phase", span)?;
            let mut phase = Dict::default();
            phase.insert(Value::str("name"), name.unwrap_or(Value::None));
            phase.insert(Value::str("standard"), standard.unwrap_or(Value::None));
            phase.insert(
                Value::str("steps"),
                steps.unwrap_or_else(|| Value::list(Vec::new())),
            );
            Ok(Value::dict(phase))
        }
        Builtin::Workflow => {
            let [name, phases, description, params] = bind::<4>(
                args,
                ["name", "phases", "description", "params"],
                2,
                "workflow",
                span,
            )?;
            let mut workflow = Dict::default();
            workflow.insert(Value::str("name"), name.unwrap_or(Value::None));
            workflow.insert(Value::str("phases"), phases.unwrap_or(Value::None));
            if let Some(description) = description {
                workflow.insert(Value::str("description"), description);
            }
            if let Some(params) = params {
                workflow.insert(Value::str("params"), params);
            }
            interp.set_workflow(Value::dict(workflow), span)?;
            Ok(Value::None)
        }
        Builtin::Step(kind) => step(kind, args, span),
    }
}

/// A step from its builder: keyword arguments only, each a field of core's `Step`.
fn step(kind: StepKind, args: Args, span: Span) -> Eval<Value> {
    let builder = super::value::STEP_BUILDERS
        .iter()
        .find(|(_, k)| *k == kind)
        .map_or("step", |(name, _)| *name);
    if !args.positional.is_empty() {
        return fail(span, format!("`{builder}` takes its fields by keyword"));
    }
    let kind_name = serde_json::to_value(kind)
        .ok()
        .and_then(|value| value.as_str().map(str::to_owned))
        .unwrap_or_default();
    let mut step = Dict::default();
    step.insert(Value::str("kind"), Value::str(&kind_name));
    for (field, value) in args.named {
        if !STEP_FIELDS.contains(&field.as_str()) {
            return fail(span, format!("`{builder}` has no field `{field}`"));
        }
        if !matches!(value, Value::None) {
            step.insert(Value::str(&field), value);
        }
    }
    Ok(Value::dict(step))
}

/// Binds a builtin's arguments to `names`, positional ones first. The first `required` must be
/// given.
fn bind<const N: usize>(
    args: Args,
    names: [&str; N],
    required: usize,
    what: &str,
    span: Span,
) -> Eval<[Option<Value>; N]> {
    let mut out: [Option<Value>; N] = std::array::from_fn(|_| None);
    if args.positional.len() > N {
        return fail(span, format!("`{what}` takes at most {N} arguments"));
    }
    for (slot, value) in out.iter_mut().zip(args.positional) {
        *slot = Some(value);
    }
    for (name, value) in args.named {
        let Some(index) = names.iter().position(|n| *n == name) else {
            return fail(span, format!("`{what}` has no argument `{name}`"));
        };
        if out[index].is_some() {
            return fail(span, format!("`{what}` got `{name}` twice"));
        }
        out[index] = Some(value);
    }
    for (name, slot) in names.iter().zip(&out).take(required) {
        if slot.is_none() {
            return fail(span, format!("`{what}` needs `{name}`"));
        }
    }
    Ok(out)
}

pub fn attribute(object: Value, name: &str, span: Span) -> Eval<Value> {
    let methods = match &object {
        Value::List(_) => LIST_METHODS,
        Value::Dict(_) => DICT_METHODS,
        Value::Str(_) => STRING_METHODS,
        _ => &[],
    };
    if methods.contains(&name) {
        Ok(Value::Method(Box::new(object), Rc::from(name)))
    } else {
        fail(span, format!("a {} has no `{name}`", object.type_name()))
    }
}

pub fn method(object: Value, name: &str, args: Args, span: Span) -> Eval<Value> {
    match &object {
        Value::List(list) => match name {
            "append" => {
                let [x] = bind::<1>(args, ["x"], 1, "append", span)?;
                let x = x.unwrap_or(Value::None);
                check_store(&object, std::slice::from_ref(&x), span)?;
                let mut list = list.borrow_mut();
                list.push(x);
                check_len(list.len(), span)?;
                Ok(Value::None)
            }
            "extend" => {
                let [x] = bind::<1>(args, ["x"], 1, "extend", span)?;
                let more = collect(&x.unwrap_or(Value::None), span)?;
                check_store(&object, &more, span)?;
                let mut list = list.borrow_mut();
                list.extend(more);
                check_len(list.len(), span)?;
                Ok(Value::None)
            }
            "insert" => {
                let [at, x] = bind::<2>(args, ["index", "x"], 2, "insert", span)?;
                let Some(Value::Int(at)) = at else {
                    return fail(span, "`insert` needs an int index");
                };
                let x = x.unwrap_or(Value::None);
                check_store(&object, std::slice::from_ref(&x), span)?;
                let mut list = list.borrow_mut();
                let len = list.len() as i64;
                let at = if at < 0 { at + len } else { at }.clamp(0, len) as usize;
                list.insert(at, x);
                check_len(list.len(), span)?;
                Ok(Value::None)
            }
            "pop" => {
                let [at] = bind::<1>(args, ["index"], 0, "pop", span)?;
                let mut list = list.borrow_mut();
                let at = position(&at.unwrap_or(Value::Int(-1)), list.len(), span)?;
                Ok(list.remove(at))
            }
            "remove" => {
                let [x] = bind::<1>(args, ["x"], 1, "remove", span)?;
                let x = x.unwrap_or(Value::None);
                let mut list = list.borrow_mut();
                match list.iter().position(|item| equals(item, &x)) {
                    Some(at) => {
                        list.remove(at);
                        Ok(Value::None)
                    }
                    None => fail(span, format!("{} isn't in the list", x.repr())),
                }
            }
            "index" => {
                let [x] = bind::<1>(args, ["x"], 1, "index", span)?;
                let x = x.unwrap_or(Value::None);
                match list.borrow().iter().position(|item| equals(item, &x)) {
                    Some(at) => Ok(Value::Int(at as i64)),
                    None => fail(span, format!("{} isn't in the list", x.repr())),
                }
            }
            "clear" => {
                list.borrow_mut().clear();
                Ok(Value::None)
            }
            _ => fail(span, format!("a list has no `{name}`")),
        },
        Value::Dict(dict) => match name {
            "get" => {
                let [key, default] = bind::<2>(args, ["key", "default"], 1, "get", span)?;
                let key = key.unwrap_or(Value::None);
                Ok(dict
                    .borrow()
                    .get(&key)
                    .cloned()
                    .or(default)
                    .unwrap_or(Value::None))
            }
            "keys" | "values" | "items" => {
                let dict = dict.borrow();
                Ok(Value::list(
                    dict.entries
                        .iter()
                        .map(|(k, v)| match name {
                            "keys" => k.clone(),
                            "values" => v.clone(),
                            _ => Value::tuple(vec![k.clone(), v.clone()]),
                        })
                        .collect(),
                ))
            }
            "pop" => {
                let [key, default] = bind::<2>(args, ["key", "default"], 1, "pop", span)?;
                let key = key.unwrap_or(Value::None);
                let removed = dict.borrow_mut().remove(&key);
                removed
                    .or(default)
                    .map_or_else(|| fail(span, format!("key {} not found", key.repr())), Ok)
            }
            "setdefault" => {
                let [key, default] = bind::<2>(args, ["key", "default"], 1, "setdefault", span)?;
                let key = key.unwrap_or(Value::None);
                if !key.hashable() {
                    return fail(span, format!("a {} can't be a dict key", key.type_name()));
                }
                if let Some(existing) = dict.borrow().get(&key) {
                    return Ok(existing.clone());
                }
                let value = default.unwrap_or(Value::None);
                check_store(&object, std::slice::from_ref(&value), span)?;
                dict.borrow_mut().insert(key, value.clone());
                Ok(value)
            }
            "update" => {
                let mut incoming = Dict::default();
                for source in &args.positional {
                    update(&mut incoming, source, span)?;
                }
                for (key, value) in args.named {
                    incoming.insert(Value::str(&key), value);
                }
                let values: Vec<Value> = incoming.entries.iter().map(|(_, v)| v.clone()).collect();
                check_store(&object, &values, span)?;
                let mut dict = dict.borrow_mut();
                for (key, value) in incoming.entries {
                    dict.insert(key, value);
                }
                check_len(dict.entries.len(), span)?;
                Ok(Value::None)
            }
            "clear" => {
                dict.borrow_mut().entries.clear();
                Ok(Value::None)
            }
            _ => fail(span, format!("a dict has no `{name}`")),
        },
        Value::Str(text) => string_method(text, name, args, span),
        other => fail(span, format!("a {} has no `{name}`", other.type_name())),
    }
}

fn string_method(text: &str, name: &str, args: Args, span: Span) -> Eval<Value> {
    let one_string = |args: Args, what: &str| -> Eval<String> {
        let [x] = bind::<1>(args, ["x"], 1, what, span)?;
        match x {
            Some(Value::Str(s)) => Ok(s.to_string()),
            _ => fail(span, format!("`{what}` needs a string")),
        }
    };
    let result = match name {
        "join" => {
            let [x] = bind::<1>(args, ["items"], 1, "join", span)?;
            let mut parts = Vec::new();
            for item in collect(&x.unwrap_or(Value::None), span)? {
                let Value::Str(part) = item else {
                    return fail(
                        span,
                        format!("`join` needs strings, not {}", item.type_name()),
                    );
                };
                parts.push(part.to_string());
            }
            Value::str(&parts.join(text))
        }
        "upper" => Value::str(&text.to_uppercase()),
        "lower" => Value::str(&text.to_lowercase()),
        "strip" => Value::str(text.trim()),
        "lstrip" => Value::str(text.trim_start()),
        "rstrip" => Value::str(text.trim_end()),
        "startswith" => Value::Bool(text.starts_with(&one_string(args, "startswith")?)),
        "endswith" => Value::Bool(text.ends_with(&one_string(args, "endswith")?)),
        "replace" => {
            let [old, new] = bind::<2>(args, ["old", "new"], 2, "replace", span)?;
            let (Some(Value::Str(old)), Some(Value::Str(new))) = (old, new) else {
                return fail(span, "`replace` needs two strings");
            };
            Value::str(&text.replace(&*old, &new))
        }
        "split" => {
            let [separator] = bind::<1>(args, ["sep"], 0, "split", span)?;
            let parts: Vec<Value> = match separator {
                Some(Value::Str(separator)) if !separator.is_empty() => {
                    text.split(&*separator).map(Value::str).collect()
                }
                None | Some(Value::None) => text.split_whitespace().map(Value::str).collect(),
                Some(_) => return fail(span, "`split` needs a non-empty string"),
            };
            Value::list(parts)
        }
        "format" => Value::str(&format(text, &args, span)?),
        "elems" => Value::list(text.chars().map(|c| Value::str(&c.to_string())).collect()),
        _ => return fail(span, format!("a string has no `{name}`")),
    };
    if let Value::Str(out) = &result {
        check_len(out.len(), span)?;
    }
    Ok(result)
}

/// `"{} and {name}".format(a, name = b)`.
fn format(template: &str, args: &Args, span: Span) -> Eval<String> {
    let mut out = String::new();
    let mut next = 0;
    let mut chars = template.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '{' if chars.peek() == Some(&'{') => {
                chars.next();
                out.push('{');
            }
            '}' if chars.peek() == Some(&'}') => {
                chars.next();
                out.push('}');
            }
            '{' => {
                let mut field = String::new();
                loop {
                    match chars.next() {
                        Some('}') => break,
                        Some(c) => field.push(c),
                        None => return fail(span, "a `{` in the format is never closed"),
                    }
                }
                let value = if field.is_empty() {
                    next += 1;
                    args.positional.get(next - 1)
                } else if let Ok(index) = field.parse::<usize>() {
                    args.positional.get(index)
                } else {
                    args.named.iter().find(|(n, _)| *n == field).map(|(_, v)| v)
                };
                let Some(value) = value else {
                    return fail(span, format!("nothing fills `{{{field}}}`"));
                };
                out.push_str(&value.to_text());
            }
            '}' => return fail(span, "a lone `}` in the format"),
            c => out.push(c),
        }
    }
    Ok(out)
}

/// Adds a dict's entries, or `(key, value)` pairs, to `dict`.
fn update(dict: &mut Dict, source: &Value, span: Span) -> Eval<()> {
    if let Value::Dict(other) = source {
        for (key, value) in &other.borrow().entries {
            dict.insert(key.clone(), value.clone());
        }
        return Ok(());
    }
    for pair in collect(source, span)? {
        let items = collect(&pair, span)?;
        let [key, value] = <[Value; 2]>::try_from(items).map_err(|_| super::EvalError {
            span,
            message: "each item must be a (key, value) pair".to_owned(),
        })?;
        if !key.hashable() {
            return fail(span, format!("a {} can't be a dict key", key.type_name()));
        }
        dict.insert(key, value);
    }
    Ok(())
}

pub fn index(container: &Value, index: &Value, span: Span) -> Eval<Value> {
    match container {
        Value::List(items) => {
            let items = items.borrow();
            Ok(items[position(index, items.len(), span)?].clone())
        }
        Value::Tuple(items) => Ok(items[position(index, items.len(), span)?].clone()),
        Value::Str(text) => {
            let chars: Vec<char> = text.chars().collect();
            Ok(Value::str(
                &chars[position(index, chars.len(), span)?].to_string(),
            ))
        }
        Value::Range(range) => {
            let len = usize::try_from(range.len()).unwrap_or(usize::MAX);
            Ok(Value::Int(range.get(position(index, len, span)? as i64)))
        }
        Value::Dict(dict) => dict
            .borrow()
            .get(index)
            .cloned()
            .map_or_else(|| fail(span, format!("key {} not found", index.repr())), Ok),
        other => fail(span, format!("a {} can't be indexed", other.type_name())),
    }
}

pub fn set_index(container: &Value, index: Value, value: Value, span: Span) -> Eval<()> {
    check_store(container, std::slice::from_ref(&value), span)?;
    match container {
        Value::List(items) => {
            let mut items = items.borrow_mut();
            let at = position(&index, items.len(), span)?;
            items[at] = value;
            Ok(())
        }
        Value::Dict(dict) => {
            if !index.hashable() {
                return fail(span, format!("a {} can't be a dict key", index.type_name()));
            }
            let mut dict = dict.borrow_mut();
            dict.insert(index, value);
            check_len(dict.entries.len(), span)
        }
        other => fail(
            span,
            format!("a {} can't be assigned into", other.type_name()),
        ),
    }
}

fn position(index: &Value, len: usize, span: Span) -> Eval<usize> {
    let Value::Int(i) = index else {
        return fail(
            span,
            format!("an index is an int, not {}", index.type_name()),
        );
    };
    let len = len as i64;
    let at = if *i < 0 { i + len } else { *i };
    if (0..len).contains(&at) {
        Ok(at as usize)
    } else {
        fail(span, format!("index {i} is out of range for {len} items"))
    }
}

pub fn slice(
    container: &Value,
    start: Option<i64>,
    stop: Option<i64>,
    step: Option<i64>,
    span: Span,
) -> Eval<Value> {
    let step = step.unwrap_or(1);
    if step == 0 {
        return fail(span, "a slice can't step by 0");
    }
    let pick = |len: usize| slice_indices(len as i64, start, stop, step);
    match container {
        Value::List(items) => {
            let items = items.borrow();
            Ok(Value::list(
                pick(items.len()).map(|i| items[i].clone()).collect(),
            ))
        }
        Value::Tuple(items) => Ok(Value::tuple(
            pick(items.len()).map(|i| items[i].clone()).collect(),
        )),
        Value::Str(text) => {
            let chars: Vec<char> = text.chars().collect();
            Ok(Value::str(
                &pick(chars.len()).map(|i| chars[i]).collect::<String>(),
            ))
        }
        other => fail(span, format!("a {} can't be sliced", other.type_name())),
    }
}

fn slice_indices(
    len: i64,
    start: Option<i64>,
    stop: Option<i64>,
    step: i64,
) -> impl Iterator<Item = usize> {
    let normalize = |i: i64| if i < 0 { i + len } else { i };
    let (mut at, end) = if step > 0 {
        (
            start.map_or(0, normalize).clamp(0, len),
            stop.map_or(len, normalize).clamp(0, len),
        )
    } else {
        (
            start.map_or(len - 1, normalize).clamp(-1, len - 1),
            stop.map_or(-1, normalize).clamp(-1, len - 1),
        )
    };
    std::iter::from_fn(move || {
        let inside = if step > 0 { at < end } else { at > end };
        if !inside {
            return None;
        }
        let here = at;
        at += step;
        Some(here as usize)
    })
}

/// Refuses storing a value that holds `container` itself. A list inside itself would send
/// printing, comparing, and the conversion to JSON round forever.
pub fn check_store(container: &Value, values: &[Value], span: Span) -> Eval<()> {
    let target = match container {
        Value::List(list) => Rc::as_ptr(list) as *const (),
        Value::Dict(dict) => Rc::as_ptr(dict) as *const (),
        _ => return Ok(()),
    };
    if values.iter().any(|value| reaches(value, target)) {
        fail(span, "a list or dict can't hold itself")
    } else {
        Ok(())
    }
}

fn reaches(value: &Value, target: *const ()) -> bool {
    match value {
        Value::List(list) => {
            Rc::as_ptr(list) as *const () == target
                || list.borrow().iter().any(|item| reaches(item, target))
        }
        Value::Dict(dict) => {
            Rc::as_ptr(dict) as *const () == target
                || dict
                    .borrow()
                    .entries
                    .iter()
                    .any(|(k, v)| reaches(k, target) || reaches(v, target))
        }
        Value::Tuple(items) => items.iter().any(|item| reaches(item, target)),
        Value::Method(object, _) => reaches(object, target),
        _ => false,
    }
}
