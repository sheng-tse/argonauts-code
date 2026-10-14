//! Validating a task graph proposal: unique keys, no cycles, owned paths disjoint, no owned
//! path in `.argonauts/**`, `limits.protected_paths`, or the task's forbidden paths, and at most
//! `divide.max_tasks`.

use std::collections::{BTreeMap, BTreeSet};

use argonauts_core::events::task::{TaskKind, TaskLimit, TaskSpec};
use argonauts_core::labels;
use argonauts_core::settings::Settings;
use argonauts_core::steps::plan::{ProposedKind, TaskGraphProposal};
use argonauts_core::{RunId, TaskId};

use super::paths;

/// No task may own anything here, whatever the settings say.
pub const CONDUCTOR_AREA: &str = ".argonauts/**";

/// The tasks, with ids given in order, or every reason the proposal fails. Ids don't depend on
/// the run; `_run` stays for callers that check an orchestrator's tasks the same way.
pub fn validate(
    _run: &RunId,
    proposal: &TaskGraphProposal,
    settings: &Settings,
) -> Result<Vec<TaskSpec>, Vec<String>> {
    let (tasks, mut reasons) = convert(proposal);
    let max = settings.divide.max_tasks as usize;
    if proposal.tasks.len() > max {
        reasons.push(format!(
            "The plan has {} tasks, more than divide.max_tasks ({max}).",
            proposal.tasks.len()
        ));
    }
    reasons.extend(rules(&tasks, settings));
    if reasons.is_empty() {
        Ok(tasks)
    } else {
        Err(reasons)
    }
}

/// The proposal as tasks numbered in order, with the key problems found on the way. A
/// dependency on an unknown key is left out of the task and reported.
pub fn convert(proposal: &TaskGraphProposal) -> (Vec<TaskSpec>, Vec<String>) {
    let mut reasons = Vec::new();
    if proposal.tasks.is_empty() {
        reasons.push("The plan has no tasks.".to_owned());
    }
    let mut ids: BTreeMap<&str, TaskId> = BTreeMap::new();
    for (n, task) in proposal.tasks.iter().enumerate() {
        let key = task.key.trim();
        if key.is_empty() {
            reasons.push(format!("Task {} (\"{}\") has no key.", n + 1, task.title));
        } else if ids.contains_key(key) {
            reasons.push(format!("The key `{key}` is used by more than one task."));
        } else {
            ids.insert(key, TaskId::numbered(n as u32 + 1));
        }
    }
    let tasks = proposal
        .tasks
        .iter()
        .enumerate()
        .map(|(n, task)| {
            let mut depends_on = Vec::new();
            for key in &task.depends_on {
                match ids.get(key.trim()) {
                    Some(id) if !depends_on.contains(id) => depends_on.push(id.clone()),
                    Some(_) => {}
                    None => reasons.push(format!(
                        "`{}` depends on `{key}`, which isn't a task in the plan.",
                        task.key
                    )),
                }
            }
            TaskSpec {
                id: TaskId::numbered(n as u32 + 1),
                title: task.title.trim().to_owned(),
                task: task.task.clone(),
                kind: match task.kind {
                    ProposedKind::Interface => TaskKind::Interface,
                    ProposedKind::Build => TaskKind::Build,
                    ProposedKind::Chore => TaskKind::Chore,
                    ProposedKind::Docs => TaskKind::Docs,
                },
                owned_paths: trimmed(&task.owned_paths),
                read_only_paths: trimmed(&task.read_only_paths),
                forbidden_paths: Vec::new(),
                hidden_paths: Vec::new(),
                depends_on,
                criteria: trimmed(&task.criteria),
                worker: None,
                reviewers: Vec::new(),
                limit: TaskLimit::default(),
                predictions: None,
                item: Some(labels::slug(&task.key, 3)).filter(|item| !item.is_empty()),
            }
        })
        .collect();
    (tasks, reasons)
}

/// The rules on finished tasks, also run after contracts first adds its tasks and after your
/// edits at the plan approval.
pub fn rules(tasks: &[TaskSpec], settings: &Settings) -> Vec<String> {
    let mut reasons = Vec::new();
    let known: BTreeSet<&TaskId> = tasks.iter().map(|task| &task.id).collect();
    if known.len() != tasks.len() {
        reasons.push("Two tasks share an id.".to_owned());
    }
    for task in tasks {
        for dependency in &task.depends_on {
            if !known.contains(dependency) {
                reasons.push(format!(
                    "{} depends on {dependency}, which isn't in the graph.",
                    name(task)
                ));
            }
        }
    }
    let cycle = in_cycles(tasks);
    if !cycle.is_empty() {
        let names: Vec<String> = cycle.iter().map(|task| name(task)).collect();
        reasons.push(format!(
            "These tasks depend on each other in a cycle: {}.",
            names.join(", ")
        ));
    }
    for task in tasks {
        reasons.extend(path_problems(task, settings));
    }
    for (i, a) in tasks.iter().enumerate() {
        for b in &tasks[i + 1..] {
            for x in &a.owned_paths {
                if let Some(y) = b.owned_paths.iter().find(|y| paths::overlap(x, y)) {
                    reasons.push(format!(
                        "{} owns `{x}` and {} owns `{y}`, which overlap. Owned paths must be disjoint.",
                        name(a),
                        name(b)
                    ));
                }
            }
        }
    }
    reasons
}

fn path_problems(task: &TaskSpec, settings: &Settings) -> Vec<String> {
    let mut reasons = Vec::new();
    if task.owned_paths.is_empty() && task.kind != TaskKind::Captain {
        reasons.push(format!("{} owns no paths.", name(task)));
    }
    let guarded: Vec<&str> = std::iter::once(CONDUCTOR_AREA)
        .chain(settings.limits.protected_paths.iter().map(String::as_str))
        .collect();
    let areas: Vec<&str> = guarded
        .iter()
        .copied()
        .chain(task.forbidden_paths.iter().map(String::as_str))
        .collect();
    for owned in &task.owned_paths {
        if let Some(problem) = paths::malformed(owned) {
            reasons.push(format!("{}: {problem}.", name(task)));
            continue;
        }
        // A task may own a directory with forbidden parts, which its sandbox keeps it out of,
        // but never one holding Argonauts Code's area or a protected path.
        if let Some(area) = guarded.iter().find(|area| paths::contains(owned, area)) {
            reasons.push(format!(
                "{} owns `{owned}`, which holds `{area}`, which no task may own.",
                name(task)
            ));
            continue;
        }
        if let Some(area) = areas.iter().find(|area| paths::inside(owned, area)) {
            reasons.push(format!(
                "{} owns `{owned}`, inside `{area}`, which no task may own.",
                name(task)
            ));
        }
    }
    reasons
}

/// Tasks on a dependency cycle, or depending on one, found by peeling off tasks whose
/// dependencies are all peeled.
fn in_cycles(tasks: &[TaskSpec]) -> Vec<&TaskSpec> {
    let mut done: BTreeSet<&TaskId> = BTreeSet::new();
    loop {
        let before = done.len();
        for task in tasks {
            if !done.contains(&task.id)
                && task
                    .depends_on
                    .iter()
                    .all(|d| done.contains(d) || !tasks.iter().any(|c| &c.id == d))
            {
                done.insert(&task.id);
            }
        }
        if done.len() == before {
            break;
        }
    }
    tasks
        .iter()
        .filter(|task| !done.contains(&task.id))
        .collect()
}

fn name(task: &TaskSpec) -> String {
    format!("{} (\"{}\")", task.id, task.title)
}

fn trimmed(values: &[String]) -> Vec<String> {
    values
        .iter()
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
        .collect()
}

#[cfg(test)]
mod tests {
    use argonauts_core::steps::plan::TaskProposal;

    use super::*;

    fn task(key: &str, kind: ProposedKind, owned: &[&str], depends_on: &[&str]) -> TaskProposal {
        TaskProposal {
            key: key.to_owned(),
            title: format!("The {key}"),
            task: format!("Build the {key}."),
            kind,
            owned_paths: owned.iter().map(|p| (*p).to_owned()).collect(),
            read_only_paths: Vec::new(),
            depends_on: depends_on.iter().map(|d| (*d).to_owned()).collect(),
            criteria: vec!["G1".to_owned()],
        }
    }

    fn run() -> RunId {
        RunId::new("run-01").unwrap()
    }

    #[test]
    fn divide_task_graph_valid() {
        let mut settings = Settings::default();
        settings.limits.protected_paths = vec!["**/*.lock".to_owned(), ".github/**".to_owned()];
        let good = TaskGraphProposal {
            tasks: vec![
                task("api", ProposedKind::Interface, &["src/limiter/api.rs"], &[]),
                task(
                    "store",
                    ProposedKind::Build,
                    &["src/limiter/store/**"],
                    &["api"],
                ),
                task(
                    "middleware",
                    ProposedKind::Build,
                    &["src/middleware.rs"],
                    &["api"],
                ),
                task(
                    "docs",
                    ProposedKind::Docs,
                    &["docs/limiter.md"],
                    &["middleware"],
                ),
            ],
        };
        let tasks = validate(&run(), &good, &settings).unwrap();
        let ids: Vec<&str> = tasks.iter().map(|c| c.id.as_str()).collect();
        assert_eq!(ids, ["task-001", "task-002", "task-003", "task-004"]);
        assert_eq!(tasks[1].depends_on, vec![TaskId::numbered(1)]);
        assert_eq!(tasks[3].kind, TaskKind::Docs);

        let bad = TaskGraphProposal {
            tasks: vec![
                task("a", ProposedKind::Build, &["src/a/**"], &["b"]),
                task("b", ProposedKind::Build, &["src/a/b.rs"], &["a"]),
                task("a", ProposedKind::Build, &[".argonauts/notes/x.md"], &[]),
                task("c", ProposedKind::Build, &["Cargo.lock"], &["nowhere"]),
                task("d", ProposedKind::Chore, &[], &[]),
                task("e", ProposedKind::Build, &["/etc/hosts"], &[]),
            ],
        };
        settings.divide.max_tasks = 5;
        let reasons = validate(&run(), &bad, &settings).unwrap_err();
        let has = |needle: &str| reasons.iter().any(|reason| reason.contains(needle));
        assert!(has("used by more than one task"), "{reasons:#?}");
        assert!(has("cycle"), "{reasons:#?}");
        assert!(has("which overlap"), "{reasons:#?}");
        assert!(has("inside `.argonauts/**`"), "{reasons:#?}");
        assert!(has("inside `**/*.lock`"), "{reasons:#?}");
        assert!(has("`nowhere`, which isn't a task"), "{reasons:#?}");
        assert!(has("owns no paths"), "{reasons:#?}");
        assert!(has("isn't relative to the repository"), "{reasons:#?}");
        assert!(has("more than divide.max_tasks (5)"), "{reasons:#?}");
    }

    #[test]
    fn a_tasks_own_forbidden_paths_are_checked() {
        let settings = Settings::default();
        let proposal = TaskGraphProposal {
            tasks: vec![task("a", ProposedKind::Build, &["src/a/**"], &[])],
        };
        let (mut tasks, reasons) = convert(&proposal);
        assert!(reasons.is_empty());
        tasks[0].forbidden_paths = vec!["src/a/secret/**".to_owned()];
        tasks[0].owned_paths.push("src/a/secret/key.rs".to_owned());
        let reasons = rules(&tasks, &settings);
        assert_eq!(reasons.len(), 1, "{reasons:#?}");
        assert!(
            reasons
                .iter()
                .any(|r| r.contains("inside `src/a/secret/**`"))
        );
    }

    #[test]
    fn no_task_owns_the_whole_repository_or_a_protected_area() {
        let mut settings = Settings::default();
        settings.limits.protected_paths = vec!["src/secrets/**".to_owned()];
        for owned in [".", "./"] {
            let whole = TaskGraphProposal {
                tasks: vec![task("all", ProposedKind::Chore, &[owned], &[])],
            };
            let reasons = validate(&run(), &whole, &settings).unwrap_err();
            assert!(
                reasons
                    .iter()
                    .any(|r| r.contains("names the whole repository")),
                "{reasons:#?}"
            );
        }
        let holding = TaskGraphProposal {
            tasks: vec![task("src", ProposedKind::Chore, &["src"], &[])],
        };
        let reasons = validate(&run(), &holding, &settings).unwrap_err();
        assert!(
            reasons.iter().any(|r| r.contains("holds `src/secrets/**`")),
            "{reasons:#?}"
        );
    }
}
