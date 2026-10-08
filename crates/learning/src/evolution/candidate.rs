//! Turning the retro's proposal into a candidate's recipe folder: the parent's files, plus the
//! one changed setting, prompt, or workflow, written as the next version.

use std::path::Path;

use serde_json::Value;

use argonauts_core::steps::card::ProcessChange;

use crate::error::{LearningError, Result};
use crate::recipes::{
    LoadedRecipe, WORKFLOW_KEY, is_workflow_file, load_file, recipe_dir, render, toml_to_json,
    version_file,
};

/// Writes version `version` of the parent's recipe, with `proposal` applied, into `folder`.
pub fn materialize(
    parent: &LoadedRecipe,
    version: u32,
    proposal: &ProcessChange,
    folder: &Path,
) -> Result<LoadedRecipe> {
    if folder.exists() {
        std::fs::remove_dir_all(folder)?;
    }
    std::fs::create_dir_all(folder)?;
    let mut beside: Vec<&String> = parent.recipe.prompts.values().collect();
    if let Some(workflow) = parent
        .recipe
        .phases
        .as_ref()
        .filter(|w| is_workflow_file(w))
    {
        beside.push(workflow);
    }
    for file in beside {
        std::fs::copy(recipe_dir(parent).join(file), folder.join(file))?;
    }

    let mut child = parent.recipe.clone();
    child.version = version;
    child.parent = Some(parent.recipe.version);
    let refuse = |message: String| LearningError::Refused(message);
    let value = toml_to_json(&proposal.value).map_err(refuse)?;
    let key = proposal.setting.as_str();
    if key == WORKFLOW_KEY {
        let Value::String(workflow) = value else {
            return Err(refuse(format!("`{key}` takes a workflow name")));
        };
        child.phases = Some(workflow);
    } else if let Some(prompt) = key.strip_prefix("prompts.") {
        let Value::String(template) = value else {
            return Err(refuse(format!("`{key}` takes the template's text")));
        };
        let file = format!("{prompt}-v{version}.md");
        std::fs::write(folder.join(&file), template)?;
        child.prompts.insert(prompt.to_owned(), file);
    } else {
        set_dotted(&mut child.settings, key, value);
    }

    let path = folder.join(version_file(version));
    std::fs::write(&path, render(&child)?)?;
    load_file(&path)
}

/// Sets `a.b.c` in a recipe's settings, making the tables on the way.
fn set_dotted(settings: &mut std::collections::BTreeMap<String, Value>, key: &str, value: Value) {
    let mut parts = key.split('.');
    let Some(first) = parts.next() else {
        return;
    };
    let rest: Vec<&str> = parts.collect();
    let Some((last, middle)) = rest.split_last() else {
        settings.insert(first.to_owned(), value);
        return;
    };
    let mut node = settings
        .entry(first.to_owned())
        .or_insert_with(|| Value::Object(Default::default()));
    for part in middle {
        if !node.is_object() {
            *node = Value::Object(Default::default());
        }
        let Value::Object(map) = node else {
            return;
        };
        node = map
            .entry((*part).to_owned())
            .or_insert_with(|| Value::Object(Default::default()));
    }
    if !node.is_object() {
        *node = Value::Object(Default::default());
    }
    if let Value::Object(map) = node {
        map.insert((*last).to_owned(), value);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dotted_keys_make_their_tables() {
        let mut settings = std::collections::BTreeMap::new();
        set_dotted(&mut settings, "critique.rounds", Value::from(2));
        set_dotted(
            &mut settings,
            "critique.checklist",
            serde_json::json!(["a"]),
        );
        set_dotted(
            &mut settings,
            "roles.worker.native_fanout.max",
            Value::from(2),
        );
        set_dotted(&mut settings, "crew", serde_json::json!(["codex"]));
        assert_eq!(
            serde_json::to_value(&settings).unwrap(),
            serde_json::json!({
                "critique": { "rounds": 2, "checklist": ["a"] },
                "roles": { "worker": { "native_fanout": { "max": 2 } } },
                "crew": ["codex"],
            })
        );
    }
}
