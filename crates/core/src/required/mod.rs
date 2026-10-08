//! Steps every workflow must contain, and a task list a request fixed. You keep the steps in
//! `workflows/always.toml`, in this repository's `.argonauts/` or in yours, or say them in a
//! request ("every workflow must end with a benchmark run"); a request may also fix the tasks
//! ("do exactly these three tasks: ..."). Whoever writes the workflow is told them
//! ([`brief`]), and what it wrote, or what Claude Code reports of its own, is checked against
//! them ([`check`]).

mod always;
pub mod brief;
pub mod check;
mod words;

use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::prompt_parts::CrewRole;

pub use always::{FILE, GUIDE, STARTER, parse, steps_toml};
pub use words::label;

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Required {
    pub steps: Vec<RequiredStep>,
    /// The tasks to do, exactly these, in this order. Empty leaves the list to the plan.
    pub tasks: Vec<String>,
}

// A step a setup kept for one conversation or one request travels with the run it starts.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RequiredStep {
    pub name: String,
    pub role: CrewRole,
    pub does: Option<String>,
    /// `first`, `last`, `before <step>` or `after <step>`. Without it, before the sign-off.
    pub when: Option<String>,
}

impl Required {
    pub fn is_empty(&self) -> bool {
        self.steps.is_empty() && self.tasks.is_empty()
    }

    /// The required step named `name`, whatever its case and punctuation.
    pub fn step(&self, name: &str) -> Option<&RequiredStep> {
        self.steps.iter().find(|step| same_name(&step.name, name))
    }

    /// Adds `step`, or puts it in place of one of the same name.
    fn insert(&mut self, step: RequiredStep) {
        match self
            .steps
            .iter_mut()
            .find(|known| same_name(&known.name, &step.name))
        {
            Some(known) => *known = step,
            None => self.steps.push(step),
        }
    }
}

/// What the files and a request require, and what couldn't be read.
#[derive(Debug, Default)]
pub struct Read {
    pub required: Required,
    /// One line each, naming the file or the words: a file that isn't valid is left out whole,
    /// and a task list whose count disagrees with itself isn't fixed.
    pub problems: Vec<String>,
}

/// What a request must contain. `folders` are the workflows folders holding `always.toml`,
/// yours first and then this repository's, so the repository's step of the same name wins, and
/// the request's own words win over both.
pub fn read(folders: &[&Path], request: &str) -> Read {
    read_with(folders, &[], request)
}

/// [`read`], with the steps a setup `kept` for this conversation or this request alone. They
/// win over the files, and the request's own words win over them.
pub fn read_with(folders: &[&Path], kept: &[RequiredStep], request: &str) -> Read {
    let mut read = Read::default();
    for folder in folders {
        match always::read(folder) {
            Ok(steps) => steps
                .into_iter()
                .for_each(|step| read.required.insert(step)),
            Err(problem) => read.problems.push(problem),
        }
    }
    kept.iter()
        .cloned()
        .for_each(|step| read.required.insert(step));
    let said = words::said(request);
    said.steps
        .into_iter()
        .for_each(|step| read.required.insert(step));
    read.required.tasks = said.tasks;
    read.problems.extend(said.problems);
    read
}

/// Names match whatever their case and punctuation, so `sign-off` finds `Sign off`.
pub fn same_name(a: &str, b: &str) -> bool {
    let plain = |name: &str| -> String {
        name.chars()
            .filter(|c| c.is_alphanumeric())
            .flat_map(char::to_lowercase)
            .collect()
    };
    plain(a) == plain(b)
}

/// Whether `when` is one of the places a step can go, as `always.toml` and the setup take it.
pub fn when_problem(when: &str) -> Option<String> {
    let when = when.trim();
    let plain = when.to_ascii_lowercase();
    let names_a_step = ["before ", "after "]
        .iter()
        .any(|word| plain.starts_with(word) && !when[word.len()..].trim().is_empty());
    (plain != "first" && plain != "last" && !names_a_step).then(|| {
        format!(
            "when = {when:?} isn't a place: use first, last, before <step> or after <step>, such \
             as \"before sign-off\""
        )
    })
}

/// The crew role a file or the setup names, in the singular too: `reviewer` is `reviewers`.
pub fn role_named(name: &str) -> Option<CrewRole> {
    let name = name.trim().to_ascii_lowercase();
    match name.as_str() {
        "worker" => Some(CrewRole::Workers),
        "reviewer" => Some(CrewRole::Reviewers),
        _ => CrewRole::parse(&name),
    }
}

#[cfg(test)]
mod tests;
