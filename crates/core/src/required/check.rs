//! A written workflow against what you required: the steps it lacks or put elsewhere, and how
//! its tasks are different from a list you fixed. The same check reads a workflow the orchestrator
//! wrote, a file of yours, and what Claude Code reports of its own, since all three come in
//! one shape: steps in order, and the tasks, or the agents' labels, in order.

use super::{Required, RequiredStep, label, same_name};

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Steps {
    /// Required steps the workflow has no step for, by name.
    pub missing: Vec<String>,
    /// Required steps it has, but not where `when` says.
    pub misplaced: Vec<String>,
}

impl Steps {
    pub fn is_empty(&self) -> bool {
        self.missing.is_empty() && self.misplaced.is_empty()
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TaskChanges {
    /// Your tasks the plan doesn't have, merged into another or split away.
    pub missing: Vec<String>,
    /// Tasks the plan has that you didn't list.
    pub added: Vec<String>,
    /// Your tasks are all there, in another order.
    pub reordered: bool,
}

impl TaskChanges {
    pub fn is_empty(&self) -> bool {
        self.missing.is_empty() && self.added.is_empty() && !self.reordered
    }
}

/// How the workflow's steps, `names` in order, meet the required ones.
pub fn steps(required: &Required, names: &[&str]) -> Steps {
    let at = |name: &str| names.iter().position(|step| same_name(step, name));
    let mut found = Steps::default();
    for step in &required.steps {
        match at(&step.name) {
            None => found.missing.push(step.name.clone()),
            Some(index) if !in_place(required, step, index, names) => {
                found.misplaced.push(step.name.clone());
            }
            Some(_) => {}
        }
    }
    found
}

/// Whether `step` at `index` stands where its `when` says. A place naming a step the workflow
/// doesn't have can't be wrong, and several steps that must come first may come in any order.
fn in_place(required: &Required, step: &RequiredStep, index: usize, names: &[&str]) -> bool {
    let Some(when) = step.when.as_deref().map(str::trim) else {
        return true;
    };
    let also = |name: &&str, place: &str| {
        required
            .step(name)
            .and_then(|other| other.when.as_deref())
            .is_some_and(|when| when.trim().eq_ignore_ascii_case(place))
    };
    let plain = when.to_ascii_lowercase();
    if plain == "first" {
        return names[..index].iter().all(|name| also(name, "first"));
    }
    if plain == "last" {
        return names[index + 1..].iter().all(|name| also(name, "last"));
    }
    let target = |word: &str| {
        plain
            .starts_with(word)
            .then(|| when[word.len()..].trim())
            .and_then(|target| names.iter().position(|name| same_name(name, target)))
    };
    match (target("before "), target("after ")) {
        (Some(other), _) => index < other,
        (_, Some(other)) => index > other,
        _ => true,
    }
}

/// How the plan's tasks, `planned` in order, are different from the list the request fixed; `None`
/// when it fixed none. A planned task matches yours by its name, or by a short form of it such
/// as its label: `build:migrate-schema` and `migrate-schema` both stand for `migrate the
/// schema`.
pub fn tasks(required: &Required, planned: &[&str]) -> Option<TaskChanges> {
    if required.tasks.is_empty() {
        return None;
    }
    let mut taken = vec![false; required.tasks.len()];
    let mut order = Vec::new();
    let mut changes = TaskChanges::default();
    for task in planned {
        let matched = required
            .tasks
            .iter()
            .enumerate()
            .position(|(at, yours)| !taken[at] && stands_for(task, yours));
        match matched {
            Some(at) => {
                taken[at] = true;
                order.push(at);
            }
            None => changes.added.push((*task).to_owned()),
        }
    }
    changes.missing = required
        .tasks
        .iter()
        .zip(&taken)
        .filter(|(_, taken)| !**taken)
        .map(|(task, _)| task.clone())
        .collect();
    changes.reordered = order.windows(2).any(|pair| pair[0] > pair[1]);
    Some(changes)
}

/// Whether a task an agent is asked to work on is one of the tasks you fixed, or anything at
/// all when you fixed none.
pub fn is_one_of(fixed: &[String], task: &str) -> bool {
    fixed.is_empty() || fixed.iter().any(|yours| stands_for(task, yours))
}

/// Whether the plan's `task` is your `yours`: the same name, its label, or its words in order
/// starting with its first, so `switch-reads` is `switch reads to the new table`.
fn stands_for(task: &str, yours: &str) -> bool {
    let task = task.rsplit(':').next().unwrap_or(task);
    if same_name(task, yours) || same_name(task, &label(yours)) {
        return true;
    }
    let words = |text: &str| -> Vec<String> {
        text.split(|c: char| !c.is_alphanumeric())
            .filter(|word| !word.is_empty())
            .map(str::to_lowercase)
            .collect()
    };
    let (short, long) = (words(task), words(yours));
    if short.is_empty() || short.first() != long.first() {
        return false;
    }
    let mut rest = long.iter();
    short.iter().all(|word| rest.any(|other| other == word))
}

/// What the orchestrator is told, once, when the workflow it wrote lacks a step you require or
/// put one elsewhere. After that Argonauts Code places the step itself.
pub fn ask_to_place(required: &Required, found: &Steps) -> Option<String> {
    if found.is_empty() {
        return None;
    }
    let mut text = String::from(
        "The workflow you wrote doesn't have every step the user requires where it must be. \
         Write it again with each of these as a step of its own, by this name, where it says. \
         If one is still missing, Argonauts Code adds it itself.\n",
    );
    for name in found.missing.iter().chain(&found.misplaced) {
        if let Some(step) = required.step(name) {
            let state = if found.missing.contains(name) {
                "missing"
            } else {
                "in the wrong place"
            };
            text.push_str(&format!("- {} ({state})\n", super::brief::step_line(step)));
        }
    }
    Some(text)
}

/// What the orchestrator is told when its plan changed the task list the request fixed.
pub fn ask_to_keep_tasks(required: &Required, changes: &TaskChanges) -> Option<String> {
    if changes.is_empty() {
        return None;
    }
    let mut text = String::from(
        "The user fixed the task list, and your plan changed it. Plan exactly these tasks, by \
         these names and in this order, with none merged, split or added:\n",
    );
    for (at, task) in required.tasks.iter().enumerate() {
        text.push_str(&format!("{}. {task}\n", at + 1));
    }
    text.push_str(&changed(changes).join(" "));
    text.push_str(
        "\nYou still decide who takes each and how it is reviewed. If you believe a task is \
         missing, ask the user; don't add it.",
    );
    Some(text)
}

/// Plain lines for a workflow Argonauts Code can't change, such as Claude Code's own: what it
/// lacked of what you required, for the closing answer and the workflow's notice.
pub fn said(found: &Steps, tasks: Option<&TaskChanges>) -> Vec<String> {
    let mut lines = Vec::new();
    if !found.missing.is_empty() {
        lines.push(format!(
            "It had no step for {}, which you require.",
            listed(&found.missing)
        ));
    }
    if !found.misplaced.is_empty() {
        lines.push(format!(
            "It ran {} somewhere other than where you asked.",
            listed(&found.misplaced)
        ));
    }
    if let Some(changes) = tasks.filter(|changes| !changes.is_empty()) {
        lines.extend(changed(changes));
    }
    lines
}

fn changed(changes: &TaskChanges) -> Vec<String> {
    let mut lines = Vec::new();
    if !changes.missing.is_empty() {
        lines.push(format!(
            "Your task list's {} wasn't planned as a task of its own.",
            listed(&changes.missing)
        ));
    }
    if !changes.added.is_empty() {
        lines.push(format!(
            "The plan added {}, which you didn't list.",
            listed(&changes.added)
        ));
    }
    if changes.reordered {
        lines.push("Your tasks weren't kept in your order.".to_owned());
    }
    lines
}

fn listed(names: &[String]) -> String {
    let quoted: Vec<String> = names.iter().map(|name| format!("\"{name}\"")).collect();
    match quoted.as_slice() {
        [] => String::new(),
        [one] => one.clone(),
        [head @ .., last] => format!("{} and {last}", head.join(", ")),
    }
}
