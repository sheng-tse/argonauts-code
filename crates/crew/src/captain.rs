//! Captain: one task for the main agent. The prompt is the goal (`GoalSource::Prompt`) unless
//! `captain.understand_first`; `captain.tries` above 1 runs independent tries and keeps one that
//! passes the visible checks; the Captain-or-crew decision's `move_to_crew` switches to the crew if
//! the task needs you.

use std::collections::BTreeMap;

use argonauts_core::TaskId;
use argonauts_core::events::goal::GoalFile;
use argonauts_core::events::run::{Party, RunMode};
use argonauts_core::events::task::{TaskKind, TaskLimit, TaskSpec, TaskState};
use argonauts_core::names::{ModelRef, RoleName};
use argonauts_core::settings::protocol::ForcedMode;

use crate::engine::host::Host;
use crate::error::{EngineError, EngineResult};
use crate::roles;
use crate::state::RunState;
use crate::state::tasks::TaskEntry;
use crate::understand::{approval, draft, round};

/// Records the prompt as the approved goal and writes it in a commit of Argonauts Code's. An
/// approved goal is returned as it is, so every caller may ask. With `captain.understand_first` the
/// goal waits for your approval instead.
pub async fn goal_from_prompt(host: &Host) -> EngineResult<GoalFile> {
    if let Some(approved) = host.read(|state| state.goal.approved.clone()) {
        return Ok(approved);
    }
    if host.settings().captain.understand_first {
        return Err(EngineError::Refused(
            "captain.understand_first is on, so the goal waits for your approval".to_owned(),
        ));
    }
    prompt_is_the_goal(host).await
}

/// Records the prompt as the approved goal, as the configuration's approval: a Captain run, or a
/// crew run where Understand doesn't run.
pub async fn prompt_is_the_goal(host: &Host) -> EngineResult<GoalFile> {
    if let Some(approved) = host.read(|state| state.goal.approved.clone()) {
        return Ok(approved);
    }
    let request = host.read(round::request)?;
    round::begin(host)?;
    let goal = draft::from_prompt(host.run_id(), &request);
    let goal = round::write_draft(host, goal).await?;
    // The setting made your prompt the goal, so the approval is the configuration's.
    approval::approve(host, goal.clone(), Party::Config)?;
    Ok(goal)
}

/// The one `Captain` task, owned by the main agent, serving every goal criterion. With
/// `captain.review` its reviewer comes from another model provider than the worker.
pub fn task(state: &RunState, host: &Host) -> EngineResult<TaskSpec> {
    let started = state
        .run
        .started
        .as_ref()
        .ok_or_else(|| EngineError::Refused("the run hasn't started".to_owned()))?;
    let settings = host.settings();
    let worker = settings.crew.main.clone();
    let reviewers = if settings.captain.review {
        vec![reviewer(host, &worker)?]
    } else {
        Vec::new()
    };
    let criteria = state
        .goal
        .approved
        .as_ref()
        .map(|goal| {
            goal.criteria
                .iter()
                .map(|criterion| criterion.id.clone())
                .collect()
        })
        .unwrap_or_default();
    // Named from the words the run's name comes from: your own may say how to run the work
    // ("with Codex as the reviewer…") rather than what it is.
    let words = crate::workflow::naming::words_of(started);
    let title = words.lines().next().unwrap_or(words).trim().to_owned();
    let item = crate::workflow::brief::item(state).or_else(|| {
        let name = crate::workflow::brief::name_from(words);
        Some(argonauts_core::labels::slug(&name, 3)).filter(|item| !item.is_empty())
    });
    // The review comes after the report, so the worker never sets out to ask for it.
    let mut task = started.request.clone();
    if !reviewers.is_empty() {
        let names: Vec<String> = reviewers.iter().map(roles::display_name).collect();
        task.push_str(&format!(
            "\n\nArgonauts Code has {} review your change after you report.",
            names.join(" and ")
        ));
    }
    Ok(TaskSpec {
        id: TaskId::numbered(1),
        title,
        task,
        kind: TaskKind::Captain,
        owned_paths: Vec::new(),
        read_only_paths: Vec::new(),
        forbidden_paths: Vec::new(),
        hidden_paths: Vec::new(),
        depends_on: Vec::new(),
        criteria,
        worker: Some(worker),
        reviewers,
        limit: TaskLimit::default(),
        predictions: None,
        item,
        way: None,
    })
}

/// The first reviewer candidate (the reviewer role's, else every available worker) from another
/// provider than the worker, with no role conflict.
fn reviewer(host: &Host, worker: &ModelRef) -> EngineResult<ModelRef> {
    let provider = |model: &ModelRef| roles::provider(host, model);
    let worker_provider = provider(worker);
    let mut pool = roles::candidates(host.settings(), RoleName::Reviewer);
    pool.extend(host.services().workers.available());
    pool.into_iter()
        .find(|model| {
            model != worker
                && provider(model) != worker_provider
                && roles::conflict(RoleName::Reviewer, model, Some(worker), &provider).is_none()
        })
        .ok_or_else(|| {
            EngineError::Refused(format!(
                "captain.review needs a reviewer from another provider than {worker} \
                 ({worker_provider}), and none is available"
            ))
        })
}

/// Best of n: the try to keep among those that passed the visible checks. The one that
/// needed the fewest tries wins, then the earliest.
pub fn pick_try(state: &RunState, tries: &[TaskId]) -> Option<TaskId> {
    tries
        .iter()
        .enumerate()
        .filter_map(|(order, id)| {
            let record = state.tasks.get(id)?;
            passed_visible_checks(record).then_some((record.tries, order, id))
        })
        .min_by_key(|(tries, order, _)| (*tries, *order))
        .map(|(_, _, id)| id.clone())
}

/// Every check the task ran passed on its newest run, and the task is still in play.
fn passed_visible_checks(record: &TaskEntry) -> bool {
    let out_of_play = matches!(
        record.state,
        TaskState::Failed | TaskState::NeedsYou | TaskState::Cancelled | TaskState::Deferred
    );
    if out_of_play || record.checks.is_empty() {
        return false;
    }
    let mut newest = BTreeMap::new();
    for check in &record.checks {
        newest.insert(check.name.as_str(), check.passed);
    }
    newest.values().all(|passed| *passed)
}

/// Whether the Captain task needed you and the rules chose Captain first, then crew. A forced
/// Captain never moves to the crew.
pub fn moves_to_crew(state: &RunState, host: &Host) -> bool {
    let captain_first = state
        .run
        .started
        .as_ref()
        .and_then(|started| started.captain_or_crew.as_ref())
        .is_some_and(|captain_or_crew| captain_or_crew.move_to_crew);
    let forced_captain = host.settings().captain_or_crew.force == Some(ForcedMode::Captain);
    let needs_you =
        state.tasks.by_id.values().any(|record| {
            record.spec.kind == TaskKind::Captain && record.state == TaskState::NeedsYou
        });
    round::mode(state) == RunMode::Captain && captain_first && !forced_captain && needs_you
}

#[cfg(test)]
mod tests {
    use argonauts_core::events::task::{CheckKind, CheckOutcome};

    use super::*;

    fn check(name: &str, passed: bool) -> CheckOutcome {
        CheckOutcome {
            name: name.to_owned(),
            kind: CheckKind::Command,
            passed,
            command: None,
            exit_code: Some(i32::from(!passed)),
            duration_ms: 1,
            summary: String::new(),
            output_tail: String::new(),
            violations: Vec::new(),
            sandboxed: true,
            skipped: false,
        }
    }

    fn record(n: u32, state: TaskState, tries: u32, checks: Vec<CheckOutcome>) -> TaskEntry {
        TaskEntry {
            spec: TaskSpec {
                id: TaskId::numbered(n),
                title: format!("try {n}"),
                task: "t".to_owned(),
                kind: TaskKind::Captain,
                owned_paths: Vec::new(),
                read_only_paths: Vec::new(),
                forbidden_paths: Vec::new(),
                hidden_paths: Vec::new(),
                depends_on: Vec::new(),
                criteria: Vec::new(),
                worker: None,
                reviewers: Vec::new(),
                limit: TaskLimit::default(),
                predictions: None,
                item: None,
                way: None,
            },
            state,
            tries,
            worker: None,
            reassigned: 0,
            worktree: None,
            branch: None,
            base: None,
            commits: Vec::new(),
            reports: Vec::new(),
            checks,
            reviews: Vec::new(),
            fallbacks: 0,
            debt: None,
        }
    }

    fn state(records: Vec<TaskEntry>) -> RunState {
        let mut state = RunState::default();
        for record in records {
            state.tasks.by_id.insert(record.spec.id.clone(), record);
        }
        state
    }

    #[test]
    fn best_of_n_keeps_a_try_that_passed_the_checks() {
        let state = state(vec![
            record(1, TaskState::InReview, 1, vec![check("test", false)]),
            record(
                2,
                TaskState::InReview,
                2,
                vec![check("test", false), check("test", true)],
            ),
            record(
                3,
                TaskState::InReview,
                1,
                vec![check("test", true), check("lint", true)],
            ),
            record(4, TaskState::Failed, 1, vec![check("test", true)]),
            record(5, TaskState::InReview, 1, Vec::new()),
        ]);
        let ids: Vec<TaskId> = (1..=5).map(TaskId::numbered).collect();
        assert_eq!(pick_try(&state, &ids), Some(TaskId::numbered(3)));
        assert_eq!(pick_try(&state, &ids[..2]), Some(TaskId::numbered(2)));
        assert_eq!(pick_try(&state, &[ids[0].clone(), ids[3].clone()]), None);
        assert_eq!(pick_try(&state, &[TaskId::numbered(9)]), None);
    }
}
