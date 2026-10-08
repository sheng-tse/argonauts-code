//! The two questions a write may need after its scope: `ConfirmSafetySetting` for a safety key,
//! and `TrustProject` for a project file whose safety keys you haven't trusted.

use std::path::Path;

use argonauts_core::app::{AppChoice, ChoiceKind};
use argonauts_core::names::ChoiceId;
use argonauts_registry::safety_keys_in;

use crate::app_settings::AppSettings;
use crate::choices::{app_choice, options};

/// Every safety key a write changes, with its value now and after. `value` is `None` for a
/// reset, where the layer below decides what comes after.
pub(crate) fn safety_changes(
    settings: &AppSettings,
    key: &str,
    value: Option<&str>,
) -> Vec<String> {
    let keys = match value {
        Some(value) => safety_keys_in(key, value),
        None => safety_keys_in(key, "\"\""),
    };
    keys.into_iter()
        .map(|safety| {
            let now = settings
                .entry(&safety)
                .map(|entry| entry.value.clone())
                .filter(|value| !value.is_empty())
                .unwrap_or_else(|| "unset".to_owned());
            let after = match value {
                Some(value) => value_at(key, value, &safety).unwrap_or_else(|| value.to_owned()),
                None => "what the layer below sets".to_owned(),
            };
            format!("{safety}: now {now}, after {after}")
        })
        .collect()
}

pub(crate) fn confirm_question(id: ChoiceId, changes: Vec<String>) -> AppChoice {
    let mut body = changes;
    body.push("Safety settings decide what agents may do and what runs without you.".to_owned());
    app_choice(
        id,
        ChoiceKind::ConfirmSafetySetting,
        "Change a safety setting?",
        body,
        options(ChoiceKind::ConfirmSafetySetting),
    )
}

pub(crate) fn trust_question(id: ChoiceId, shown: &str, unsafe_lines: &[String]) -> AppChoice {
    let mut body = vec![format!("{shown} sets safety settings you haven't trusted:")];
    body.extend(unsafe_lines.iter().map(|line| format!("  {line}")));
    body.push(
        "Your change is saved either way. Until you trust the file, its safety settings stay \
         ignored."
            .to_owned(),
    );
    app_choice(
        id,
        ChoiceKind::TrustProject,
        format!("Trust {shown}?"),
        body,
        options(ChoiceKind::TrustProject),
    )
}

/// At start, for a file whose safety keys are ignored until you trust it.
pub(crate) fn start_trust_question(
    id: ChoiceId,
    shown: &str,
    unsafe_lines: &[String],
) -> AppChoice {
    let mut body = vec![format!("{shown} sets safety settings you haven't trusted:")];
    body.extend(unsafe_lines.iter().map(|line| format!("  {line}")));
    body.push(
        "The rest of the file applies. Until you trust it, these stay ignored, and anyone who \
         can change the repo could have written them."
            .to_owned(),
    );
    app_choice(
        id,
        ChoiceKind::TrustProject,
        format!("Trust {shown}?"),
        body,
        options(ChoiceKind::TrustProject),
    )
}

/// Each safety key the file sets, as `key = value`. Empty when it sets none or can't be read.
pub(crate) fn safety_lines(file: &Path) -> Vec<String> {
    let Some(table) = std::fs::read_to_string(file)
        .ok()
        .and_then(|text| text.parse::<toml::Table>().ok())
    else {
        return Vec::new();
    };
    let mut lines = Vec::new();
    collect_safety(String::new(), &table, &mut lines);
    lines
}

fn collect_safety(prefix: String, table: &toml::Table, lines: &mut Vec<String>) {
    for (name, value) in table {
        let key = if prefix.is_empty() {
            name.clone()
        } else {
            format!("{prefix}.{name}")
        };
        match value {
            toml::Value::Table(inner) => collect_safety(key, inner, lines),
            value => {
                let text = value.to_string();
                if !safety_keys_in(&key, &text).is_empty() {
                    lines.push(format!("{key} = {text}"));
                }
            }
        }
    }
}

/// The value a table written at `key` gives the leaf `leaf`, such as `base_url` inside
/// `endpoints.ollama = { base_url = "..." }`.
fn value_at(key: &str, value: &str, leaf: &str) -> Option<String> {
    if key == leaf {
        return Some(value.to_owned());
    }
    let rest = leaf.strip_prefix(key)?.strip_prefix('.')?;
    let table = format!("value = {value}").parse::<toml::Table>().ok()?;
    let mut current = table.get("value")?;
    for segment in rest.split('.') {
        current = current.as_table()?.get(segment)?;
    }
    Some(current.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_table_value_names_each_safety_key_inside_it() {
        let value = "{ base_url = \"http://x\", api = \"chat\" }";
        assert_eq!(
            value_at("endpoints.ollama", value, "endpoints.ollama.base_url").as_deref(),
            Some("\"http://x\"")
        );
        assert_eq!(value_at("a.b", "1", "a.b").as_deref(), Some("1"));
    }

    #[test]
    fn a_file_lists_the_safety_keys_it_sets() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("config.local.toml");
        std::fs::write(
            &file,
            "[sandbox]\nmode = \"off\"\n\n[roles.main]\neffort = \"low\"\n",
        )
        .unwrap();
        assert_eq!(
            safety_lines(&file),
            vec!["sandbox.mode = \"off\"".to_owned()]
        );
        assert!(safety_lines(&dir.path().join("missing.toml")).is_empty());
    }
}
