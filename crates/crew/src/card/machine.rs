//! The card state machine, as one table in code. A move the table doesn't list is refused.

use argonauts_core::events::card::CardState;

/// What happened to a card, as the table's middle column names it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Trigger {
    DependenciesIntegrated,
    /// Owner, slot, and budget are free.
    Start,
    BudgetLow,
    MoreBudget,
    /// A question for you or another owner.
    Question,
    Answered,
    /// Budget hit, vendor stop, a change request, or `StopSession`.
    NeedsDecision,
    /// Decided or handed off.
    Decided,
    Escalate,
    TakeOver,
    HandBack,
    /// You said done after taking over.
    SaidDone,
    ReportedDone,
    /// Crash, stuck, or looping.
    Broke,
    FirstGateFailure,
    GatesFailedAgain,
    GatesPassed,
    /// The task is worked as a goal, and its check found a line that doesn't hold yet.
    GoalNotMet,
    /// Rounds 1 to `review.max_change_rounds`.
    ChangesRequested,
    ChangesRequestedAgain,
    /// Approved, or adjudicated approve.
    Approved,
    AffectedTestsFailed,
    /// Merged and tested.
    Merged,
    /// Changes asked for at the merge checkpoint, after the card is in the integration branch.
    SentBack,
    /// Not reassigned yet: a fresh session on another family, with the packet.
    Retry,
    /// You or your rules picked a new owner, or reset the branch, so the work goes back to it.
    NewOwner,
    Defer,
    /// Your note says how; the new cards go through the graph validator.
    Split,
    Drop,
}

use CardState::*;

/// Every allowed move: `(from, trigger, to)`.
pub const TABLE: &[(CardState, Trigger, CardState)] = &[
    (Waiting, Trigger::DependenciesIntegrated, Ready),
    (Ready, Trigger::Start, Running),
    (Ready, Trigger::BudgetLow, Deferred),
    (Deferred, Trigger::MoreBudget, Ready),
    (Running, Trigger::Question, Blocked),
    (Blocked, Trigger::Answered, Running),
    (Running, Trigger::NeedsDecision, NeedsDecision),
    (NeedsDecision, Trigger::Decided, Ready),
    (NeedsDecision, Trigger::Escalate, Escalated),
    // "Take over myself" on a handoff or budget card.
    (NeedsDecision, Trigger::TakeOver, TakenOver),
    (Running, Trigger::TakeOver, TakenOver),
    (TakenOver, Trigger::HandBack, Ready),
    (TakenOver, Trigger::SaidDone, Gating),
    (Running, Trigger::ReportedDone, Gating),
    (Running, Trigger::Broke, Failed),
    (Gating, Trigger::FirstGateFailure, Running),
    (Gating, Trigger::GatesFailedAgain, Failed),
    (Gating, Trigger::GatesPassed, InReview),
    (Gating, Trigger::GoalNotMet, Running),
    (Gating, Trigger::NewOwner, Running),
    (InReview, Trigger::ChangesRequested, Running),
    (InReview, Trigger::ChangesRequestedAgain, Escalated),
    (InReview, Trigger::Approved, Integrating),
    (InReview, Trigger::NewOwner, Running),
    (Integrating, Trigger::AffectedTestsFailed, Running),
    (Integrating, Trigger::Merged, Integrated),
    (Integrated, Trigger::SentBack, Running),
    (Failed, Trigger::Retry, Ready),
    (Failed, Trigger::Escalate, Escalated),
    (Escalated, Trigger::NewOwner, Ready),
    (Escalated, Trigger::TakeOver, TakenOver),
    (Escalated, Trigger::Defer, Deferred),
    (Escalated, Trigger::Split, Cancelled),
    (Escalated, Trigger::Drop, Cancelled),
];

pub fn next(from: CardState, trigger: Trigger) -> Option<CardState> {
    TABLE
        .iter()
        .find(|(state, row, _)| *state == from && *row == trigger)
        .map(|(_, _, to)| *to)
}

/// Whether the card is done moving: integrated or cancelled. Only the merge checkpoint moves
/// an integrated card again, by sending it back.
pub fn is_final(state: CardState) -> bool {
    matches!(state, Integrated | Cancelled)
}

/// Whether the card waits for a decision before it can move again.
pub fn waits_on_decision(state: CardState) -> bool {
    matches!(
        state,
        Blocked | NeedsDecision | Escalated | Deferred | TakenOver | Failed
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    const ALL_STATES: [CardState; 14] = [
        Waiting,
        Ready,
        Running,
        Blocked,
        NeedsDecision,
        Gating,
        InReview,
        Integrating,
        Integrated,
        Failed,
        Escalated,
        Deferred,
        Cancelled,
        TakenOver,
    ];

    #[test]
    fn card_state_machine_table() {
        let rows = [
            (Waiting, Trigger::DependenciesIntegrated, Ready),
            (Ready, Trigger::Start, Running),
            (Ready, Trigger::BudgetLow, Deferred),
            (Deferred, Trigger::MoreBudget, Ready),
            (Running, Trigger::Question, Blocked),
            (Blocked, Trigger::Answered, Running),
            (Running, Trigger::NeedsDecision, NeedsDecision),
            (NeedsDecision, Trigger::Decided, Ready),
            (NeedsDecision, Trigger::Escalate, Escalated),
            (NeedsDecision, Trigger::TakeOver, TakenOver),
            (Running, Trigger::TakeOver, TakenOver),
            (TakenOver, Trigger::HandBack, Ready),
            (TakenOver, Trigger::SaidDone, Gating),
            (Running, Trigger::ReportedDone, Gating),
            (Running, Trigger::Broke, Failed),
            (Gating, Trigger::FirstGateFailure, Running),
            (Gating, Trigger::GatesFailedAgain, Failed),
            (Gating, Trigger::GatesPassed, InReview),
            (Gating, Trigger::GoalNotMet, Running),
            (Gating, Trigger::NewOwner, Running),
            (InReview, Trigger::ChangesRequested, Running),
            (InReview, Trigger::ChangesRequestedAgain, Escalated),
            (InReview, Trigger::Approved, Integrating),
            (InReview, Trigger::NewOwner, Running),
            (Integrating, Trigger::AffectedTestsFailed, Running),
            (Integrating, Trigger::Merged, Integrated),
            (Integrated, Trigger::SentBack, Running),
            (Failed, Trigger::Retry, Ready),
            (Failed, Trigger::Escalate, Escalated),
            (Escalated, Trigger::NewOwner, Ready),
            (Escalated, Trigger::TakeOver, TakenOver),
            (Escalated, Trigger::Defer, Deferred),
            (Escalated, Trigger::Split, Cancelled),
            (Escalated, Trigger::Drop, Cancelled),
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
    fn final_states_have_no_way_out_but_the_merge_checkpoint() {
        for state in ALL_STATES.into_iter().filter(|s| is_final(*s)) {
            let ways_out: Vec<Trigger> = TABLE
                .iter()
                .filter(|(from, _, _)| *from == state)
                .map(|(_, trigger, _)| *trigger)
                .collect();
            let expected = if state == Integrated {
                vec![Trigger::SentBack]
            } else {
                Vec::new()
            };
            assert_eq!(ways_out, expected, "{state:?}");
        }
        assert!(!is_final(Escalated));
        assert!(waits_on_decision(Escalated));
        assert!(!waits_on_decision(Ready));
    }
}
