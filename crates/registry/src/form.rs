//! The settings half of the main session's setup form, made from the catalog `/config` is
//! drawn from. The form itself tells the model every setting there is and the values each
//! takes, so it never goes stale and needs no prompt to explain it.

use argonauts_core::settings::SettingMeta;
use serde_json::{Map, Value as Json, json};

use crate::catalog::settings_catalog;

/// A JSON schema for an object of settings by dotted key, such as
/// `{"limits.spend_per_day": 5}`. A key the catalog writes with `*`, such as
/// `agents.*.model`, is a pattern that takes any plain name there.
pub fn settings_form() -> Json {
    let mut keys = Map::new();
    let mut patterns = Map::new();
    for meta in settings_catalog() {
        let field = field(&meta);
        if meta.key.contains('*') {
            patterns.insert(pattern(&meta.key), field);
        } else {
            keys.insert(meta.key, field);
        }
    }
    json!({
        "type": "object",
        "properties": keys,
        "patternProperties": patterns,
        "additionalProperties": false
    })
}

fn field(meta: &SettingMeta) -> Json {
    let mut field = Map::new();
    let mut description = meta.description.clone();
    if meta.key == "workflow" {
        // Workflows of the user's own aren't in the catalog, so the list can't be the rule.
        description.push_str(&format!(
            ". Built in: {}, or a workflow of the user's own by its name",
            meta.allowed.join(", ")
        ));
    } else {
        let values: Vec<Json> = meta
            .allowed
            .iter()
            .filter_map(|text| json_of(text))
            .collect();
        if !values.is_empty() {
            field.insert("enum".to_owned(), Json::Array(values));
        }
    }
    if !description.is_empty() {
        field.insert("description".to_owned(), Json::String(description));
    }
    Json::Object(field)
}

/// `agents.*.model` as `^agents\.[A-Za-z0-9_-]+\.model$`, with a map key a plain name.
fn pattern(key: &str) -> String {
    let parts: Vec<String> = key
        .split('.')
        .map(|part| match part {
            "*" => "[A-Za-z0-9_-]+".to_owned(),
            name => name.replace('-', "\\-"),
        })
        .collect();
    format!("^{}$", parts.join("\\."))
}

/// A value as the catalog writes it, TOML text, as JSON.
fn json_of(toml_text: &str) -> Option<Json> {
    let table: toml::Table = format!("value = {toml_text}").parse().ok()?;
    serde_json::to_value(table.get("value")?).ok()
}

#[cfg(test)]
mod tests {
    use regex::Regex;

    use super::*;

    fn form() -> Json {
        settings_form()
    }

    #[test]
    fn the_form_names_every_setting_in_the_catalog() {
        let form = form();
        let keys = form["properties"].as_object().expect("keys");
        let patterns: Vec<Regex> = form["patternProperties"]
            .as_object()
            .expect("patterns")
            .keys()
            .map(|pattern| Regex::new(pattern).expect("a pattern"))
            .collect();
        for meta in settings_catalog() {
            let filled = meta.key.replace('*', "codex");
            let named = keys.contains_key(&filled) || patterns.iter().any(|p| p.is_match(&filled));
            assert!(named, "{} isn't on the form", meta.key);
        }
        assert_eq!(form["additionalProperties"], false);
    }

    #[test]
    fn a_setting_with_choices_lists_them_as_json() {
        let form = form();
        let keys = &form["properties"];
        let theme = settings_catalog()
            .into_iter()
            .find(|meta| meta.key == "ui.theme")
            .expect("ui.theme");
        let listed = keys["ui.theme"]["enum"].as_array().expect("its choices");
        assert_eq!(listed.len(), theme.allowed.len());
        assert!(listed.iter().all(Json::is_string), "{listed:?}");
        let a_switch = settings_catalog()
            .into_iter()
            .find(|meta| meta.allowed == ["true", "false"] && !meta.key.contains('*'))
            .expect("a boolean setting");
        assert_eq!(keys[&a_switch.key]["enum"], json!([true, false]));
    }

    #[test]
    fn a_workflow_of_your_own_is_allowed_by_name() {
        let form = form();
        let workflow = &form["properties"]["workflow"];
        assert!(workflow.get("enum").is_none(), "{workflow}");
        let said = workflow["description"].as_str().unwrap_or_default();
        assert!(said.contains("of the user's own"), "{said}");
    }

    #[test]
    fn a_map_key_is_any_plain_name() {
        let pattern = Regex::new(&pattern("agents.*.model")).expect("a pattern");
        assert!(pattern.is_match("agents.codex.model"));
        assert!(pattern.is_match("agents.my-cli_2.model"));
        assert!(!pattern.is_match("agents.codex.model.extra"));
        assert!(!pattern.is_match("agents..model"));
    }
}
