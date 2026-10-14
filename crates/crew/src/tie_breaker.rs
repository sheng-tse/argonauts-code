//! The tie-breaker, by a fixed rule: the main agent for Understand and planning (process work
//! only), the worker of the critical-path task for build (longest dependency chain, ties by task
//! id), and the merge chore's holder for merge and the final check. A tie-breaker that is
//! a party to a tie never breaks it; the tie goes to you.

use std::collections::{BTreeMap, BTreeSet};

use argonauts_core::TaskId;
use argonauts_core::events::run::RunPhase;
use argonauts_core::events::task::{TaskKind, TaskSpec, TaskState};
use argonauts_core::names::ModelRef;
use argonauts_core::settings::Settings;
use argonauts_core::settings::approvals::Answerer;

use crate::state::RunState;

/// What the rule needs to know about one task.
#[derive(Debug, Clone, Copy)]
pub struct TaskView<'a> {
    pub spec: &'a TaskSpec,
    pub state: TaskState,
    pub worker: Option<&'a ModelRef>,
}

/// `None` before the run has a phase.
pub fn tie_breaker(state: &RunState, settings: &Settings) -> Option<ModelRef> {
    let phase = state.run.phase?;
    let tasks: Vec<TaskView<'_>> = state
        .tasks
        .by_id
        .values()
        .map(|record| TaskView {
            spec: &record.spec,
            state: record.state,
            worker: record.worker.as_ref().or(record.spec.worker.as_ref()),
        })
        .collect();
    let critical = state
        .divide
        .graph
        .as_ref()
        .map(|graph| graph.critical_path.as_slice())
        .filter(|path| !path.is_empty());
    Some(for_phase(phase, &tasks, critical, &settings.crew.main))
}

/// Who answers the crew's questions in your place: in `autopilot` the model
/// `approvals.answers` picked, else the tie-breaker of the phase the run is in, else the main
/// agent.
pub fn answering(state: &RunState, settings: &Settings, autopilot: bool) -> ModelRef {
    match &settings.approvals.answers {
        Some(Answerer::Model(model)) if autopilot => model.clone(),
        _ => tie_breaker(state, settings).unwrap_or_else(|| settings.crew.main.clone()),
    }
}

/// The rule itself: `critical` is the task graph's critical path when it has one.
pub fn for_phase(
    phase: RunPhase,
    tasks: &[TaskView<'_>],
    critical: Option<&[TaskId]>,
    default: &ModelRef,
) -> ModelRef {
    let tie_breaker = match phase {
        RunPhase::Understand | RunPhase::Planning | RunPhase::Dividing | RunPhase::RunReview => {
            None
        }
        RunPhase::Contracts | RunPhase::Build => critical_task_worker(tasks, critical),
        RunPhase::FinalCheck | RunPhase::Merge => merge_chore_holder(tasks),
    };
    tie_breaker.unwrap_or_else(|| default.clone())
}

/// The worker of the first task on the critical path that isn't in yet: the task the build's
/// end waits on now.
fn critical_task_worker(tasks: &[TaskView<'_>], critical: Option<&[TaskId]>) -> Option<ModelRef> {
    let path = match critical {
        Some(path) => path.to_vec(),
        None => {
            let specs: Vec<TaskSpec> = tasks.iter().map(|task| task.spec.clone()).collect();
            critical_path(&specs)
        }
    };
    let on_path: Vec<&TaskView<'_>> = path
        .iter()
        .filter_map(|id| tasks.iter().find(|task| &task.spec.id == id))
        .collect();
    on_path
        .iter()
        .find(|task| !finished(task.state))
        .and_then(|task| task.worker.cloned())
        .or_else(|| on_path.last().and_then(|task| task.worker.cloned()))
}

fn finished(task: TaskState) -> bool {
    matches!(task, TaskState::Merged | TaskState::Cancelled)
}

/// The holder of the merge chore: the first chore task by id with a worker.
fn merge_chore_holder(tasks: &[TaskView<'_>]) -> Option<ModelRef> {
    let mut chores: Vec<&TaskView<'_>> = tasks
        .iter()
        .filter(|task| task.spec.kind == TaskKind::Chore && task.worker.is_some())
        .collect();
    chores.sort_by(|a, b| a.spec.id.cmp(&b.spec.id));
    chores.first().and_then(|task| task.worker.cloned())
}

/// The longest dependency chain, ties broken by task id.
pub fn critical_path(tasks: &[TaskSpec]) -> Vec<TaskId> {
    let deps: BTreeMap<&TaskId, Vec<&TaskId>> = tasks
        .iter()
        .map(|task| (&task.id, task.depends_on.iter().collect()))
        .collect();
    let mut memo: BTreeMap<&TaskId, Vec<TaskId>> = BTreeMap::new();
    let mut best: Vec<TaskId> = Vec::new();
    for id in deps.keys() {
        let chain = longest_to(id, &deps, &mut memo, &mut BTreeSet::new());
        if better(&chain, &best) {
            best = chain;
        }
    }
    best
}

/// The longest chain ending at `id`, from its first dependency to `id`. A cycle, which a
/// checked graph never has, is cut where it closes.
fn longest_to<'a>(
    id: &'a TaskId,
    deps: &BTreeMap<&'a TaskId, Vec<&'a TaskId>>,
    memo: &mut BTreeMap<&'a TaskId, Vec<TaskId>>,
    visiting: &mut BTreeSet<&'a TaskId>,
) -> Vec<TaskId> {
    if let Some(chain) = memo.get(id) {
        return chain.clone();
    }
    visiting.insert(id);
    let mut best: Vec<TaskId> = Vec::new();
    for dep in deps.get(id).into_iter().flatten() {
        if visiting.contains(dep) || !deps.contains_key(dep) {
            continue;
        }
        let chain = longest_to(dep, deps, memo, visiting);
        if better(&chain, &best) {
            best = chain;
        }
    }
    visiting.remove(id);
    best.push(id.clone());
    memo.insert(id, best.clone());
    best
}

/// Longer wins; between equals, the one whose ids sort first.
fn better(chain: &[TaskId], than: &[TaskId]) -> bool {
    chain.len() > than.len() || (chain.len() == than.len() && chain < than)
}

#[cfg(test)]
mod tests {
    use argonauts_core::events::task::TaskLimit;

    use super::*;

    fn id(s: &str) -> TaskId {
        TaskId::new(s).unwrap()
    }

    fn spec(task: &str, kind: TaskKind, deps: &[&str], worker: Option<&str>) -> TaskSpec {
        TaskSpec {
            id: id(task),
            title: task.to_owned(),
            task: String::new(),
            kind,
            owned_paths: Vec::new(),
            read_only_paths: Vec::new(),
            forbidden_paths: Vec::new(),
            hidden_paths: Vec::new(),
            depends_on: deps.iter().map(|d| id(d)).collect(),
            criteria: Vec::new(),
            worker: worker.map(|o| o.parse().unwrap()),
            reviewers: Vec::new(),
            limit: TaskLimit::default(),
            predictions: None,
            item: None,
        }
    }

    fn ids(list: &[&str]) -> Vec<TaskId> {
        list.iter().map(|s| id(s)).collect()
    }

    #[test]
    fn the_longest_chain_wins() {
        let tasks = vec![
            spec("task-001", TaskKind::Interface, &[], None),
            spec("task-002", TaskKind::Build, &["task-001"], None),
            spec("task-003", TaskKind::Build, &["task-002"], None),
            spec("task-004", TaskKind::Build, &["task-001"], None),
        ];
        assert_eq!(
            critical_path(&tasks),
            ids(&["task-001", "task-002", "task-003"])
        );
    }

    #[test]
    fn equal_chains_go_by_task_id() {
        let tasks = vec![
            spec("task-004", TaskKind::Build, &["task-003"], None),
            spec("task-003", TaskKind::Build, &[], None),
            spec("task-002", TaskKind::Build, &["task-001"], None),
            spec("task-001", TaskKind::Build, &[], None),
        ];
        assert_eq!(critical_path(&tasks), ids(&["task-001", "task-002"]));
    }

    #[test]
    fn cycles_and_unknown_dependencies_do_not_hang() {
        let tasks = vec![
            spec("task-001", TaskKind::Build, &["task-002", "task-009"], None),
            spec("task-002", TaskKind::Build, &["task-001"], None),
        ];
        assert_eq!(critical_path(&tasks).len(), 2);
        assert!(critical_path(&[]).is_empty());
    }

    #[test]
    fn the_tie_breaker_follows_the_phase() {
        let default: ModelRef = "claude-code".parse().unwrap();
        let specs = [
            spec("task-001", TaskKind::Interface, &[], None),
            spec("task-002", TaskKind::Build, &["task-001"], None),
            spec("task-003", TaskKind::Build, &["task-001"], None),
            spec("task-004", TaskKind::Chore, &[], None),
        ];
        let workers: Vec<ModelRef> = ["claude-code", "codex", "antigravity-cli", "antigravity-cli"]
            .iter()
            .map(|o| o.parse().unwrap())
            .collect();
        let states = [
            TaskState::Merged,
            TaskState::Running,
            TaskState::Running,
            TaskState::Waiting,
        ];
        let tasks: Vec<TaskView<'_>> = specs
            .iter()
            .zip(&workers)
            .zip(states)
            .map(|((spec, worker), state)| TaskView {
                spec,
                state,
                worker: Some(worker),
            })
            .collect();
        let in_phase = |phase, critical| for_phase(phase, &tasks, critical, &default).to_string();
        assert_eq!(in_phase(RunPhase::Planning, None), "claude-code");
        assert_eq!(in_phase(RunPhase::Build, None), "codex");
        assert_eq!(in_phase(RunPhase::FinalCheck, None), "antigravity-cli");
        let path = ids(&["task-001", "task-003"]);
        assert_eq!(
            in_phase(RunPhase::Build, Some(path.as_slice())),
            "antigravity-cli"
        );
        assert_eq!(for_phase(RunPhase::Build, &[], None, &default), default);
        assert_eq!(
            tie_breaker(&RunState::default(), &Settings::default()),
            None
        );
    }
}
