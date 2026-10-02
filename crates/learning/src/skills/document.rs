//! `SKILL.md` as text: splitting off the frontmatter, writing it, the spec's rules as
//! `skills-ref` checks them, and cleaning a foreign frontmatter into the six fields.

use std::collections::BTreeMap;
use std::fmt::Write as _;

use serde_json::{Map, Value};

use argonauts_core::hash::sha256_hex;

use super::{Frontmatter, META_SHA256, Skill};

pub const FIELDS: [&str; 6] = [
    "name",
    "description",
    "license",
    "compatibility",
    "metadata",
    "allowed-tools",
];
pub const MAX_NAME: usize = 64;
pub const MAX_DESCRIPTION: usize = 1024;
pub const MAX_COMPATIBILITY: usize = 500;

/// The frontmatter's YAML and the body after it, or `None` when there's no frontmatter.
pub fn split(text: &str) -> Option<(&str, &str)> {
    let rest = text.strip_prefix("---")?;
    let rest = rest
        .strip_prefix("\r\n")
        .or_else(|| rest.strip_prefix('\n'))?;
    let mut offset = 0;
    for line in rest.split_inclusive('\n') {
        if line.trim_end_matches(['\n', '\r']) == "---" {
            return Some((&rest[..offset], &rest[offset + line.len()..]));
        }
        offset += line.len();
    }
    None
}

/// The frontmatter as a value. Empty YAML is `null`, which no rule accepts.
pub fn parse_yaml(yaml: &str) -> Result<Value, String> {
    if yaml.trim().is_empty() {
        return Ok(Value::Null);
    }
    serde_saphyr::from_str::<Value>(yaml).map_err(|err| err.to_string())
}

/// The SHA-256 a skill's `argonauts-sha256` holds: of the body exactly as written.
pub fn body_hash(body: &str) -> String {
    sha256_hex(body.as_bytes())
}

/// The body as `render` writes it, ending in one newline.
pub fn written_body(body: &str) -> String {
    if body.is_empty() || body.ends_with('\n') {
        body.to_owned()
    } else {
        format!("{body}\n")
    }
}

pub fn render(skill: &Skill) -> String {
    let body = written_body(&skill.body);
    let fm = &skill.frontmatter;
    let mut text = String::from("---\n");
    let _ = writeln!(text, "name: {}", scalar(&fm.name));
    let _ = writeln!(text, "description: {}", quoted(&fm.description));
    if let Some(license) = &fm.license {
        let _ = writeln!(text, "license: {}", quoted(license));
    }
    if let Some(compatibility) = &fm.compatibility {
        let _ = writeln!(text, "compatibility: {}", quoted(compatibility));
    }
    let mut metadata = fm.metadata.clone();
    metadata.insert(META_SHA256.to_owned(), body_hash(&body));
    text.push_str("metadata:\n");
    for (key, value) in &metadata {
        let _ = writeln!(text, "  {}: {}", scalar(key), quoted(value));
    }
    if let Some(tools) = &fm.allowed_tools {
        let _ = writeln!(text, "allowed-tools: {}", quoted(tools));
    }
    text.push_str("---\n");
    text.push_str(&body);
    text
}

/// Plain when YAML would read it back as the same string, else quoted.
fn scalar(text: &str) -> String {
    let plain = text.chars().next().is_some_and(|c| c.is_ascii_alphabetic())
        && text
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
        && !matches!(
            text.to_ascii_lowercase().as_str(),
            "true" | "false" | "yes" | "no" | "on" | "off" | "null" | "y" | "n"
        );
    if plain { text.to_owned() } else { quoted(text) }
}

/// A YAML double-quoted string. Line and paragraph separators are escaped too, since YAML
/// reads them as line breaks.
fn quoted(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 2);
    out.push('"');
    for c in text.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            '\r' => out.push_str("\\r"),
            c if c.is_control() || matches!(c, '\u{2028}' | '\u{2029}' | '\u{FEFF}') => {
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// What breaks the spec, in `skills-ref`'s words.
pub fn validate(text: &str, folder: &str) -> Vec<String> {
    if !text.starts_with("---") {
        return vec!["SKILL.md must start with YAML frontmatter (---)".to_owned()];
    }
    let Some((yaml, _)) = split(text) else {
        return vec!["SKILL.md frontmatter not properly closed with ---".to_owned()];
    };
    let map = match parse_yaml(yaml) {
        Ok(Value::Object(map)) => map,
        Ok(_) => return vec!["SKILL.md frontmatter must be a YAML mapping".to_owned()],
        Err(err) => return vec![format!("Invalid YAML in frontmatter: {err}")],
    };
    let mut problems = Vec::new();
    let mut unexpected: Vec<&str> = map
        .keys()
        .map(String::as_str)
        .filter(|key| !FIELDS.contains(key))
        .collect();
    if !unexpected.is_empty() {
        unexpected.sort_unstable();
        let mut allowed = FIELDS;
        allowed.sort_unstable();
        problems.push(format!(
            "Unexpected fields in frontmatter: {}. Only {} are allowed.",
            unexpected.join(", "),
            allowed.join(", ")
        ));
    }
    match map.get("name") {
        None => problems.push("Missing required field in frontmatter: name".to_owned()),
        Some(Value::String(name)) if !name.trim().is_empty() => {
            problems.extend(name_problems(name.trim(), folder));
        }
        Some(_) => problems.push("Field 'name' must be a non-empty string".to_owned()),
    }
    match map.get("description") {
        None => problems.push("Missing required field in frontmatter: description".to_owned()),
        Some(Value::String(text)) if !text.trim().is_empty() => {
            let chars = text.chars().count();
            if chars > MAX_DESCRIPTION {
                problems.push(format!(
                    "Description exceeds {MAX_DESCRIPTION} character limit ({chars} chars)"
                ));
            }
        }
        Some(_) => problems.push("Field 'description' must be a non-empty string".to_owned()),
    }
    for field in ["license", "compatibility", "allowed-tools"] {
        match map.get(field) {
            None | Some(Value::String(_)) => {}
            Some(_) => problems.push(format!("Field '{field}' must be a string")),
        }
    }
    if let Some(Value::String(text)) = map.get("compatibility") {
        let chars = text.chars().count();
        if chars > MAX_COMPATIBILITY {
            problems.push(format!(
                "Compatibility exceeds {MAX_COMPATIBILITY} character limit ({chars} chars)"
            ));
        }
    }
    match map.get("metadata") {
        None => {}
        Some(Value::Object(metadata)) => {
            for (key, value) in metadata {
                if !value.is_string() {
                    problems.push(format!(
                        "Field 'metadata' must map strings to strings, and '{key}' doesn't"
                    ));
                }
            }
        }
        Some(_) => problems.push("Field 'metadata' must be a mapping".to_owned()),
    }
    problems
}

fn name_problems(name: &str, folder: &str) -> Vec<String> {
    let mut problems = Vec::new();
    let chars = name.chars().count();
    if chars > MAX_NAME {
        problems.push(format!(
            "Skill name '{name}' exceeds {MAX_NAME} character limit ({chars} chars)"
        ));
    }
    if name != name.to_lowercase() {
        problems.push(format!("Skill name '{name}' must be lowercase"));
    }
    if name.starts_with('-') || name.ends_with('-') {
        problems.push("Skill name cannot start or end with a hyphen".to_owned());
    }
    if name.contains("--") {
        problems.push("Skill name cannot contain consecutive hyphens".to_owned());
    }
    if !name.chars().all(|c| c.is_alphanumeric() || c == '-') {
        problems.push(format!(
            "Skill name '{name}' contains invalid characters. Only letters, digits, and hyphens \
             are allowed."
        ));
    }
    if folder != name {
        problems.push(format!(
            "Directory name '{folder}' must match skill name '{name}'"
        ));
    }
    problems
}

/// A name the spec accepts, made from any text: lowercase ASCII letters and digits, with one
/// hyphen for each run of anything else. Empty when nothing usable is left.
pub fn normalize_name(raw: &str) -> String {
    let mut name = String::new();
    for c in raw.chars().flat_map(char::to_lowercase) {
        if c.is_ascii_alphanumeric() {
            name.push(c);
        } else if !name.is_empty() && !name.ends_with('-') {
            name.push('-');
        }
    }
    let mut name: String = name.trim_end_matches('-').chars().take(MAX_NAME).collect();
    while name.ends_with('-') {
        name.pop();
    }
    name
}

/// `text` cut to `max` characters.
pub fn cut(text: &str, max: usize) -> String {
    text.chars().take(max).collect()
}

/// A foreign frontmatter brought into the six fields.
pub struct Cleaned {
    pub frontmatter: Frontmatter,
    /// What changed, in words, for the pull request.
    pub changes: Vec<String>,
}

/// Keeps the spec's fields, flattens `metadata`, and moves anything else that carries meaning
/// under `metadata` as a flat string. `folder` names the skill when the frontmatter doesn't.
pub fn clean(value: Value, folder: &str) -> Cleaned {
    let mut changes = Vec::new();
    let mut map = match value {
        Value::Object(map) => map,
        Value::Null => Map::new(),
        _ => {
            changes.push("dropped a frontmatter that wasn't a mapping".to_owned());
            Map::new()
        }
    };
    let mut fm = Frontmatter::default();

    let raw_name = map.remove("name").map(|value| text_of(&value));
    let wanted = raw_name.clone().unwrap_or_else(|| folder.to_owned());
    fm.name = normalize_name(&wanted);
    match raw_name {
        None => changes.push(format!("named it `{}` after its folder", fm.name)),
        Some(raw) if raw != fm.name => {
            changes.push(format!("renamed it from `{raw}` to `{}`", fm.name));
        }
        Some(_) => {}
    }

    match map.remove("description") {
        Some(Value::String(text)) => fm.description = text.trim().to_owned(),
        Some(Value::Null) | None => {}
        Some(other) => {
            fm.description = text_of(&other);
            changes.push("turned the description into a string".to_owned());
        }
    }
    if fm.description.chars().count() > MAX_DESCRIPTION {
        fm.description = cut(&fm.description, MAX_DESCRIPTION);
        changes.push(format!(
            "cut the description to {MAX_DESCRIPTION} characters"
        ));
    }

    let mut metadata = BTreeMap::new();
    match map.remove("metadata") {
        None | Some(Value::Null) => {}
        Some(Value::Object(entries)) => {
            for (key, value) in entries {
                if !value.is_string() {
                    changes.push(format!("flattened `metadata.{key}` into flat strings"));
                }
                flatten(&key, &value, &mut metadata);
            }
        }
        Some(other) => {
            changes.push(
                "moved a `metadata` that wasn't a mapping under `metadata.metadata`".to_owned(),
            );
            flatten("metadata", &other, &mut metadata);
        }
    }

    for (field, slot, max) in [
        ("license", &mut fm.license, usize::MAX),
        ("compatibility", &mut fm.compatibility, MAX_COMPATIBILITY),
    ] {
        match map.remove(field) {
            None | Some(Value::Null) => {}
            Some(Value::String(text)) => {
                if text.chars().count() > max {
                    changes.push(format!("cut `{field}` to {max} characters"));
                }
                *slot = Some(cut(text.trim(), max));
            }
            Some(other @ (Value::Bool(_) | Value::Number(_))) => {
                changes.push(format!("turned `{field}` into a string"));
                *slot = Some(text_of(&other));
            }
            Some(other) => {
                changes.push(format!("moved `{field}` into metadata"));
                flatten(field, &other, &mut metadata);
            }
        }
    }

    match map.remove("allowed-tools") {
        None | Some(Value::Null) => {}
        Some(Value::String(text)) => fm.allowed_tools = Some(text.trim().to_owned()),
        Some(Value::Array(items)) if items.iter().all(Value::is_string) => {
            changes.push("joined `allowed-tools` into one string".to_owned());
            let tools: Vec<String> = items.iter().map(text_of).collect();
            fm.allowed_tools = Some(tools.join(" "));
        }
        Some(other) => {
            changes.push("moved `allowed-tools` into metadata".to_owned());
            flatten("allowed-tools", &other, &mut metadata);
        }
    }

    for (key, value) in map {
        if is_empty(&value) {
            changes.push(format!("dropped the empty field `{key}`"));
            continue;
        }
        if metadata.contains_key(&key) {
            changes.push(format!("dropped `{key}`, which metadata already has"));
            continue;
        }
        changes.push(format!("moved `{key}` into metadata"));
        flatten(&key, &value, &mut metadata);
    }
    fm.metadata = metadata;
    Cleaned {
        frontmatter: fm,
        changes,
    }
}

fn is_empty(value: &Value) -> bool {
    match value {
        Value::Null => true,
        Value::String(text) => text.trim().is_empty(),
        Value::Array(items) => items.iter().all(is_empty),
        Value::Object(map) => map.values().all(is_empty),
        Value::Bool(_) | Value::Number(_) => false,
    }
}

fn text_of(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        Value::Null => String::new(),
        other => other.to_string(),
    }
}

/// Nested mappings become dotted keys and lists of plain values one comma-separated string,
/// since the spec's `metadata` is flat, string to string.
fn flatten(key: &str, value: &Value, out: &mut BTreeMap<String, String>) {
    match value {
        Value::Null => {}
        Value::Object(map) => {
            for (inner, value) in map {
                flatten(&format!("{key}.{inner}"), value, out);
            }
        }
        Value::Array(items)
            if items
                .iter()
                .all(|item| !item.is_object() && !item.is_array()) =>
        {
            let parts: Vec<String> = items
                .iter()
                .filter(|item| !item.is_null())
                .map(text_of)
                .collect();
            if !parts.is_empty() {
                out.entry(key.to_owned()).or_insert(parts.join(", "));
            }
        }
        Value::Array(items) => {
            for (index, item) in items.iter().enumerate() {
                flatten(&format!("{key}.{index}"), item, out);
            }
        }
        other => {
            let text = text_of(other);
            if !text.trim().is_empty() {
                out.entry(key.to_owned()).or_insert(text);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_finds_the_body() {
        assert_eq!(split("---\na: 1\n---\nbody\n"), Some(("a: 1\n", "body\n")));
        assert_eq!(split("---\r\na: 1\r\n---\r\n"), Some(("a: 1\r\n", "")));
        assert_eq!(split("no frontmatter"), None);
        assert_eq!(split("---\na: 1\n"), None);
    }

    #[test]
    fn names_normalize() {
        assert_eq!(normalize_name("My Skill!"), "my-skill");
        assert_eq!(normalize_name("--a__b--"), "a-b");
        assert_eq!(normalize_name("✓"), "");
        assert_eq!(normalize_name(&"a".repeat(70)).len(), MAX_NAME);
    }

    #[test]
    fn quoting_round_trips() {
        for text in [
            "plain",
            "a: b",
            "\"quoted\" \\ back",
            "tab\tline\nnext",
            "yes",
            "3",
        ] {
            let yaml = format!("k: {}\n", scalar(text));
            let value = parse_yaml(&yaml).unwrap();
            assert_eq!(value["k"], Value::String(text.to_owned()), "{yaml}");
        }
    }
}
