//! The structural diff between two versions of a learned workflow: settings key by key, checklist
//! items one by one, prompt files by content, and the workflow step by step. The mutation rules
//! count what it finds, so "one change at a time" is computed, never trusted.

use std::collections::BTreeSet;
use std::path::Path;

use serde_json::Value;

use argonauts_core::layout::project_files::WORKFLOWS;
use argonauts_core::workflow::Workflow;

use super::keys::{CHECKLIST_KEY, WORKFLOW_KEY, class_for_key, flatten};
use super::{
    Change, ChangeClass, LearnedWorkflow, LoadedLearnedWorkflow, graph, is_workflow_file,
    learned_workflow_dir, loops,
};
use crate::error::{LearningError, Result};

/// Every difference between `parent` and `child`: settings key by key, prompt files by
/// content, and the workflow step by step. The mutation rules allow exactly one.
pub fn diff(parent: &LoadedLearnedWorkflow, child: &LoadedLearnedWorkflow) -> Result<Vec<Change>> {
    let mut changes = Vec::new();
    header_changes(
        &parent.learned_workflow,
        &child.learned_workflow,
        &mut changes,
    );
    settings_changes(
        &parent.learned_workflow,
        &child.learned_workflow,
        &mut changes,
    );
    prompt_changes(parent, child, &mut changes)?;
    workflow_changes(parent, child, &mut changes)?;
    Ok(changes)
}

fn header_changes(parent: &LearnedWorkflow, child: &LearnedWorkflow, out: &mut Vec<Change>) {
    let pairs = [
        (
            "workflow",
            to_json(&parent.workflow),
            to_json(&child.workflow),
        ),
        ("mode", to_json(&parent.mode), to_json(&child.mode)),
        (
            "match.task_types",
            to_json(&parent.matches.task_types),
            to_json(&child.matches.task_types),
        ),
        (
            "match.labels_any",
            to_json(&parent.matches.labels_any),
            to_json(&child.matches.labels_any),
        ),
    ];
    for (key, before, after) in pairs {
        let (before, after) = (present(before), present(after));
        if before != after {
            out.push(setting_change(key, before, after));
        }
    }
}

fn settings_changes(parent: &LearnedWorkflow, child: &LearnedWorkflow, out: &mut Vec<Change>) {
    let before = flatten(&parent.settings);
    let after = flatten(&child.settings);
    let keys: BTreeSet<&String> = before.keys().chain(after.keys()).collect();
    for key in keys {
        let (old, new) = (before.get(key), after.get(key));
        if old == new {
            continue;
        }
        if key == CHECKLIST_KEY {
            checklist_changes(old, new, out);
        } else {
            out.push(setting_change(key, old.cloned(), new.cloned()));
        }
    }
}

/// Each item added or removed is its own change; moving one within the list isn't a change.
fn checklist_changes(before: Option<&Value>, after: Option<&Value>, out: &mut Vec<Change>) {
    let items = |value: Option<&Value>| match value {
        Some(Value::Array(items)) => items.clone(),
        Some(other) => vec![other.clone()],
        None => Vec::new(),
    };
    let (before, after) = (items(before), items(after));
    for item in before.iter().filter(|item| !after.contains(item)) {
        out.push(Change {
            key: CHECKLIST_KEY.to_owned(),
            class: ChangeClass::ChecklistItem,
            before: Some(item.clone()),
            after: None,
        });
    }
    for item in after.iter().filter(|item| !before.contains(item)) {
        out.push(Change {
            key: CHECKLIST_KEY.to_owned(),
            class: ChangeClass::ChecklistItem,
            before: None,
            after: Some(item.clone()),
        });
    }
}

/// Prompt files count by what they say, so a renamed file with the same text is no change.
fn prompt_changes(
    parent: &LoadedLearnedWorkflow,
    child: &LoadedLearnedWorkflow,
    out: &mut Vec<Change>,
) -> Result<()> {
    let keys: BTreeSet<&String> = parent
        .learned_workflow
        .prompts
        .keys()
        .chain(child.learned_workflow.prompts.keys())
        .collect();
    for key in keys {
        let before = parent.learned_workflow.prompts.get(key);
        let after = child.learned_workflow.prompts.get(key);
        let same = match (before, after) {
            (Some(old), Some(new)) => read_beside(parent, old)? == read_beside(child, new)?,
            _ => false,
        };
        if !same {
            let name = format!("prompts.{key}");
            out.push(setting_change(
                &name,
                before.map(|file| Value::String(file.clone())),
                after.map(|file| Value::String(file.clone())),
            ));
        }
    }
    Ok(())
}

fn workflow_changes(
    parent: &LoadedLearnedWorkflow,
    child: &LoadedLearnedWorkflow,
    out: &mut Vec<Change>,
) -> Result<()> {
    let before = parent.learned_workflow.phases.as_deref();
    let after = child.learned_workflow.phases.as_deref();
    if before.is_none() && after.is_none() {
        return Ok(());
    }
    match (load_workflow(parent)?, load_workflow(child)?) {
        (Some(old), Some(new)) => graph::compare(&old, &new, out),
        _ if before == after => {}
        // A built-in workflow by another name: its steps can't be seen from here, so the whole
        // graph counts as one change the mutation rules can't vouch for.
        _ => out.push(Change {
            key: WORKFLOW_KEY.to_owned(),
            class: ChangeClass::StepGraph,
            before: before.map(|name| Value::String(name.to_owned())),
            after: after.map(|name| Value::String(name.to_owned())),
        }),
    }
    Ok(())
}

/// The learned workflow's workflow as steps: a `.star` file built, a YAML file beside it, or a
/// named one in the project's `.argonauts/workflows/`. `None` for a built-in one, known only by
/// name.
pub fn load_workflow(loaded: &LoadedLearnedWorkflow) -> Result<Option<Workflow>> {
    let Some(workflow) = loaded.learned_workflow.phases.as_deref() else {
        return Ok(None);
    };
    let dir = learned_workflow_dir(loaded);
    if workflow.ends_with(".star") {
        return loops::build_file(&dir.join(workflow)).map(Some);
    }
    if is_workflow_file(workflow) {
        return parse_workflow(&dir.join(workflow)).map(Some);
    }
    let Some(argonauts) = dir.parent().and_then(Path::parent) else {
        return Ok(None);
    };
    for extension in ["yaml", "yml"] {
        let path = argonauts
            .join(WORKFLOWS)
            .join(format!("{workflow}.{extension}"));
        if path.is_file() {
            return parse_workflow(&path).map(Some);
        }
    }
    Ok(None)
}

fn parse_workflow(path: &Path) -> Result<Workflow> {
    let text = std::fs::read_to_string(path).map_err(|error| LearningError::Parse {
        path: path.to_owned(),
        message: error.to_string(),
    })?;
    serde_saphyr::from_str(&text).map_err(|error| LearningError::Parse {
        path: path.to_owned(),
        message: error.to_string(),
    })
}

fn read_beside(loaded: &LoadedLearnedWorkflow, file: &str) -> Result<String> {
    let path = learned_workflow_dir(loaded).join(file);
    std::fs::read_to_string(&path).map_err(|error| LearningError::Parse {
        path,
        message: error.to_string(),
    })
}

fn setting_change(key: &str, before: Option<Value>, after: Option<Value>) -> Change {
    Change {
        key: key.to_owned(),
        class: class_for_key(key),
        before,
        after,
    }
}

fn to_json<T: serde::Serialize>(value: &T) -> Value {
    serde_json::to_value(value).unwrap_or(Value::Null)
}

/// An absent header field and an empty one read the same.
fn present(value: Value) -> Option<Value> {
    match &value {
        Value::Null => None,
        Value::Array(items) if items.is_empty() => None,
        _ => Some(value),
    }
}
