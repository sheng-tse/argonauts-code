//! A recipe as step-by-step instructions for one agent. The body is built from the recipe's
//! shape: what it's for, its phases and steps, its checklist, and its gates by name. It never
//! reads a setting's value, the environment, or a gate's command, so none can leak into a skill
//! every later session reads.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::sync::LazyLock;

use regex::Regex;
use serde_json::Value;

use argonauts_core::workflow::{Phase, Step, StepKind};

use super::document::{self, MAX_DESCRIPTION};
use super::{Frontmatter, META_RECIPE, META_SHA256, META_VERSION, Skill};
use crate::error::{LearningError, Result};
use crate::recipes::{LoadedRecipe, Match, Recipe};
use crate::scan::{Finding, scan};

pub fn export(loaded: &LoadedRecipe) -> Result<Skill> {
    let recipe = &loaded.recipe;
    let name = recipe.recipe.clone();
    let body = document::written_body(&body(recipe));
    let mut metadata = BTreeMap::new();
    metadata.insert(META_RECIPE.to_owned(), name.clone());
    metadata.insert(META_VERSION.to_owned(), recipe.version.to_string());
    metadata.insert(META_SHA256.to_owned(), document::body_hash(&body));
    let skill = Skill {
        frontmatter: Frontmatter {
            name: name.clone(),
            description: description(recipe),
            license: None,
            compatibility: None,
            metadata,
            allowed_tools: None,
        },
        body,
    };
    let text = document::render(&skill);
    refuse_unsafe(&text)?;
    let problems = document::validate(&text, &name);
    if !problems.is_empty() {
        return Err(LearningError::Refused(format!(
            "The {name} workflow can't be exported as a skill: {}.",
            problems.join("; ")
        )));
    }
    Ok(skill)
}

/// Refuses a secret by its kind only, hidden text, and paths under a home folder.
fn refuse_unsafe(text: &str) -> Result<()> {
    let findings = scan(text);
    if let Some(Finding::Secret { kind }) = findings
        .iter()
        .find(|finding| matches!(finding, Finding::Secret { .. }))
    {
        return Err(LearningError::Refused(format!(
            "The skill would carry a {kind}, so the export is refused. Take it out of the \
             learned workflow first."
        )));
    }
    if let Some(hidden) = findings.iter().find(|finding| finding.refuses()) {
        return Err(LearningError::Refused(format!(
            "The skill would carry {hidden}, so the export is refused."
        )));
    }
    if holds_home_path(text) {
        return Err(LearningError::Refused(
            "The skill would carry a path under a home folder, so the export is refused. Use a \
             path inside the repository instead."
                .to_owned(),
        ));
    }
    Ok(())
}

static HOME_PATH: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?:^|[\s`'\x22(=:])(?:~/|/Users/[^/\s]+/|/home/[^/\s]+/|[A-Za-z]:\\Users\\)")
        .unwrap_or_else(|err| panic!("home path pattern: {err}"))
});

fn holds_home_path(text: &str) -> bool {
    if HOME_PATH.is_match(text) {
        return true;
    }
    std::env::var("HOME")
        .ok()
        .filter(|home| home.len() > 1)
        .is_some_and(|home| text.contains(home.trim_end_matches('/')))
}

fn description(recipe: &Recipe) -> String {
    let mut text = one_line(&recipe.description);
    if text.is_empty() {
        text = format!(
            "The {} workflow, as steps one agent can follow.",
            recipe.recipe
        );
    }
    if !text.ends_with(['.', '!', '?']) {
        text.push('.');
    }
    if let Some(when) = when_to_use(&recipe.matches) {
        text.push(' ');
        text.push_str(&when);
    }
    document::cut(&text, MAX_DESCRIPTION)
}

fn one_line(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn when_to_use(matches: &Match) -> Option<String> {
    let types = listed(&matches.task_types);
    let labels = listed(&matches.labels_any);
    match (types, labels) {
        (Some(types), Some(labels)) => Some(format!(
            "Use it for {types} tasks, or tasks labeled {labels}."
        )),
        (Some(types), None) => Some(format!("Use it for {types} tasks.")),
        (None, Some(labels)) => Some(format!("Use it for tasks labeled {labels}.")),
        (None, None) => None,
    }
}

/// "a", "a or b", "a, b, or c".
fn listed(items: &[String]) -> Option<String> {
    let items: Vec<String> = items
        .iter()
        .map(|item| one_line(item))
        .filter(|item| !item.is_empty())
        .collect();
    match items.as_slice() {
        [] => None,
        [one] => Some(one.clone()),
        [first, second] => Some(format!("{first} or {second}")),
        [rest @ .., last] => Some(format!("{}, or {last}", rest.join(", "))),
    }
}

fn body(recipe: &Recipe) -> String {
    let name = &recipe.recipe;
    let mut text = format!("# {name}\n\n");
    let summary = one_line(&recipe.description);
    if !summary.is_empty() {
        let _ = writeln!(text, "{summary}\n");
    }
    if let Some(when) = when_to_use(&recipe.matches) {
        let _ = writeln!(text, "{when}\n");
    }
    let _ = writeln!(
        text,
        "These steps are version {} of `{name}`, the learned workflow that worked best for \
         this kind of task when a crew of agents ran it in this repository, written out for one \
         agent working alone.\n",
        recipe.version
    );

    let checklist = checklist(recipe);
    let gates = gates(recipe);
    text.push_str("## Steps\n\n");
    let phases =
        inline_phases(recipe).unwrap_or_else(|| default_phases(recipe, &checklist, &gates));
    for (index, (title, steps)) in phases.iter().enumerate() {
        let _ = writeln!(text, "### {}. {title}\n", index + 1);
        for (number, step) in steps.iter().enumerate() {
            let _ = writeln!(text, "{}. {step}", number + 1);
        }
        text.push('\n');
    }

    if !checklist.is_empty() {
        text.push_str("## Checklist\n\nCheck the plan against each item before you build.\n\n");
        for item in &checklist {
            let _ = writeln!(text, "- {item}");
        }
        text.push('\n');
    }
    if !gates.is_empty() {
        text.push_str("## Checks\n\nThe work is done only when each of these holds.\n\n");
        for gate in &gates {
            let _ = writeln!(text, "- {gate}");
        }
        text.push('\n');
    }
    text.push_str(
        "## Limits\n\nLeave approvals and merging to the person you work for. Never weaken, skip, \
         or delete a test to make it pass, and never commit a secret.\n",
    );
    text
}

type Phases = Vec<(String, Vec<String>)>;

fn section<'a>(recipe: &'a Recipe, key: &str) -> Option<&'a Value> {
    recipe.settings.get(key).filter(|value| !value.is_null())
}

fn enabled(recipe: &Recipe, key: &str) -> bool {
    section(recipe, key)
        .is_some_and(|value| value.get("enabled").and_then(Value::as_bool) != Some(false))
}

/// The protocol's phases, with a step for each part the recipe turns on.
fn default_phases(recipe: &Recipe, checklist: &[String], gates: &[String]) -> Phases {
    let mut plan = vec![
        "Draft a plan on your own first: the change, the files it touches, and how you'll test \
         it."
        .to_owned(),
    ];
    if enabled(recipe, "experiments") {
        plan.push(
            "Try the riskiest assumption in a small throwaway experiment, and keep only what you \
             learn."
                .to_owned(),
        );
    }
    if !checklist.is_empty() || enabled(recipe, "critique") {
        plan.push(
            "Critique the plan as a skeptic from another team would, against the checklist \
             below, and fix what it finds."
                .to_owned(),
        );
    }
    if enabled(recipe, "judging") {
        plan.push(
            "If you drafted more than one option, compare them on the evidence and keep the \
             strongest."
                .to_owned(),
        );
    }

    let mut build = Vec::new();
    if let Some(divide) = section(recipe, "divide") {
        let mut step = "Split the work into small pieces with clear interfaces.".to_owned();
        if divide.get("contracts_first").and_then(Value::as_bool) == Some(true) {
            step.push_str(" Write the interfaces and their acceptance tests first.");
        }
        build.push(step);
    }
    build.push("Build in small steps, running the tests as you go.".to_owned());

    let mut check = vec![if gates.is_empty() {
        "Run the project's tests and linters, and fix what fails.".to_owned()
    } else {
        "Check each check below, and fix what fails.".to_owned()
    }];
    check.push(
        "Review your own diff as a reviewer from another team would: correctness, tests, \
         interfaces, and anything risky."
            .to_owned(),
    );

    vec![
        (
            "Understand".to_owned(),
            vec![
                "Restate the goal, the acceptance criteria, and what's out of scope, in your own \
                 words."
                    .to_owned(),
                "Read the code the task touches before you change anything.".to_owned(),
            ],
        ),
        ("Plan".to_owned(), plan),
        ("Build".to_owned(), build),
        ("Check".to_owned(), check),
        (
            "Finish".to_owned(),
            vec![
                "Summarize what changed, how you tested it, and anything left for a person to \
                 decide."
                    .to_owned(),
            ],
        ),
    ]
}

/// A recipe that writes its workflow's phases out in full.
fn inline_phases(recipe: &Recipe) -> Option<Phases> {
    let phases: Vec<Phase> = serde_json::from_value(section(recipe, "phases")?.clone()).ok()?;
    let phases: Phases = phases
        .iter()
        .map(|phase| {
            let title = one_line(&phase.name);
            let steps = phase
                .steps
                .iter()
                .map(|step| instruction(step, 0))
                .collect();
            (title, steps)
        })
        .filter(|(title, steps): &(String, Vec<String>)| !title.is_empty() && !steps.is_empty())
        .collect();
    (!phases.is_empty()).then_some(phases)
}

/// One step as a sentence for one agent. Loops and per-item steps carry their inner steps as
/// an indented list.
fn instruction(step: &Step, depth: usize) -> String {
    let text = match step.kind {
        StepKind::Restate => {
            "Restate the goal, the acceptance criteria, and what's out of scope, in your own words."
                .to_owned()
        }
        StepKind::Differ => "Write down where the options differ, quoting each.".to_owned(),
        StepKind::Draft if step.blind => {
            "Draft your own answer before you look at anyone else's.".to_owned()
        }
        StepKind::Draft => "Draft an answer.".to_owned(),
        StepKind::Map => "Compare the drafts point by point, quoting each.".to_owned(),
        StepKind::FanOut => "Work through the task from each angle in turn, separately.".to_owned(),
        StepKind::ForEach => "For each item of the earlier output:".to_owned(),
        StepKind::Review => {
            "Review the work as a skeptical reviewer from another team would.".to_owned()
        }
        StepKind::Judge => {
            "Score the candidates against the criteria, and keep the strongest on the evidence."
                .to_owned()
        }
        StepKind::Refute => {
            "Try to disprove each finding or claim, and drop what doesn't survive.".to_owned()
        }
        StepKind::Synthesize => "Combine what survived into one answer.".to_owned(),
        StepKind::Build => {
            "Make the change in small steps, running the tests as you go.".to_owned()
        }
        StepKind::Gate => "Check the checks below, and fix what fails.".to_owned(),
        StepKind::Check => {
            "Check each done-when criterion against evidence: a passing test, or a file and line."
                .to_owned()
        }
        StepKind::LoopUntil => match step.max {
            Some(max) => format!("Repeat these steps until they succeed, at most {max} times:"),
            None => "Repeat these steps until they succeed:".to_owned(),
        },
        StepKind::Checkpoint => {
            "Stop and check with the person you work for before you go on.".to_owned()
        }
        StepKind::Merge => "Hand the change over for review. Never merge it yourself.".to_owned(),
        StepKind::Workflow => "Follow the nested workflow's steps.".to_owned(),
        StepKind::Script => "Follow the steps of the workflow in use.".to_owned(),
    };
    if step.steps.is_empty() || depth > 2 {
        return text;
    }
    let indent = "   ".repeat(depth + 1);
    let mut text = text;
    for inner in &step.steps {
        let _ = write!(text, "\n{indent}- {}", instruction(inner, depth + 1));
    }
    text
}

/// What a critic checks, with a question for each item the protocol knows.
fn checklist(recipe: &Recipe) -> Vec<String> {
    let Some(items) = section(recipe, "critique").and_then(|critique| critique.get("checklist"))
    else {
        return Vec::new();
    };
    strings(items)
        .into_iter()
        .map(|item| match item.as_str() {
            "criteria" => "Does it meet every acceptance criterion?".to_owned(),
            "risky_assumption" => {
                "What's the riskiest assumption, and how will you check it?".to_owned()
            }
            "missing_steps" => {
                "Is a step missing, such as a migration, a config change, or docs?".to_owned()
            }
            "testability" => "Can each part be tested, and how?".to_owned(),
            "interfaces" => "Are the interfaces clear, and do they fit their callers?".to_owned(),
            "premortem" => {
                "If this failed in a month, what would most likely be the cause?".to_owned()
            }
            "simpler_plan" => "Is there a simpler plan that does the job?".to_owned(),
            "evidence" => "What evidence shows each claim holds?".to_owned(),
            _ => item,
        })
        .collect()
}

/// The gates by name, never their commands.
fn gates(recipe: &Recipe) -> Vec<String> {
    let Some(gates) = section(recipe, "checks") else {
        return Vec::new();
    };
    let names = match gates {
        Value::Array(_) => strings(gates),
        other => other.get("run").map(strings).unwrap_or_default(),
    };
    names
        .into_iter()
        .filter(|name| {
            name.len() <= 64
                && name
                    .chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '_' | '-'))
        })
        .map(|name| {
            let meaning = match name.as_str() {
                "protected_paths" => Some("leave protected paths alone"),
                "test_tampering" => Some("no test is weakened, skipped, or deleted"),
                "secret_scan" => Some("no secret, key, or token in the diff"),
                "dependency_policy" => Some("no dependency the project's policy doesn't allow"),
                "lint" => Some("the linters pass"),
                "types" => Some("the type checker passes"),
                "unit_tests" => Some("the unit tests pass"),
                "acceptance_tests" => Some("the acceptance tests pass"),
                _ => None,
            };
            match meaning {
                Some(meaning) => format!("`{name}`: {meaning}"),
                None => format!("`{name}`"),
            }
        })
        .collect()
}

/// Strings from a list, each on one line, or from one string.
fn strings(value: &Value) -> Vec<String> {
    let items = match value {
        Value::Array(items) => items.iter().filter_map(Value::as_str).collect(),
        Value::String(text) => vec![text.as_str()],
        _ => Vec::new(),
    };
    items
        .into_iter()
        .map(one_line)
        .filter(|item| !item.is_empty())
        .map(|item| document::cut(&item, 200))
        .collect()
}
