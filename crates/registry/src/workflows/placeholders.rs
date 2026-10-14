//! `{{name}}` placeholders, which the main agent fills from a workflow's `params`.

use serde_json::Value;

/// The names of the placeholders in `text`, in order. Braces around anything that isn't a
/// name are left alone.
pub(super) fn names_in(text: &str) -> Vec<&str> {
    let mut names = Vec::new();
    let mut rest = text;
    while let Some(open) = rest.find("{{") {
        let after = &rest[open + 2..];
        let Some(close) = after.find("}}") else {
            break;
        };
        let inner = after[..close].trim();
        if is_name(inner) {
            names.push(inner);
        }
        rest = &after[close + 2..];
    }
    names
}

/// The name when `text` is one placeholder and nothing else, as `count` and `models` must be
/// when they aren't a number or a list.
pub(super) fn sole(text: &str) -> Option<&str> {
    let inner = text.trim().strip_prefix("{{")?.strip_suffix("}}")?.trim();
    is_name(inner).then_some(inner)
}

/// The placeholder names in every string of `value`, at any depth.
pub(super) fn names_in_value(value: &Value) -> Vec<&str> {
    match value {
        Value::String(text) => names_in(text),
        Value::Array(items) => items.iter().flat_map(names_in_value).collect(),
        Value::Object(map) => map.values().flat_map(names_in_value).collect(),
        Value::Null | Value::Bool(_) | Value::Number(_) => Vec::new(),
    }
}

fn is_name(text: &str) -> bool {
    !text.is_empty()
        && text
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn placeholders_found_in_text_and_values() {
        assert_eq!(
            names_in("review {{target}} with {{ reviewers }}, not {{two words}} or {{"),
            ["target", "reviewers"]
        );
        assert_eq!(sole(" {{reviewers}} "), Some("reviewers"));
        assert_eq!(sole("{{reviewers}} and more"), None);
        assert_eq!(sole("3"), None);
        assert_eq!(
            names_in_value(&json!({ "a": ["{{x}}", 2, { "b": "{{y}}" }], "c": true })),
            ["x", "y"]
        );
    }
}
