//! Contracts first, inside `build`: the interface and acceptance-test tasks run first, as the
//! `Contracts` phase. Once every test task is merged, a goal criterion no test task covers
//! holds the build tasks back and comes to you.

use argonauts_core::PhaseStatus;
use argonauts_core::TaskId;
use argonauts_core::runs::events::RunEventKind;
use argonauts_core::runs::events::approval::{
    AnswerAction, ApprovalKind, ApprovalPacket, FreeText, StuckPacket,
};
use argonauts_core::runs::events::run::{PhaseChange, RunPhase};
use argonauts_core::runs::events::task::{TaskKind, TaskState};
use argonauts_core::safety::policy::EvidenceFacts;

use crate::approvals::{self, ApprovalRequest};
use crate::divide;
use crate::engine::error::EngineResult;
use crate::engine::host::Host;
use crate::state::RunState;
use crate::workflow::StepAt;

fn is_contract(kind: &TaskKind) -> bool {
    matches!(kind, TaskKind::Interface | TaskKind::AcceptanceTests { .. })
}

/// Goal criteria no acceptance-test task covers, once every test task is merged. Empty
/// when the run has no test tasks.
pub(crate) fn uncovered(state: &RunState) -> Vec<String> {
    let tests: Vec<_> = state
        .tasks
        .by_id
        .values()
        .filter(|r| matches!(r.spec.kind, TaskKind::AcceptanceTests { .. }))
        .filter(|r| r.state != TaskState::Cancelled)
        .collect();
    if tests.is_empty() || tests.iter().any(|t| t.state != TaskState::Merged) {
        return Vec::new();
    }
    let covered: Vec<String> = tests
        .iter()
        .flat_map(|t| t.spec.criteria.iter().cloned())
        .collect();
    let criteria = state
        .goal
        .approved
        .as_ref()
        .map(|goal| goal.criteria.clone())
        .unwrap_or_default();
    divide::contracts::uncovered(&criteria, &covered)
}

/// A build task whose dependencies are in, and so would start, if coverage allowed.
pub(crate) fn held_build_task(state: &RunState, ready: &[TaskId]) -> Option<TaskId> {
    ready
        .iter()
        .find(|id| {
            state
                .tasks
                .get(id)
                .is_some_and(|r| r.spec.kind == TaskKind::Build)
        })
        .cloned()
}

/// Asks you about the criteria no test covers. True when the build goes on anyway.
pub(crate) async fn criteria_check(
    host: &Host,
    at: &StepAt,
    task: &TaskId,
    uncovered: &[String],
) -> EngineResult<bool> {
    let list = uncovered.join(", ");
    let packet = StuckPacket {
        task: task.clone(),
        title: format!("Goal criteria no acceptance test covers: {list}"),
        state: TaskState::Waiting,
        tries: Vec::new(),
        last_failure: format!("no acceptance test covers {list}, so the build tasks wait"),
    };
    let request = ApprovalRequest {
        at: Some(at.clone()),
        kind: ApprovalKind::Stuck,
        title: format!("Untested goal criteria · {list}"),
        task: Some(task.clone()),
        packet: ApprovalPacket::Stuck(packet),
        facts: EvidenceFacts {
            untested_criteria: uncovered.to_vec(),
            ..EvidenceFacts::default()
        },
        agent_outputs: Vec::new(),
        documents: Vec::new(),
        options: None,
        free_text: FreeText::Guidance,
        authors: Vec::new(),
    };
    let decided = approvals::open_and_wait(host, request).await?;
    Ok(!matches!(
        decided.action,
        AnswerAction::Cancel | AnswerAction::Stop | AnswerAction::Abandon
    ))
}

/// Which phase the build is in: `Contracts` while interface or test tasks remain, then
/// `Build`.
pub(crate) fn phase_now(state: &RunState) -> RunPhase {
    let open_contract = state.tasks.by_id.values().any(|r| {
        is_contract(&r.spec.kind) && !matches!(r.state, TaskState::Merged | TaskState::Cancelled)
    });
    if open_contract {
        RunPhase::Contracts
    } else {
        RunPhase::Build
    }
}

/// Records the move into `phase`, finishing the one before. `current` is what was recorded
/// last in this step.
pub(crate) fn enter_phase(
    host: &Host,
    current: &mut Option<RunPhase>,
    phase: RunPhase,
) -> EngineResult<()> {
    if *current == Some(phase) {
        return Ok(());
    }
    if let Some(done) = current.take() {
        record_phase(host, done, PhaseStatus::Finished, None)?;
    }
    record_phase(host, phase, PhaseStatus::Started, None)?;
    *current = Some(phase);
    Ok(())
}

pub(crate) fn record_phase(
    host: &Host,
    phase: RunPhase,
    status: PhaseStatus,
    summary: Option<String>,
) -> EngineResult<()> {
    let change = PhaseChange {
        phase,
        status,
        summary,
    };
    host.record(None, None, RunEventKind::Phase(change))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::tasks::TaskEntry;
    use crate::testing::task_spec;

    fn add(
        state: &mut RunState,
        id: &str,
        kind: TaskKind,
        task_state: TaskState,
        criteria: &[&str],
    ) {
        let mut spec = task_spec(id, &[], None);
        spec.kind = kind;
        spec.criteria = criteria.iter().map(|c| (*c).to_owned()).collect();
        let mut record = TaskEntry::new(spec);
        record.state = task_state;
        state.tasks.by_id.insert(record.spec.id.clone(), record);
    }

    #[test]
    fn contracts_come_first_then_build() {
        let mut state = RunState::default();
        add(
            &mut state,
            "task-001",
            TaskKind::Build,
            TaskState::Waiting,
            &[],
        );
        assert_eq!(phase_now(&state), RunPhase::Build);
        let for_task = TaskId::new("task-001").unwrap();
        add(
            &mut state,
            "test-001",
            TaskKind::AcceptanceTests { for_task },
            TaskState::Running,
            &["G1"],
        );
        assert_eq!(phase_now(&state), RunPhase::Contracts);
        state
            .tasks
            .by_id
            .get_mut(&TaskId::new("test-001").unwrap())
            .unwrap()
            .state = TaskState::Merged;
        assert_eq!(phase_now(&state), RunPhase::Build);
    }

    #[test]
    fn coverage_waits_for_every_test_task() {
        let mut state = RunState::default();
        assert!(
            uncovered(&state).is_empty(),
            "no test tasks, nothing to check"
        );
        let for_task = TaskId::new("task-001").unwrap();
        add(
            &mut state,
            "test-001",
            TaskKind::AcceptanceTests { for_task },
            TaskState::Running,
            &["G1"],
        );
        assert!(uncovered(&state).is_empty(), "not merged into the run yet");
        let ready = [TaskId::new("task-001").unwrap()];
        add(
            &mut state,
            "task-001",
            TaskKind::Build,
            TaskState::Waiting,
            &[],
        );
        assert_eq!(held_build_task(&state, &ready), Some(ready[0].clone()));
    }
}
