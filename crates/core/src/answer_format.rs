//! Answer formats in the one shape every CLI's structured output takes. Codex's strict mode is
//! the strictest: every property required at every depth, no others allowed, and a field the
//! format left optional allowed to be `null` instead. Claude Code takes the format as written,
//! with only what it requires required. The answer is read back as the format was written,
//! those nulls left out again, and mended when its fields ran on inside one string.

mod leaked;
#[cfg(test)]
mod report_tests;

use serde_json::{Map, Value, json};

use crate::steps::{strict_schema, written_schema};

pub use leaked::mended;

/// The format as Claude Code takes it: the subset every CLI accepts, with a field that has a
/// default left optional. A model that leaves out a list it has nothing for, as Haiku does,
/// still answers.
pub fn written(schema: &Value) -> Value {
    written_schema(schema.clone())
}

/// Whether `answer` is an answer to `schema`. A schema that can't be read takes nothing.
pub fn fits(schema: &Value, answer: &Value) -> bool {
    jsonschema::validator_for(schema).is_ok_and(|validator| validator.is_valid(answer))
}

/// `answer` [`mended`], when the mended one fits `schema`. `None` when nothing needed mending,
/// or mending didn't make it fit.
pub fn mended_to_fit(schema: &Value, answer: &Value) -> Option<Value> {
    mended(answer, schema).filter(|mended| fits(schema, mended))
}

/// The format as it goes to a CLI. A property the format didn't require may come back as
/// `null`, one with a default too: a model that leaves out a list it has nothing for, as Haiku
/// does, would otherwise send no answer at all. [`as_written`] leaves the null out again, so
/// serde fills the default.
pub fn strict(schema: &Value) -> Value {
    strict_schema(with_optional_nullable(schema))
}

/// The answer to [`strict`]'s format, with the nulls it allowed in place of an optional field
/// left out, so the answer reads as one to the format as written.
pub fn as_written(schema: &Value, answer: Value) -> Value {
    match answer {
        Value::String(text) if takes_any_value(schema) => {
            serde_json::from_str(&text).unwrap_or(Value::String(text))
        }
        Value::Object(fields) => {
            let required = required_of(schema);
            let properties = schema.get("properties").and_then(Value::as_object);
            let fields = fields
                .into_iter()
                .filter(|(name, value)| !value.is_null() || required.contains(&name.as_str()))
                .map(|(name, value)| {
                    let value = match properties.and_then(|props| props.get(&name)) {
                        Some(property) => as_written(property, value),
                        None => value,
                    };
                    (name, value)
                })
                .collect();
            Value::Object(fields)
        }
        Value::Array(items) => match schema.get("items") {
            Some(item) => Value::Array(
                items
                    .into_iter()
                    .map(|value| as_written(item, value))
                    .collect(),
            ),
            None => Value::Array(items),
        },
        other => other,
    }
}

/// What Codex would refuse in a format, each with where it is (`$` the root). Codex answers
/// such a format with "Invalid schema for response_format" before any work starts.
pub fn codex_problems(schema: &Value) -> Vec<String> {
    let mut problems = Vec::new();
    if schema.get("type") != Some(&json!("object")) {
        problems.push("$: the root isn't an object".to_owned());
    }
    let defs = schema.get("$defs").and_then(Value::as_object);
    problems_in(schema, "$", &mut problems);
    for (name, def) in defs.into_iter().flatten() {
        problems_in(def, &format!("$defs.{name}"), &mut problems);
    }
    problems
}

/// Keywords Codex's strict mode refuses, or that this shape leaves out.
const REFUSED: &[&str] = &[
    "oneOf",
    "allOf",
    "not",
    "if",
    "then",
    "else",
    "const",
    "default",
    "patternProperties",
    "propertyNames",
    "dependentRequired",
    "dependentSchemas",
    "unevaluatedProperties",
];

/// A schema that says nothing of its type, which [`strict`] sends as JSON text.
fn takes_any_value(schema: &Value) -> bool {
    match schema {
        Value::Bool(any) => *any,
        Value::Object(map) => ![
            "type",
            "enum",
            "anyOf",
            "oneOf",
            "allOf",
            "$ref",
            "properties",
        ]
        .iter()
        .any(|key| map.contains_key(*key)),
        _ => false,
    }
}

fn problems_in(node: &Value, path: &str, problems: &mut Vec<String>) {
    let Value::Object(map) = node else {
        let problem = if node == &Value::Bool(true) {
            "no type"
        } else {
            "not a schema"
        };
        problems.push(format!("{path}: {problem}"));
        return;
    };
    for key in map.keys() {
        if REFUSED.contains(&key.as_str()) {
            problems.push(format!("{path}: {key} isn't allowed"));
        }
    }
    // Codex's strict mode refuses a node that says nothing of its type.
    if !["type", "anyOf", "$ref", "enum"]
        .iter()
        .any(|key| map.contains_key(*key))
    {
        problems.push(format!("{path}: no type"));
    }
    if is_object(map) {
        if map.get("additionalProperties") != Some(&Value::Bool(false)) {
            problems.push(format!("{path}: additionalProperties must be false"));
        }
        let properties = map.get("properties").and_then(Value::as_object);
        let required = required_of(node);
        for name in properties.into_iter().flat_map(Map::keys) {
            if !required.contains(&name.as_str()) {
                problems.push(format!("{path}: required is missing {name:?}"));
            }
        }
        for name in &required {
            if properties.is_none_or(|props| !props.contains_key(*name)) {
                problems.push(format!(
                    "{path}: required names {name:?}, which isn't a property"
                ));
            }
        }
        for (name, property) in properties.into_iter().flatten() {
            problems_in(property, &format!("{path}.{name}"), problems);
        }
    }
    if let Some(items) = map.get("items") {
        problems_in(items, &format!("{path}[]"), problems);
    }
    if let Some(Value::Array(options)) = map.get("anyOf") {
        for (i, option) in options.iter().enumerate() {
            problems_in(option, &format!("{path}|{i}"), problems);
        }
    }
    if let (Some(Value::Array(values)), true) = (map.get("enum"), allows_null(map))
        && !values.contains(&Value::Null)
    {
        problems.push(format!(
            "{path}: its type allows null, and its enum doesn't"
        ));
    }
}

/// Makes every optional property nullable, at every depth and in every definition.
fn with_optional_nullable(node: &Value) -> Value {
    let Value::Object(map) = node else {
        return node.clone();
    };
    let required = required_of(node);
    let mut out = Map::new();
    for (key, value) in map {
        let value = match key.as_str() {
            "properties" => Value::Object(
                value
                    .as_object()
                    .into_iter()
                    .flatten()
                    .map(|(name, property)| {
                        let property = with_optional_nullable(property);
                        let property = if !required.contains(&name.as_str()) {
                            nullable(property)
                        } else {
                            property
                        };
                        (name.clone(), property)
                    })
                    .collect(),
            ),
            "items" | "additionalProperties" => with_optional_nullable(value),
            "anyOf" | "oneOf" | "allOf" | "prefixItems" => Value::Array(
                value
                    .as_array()
                    .into_iter()
                    .flatten()
                    .map(with_optional_nullable)
                    .collect(),
            ),
            "$defs" | "definitions" => Value::Object(
                value
                    .as_object()
                    .into_iter()
                    .flatten()
                    .map(|(name, def)| (name.clone(), with_optional_nullable(def)))
                    .collect(),
            ),
            _ => value.clone(),
        };
        out.insert(key.clone(), value);
    }
    Value::Object(out)
}

/// The schema with `null` allowed as well.
fn nullable(schema: Value) -> Value {
    let Value::Object(mut map) = schema else {
        return schema;
    };
    if allows_null(&map) {
        return Value::Object(map);
    }
    if let Some(Value::Array(values)) = map.get_mut("enum")
        && !values.contains(&Value::Null)
    {
        values.push(Value::Null);
    }
    match map.get_mut("type") {
        Some(Value::String(kind)) => {
            let kind = std::mem::take(kind);
            map.insert("type".to_owned(), json!([kind, "null"]));
        }
        Some(Value::Array(kinds)) => kinds.push(json!("null")),
        _ => {
            if let Some(Value::Array(options)) = map.get_mut("anyOf") {
                options.push(json!({"type": "null"}));
            } else if map.contains_key("$ref") || map.contains_key("oneOf") {
                // The reference keeps its own description beside it.
                let description = map.remove("description");
                let mut wrapped = Map::new();
                wrapped.insert(
                    "anyOf".to_owned(),
                    json!([Value::Object(map), {"type": "null"}]),
                );
                if let Some(description) = description {
                    wrapped.insert("description".to_owned(), description);
                }
                return Value::Object(wrapped);
            }
            // No type at all already takes anything, null included.
        }
    }
    Value::Object(map)
}

fn allows_null(map: &Map<String, Value>) -> bool {
    match map.get("type") {
        Some(Value::String(kind)) => kind == "null",
        Some(Value::Array(kinds)) => kinds.iter().any(|kind| kind == "null"),
        _ => map
            .get("anyOf")
            .and_then(Value::as_array)
            .is_some_and(|options| {
                options
                    .iter()
                    .any(|option| option.get("type") == Some(&json!("null")))
            }),
    }
}

fn is_object(map: &Map<String, Value>) -> bool {
    let typed = match map.get("type") {
        Some(Value::String(kind)) => kind == "object",
        Some(Value::Array(kinds)) => kinds.iter().any(|kind| kind == "object"),
        _ => false,
    };
    typed || map.contains_key("properties")
}

fn required_of(schema: &Value) -> Vec<&str> {
    schema
        .get("required")
        .and_then(Value::as_array)
        .map(|names| names.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The review format whose missing "summary" Codex refused.
    fn loose_review() -> Value {
        json!({
            "type": "object",
            "properties": {
                "accept": {"type": "boolean"},
                "summary": {"type": "string"},
                "findings": {"type": "array", "items": {"type": "string"}},
                "severity": {"type": "string", "enum": ["low", "high"]},
                "files": {"type": "array", "items": {
                    "type": "object",
                    "properties": {"path": {"type": "string"}, "line": {"type": "integer"}},
                    "required": ["path"],
                }},
            },
            "required": ["accept"],
        })
    }

    #[test]
    fn an_answer_fits_the_format_as_written() {
        let format = loose_review();
        assert!(fits(&format, &json!({"accept": true})));
        assert!(fits(&format, &json!({"accept": false, "severity": "low"})));
        assert!(!fits(&format, &json!({"summary": "no verdict"})));
        assert!(!fits(
            &format,
            &json!({"accept": true, "severity": "medium"})
        ));
        assert!(!fits(&json!({"type": 7}), &json!({})));
    }

    #[test]
    fn codex_refuses_a_format_that_leaves_a_property_out_of_required() {
        let problems = codex_problems(&loose_review());
        assert!(
            problems
                .iter()
                .any(|problem| problem == "$: required is missing \"summary\""),
            "{problems:#?}"
        );
        assert!(
            problems
                .iter()
                .any(|p| p.starts_with("$.files[]: required"))
        );
    }

    #[test]
    fn the_strict_format_requires_everything_and_lets_optional_fields_be_null() {
        let strict = strict(&loose_review());
        assert_eq!(codex_problems(&strict), Vec::<String>::new(), "{strict:#}");
        assert_eq!(
            strict["required"],
            json!(["accept", "summary", "findings", "severity", "files"])
        );
        assert_eq!(strict["properties"]["accept"]["type"], "boolean");
        assert_eq!(
            strict["properties"]["summary"]["type"],
            json!(["string", "null"])
        );
        assert_eq!(
            strict["properties"]["severity"]["enum"],
            json!(["low", "high", null])
        );
        let line = &strict["properties"]["files"]["type"];
        assert_eq!(line, &json!(["array", "null"]));
        let file = &strict["properties"]["files"]["items"];
        assert_eq!(file["required"], json!(["path", "line"]));
        assert_eq!(
            file["properties"]["line"]["type"],
            json!(["integer", "null"])
        );
        assert_eq!(file["properties"]["path"]["type"], "string");
        assert_eq!(
            strict,
            super::strict(&strict),
            "strict twice is strict once"
        );
    }

    #[test]
    fn a_field_with_a_default_may_be_null_and_reads_as_its_default() {
        let schema = json!({
            "type": "object",
            "properties": {"tags": {"type": "array", "items": {"type": "string"}, "default": []}},
        });
        let strict = strict(&schema);
        assert_eq!(
            strict["properties"]["tags"]["type"],
            json!(["array", "null"])
        );
        assert_eq!(codex_problems(&strict), Vec::<String>::new());
        assert_eq!(as_written(&schema, json!({"tags": null})), json!({}));
    }

    #[test]
    fn a_reference_left_optional_may_be_null() {
        let schema = json!({
            "type": "object",
            "properties": {"kind": {"$ref": "#/$defs/Kind", "description": "what kind"}},
            "$defs": {"Kind": {"type": "string", "enum": ["a", "b"]}},
        });
        let strict = strict(&schema);
        assert_eq!(codex_problems(&strict), Vec::<String>::new(), "{strict:#}");
        let kind = &strict["properties"]["kind"];
        assert_eq!(kind["description"], "what kind");
        assert_eq!(kind["anyOf"][1], json!({"type": "null"}));
    }

    #[test]
    fn the_answer_reads_as_the_format_was_written() {
        let answer = json!({
            "accept": false,
            "summary": null,
            "findings": ["a test fails"],
            "severity": null,
            "files": [{"path": "a.rs", "line": null}],
        });
        assert_eq!(
            as_written(&loose_review(), answer),
            json!({
                "accept": false,
                "findings": ["a test fails"],
                "files": [{"path": "a.rs"}],
            })
        );
        // A null the format itself allows for a required field stays.
        let schema = json!({
            "type": "object",
            "properties": {"note": {"type": ["string", "null"]}},
            "required": ["note"],
        });
        assert_eq!(
            as_written(&schema, json!({"note": null})),
            json!({"note": null})
        );
    }

    #[test]
    fn a_field_that_takes_any_value_goes_as_json_text_and_comes_back() {
        let schema = json!({
            "type": "object",
            "properties": {"data": true},
            "required": ["data"],
        });
        let sent = strict(&schema);
        assert_eq!(sent["properties"]["data"]["type"], "string");
        assert!(codex_problems(&sent).is_empty(), "{sent:#}");
        assert_eq!(
            as_written(&schema, json!({"data": "{\"a\": [1, 2]}"})),
            json!({"data": {"a": [1, 2]}})
        );
        assert_eq!(
            as_written(&schema, json!({"data": "plain words"})),
            json!({"data": "plain words"})
        );
    }
}
