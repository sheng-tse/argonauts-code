//! Your edits at the plan approval. Lines of the note in these forms change the graph, and
//! the whole note becomes a decision record:
//!
//! ```text
//! task-002 worker codex/gpt-5.6
//! task-002 reviewers agy, claude-code/opus
//! task-002 paths src/store/**, src/cache.rs
//! task-002 limit 4.5
//! limit tasks 0.45
//! ```
//!
//! An edited graph goes through the same rules as a proposal, so an edit that breaks one
//! reopens the approval with the reason.

use argonauts_core::TaskId;
use argonauts_core::events::plan::{DesignDecision, LimitPlan, TaskGraph};
use argonauts_core::events::task::{TaskKind, TaskSpec};
use argonauts_core::names::{DecisionId, ModelRef, RoleName};
use argonauts_core::settings::Settings;
use argonauts_core::settings::limit::{LimitAmount, LimitLineKind, LimitUnit};

use super::{graph, limit_lines};
use crate::roles;

/// Its title gives the record's file name, `D-NNN-your-edits.md`.
pub const DECISION_TITLE: &str = "Your edits";

#[derive(Debug, Clone, PartialEq)]
pub enum Edit {
    Worker {
        task: TaskId,
        worker: ModelRef,
    },
    Reviewers {
        task: TaskId,
        workers: Vec<ModelRef>,
    },
    Paths {
        task: TaskId,
        paths: Vec<String>,
    },
    TaskLimit {
        task: TaskId,
        amount: f64,
    },
    Line {
        line: LimitLineKind,
        share: f64,
    },
}

/// The graph and limit after your edits, and what changed.
#[derive(Debug, Clone, PartialEq)]
pub struct Edited {
    pub graph: TaskGraph,
    pub limit: LimitPlan,
    pub workers: Vec<TaskId>,
    pub reviewers: Vec<TaskId>,
    pub limit_changed: bool,
}

/// The edits in the note. A line that starts like an edit but doesn't parse is an error, so a
/// typo never passes as prose.
pub fn parse(note: &str) -> Result<Vec<Edit>, String> {
    let mut edits = Vec::new();
    for line in note.lines().map(str::trim) {
        let mut words = line.splitn(3, char::is_whitespace);
        let (Some(first), Some(second)) = (words.next(), words.next()) else {
            continue;
        };
        let rest = words.next().unwrap_or("").trim();
        let keyword = second.trim_end_matches(':').to_ascii_lowercase();
        if first.eq_ignore_ascii_case("limit") {
            let Some(line_kind) = limit_lines::line_named(second.trim_end_matches(':')) else {
                continue;
            };
            let share = number(rest, line)?;
            edits.push(Edit::Line {
                line: line_kind,
                share,
            });
            continue;
        }
        if !looks_like_task(first)
            || !matches!(keyword.as_str(), "worker" | "reviewers" | "paths" | "limit")
        {
            continue;
        }
        let task = TaskId::new(first).map_err(|_| format!("`{first}` isn't a task id."))?;
        let edit = match keyword.as_str() {
            "worker" => Edit::Worker {
                task,
                worker: worker(rest, line)?,
            },
            "reviewers" => Edit::Reviewers {
                task,
                workers: list(rest)
                    .iter()
                    .map(|item| worker(item, line))
                    .collect::<Result<_, _>>()?,
            },
            "paths" => Edit::Paths {
                task,
                paths: list(rest),
            },
            _ => Edit::TaskLimit {
                task,
                amount: number(rest, line)?,
            },
        };
        edits.push(edit);
    }
    Ok(edits)
}

/// Applies the edits and checks the result like a proposal. `Err` is the reason to reopen the
/// approval with.
pub fn apply(
    graph: &TaskGraph,
    limit: &LimitPlan,
    edits: &[Edit],
    settings: &Settings,
    provider: &dyn Fn(&ModelRef) -> String,
) -> Result<Edited, String> {
    let mut tasks = graph.tasks.clone();
    let mut limit = limit.clone();
    let (mut workers, mut reviewers, mut limit_changed) = (Vec::new(), Vec::new(), false);
    let worker_pool = roles::candidates(settings, RoleName::Worker);
    let reviewer_pool = roles::candidates(settings, RoleName::Reviewer);
    for edit in edits {
        match edit {
            Edit::Worker {
                task,
                worker: model,
            } => {
                if !worker_pool.contains(model) {
                    return Err(format!("{model} isn't one of the worker candidates."));
                }
                find(&mut tasks, task)?.worker = Some(model.clone());
                workers.push(task.clone());
            }
            Edit::Reviewers {
                task,
                workers: models,
            } => {
                if let Some(model) = models.iter().find(|w| !reviewer_pool.contains(w)) {
                    return Err(format!("{model} isn't one of the reviewer candidates."));
                }
                find(&mut tasks, task)?.reviewers.clone_from(models);
                reviewers.push(task.clone());
            }
            Edit::Paths { task, paths } => {
                find(&mut tasks, task)?.owned_paths.clone_from(paths);
            }
            Edit::TaskLimit { task, amount } => {
                if !amount.is_finite() || *amount <= 0.0 {
                    return Err(format!("{task}'s limit must be more than 0."));
                }
                let spec = find(&mut tasks, task)?;
                let unit = spec.limit.limit.map_or(LimitUnit::Usd, |limit| limit.unit);
                spec.limit.limit = Some(LimitAmount {
                    unit,
                    amount: *amount,
                });
            }
            Edit::Line { line, share } => {
                if !(0.0..=1.0).contains(share) {
                    return Err(format!(
                        "The {} limit line's share must be between 0 and 1.",
                        limit_lines::line_name(*line)
                    ));
                }
                let total = limit.total.amount;
                let entry = limit
                    .lines
                    .iter_mut()
                    .find(|entry| entry.line == *line)
                    .ok_or_else(|| {
                        format!("The plan has no {} line.", limit_lines::line_name(*line))
                    })?;
                entry.amount.amount = total * share;
                limit_changed = true;
            }
        }
    }
    follow_reviewers(&mut tasks, &reviewers, &workers);
    let mut broken = graph::rules(&tasks, settings);
    broken.extend(conflicts(&tasks, settings, provider));
    if limit_changed && let Err(reason) = limit_lines::check(&limit) {
        broken.push(reason);
    }
    if !broken.is_empty() {
        return Err(broken.join(" "));
    }
    Ok(Edited {
        graph: TaskGraph {
            critical_path: graph.critical_path.clone(),
            tasks,
        },
        limit,
        workers,
        reviewers,
        limit_changed,
    })
}

/// The decision record for an approval with edits.
pub fn decision(id: DecisionId, note: &str) -> DesignDecision {
    DesignDecision {
        id,
        title: DECISION_TITLE.to_owned(),
        choice: note.trim().to_owned(),
        rationale: "You approved the plan with these edits at the plan approval.".to_owned(),
        alternatives: Vec::new(),
        path: None,
    }
}

/// A test task is written by its task's first reviewer, so it follows a reviewer edit unless
/// you set its worker yourself.
fn follow_reviewers(tasks: &mut [TaskSpec], reviewed: &[TaskId], owned: &[TaskId]) {
    let firsts: Vec<(TaskId, ModelRef)> = tasks
        .iter()
        .filter(|task| reviewed.contains(&task.id))
        .filter_map(|task| Some((task.id.clone(), task.reviewers.first()?.clone())))
        .collect();
    for task in tasks.iter_mut() {
        if let TaskKind::AcceptanceTests { for_task } = &task.kind
            && !owned.contains(&task.id)
            && let Some((_, first)) = firsts.iter().find(|(id, _)| id == for_task)
        {
            task.worker = Some(first.clone());
        }
    }
}

fn conflicts(
    tasks: &[TaskSpec],
    settings: &Settings,
    provider: &dyn Fn(&ModelRef) -> String,
) -> Vec<String> {
    let mut broken = Vec::new();
    for task in tasks {
        let Some(worker) = &task.worker else { continue };
        for reviewer in &task.reviewers {
            if let Some(reason) =
                roles::conflict(RoleName::Reviewer, reviewer, Some(worker), provider)
            {
                broken.push(format!("{reason} ({}).", task.id));
            } else if settings.review.other_provider && provider(reviewer) == provider(worker) {
                broken.push(format!(
                    "{reviewer} is in the same provider as {worker}, {}'s worker.",
                    task.id
                ));
            }
        }
        if let TaskKind::AcceptanceTests { for_task } = &task.kind
            && let Some(tested) = tasks.iter().find(|c| &c.id == for_task)
            && let Some(tested_worker) = &tested.worker
            && roles::conflict(RoleName::Reviewer, worker, Some(tested_worker), provider).is_some()
        {
            broken.push(format!(
                "{} can't write the acceptance tests for its own task {for_task}.",
                roles::display_name(worker)
            ));
        }
    }
    broken
}

fn find<'a>(tasks: &'a mut [TaskSpec], id: &TaskId) -> Result<&'a mut TaskSpec, String> {
    tasks
        .iter_mut()
        .find(|task| &task.id == id)
        .ok_or_else(|| format!("There's no {id} in the plan."))
}

fn looks_like_task(word: &str) -> bool {
    word.split_once('-').is_some_and(|(prefix, number)| {
        !prefix.is_empty()
            && prefix.chars().all(|c| c.is_ascii_lowercase())
            && !number.is_empty()
            && number.chars().all(|c| c.is_ascii_digit())
    })
}

fn list(text: &str) -> Vec<String> {
    text.split(',')
        .map(|item| item.trim().to_owned())
        .filter(|item| !item.is_empty())
        .collect()
}

fn worker(text: &str, line: &str) -> Result<ModelRef, String> {
    text.trim()
        .parse()
        .map_err(|_| format!("\"{line}\" doesn't name an agent."))
}

fn number(text: &str, line: &str) -> Result<f64, String> {
    text.trim()
        .trim_start_matches('$')
        .parse()
        .map_err(|_| format!("\"{line}\" doesn't end in a number."))
}

#[cfg(test)]
mod tests {
    use argonauts_core::events::plan::LimitLine;
    use argonauts_core::events::task::TaskLimit;

    use super::*;

    fn worker(text: &str) -> ModelRef {
        text.parse().unwrap()
    }

    fn provider(worker: &ModelRef) -> String {
        worker.agent.clone()
    }

    fn spec(id: &str, kind: TaskKind, owned: &str, who: &str, reviewer: &str) -> TaskSpec {
        TaskSpec {
            id: TaskId::new(id).unwrap(),
            title: id.to_owned(),
            task: id.to_owned(),
            kind,
            owned_paths: vec![owned.to_owned()],
            read_only_paths: Vec::new(),
            forbidden_paths: Vec::new(),
            hidden_paths: Vec::new(),
            depends_on: Vec::new(),
            criteria: Vec::new(),
            worker: Some(worker(who)),
            reviewers: vec![worker(reviewer)],
            limit: TaskLimit::default(),
            predictions: None,
            item: None,
        }
    }

    fn fixture() -> (TaskGraph, LimitPlan, Settings) {
        let settings = Settings {
            crew: vec![worker("claude-code"), worker("codex"), worker("agy")].into(),
            ..Settings::default()
        };
        let tests = TaskKind::AcceptanceTests {
            for_task: TaskId::numbered(1),
        };
        let graph = TaskGraph {
            tasks: vec![
                spec(
                    "test-002",
                    tests,
                    "tests/acceptance/task-001/**",
                    "codex",
                    "agy",
                ),
                spec(
                    "task-001",
                    TaskKind::Build,
                    "src/store/**",
                    "claude-code",
                    "codex",
                ),
            ],
            critical_path: Vec::new(),
        };
        let limit = LimitPlan {
            lines: [(LimitLineKind::Tasks, 6.0), (LimitLineKind::Reserve, 4.0)]
                .map(|(line, amount)| LimitLine {
                    line,
                    amount: LimitAmount {
                        unit: LimitUnit::Usd,
                        amount,
                    },
                })
                .to_vec(),
            total: LimitAmount {
                unit: LimitUnit::Usd,
                amount: 10.0,
            },
            estimate_low: 5.0,
            estimate_high: 9.0,
        };
        (graph, limit, settings)
    }

    #[test]
    fn edits_parse_from_the_note() {
        let note = "Keep the store small.\ntask-001 worker: codex\ntask-001 paths src/store/**, src/cache.rs\nlimit tasks 0.5\ntest-002 limit $2\nThe limit matters.";
        let edits = parse(note).unwrap();
        assert_eq!(
            edits,
            [
                Edit::Worker {
                    task: TaskId::numbered(1),
                    worker: worker("codex")
                },
                Edit::Paths {
                    task: TaskId::numbered(1),
                    paths: vec!["src/store/**".to_owned(), "src/cache.rs".to_owned()]
                },
                Edit::Line {
                    line: LimitLineKind::Tasks,
                    share: 0.5
                },
                Edit::TaskLimit {
                    task: TaskId::new("test-002").unwrap(),
                    amount: 2.0
                },
            ]
        );
        assert!(parse("task-001 limit lots").is_err());
    }

    #[test]
    fn edits_are_checked_like_a_proposal() {
        let (graph, limit, settings) = fixture();
        let try_edit =
            |note: &str| apply(&graph, &limit, &parse(note).unwrap(), &settings, &provider);

        // The test task follows its task's new reviewer, and then needs another reviewer.
        let reason = try_edit("task-001 reviewers agy").unwrap_err();
        assert!(
            reason.contains("agy can't review its own task (test-002)"),
            "{reason}"
        );
        let edited =
            try_edit("task-001 reviewers agy\ntest-002 reviewers codex\ntask-001 limit 4").unwrap();
        let tests = &edited.graph.tasks[0];
        assert_eq!(
            tests.worker,
            Some(worker("agy")),
            "the test task follows the reviewer"
        );
        assert_eq!(
            edited.reviewers,
            [TaskId::numbered(1), TaskId::new("test-002").unwrap()]
        );
        assert_eq!(edited.graph.tasks[1].limit.limit.unwrap().amount, 4.0);
        assert!(!edited.limit_changed);

        let reason = try_edit("task-001 worker codex").unwrap_err();
        assert!(
            reason.contains("Codex can't review its own task"),
            "{reason}"
        );
        assert!(
            reason.contains("acceptance tests for its own task task-001"),
            "{reason}"
        );
        assert_eq!(
            try_edit("task-001 worker agy").unwrap().workers,
            [TaskId::numbered(1)]
        );
        let reason = try_edit("task-001 paths tests/acceptance/**").unwrap_err();
        assert!(reason.contains("overlap"), "{reason}");
        let reason = try_edit("limit tasks 0.7").unwrap_err();
        assert_eq!(
            reason,
            "The limit lines add up to $11.00, not the run limit of $10.00."
        );
        assert!(
            try_edit("limit tasks 0.7\nlimit reserve 0.3")
                .unwrap()
                .limit_changed
        );
        assert!(
            try_edit("task-009 worker agy")
                .unwrap_err()
                .contains("no task-009")
        );
        assert!(
            try_edit("task-001 worker ollama")
                .unwrap_err()
                .contains("worker candidates")
        );
        assert!(try_edit("Just be careful.").is_ok());
    }

    #[test]
    fn plan_edits_become_a_decision_titled_your_edits() {
        let record = decision(DecisionId::numbered(4), "  task-001 worker agy\n");
        assert_eq!(record.title, "Your edits");
        assert_eq!(record.choice, "task-001 worker agy");
        assert_eq!(record.id.as_str(), "D-004");
    }
}
