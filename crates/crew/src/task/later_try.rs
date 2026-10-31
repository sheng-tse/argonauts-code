//! A later try that takes over a task's committed work. An earlier session may have done the
//! work and only failed to say so, so this try is watched for calls that change nothing.

use argonauts_core::TaskId;
use argonauts_core::runs::events::task::TryCause;

use crate::state::RunState;

/// Whether the task's newest try is a fresh session taking over work an earlier session
/// committed: after a failure, a fallback, a reassignment, or a restart. A try that resumes the
/// same session with checks, a review or the goal to answer has a change to make, and a
/// rotation or a resume carries on where it was.
pub(crate) fn on_committed_work(state: &RunState, task: &TaskId) -> bool {
    let Some(record) = state.tasks.get(task) else {
        return false;
    };
    let takes_over = matches!(
        state.tasks.progress(task).last_cause,
        Some(
            TryCause::AfterFailure
                | TryCause::Fallback
                | TryCause::Reassigned
                | TryCause::Restarted
        )
    );
    takes_over && !record.commits.is_empty()
}

#[cfg(test)]
mod tests {
    use argonauts_core::base::names::ModelRef;
    use argonauts_core::runs::events::task::TaskEvent;
    use argonauts_core::runs::events::{RunEvent, RunEventKind};
    use argonauts_core::{CommitInfo, DiffStat};

    use super::*;
    use crate::testing::{run_event, task_spec};

    const QWEN: &str = "opencode/ollama/qwen3-coder:30b";

    fn worker() -> ModelRef {
        QWEN.parse().unwrap()
    }

    fn task(kind: TaskEvent) -> RunEventKind {
        RunEventKind::Task(kind)
    }

    fn try_event(seq: u64, session: &str, cause: TryCause) -> RunEvent {
        run_event(
            seq,
            Some("task-001"),
            Some(session),
            task(TaskEvent::Try {
                try_number: 1,
                worker: worker(),
                cause,
                told: Vec::new(),
            }),
        )
    }

    fn committed(seq: u64) -> RunEvent {
        run_event(
            seq,
            Some("task-001"),
            Some("s-001"),
            task(TaskEvent::Committed(CommitInfo {
                branch: "argonauts/run-01/task-001".into(),
                sha: Some("58721bae".into()),
                message: "task-001: work from s-001".into(),
                stat: DiffStat {
                    files_changed: 1,
                    insertions: 13,
                    deletions: 1,
                },
                by: Some(worker()),
            })),
        )
    }

    fn state(events: Vec<RunEvent>) -> RunState {
        let created = run_event(
            1,
            Some("task-001"),
            None,
            task(TaskEvent::Created(Box::new(task_spec(
                "task-001",
                &[],
                Some(QWEN),
            )))),
        );
        RunState::fold(&[vec![created], events].concat())
    }

    #[test]
    fn only_a_fresh_try_on_committed_work_is_watched() {
        let id = TaskId::new("task-001").unwrap();
        let first = state(vec![try_event(2, "s-001", TryCause::First)]);
        assert!(!on_committed_work(&first, &id));

        // The run in the record: the first session committed its fix, then a second took over.
        let taken_over = state(vec![
            try_event(2, "s-001", TryCause::First),
            committed(3),
            try_event(4, "s-002", TryCause::Reassigned),
        ]);
        assert!(on_committed_work(&taken_over, &id));

        let nothing_committed = state(vec![
            try_event(2, "s-001", TryCause::First),
            try_event(3, "s-002", TryCause::AfterFailure),
        ]);
        assert!(!on_committed_work(&nothing_committed, &id));

        // A try with something to answer has a change to make, and one that carries on a
        // session goes on where it was.
        for cause in [
            TryCause::CheckRetry,
            TryCause::ReviewChanges,
            TryCause::GoalRetry,
            TryCause::Rotation,
            TryCause::Resumed,
            TryCause::MergeFailed,
        ] {
            let answering = state(vec![
                try_event(2, "s-001", TryCause::First),
                committed(3),
                try_event(4, "s-001", cause),
            ]);
            assert!(!on_committed_work(&answering, &id), "{cause:?}");
        }
        for cause in [TryCause::Fallback, TryCause::Restarted] {
            let fresh = state(vec![
                try_event(2, "s-001", TryCause::First),
                committed(3),
                try_event(4, "s-002", cause),
            ]);
            assert!(on_committed_work(&fresh, &id), "{cause:?}");
        }
        assert!(
            !on_committed_work(&RunState::fold(&[]), &id),
            "no such task"
        );
    }
}
