//! What a workflow file can get wrong on its own. The engine's validator checks the run rules
//! (checks and a cross-provider review after every `build`, loop maximums, limits, allowed
//! workers, and required approvals) when it runs one.

use argonauts_core::workflows::workflow::{
    Count, Step, StepKind, StepModels, Workflow, WorkflowRef,
};

use super::lookup::load_unchecked;
use super::placeholders::{names_in, names_in_value, sole};
use super::{WorkflowDirs, WorkflowError};

/// Nesting never goes deeper than this, whatever `workflows.max_depth` says. The run's own
/// workflow is depth 1.
pub const MAX_DEPTH: u32 = 2;

/// Problems, each naming the phase and step: a name that doesn't match the file, a phase with
/// no steps, `{{name}}` placeholders that `params` doesn't declare, and `workflow` steps that
/// name a workflow that can't be found, nest deeper than `max_depth`, or loop back.
pub fn check_workflow(
    workflow: &Workflow,
    file_stem: Option<&str>,
    dirs: &WorkflowDirs<'_>,
    max_depth: u32,
) -> Vec<String> {
    let mut problems = Vec::new();
    if let Some(stem) = file_stem
        && stem != workflow.name
    {
        problems.push(format!(
            "the name {:?} doesn't match the file name {stem:?}",
            workflow.name
        ));
    }
    if workflow.phases.is_empty() {
        problems.push("there are no phases".to_owned());
    }
    // Other workflows name this one by its file, so a loop back is spotted by that name.
    let own_name = file_stem.unwrap_or(&workflow.name);
    let limit = max_depth.min(MAX_DEPTH);
    for phase in &workflow.phases {
        if phase.steps.is_empty() {
            problems.push(format!("phase {:?} has no steps", phase.name));
        }
        visit_steps(&phase.steps, &mut Vec::new(), &mut |path, step| {
            let at = format!(
                "phase {:?}, step {} ({})",
                phase.name,
                dotted(path),
                kind_name(step.kind)
            );
            let mut found = placeholder_problems(step, workflow);
            if step.kind == StepKind::Workflow {
                found.extend(nesting_problems(step, own_name, dirs, limit));
            }
            problems.extend(found.into_iter().map(|problem| format!("{at}: {problem}")));
        });
    }
    problems
}

/// `count` and `models` are a value or one placeholder, and every placeholder in them, the
/// task, and a nested workflow's `params` is declared.
fn placeholder_problems(step: &Step, workflow: &Workflow) -> Vec<String> {
    let mut problems = Vec::new();
    let mut used: Vec<(&str, &str)> = Vec::new();
    if let Some(Count::Param(text)) = &step.count {
        match sole(text) {
            Some(name) => used.push(("count", name)),
            None => problems.push(format!(
                "count {text:?} must be a number or one {{{{name}}}} placeholder"
            )),
        }
    }
    if let StepModels::Param(text) = &step.models {
        match sole(text) {
            Some(name) => used.push(("models", name)),
            None => problems.push(format!(
                "models {text:?} must be a list or one {{{{name}}}} placeholder"
            )),
        }
    }
    if let Some(task) = &step.task {
        used.extend(names_in(task).into_iter().map(|name| ("task", name)));
    }
    for value in step.params.0.values() {
        used.extend(
            names_in_value(value)
                .into_iter()
                .map(|name| ("params", name)),
        );
    }
    let mut undeclared: Vec<(&str, &str)> = Vec::new();
    for (field, name) in used {
        if !workflow.params.0.contains_key(name) && !undeclared.contains(&(field, name)) {
            undeclared.push((field, name));
        }
    }
    problems.extend(
        undeclared
            .into_iter()
            .map(|(field, name)| format!("{{{{{name}}}}} in {field} isn't declared in params")),
    );
    problems
}

/// The workflow a `workflow` step runs must load, stay within the depth limit, never run a
/// workflow it's already inside, and take only the params it declares.
fn nesting_problems(
    step: &Step,
    own_name: &str,
    dirs: &WorkflowDirs<'_>,
    limit: u32,
) -> Vec<String> {
    let Some(target) = &step.workflow else {
        return vec!["names no workflow to run".to_owned()];
    };
    let mut problems = Vec::new();
    let mut chain = vec![own_name.to_owned()];
    if let Some(nested) = follow(target, &mut chain, dirs, limit, &mut problems) {
        for key in step.params.0.keys() {
            if !nested.params.0.contains_key(key) {
                problems.push(format!(
                    "passes {key:?}, which {target} doesn't declare in its params"
                ));
            }
        }
    }
    problems
}

/// Walks into the workflow `target`. `chain` holds the workflows it runs inside, outermost
/// first. Returns the workflow when it loads.
fn follow(
    target: &WorkflowRef,
    chain: &mut Vec<String>,
    dirs: &WorkflowDirs<'_>,
    limit: u32,
    problems: &mut Vec<String>,
) -> Option<Workflow> {
    let route = chain
        .iter()
        .map(String::as_str)
        .chain([target.as_str()])
        .collect::<Vec<_>>()
        .join(" →  ");
    if chain.iter().any(|name| name == target.as_str()) {
        problems.push(format!("loops back to {target}: {route}"));
        return None;
    }
    let depth = chain.len() + 1;
    if depth > limit as usize {
        problems.push(format!(
            "nests too deep: {route} is {depth} deep, and the most is {limit}"
        ));
        return None;
    }
    let nested = match load_unchecked(target, dirs) {
        Ok(nested) => nested,
        Err(WorkflowError::NotFound { .. }) => {
            problems.push(format!("can't find {target} ({route})"));
            return None;
        }
        Err(error) => {
            problems.push(format!(
                "{target} doesn't load ({route}): {}",
                error.reason()
            ));
            return None;
        }
    };
    chain.push(target.to_string());
    for next in workflow_refs(&nested) {
        follow(next, chain, dirs, limit, problems);
    }
    chain.pop();
    Some(nested)
}

/// The workflows that `workflow` steps name, at any depth of `steps`.
fn workflow_refs(workflow: &Workflow) -> Vec<&WorkflowRef> {
    let mut refs = Vec::new();
    for phase in &workflow.phases {
        visit_steps(&phase.steps, &mut Vec::new(), &mut |_, step| {
            if step.kind == StepKind::Workflow
                && let Some(target) = &step.workflow
            {
                refs.push(target);
            }
        });
    }
    refs
}

/// Calls `visit` on every step, sub-steps after their parent, with its 1-based path.
fn visit_steps<'a>(
    steps: &'a [Step],
    path: &mut Vec<usize>,
    visit: &mut dyn FnMut(&[usize], &'a Step),
) {
    for (index, step) in steps.iter().enumerate() {
        path.push(index + 1);
        visit(path, step);
        visit_steps(&step.steps, path, visit);
        path.pop();
    }
}

fn dotted(path: &[usize]) -> String {
    path.iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join(".")
}

/// The kind as a file spells it, such as `fan_out`.
fn kind_name(kind: StepKind) -> String {
    serde_json::to_value(kind)
        .ok()
        .and_then(|value| value.as_str().map(str::to_owned))
        .unwrap_or_else(|| format!("{kind:?}"))
}
