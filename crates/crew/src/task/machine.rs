//! The task state machine, as one table in code. A move the table doesn't list is refused.

use argonauts_core::events::task::TaskState;

/// What happened to a task, as the table's middle column names it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Trigger {
    DependenciesMerged,
    /// The worker and a slot are free, and the limit has room.
    Start,
    LimitNear,
    RaiseLimit,
    /// A question for you or another worker.
    Question,
    Answered,
    /// Limit hit, agent stop, a change request, or `StopSession`.
    NeedsDecision,
    /// Decided or fell back.
    Decided,
    AskYou,
    TakeOver,
    GiveBack,
    /// You said done after taking over.
    SaidDone,
    /// You kept what a stopped session committed, so the checks decide.
    KeptWork,
    ReportedDone,
    /// Crash, stuck, or looping.
    Broke,
    FirstCheckFailure,
    ChecksFailedAgain,
    ChecksPassed,
    /// The task is worked as a goal, and its check found a line that doesn't hold yet.
    GoalNotMet,
    /// Rounds 1 to `review.max_change_rounds`.
    ChangesRequested,
    ChangesRequestedAgain,
    /// Approved, or the judge settled on approve.
    Approved,
    AffectedTestsFailed,
    /// Merged and tested.
    Merged,
    /// Changes asked for at the merge approval, after the task is in the run branch.
    SentBack,
    /// Not reassigned yet: a fresh session on another provider, with the note.
    Retry,
    /// You or your rules picked a new worker, or reset the branch, so the work goes back to it.
    NewWorker,
    Defer,
    /// Your note says how; the new tasks go through the graph validator.
    Split,
    Drop,
}

use TaskState::*;

/// Every allowed move: `(from, trigger, to)`.
pub const TABLE: &[(TaskState, Trigger, TaskState)] = &[
    (Waiting, Trigger::DependenciesMerged, Ready),
    (Ready, Trigger::Start, Running),
    (Ready, Trigger::LimitNear, Deferred),
    (Deferred, Trigger::RaiseLimit, Ready),
    (Running, Trigger::Question, Blocked),
    (Blocked, Trigger::Answered, Running),
    (Running, Trigger::NeedsDecision, NeedsDecision),
    (NeedsDecision, Trigger::Decided, Ready),
    (NeedsDecision, Trigger::AskYou, NeedsYou),
    // "Take over myself" on a fallback or limit question.
    (NeedsDecision, Trigger::TakeOver, TakenOver),
    (Running, Trigger::TakeOver, TakenOver),
    (TakenOver, Trigger::GiveBack, Ready),
    (TakenOver, Trigger::SaidDone, Checking),
    (Running, Trigger::ReportedDone, Checking),
    (Running, Trigger::Broke, Failed),
    (Checking, Trigger::FirstCheckFailure, Running),
    (Checking, Trigger::ChecksFailedAgain, Failed),
    (Checking, Trigger::ChecksPassed, InReview),
    (Checking, Trigger::GoalNotMet, Running),
    (Checking, Trigger::NewWorker, Running),
    (InReview, Trigger::ChangesRequested, Running),
    (InReview, Trigger::ChangesRequestedAgain, NeedsYou),
    (InReview, Trigger::Approved, Merging),
    (InReview, Trigger::NewWorker, Running),
    (Merging, Trigger::AffectedTestsFailed, Running),
    (Merging, Trigger::Merged, Merged),
    (Merged, Trigger::SentBack, Running),
    (Failed, Trigger::Retry, Ready),
    (Failed, Trigger::AskYou, NeedsYou),
    (NeedsYou, Trigger::NewWorker, Ready),
    (NeedsYou, Trigger::TakeOver, TakenOver),
    (NeedsYou, Trigger::KeptWork, Checking),
    (NeedsYou, Trigger::Defer, Deferred),
    (NeedsYou, Trigger::Split, Cancelled),
    (NeedsYou, Trigger::Drop, Cancelled),
];

pub fn next(from: TaskState, trigger: Trigger) -> Option<TaskState> {
    TABLE
        .iter()
        .find(|(state, row, _)| *state == from && *row == trigger)
        .map(|(_, _, to)| *to)
}

/// Whether the task is done moving: merged or cancelled. Only the merge approval moves
/// a merged task again, by sending it back.
pub fn is_final(state: TaskState) -> bool {
    matches!(state, Merged | Cancelled)
}

/// Whether the task waits for a decision before it can move again.
pub fn waits_on_decision(state: TaskState) -> bool {
    matches!(
        state,
        Blocked | NeedsDecision | NeedsYou | Deferred | TakenOver | Failed
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    const ALL_STATES: [TaskState; 14] = [
        Waiting,
        Ready,
        Running,
        Blocked,
        NeedsDecision,
        Checking,
        InReview,
        Merging,
        Merged,
        Failed,
        NeedsYou,
        Deferred,
        Cancelled,
        TakenOver,
    ];

    #[test]
    fn task_state_machine_table() {
        let rows = [
            (Waiting, Trigger::DependenciesMerged, Ready),
            (Ready, Trigger::Start, Running),
            (Ready, Trigger::LimitNear, Deferred),
            (Deferred, Trigger::RaiseLimit, Ready),
            (Running, Trigger::Question, Blocked),
            (Blocked, Trigger::Answered, Running),
            (Running, Trigger::NeedsDecision, NeedsDecision),
            (NeedsDecision, Trigger::Decided, Ready),
            (NeedsDecision, Trigger::AskYou, NeedsYou),
            (NeedsDecision, Trigger::TakeOver, TakenOver),
            (Running, Trigger::TakeOver, TakenOver),
            (TakenOver, Trigger::GiveBack, Ready),
            (TakenOver, Trigger::SaidDone, Checking),
            (Running, Trigger::ReportedDone, Checking),
            (Running, Trigger::Broke, Failed),
            (Checking, Trigger::FirstCheckFailure, Running),
            (Checking, Trigger::ChecksFailedAgain, Failed),
            (Checking, Trigger::ChecksPassed, InReview),
            (Checking, Trigger::GoalNotMet, Running),
            (Checking, Trigger::NewWorker, Running),
            (InReview, Trigger::ChangesRequested, Running),
            (InReview, Trigger::ChangesRequestedAgain, NeedsYou),
            (InReview, Trigger::Approved, Merging),
            (InReview, Trigger::NewWorker, Running),
            (Merging, Trigger::AffectedTestsFailed, Running),
            (Merging, Trigger::Merged, Merged),
            (Merged, Trigger::SentBack, Running),
            (Failed, Trigger::Retry, Ready),
            (Failed, Trigger::AskYou, NeedsYou),
            (NeedsYou, Trigger::NewWorker, Ready),
            (NeedsYou, Trigger::TakeOver, TakenOver),
            (NeedsYou, Trigger::KeptWork, Checking),
            (NeedsYou, Trigger::Defer, Deferred),
            (NeedsYou, Trigger::Split, Cancelled),
            (NeedsYou, Trigger::Drop, Cancelled),
        ];
        for (from, trigger, to) in rows {
            assert_eq!(next(from, trigger), Some(to), "{from:?} --{trigger:?}-->");
        }
        assert_eq!(
            TABLE.len(),
            rows.len(),
            "the table has rows the brief doesn't"
        );

        // Every other pair is refused.
        let triggers: Vec<Trigger> = TABLE.iter().map(|(_, t, _)| *t).collect();
        for from in ALL_STATES {
            for trigger in &triggers {
                let listed = rows.iter().any(|(f, t, _)| *f == from && t == trigger);
                if !listed {
                    assert_eq!(
                        next(from, *trigger),
                        None,
                        "{from:?} --{trigger:?}--> allowed"
                    );
                }
            }
        }
    }

    #[test]
    fn each_state_and_trigger_pair_has_one_row_at_most() {
        for (i, (from, trigger, _)) in TABLE.iter().enumerate() {
            let twins = TABLE[i + 1..]
                .iter()
                .filter(|(f, t, _)| f == from && t == trigger)
                .count();
            assert_eq!(twins, 0, "{from:?} {trigger:?}");
        }
    }

    #[test]
    fn final_states_have_no_way_out_but_the_merge_approval() {
        for state in ALL_STATES.into_iter().filter(|s| is_final(*s)) {
            let ways_out: Vec<Trigger> = TABLE
                .iter()
                .filter(|(from, _, _)| *from == state)
                .map(|(_, trigger, _)| *trigger)
                .collect();
            let expected = if state == Merged {
                vec![Trigger::SentBack]
            } else {
                Vec::new()
            };
            assert_eq!(ways_out, expected, "{state:?}");
        }
        assert!(!is_final(NeedsYou));
        assert!(waits_on_decision(NeedsYou));
        assert!(!waits_on_decision(Ready));
    }
}
