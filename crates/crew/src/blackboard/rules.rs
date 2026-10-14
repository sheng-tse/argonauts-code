//! Who decides a change request, by the paths it touches: local and reversible, the worker of
//! that piece; cross-cutting, the affected workers by majority, the tie-breaker breaking a tie
//! only when it isn't a party; irreversible or goal-changing, you. A request that moves a task
//! into a protected path sends the run back to the plan approval.

use argonauts_core::TaskId;
use argonauts_core::names::ModelRef;
use argonauts_core::settings::Settings;
use argonauts_core::steps::task::ChangeRequestOut;

use crate::blackboard::refs;
use crate::blackboard::route::still_working;
use crate::state::RunState;
use crate::state::tasks::TaskEntry;

/// Agents never write here, on top of `limits.protected_paths`.
pub const CONDUCTOR_PATHS: &str = ".argonauts/**";

/// The approved goal files. A request touching one asks to change the goal.
pub const GOAL_PATHS: &str = ".argonauts/goal/**";

/// Changes nobody can take back once they run elsewhere: data migrations and release or
/// deployment config, each with the outside action it sets off, as
/// `limits.irreversible_allowlist` names them.
pub const IRREVERSIBLE_PATHS: &[(&str, &str)] = &[
    ("**/migrations/**", "migrate"),
    ("**/migrate/**", "migrate"),
    ("**/deploy/**", "deploy"),
    ("**/release/**", "release"),
    (".github/workflows/release*", "release"),
    (".github/workflows/deploy*", "deploy"),
];

/// The outside actions a change to `paths` sets off, each once, in the table's order.
pub fn irreversible_actions(paths: &[String]) -> Vec<String> {
    let mut actions: Vec<String> = Vec::new();
    for (pattern, action) in IRREVERSIBLE_PATHS {
        let touched = paths.iter().any(|path| refs::under(pattern, path));
        if touched && !actions.iter().any(|seen| seen == action) {
            actions.push((*action).to_owned());
        }
    }
    actions
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decider {
    Worker(ModelRef),
    /// Each affected worker posts one `Decision` in their next report.
    Workers(Vec<ModelRef>),
    /// Irreversible, or nobody's piece.
    User,
    /// It changes the goal: `approvals.goal_changes` says what happens.
    Goal,
    /// Back to the plan approval.
    Replan,
}

pub fn decider(state: &RunState, settings: &Settings, request: &ChangeRequestOut) -> Decider {
    decider_for(state, settings, None, request)
}

/// The decider, leaving out the requesting task: it's a party, never a voter on its own request.
pub fn decider_for(
    state: &RunState,
    settings: &Settings,
    requester: Option<&TaskId>,
    request: &ChangeRequestOut,
) -> Decider {
    let touches = |patterns: &[&str]| {
        request
            .paths
            .iter()
            .any(|path| patterns.iter().any(|pattern| refs::under(pattern, path)))
    };
    if touches(&[GOAL_PATHS]) {
        return Decider::Goal;
    }
    let protected: Vec<&str> = settings
        .limits
        .protected_paths
        .iter()
        .map(String::as_str)
        .chain([CONDUCTOR_PATHS])
        .collect();
    if touches(&protected) {
        return Decider::Replan;
    }
    if !irreversible_actions(&request.paths).is_empty() {
        return Decider::User;
    }

    // Workers of the other tasks still working that own or read a touched path, the owning tasks'
    // first.
    let affected = state.tasks.by_id.values().filter(|record| {
        Some(&record.spec.id) != requester
            && still_working(record)
            && (any_under(&record.spec.owned_paths, &request.paths)
                || any_under(&record.spec.read_only_paths, &request.paths))
    });
    let (owning, reading): (Vec<&TaskEntry>, Vec<&TaskEntry>) =
        affected.partition(|record| any_under(&record.spec.owned_paths, &request.paths));
    let mut workers: Vec<ModelRef> = Vec::new();
    for record in owning.iter().chain(&reading) {
        if let Some(worker) = worker_of(record)
            && !workers.contains(&worker)
        {
            workers.push(worker);
        }
    }
    // One worker decides alone whether the change is local or read by its other tasks; nobody's
    // piece is yours.
    match workers.len() {
        0 => Decider::User,
        1 => Decider::Worker(workers.remove(0)),
        _ => Decider::Workers(workers),
    }
}

/// Tasks that depend on the paths the request touches: they own or read one, or depend, at any
/// distance, on a task that owns one. They move to `Blocked` first.
pub fn dependents(state: &RunState, request: &ChangeRequestOut) -> Vec<TaskId> {
    let tasks = &state.tasks.by_id;
    let mut found: Vec<TaskId> = tasks
        .values()
        .filter(|record| {
            any_under(&record.spec.owned_paths, &request.paths)
                || any_under(&record.spec.read_only_paths, &request.paths)
        })
        .map(|record| record.spec.id.clone())
        .collect();
    loop {
        let before = found.len();
        for record in tasks.values() {
            if !found.contains(&record.spec.id)
                && record
                    .spec
                    .depends_on
                    .iter()
                    .any(|task| found.contains(task))
            {
                found.push(record.spec.id.clone());
            }
        }
        if found.len() == before {
            break;
        }
    }
    found.retain(|task| tasks.get(task).is_some_and(still_working));
    found.sort();
    found
}

/// The outcome of the workers' votes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Tally {
    Accepted,
    Rejected,
    /// A tie the tie-breaker isn't a party to: its one vote decides.
    TieBreakerDecides(ModelRef),
    /// A tie the tie-breaker can't break because it's a party, or there's no tie-breaker.
    YouDecide,
}

/// `votes` are `(worker, accepts)`; `tie_breaker` breaks a tie only when it didn't vote.
pub fn tally(votes: &[(ModelRef, bool)], tie_breaker: Option<&ModelRef>) -> Tally {
    let accepts = votes.iter().filter(|(_, accepts)| *accepts).count();
    let rejects = votes.len() - accepts;
    if accepts > rejects {
        return Tally::Accepted;
    }
    if rejects > accepts {
        return Tally::Rejected;
    }
    match tie_breaker {
        Some(tie_breaker) if !votes.iter().any(|(worker, _)| worker == tie_breaker) => {
            Tally::TieBreakerDecides(tie_breaker.clone())
        }
        _ => Tally::YouDecide,
    }
}

fn worker_of(record: &TaskEntry) -> Option<ModelRef> {
    record.worker.clone().or_else(|| record.spec.worker.clone())
}

fn any_under(patterns: &[String], paths: &[String]) -> bool {
    paths
        .iter()
        .any(|path| patterns.iter().any(|pattern| refs::under(pattern, path)))
}
