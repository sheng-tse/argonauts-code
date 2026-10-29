//! Which tasks run next: Waiting tasks whose dependencies are all merged become Ready; Ready
//! tasks start while `concurrency` and the limit allow and their worker is free.

use std::collections::{BTreeMap, BTreeSet};

use tokio::task::JoinSet;

use argonauts_core::PhaseStatus;
use argonauts_core::TaskId;
use argonauts_core::events::run::{RunMode, RunPhase};
use argonauts_core::events::task::{TaskKind, TaskState};
use argonauts_core::events::workflow::StepEnd;
use argonauts_core::names::ModelRef;
use argonauts_core::settings::Settings;
use argonauts_core::settings::limit::LimitUnit;

use crate::captain;
use crate::council::ways;
use crate::engine::host::Host;
use crate::error::{EngineError, EngineResult};
use crate::state::RunState;
use crate::state::tasks::TaskEntry;
use crate::task::contracts;
use crate::task::machine::{self, Trigger};
use crate::task::pipeline::{dependencies_done, transition};
use crate::task::support::changed_since;
use crate::workflow::{Flow, StepAt, StepCall, StepDone, runner};

pub fn unblocked(state: &RunState) -> Vec<TaskId> {
    state
        .tasks
        .by_id
        .values()
        .filter(|record| record.state == TaskState::Waiting)
        .map(|record| record.spec.id.clone())
        .filter(|task| dependencies_done(state, task))
        .collect()
}

/// Ready tasks that may start now, given how many are running.
pub fn startable(state: &RunState, settings: &Settings, running: usize) -> Vec<TaskId> {
    if limit_near(state) {
        return Vec::new();
    }
    let worker = |record: &TaskEntry| -> ModelRef {
        record
            .worker
            .clone()
            .or_else(|| record.spec.worker.clone())
            .unwrap_or_else(|| settings.crew.main.clone())
    };
    let active: Vec<&TaskEntry> = state
        .tasks
        .by_id
        .values()
        .filter(|record| record.state == TaskState::Running)
        .collect();
    let mut busy: BTreeSet<ModelRef> = active.iter().map(|record| worker(record)).collect();
    let mut per_agent: BTreeMap<String, u32> = BTreeMap::new();
    for record in &active {
        *per_agent.entry(worker(record).agent).or_default() += 1;
    }
    let max_sessions = usize::try_from(settings.concurrency.max_sessions).unwrap_or(usize::MAX);
    let mut slots = max_sessions.saturating_sub(running);
    let mut picked = Vec::new();
    for record in state.tasks.by_id.values() {
        if slots == 0 {
            break;
        }
        if record.state != TaskState::Ready {
            continue;
        }
        let worker = worker(record);
        let agent_count = per_agent.entry(worker.agent.clone()).or_default();
        if busy.contains(&worker) || *agent_count >= settings.concurrency.max_per_agent {
            continue;
        }
        *agent_count += 1;
        busy.insert(worker);
        slots -= 1;
        picked.push(record.spec.id.clone());
    }
    picked
}

/// The run's limit is spent and its reserve with it, so tasks that haven't started wait.
pub(crate) fn limit_near(state: &RunState) -> bool {
    let Some(spend) = &state.run.spend else {
        return false;
    };
    let Some(limit) = &spend.limit else {
        return false;
    };
    let spent = match limit.unit {
        LimitUnit::Usd => spend.spent.cost_usd,
        LimitUnit::Tokens => spend.spent.billable_tokens() as f64,
        LimitUnit::PlanUsage | LimitUnit::Minutes => return false,
    };
    let reserve = spend.reserve_left.as_ref().map_or(0.0, |r| r.amount);
    spent >= limit.amount && reserve <= 0.0
}

/// Runs the tasks until every one is merged, cancelled, or deferred, or nothing can move
/// without you.
pub async fn run_tasks(host: &Host, call: &StepCall) -> EngineResult<StepDone> {
    let at = call.at.clone();
    let mut jobs: JoinSet<(TaskId, EngineResult<TaskState>)> = JoinSet::new();
    let mut active: BTreeSet<TaskId> = BTreeSet::new();
    // Tasks whose pipeline returned waiting, with their change count then; they run again once
    // anything about them changes.
    let mut idle: BTreeMap<TaskId, u64> = BTreeMap::new();
    let mut phase = host.read(|state| {
        state
            .run
            .phase
            .filter(|phase| matches!(phase, RunPhase::Contracts | RunPhase::Build))
    });
    let mut coverage_accepted = false;
    loop {
        contracts::enter_phase(host, &mut phase, host.read(contracts::phase_now))?;
        if let Some(done) = settle_without_sessions(host, &at, &mut coverage_accepted).await? {
            return Ok(done);
        }
        for task in tasks_to_drive(host, &active, &idle) {
            idle.remove(&task);
            active.insert(task.clone());
            let (host, at) = (host.clone(), at.clone());
            jobs.spawn(async move {
                let state = ways::drive_planned(&host, &task, &at).await;
                (task, state)
            });
        }
        let seq = host.read(|state| state.seq);
        if jobs.is_empty() {
            if let Some(done) = finished(host) {
                if let Some(phase) = phase {
                    contracts::record_phase(
                        host,
                        phase,
                        PhaseStatus::Finished,
                        done.summary.clone(),
                    )?;
                }
                return Ok(done);
            }
            changed_since(host, seq).await?;
            continue;
        }
        tokio::select! {
            joined = jobs.join_next() => {
                let Some(joined) = joined else { continue };
                let (task, result) = joined.map_err(|err| {
                    EngineError::Refused(format!("a task stopped: {err}"))
                })?;
                active.remove(&task);
                let state = result?;
                if !machine::is_final(state) {
                    let changes = host.read(|s| s.tasks.progress(&task).changes);
                    idle.insert(task, changes);
                }
            }
            changed = changed_since(host, seq) => changed?,
        }
    }
}

/// Moves that need no session: dependencies in, the limit spent or with room again, and the
/// coverage check.
/// `Some` when the step ends here.
async fn settle_without_sessions(
    host: &Host,
    at: &StepAt,
    coverage_accepted: &mut bool,
) -> EngineResult<Option<StepDone>> {
    let ready = host.read(unblocked);
    let uncovered = host.read(contracts::uncovered);
    let held = host.read(|state| contracts::held_build_task(state, &ready));
    let hold_builds = !uncovered.is_empty() && !*coverage_accepted;
    if let (true, Some(task)) = (hold_builds, held) {
        if !contracts::criteria_check(host, at, &task, &uncovered).await? {
            return Ok(Some(StepDone {
                end: StepEnd::Failed,
                summary: Some(format!(
                    "stopped: no acceptance test covers {}",
                    uncovered.join(", ")
                )),
                flow: Flow::Continue,
            }));
        }
        *coverage_accepted = true;
    }
    for task in ready {
        transition(
            host,
            &task,
            Trigger::DependenciesMerged,
            "its dependencies are merged into the run",
        )?;
    }
    let (low, deferred, ready_now) = host.read(|state| {
        (
            limit_near(state),
            state
                .tasks
                .by_id
                .values()
                .filter(|r| {
                    r.state == TaskState::Deferred
                        && state.tasks.progress(&r.spec.id).deferred_for_limit
                })
                .map(|r| r.spec.id.clone())
                .collect::<Vec<_>>(),
            state.tasks.in_state(TaskState::Ready),
        )
    });
    if low {
        for task in ready_now {
            transition(host, &task, Trigger::LimitNear, "the run's limit is low")?;
        }
    } else {
        for task in deferred {
            transition(
                host,
                &task,
                Trigger::RaiseLimit,
                "the run is under its limit again",
            )?;
        }
    }
    Ok(None)
}

/// Tasks to start now, and tasks to drive again after a restart or after something outside
/// moved them (an answer, a give-back, an agent coming back).
fn tasks_to_drive(
    host: &Host,
    active: &BTreeSet<TaskId>,
    idle: &BTreeMap<TaskId, u64>,
) -> Vec<TaskId> {
    host.read(|state| {
        let running = state.tasks.in_state(TaskState::Running).len()
            + active
                .iter()
                .filter(|task| state.tasks.get(task).map(|r| r.state) == Some(TaskState::Ready))
                .count();
        let mut tasks: Vec<TaskId> = startable(state, host.settings(), running)
            .into_iter()
            .filter(|task| !active.contains(task) && !ways::driven_by_its_way(state, task))
            .collect();
        for record in state.tasks.by_id.values() {
            let task = &record.spec.id;
            if ways::driven_by_its_way(state, task) {
                continue;
            }
            let resumable = matches!(
                record.state,
                TaskState::Running
                    | TaskState::Checking
                    | TaskState::InReview
                    | TaskState::Merging
                    | TaskState::Failed
                    | TaskState::NeedsYou
                    | TaskState::NeedsDecision
                    | TaskState::TakenOver
            );
            let unchanged = idle.get(task) == Some(&state.tasks.progress(task).changes);
            if resumable && !unchanged && !active.contains(task) && !tasks.contains(task) {
                tasks.push(task.clone());
            }
        }
        tasks
    })
}

/// Tasks the step ends without: deferred ones, and waiting ones that depend on a task left
/// out, directly or through others, since they can't start until it comes back.
pub(crate) fn left_out(state: &RunState) -> BTreeSet<TaskId> {
    let mut out: BTreeSet<TaskId> = state
        .tasks
        .in_state(TaskState::Deferred)
        .into_iter()
        .collect();
    loop {
        let held: Vec<TaskId> = state
            .tasks
            .by_id
            .values()
            .filter(|record| record.state == TaskState::Waiting && !out.contains(&record.spec.id))
            .filter(|record| record.spec.depends_on.iter().any(|dep| out.contains(dep)))
            .map(|record| record.spec.id.clone())
            .collect();
        if held.is_empty() {
            return out;
        }
        out.extend(held);
    }
}

/// The step's end once no task can move: every task merged, cancelled, or left out; or,
/// in Captain, the task needed you with `move_to_crew` set.
fn finished(host: &Host) -> Option<StepDone> {
    let snapshot = host.read(Clone::clone);
    let captain = snapshot.run.mode == Some(RunMode::Captain);
    let needs_you = !snapshot.tasks.in_state(TaskState::NeedsYou).is_empty();
    if captain && needs_you && captain::moves_to_crew(&snapshot, host) {
        return Some(StepDone {
            end: StepEnd::Done,
            summary: Some("the Captain task needed you, so it goes to the crew".to_owned()),
            flow: Flow::Switch(runner::crew_workflow(host)),
        });
    }
    let left_out = left_out(&snapshot);
    let settled = snapshot.tasks.by_id.values().all(|record| {
        machine::is_final(record.state)
            || left_out.contains(&record.spec.id)
            || ways::driven_by_its_way(&snapshot, &record.spec.id)
    });
    settled.then(|| StepDone::done(summary(&snapshot)))
}

/// Such as "3 tasks merged, 1 deferred, 2 waiting on deferred tasks".
pub(crate) fn summary(state: &RunState) -> String {
    let count = |wanted: TaskState| {
        state
            .tasks
            .by_id
            .values()
            .filter(|r| r.state == wanted && !matches!(r.spec.kind, TaskKind::Experiment { .. }))
            .count()
    };
    let merged = count(TaskState::Merged);
    let mut text = match merged {
        1 => "1 task merged into the run".to_owned(),
        n => format!("{n} tasks merged into the run"),
    };
    for (state, word) in [
        (TaskState::Deferred, "deferred"),
        (TaskState::Cancelled, "cancelled"),
    ] {
        let n = count(state);
        if n > 0 {
            text.push_str(&format!(", {n} {word}"));
        }
    }
    let held = left_out(state)
        .iter()
        .filter(|task| state.tasks.get(task).map(|r| r.state) == Some(TaskState::Waiting))
        .count();
    if held > 0 {
        text.push_str(&format!(", {held} waiting on deferred tasks"));
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;
    use argonauts_core::Spend;
    use argonauts_core::events::agent_status::RunSpend;
    use argonauts_core::settings::limit::LimitAmount;

    use crate::testutil::task_spec;

    fn state_with(tasks: &[(&str, &[&str], Option<&str>, TaskState)]) -> RunState {
        let mut state = RunState::default();
        for (id, deps, worker, task_state) in tasks {
            let mut record = TaskEntry::new(task_spec(id, deps, *worker));
            record.state = *task_state;
            state.tasks.by_id.insert(record.spec.id.clone(), record);
        }
        state
    }

    fn ids(tasks: &[TaskId]) -> Vec<&str> {
        tasks.iter().map(TaskId::as_str).collect()
    }

    #[test]
    fn waiting_tasks_unblock_once_their_dependencies_are_in() {
        let state = state_with(&[
            ("task-001", &[], None, TaskState::Merged),
            ("task-002", &["task-001"], None, TaskState::Waiting),
            ("task-003", &["task-002"], None, TaskState::Waiting),
            ("task-004", &[], None, TaskState::Ready),
        ]);
        assert_eq!(ids(&unblocked(&state)), ["task-002"]);
    }

    #[test]
    fn a_busy_worker_or_a_full_agent_waits() {
        let mut settings = Settings::default();
        settings.concurrency.max_sessions = 6;
        settings.concurrency.max_per_agent = 1;
        let state = state_with(&[
            (
                "task-001",
                &[],
                Some("claude-code/opus"),
                TaskState::Running,
            ),
            ("task-002", &[], Some("claude-code/opus"), TaskState::Ready),
            (
                "task-003",
                &[],
                Some("claude-code/sonnet"),
                TaskState::Ready,
            ),
            ("task-004", &[], Some("codex"), TaskState::Ready),
            ("task-005", &[], Some("codex"), TaskState::Ready),
            ("task-006", &[], Some("agy"), TaskState::Ready),
        ]);
        assert_eq!(
            ids(&startable(&state, &settings, 1)),
            ["task-004", "task-006"]
        );
    }

    #[test]
    fn slots_cap_how_many_start() {
        let mut settings = Settings::default();
        settings.concurrency.max_sessions = 2;
        let state = state_with(&[
            ("task-001", &[], Some("codex"), TaskState::Ready),
            ("task-002", &[], Some("agy"), TaskState::Ready),
            ("task-003", &[], Some("claude-code"), TaskState::Ready),
        ]);
        assert_eq!(
            ids(&startable(&state, &settings, 0)),
            ["task-001", "task-002"]
        );
        assert!(startable(&state, &settings, 2).is_empty());
    }

    #[test]
    fn a_spent_limit_holds_every_start() {
        let settings = Settings::default();
        let mut state = state_with(&[("task-001", &[], Some("codex"), TaskState::Ready)]);
        let limit = LimitAmount {
            unit: LimitUnit::Usd,
            amount: 5.0,
        };
        state.run.spend = Some(RunSpend {
            spent: Spend {
                cost_usd: 5.5,
                ..Spend::default()
            },
            limit: Some(limit),
            lines: Vec::new(),
            reserve_left: None,
        });
        assert!(limit_near(&state));
        assert!(startable(&state, &settings, 0).is_empty());
        state.run.spend.as_mut().unwrap().reserve_left = Some(LimitAmount {
            unit: LimitUnit::Usd,
            amount: 1.0,
        });
        assert!(!limit_near(&state), "the reserve covers it");
    }

    #[test]
    fn the_summary_counts_merged_deferred_and_cancelled() {
        let state = state_with(&[
            ("task-001", &[], None, TaskState::Merged),
            ("task-002", &[], None, TaskState::Merged),
            ("task-003", &[], None, TaskState::Deferred),
        ]);
        assert_eq!(summary(&state), "2 tasks merged into the run, 1 deferred");
    }

    #[test]
    fn waiting_on_a_deferred_task_leaves_a_task_out() {
        let state = state_with(&[
            ("task-001", &[], None, TaskState::Merged),
            ("task-002", &[], None, TaskState::Deferred),
            ("task-003", &["task-002"], None, TaskState::Waiting),
            (
                "task-004",
                &["task-003", "task-001"],
                None,
                TaskState::Waiting,
            ),
            ("task-005", &["task-001"], None, TaskState::Waiting),
        ]);
        assert_eq!(
            left_out(&state)
                .iter()
                .map(TaskId::as_str)
                .collect::<Vec<_>>(),
            ["task-002", "task-003", "task-004"]
        );
        let settled = state_with(&[
            ("task-001", &[], None, TaskState::Merged),
            ("task-002", &[], None, TaskState::Deferred),
            ("task-003", &["task-002"], None, TaskState::Waiting),
        ]);
        assert_eq!(
            summary(&settled),
            "1 task merged into the run, 1 deferred, 1 waiting on deferred tasks"
        );
    }
}
