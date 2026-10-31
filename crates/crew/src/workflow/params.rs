//! Fills `{{name}}` placeholders from a workflow's params and the main agent's settings for them.

use serde_json::Value;

use argonauts_core::base::names::ModelRef;
use argonauts_core::workflows::workflow::{Count, Step, StepModels, Workflow, WorkflowParams};

/// The workflow with every placeholder replaced, its own defaults overridden by `params`.
/// `Err` lists each placeholder that has no value, and each value of the wrong type.
pub fn fill(workflow: &Workflow, params: &WorkflowParams) -> Result<Workflow, Vec<String>> {
    let mut values = workflow.params.0.clone();
    values.extend(params.0.clone());
    let values = WorkflowParams(values);
    let mut errors = Vec::new();
    let mut filled = workflow.clone();
    filled.params = values.clone();
    for phase in &mut filled.phases {
        for step in &mut phase.steps {
            fill_step(step, &values, &mut errors);
        }
    }
    if errors.is_empty() {
        Ok(filled)
    } else {
        errors.dedup();
        Err(errors)
    }
}

fn fill_step(step: &mut Step, values: &WorkflowParams, errors: &mut Vec<String>) {
    for text in [
        &mut step.name,
        &mut step.different_provider_from,
        &mut step.when,
        &mut step.task,
        &mut step.input,
        &mut step.until,
    ]
    .into_iter()
    .flatten()
    {
        *text = fill_text(text, values, errors);
    }
    if let Some(Count::Param(text)) = &step.count {
        step.count = count(text, values, errors).map(Count::Fixed);
    }
    if let StepModels::Param(text) = &step.models {
        step.models = StepModels::Named(models(text, values, errors));
    }
    // A nested workflow's own params may name this one's, such as `reviewers: "{{reviewers}}"`.
    for value in step.params.0.values_mut() {
        if let Value::String(text) = value {
            if let Some(name) = whole_placeholder(text) {
                match values.0.get(name) {
                    Some(found) => *value = found.clone(),
                    None => errors.push(format!("no value for `{{{{{name}}}}}`")),
                }
            } else {
                *text = fill_text(text, values, errors);
            }
        }
    }
    for inner in &mut step.steps {
        fill_step(inner, values, errors);
    }
}

/// The name inside `{{name}}` when that is the whole text.
fn whole_placeholder(text: &str) -> Option<&str> {
    let inner = text.trim().strip_prefix("{{")?.strip_suffix("}}")?;
    (!inner.contains("{{") && !inner.contains("}}")).then(|| inner.trim())
}

fn fill_text(text: &str, values: &WorkflowParams, errors: &mut Vec<String>) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find("{{") {
        let Some(len) = rest[start..].find("}}") else {
            break;
        };
        out.push_str(&rest[..start]);
        let name = rest[start + 2..start + len].trim();
        match values.0.get(name) {
            Some(Value::String(value)) => out.push_str(value),
            Some(value @ (Value::Number(_) | Value::Bool(_))) => out.push_str(&value.to_string()),
            Some(_) => errors.push(format!("`{name}` must be text, a number, or true or false")),
            None => errors.push(format!("no value for `{{{{{name}}}}}`")),
        }
        rest = &rest[start + len + 2..];
    }
    out.push_str(rest);
    out
}

fn count(text: &str, values: &WorkflowParams, errors: &mut Vec<String>) -> Option<u32> {
    let Some(name) = whole_placeholder(text) else {
        errors.push(format!(
            "count must be a number or one `{{{{name}}}}`, not `{text}`"
        ));
        return None;
    };
    let value = match values.0.get(name) {
        Some(value) => value,
        None => {
            errors.push(format!("no value for `{{{{{name}}}}}`"));
            return None;
        }
    };
    let number = match value {
        Value::Number(number) => number.as_u64(),
        // A value the main agent set as YAML text, such as "2".
        Value::String(text) => text.trim().parse().ok(),
        _ => None,
    };
    match number.and_then(|number| u32::try_from(number).ok()) {
        Some(number) => Some(number),
        None => {
            errors.push(format!("`{name}` must be a whole number, not {value}"));
            None
        }
    }
}

fn models(text: &str, values: &WorkflowParams, errors: &mut Vec<String>) -> Vec<ModelRef> {
    let Some(name) = whole_placeholder(text) else {
        errors.push(format!(
            "models must be a list or one `{{{{name}}}}`, not `{text}`"
        ));
        return Vec::new();
    };
    let items: Vec<&Value> = match values.0.get(name) {
        Some(Value::Array(items)) => items.iter().collect(),
        Some(value @ Value::String(_)) => vec![value],
        Some(value) => {
            errors.push(format!("`{name}` must be a list of workers, not {value}"));
            return Vec::new();
        }
        None => {
            errors.push(format!("no value for `{{{{{name}}}}}`"));
            return Vec::new();
        }
    };
    items
        .into_iter()
        .filter_map(|item| match item.as_str().map(str::parse::<ModelRef>) {
            Some(Ok(worker)) => Some(worker),
            _ => {
                errors.push(format!("`{name}` holds {item}, which isn't a worker"));
                None
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workflow::yaml;

    fn workflow(text: &str) -> Workflow {
        yaml::parse(text).unwrap_or_else(|err| panic!("{err}"))
    }

    const PANEL: &str = r#"
name: panel
params: { reviewers: 2, target: diff }
phases:
  - name: Review
    standard: check
    steps:
      - kind: fan_out
        role: reviewer
        count: "{{reviewers}}"
        models: "{{who}}"
        task: "Review the {{target}} for {{ focus }}"
"#;

    #[test]
    fn params_fill_placeholders_and_override_defaults() {
        let mut params = WorkflowParams::default();
        params.0.insert("reviewers".to_owned(), Value::from("3"));
        params
            .0
            .insert("who".to_owned(), serde_json::json!(["codex/gpt-5", "agy"]));
        params.0.insert("focus".to_owned(), Value::from("retries"));
        let filled = fill(&workflow(PANEL), &params).unwrap_or_else(|err| panic!("{err:?}"));
        let step = &filled.phases[0].steps[0];
        assert_eq!(step.count, Some(Count::Fixed(3)));
        assert_eq!(
            step.models,
            StepModels::Named(vec![
                ModelRef::new("codex", Some("gpt-5".to_owned())),
                ModelRef::new("agy", None)
            ])
        );
        assert_eq!(step.task.as_deref(), Some("Review the diff for retries"));
    }

    #[test]
    fn missing_and_mistyped_params_are_listed() {
        let mut params = WorkflowParams::default();
        params.0.insert("reviewers".to_owned(), Value::from("many"));
        let errors = fill(&workflow(PANEL), &params).err().unwrap_or_default();
        assert_eq!(errors.len(), 3, "{errors:?}");
        assert!(
            errors
                .iter()
                .any(|error| error.contains("`reviewers` must be a whole number"))
        );
        assert!(errors.iter().any(|error| error.contains("{{who}}")));
        assert!(errors.iter().any(|error| error.contains("{{focus}}")));
    }
}
