//! The conditions a step's `when` and a loop's `until` may name. Each is code reading the run's
//! state, never a model's opinion.

use serde_json::Value;

use argonauts_core::events::approval::{AnswerAction, ApprovalKind};
use argonauts_core::names::RoleName;
use argonauts_core::settings::Settings;

use crate::state::RunState;
use crate::state::sessions::SessionRecord;
use crate::workflow::StepAt;

/// Everything after the goal waits on it, so a goal loop that runs out of rounds stops the run.
pub const UNDERSTOOD: &str = "understood";

/// Every condition name a workflow may use. The validator refuses any other.
pub const KNOWN: &[&str] = &[
    UNDERSTOOD,
    "plan_decided",
    "plan_approved",
    "drafts_agree",
    "drafts_disagree",
    "critique_done",
    "reviewers_agree",
    "reviewers_disagree",
    "understand_first",
];

/// Whether `condition` holds now for the step at `at`. A loop's `until` is asked before each
/// round, with `at.round` the round about to run. `Err` names an unknown condition.
pub fn holds(
    state: &RunState,
    settings: &Settings,
    condition: &str,
    at: &StepAt,
) -> Result<bool, String> {
    Ok(match condition {
        UNDERSTOOD => state.goal.approved.is_some(),
        "plan_decided" => plan_answer(state).is_some_and(|action| action != AnswerAction::SendBack),
        "plan_approved" => plan_answer(state).is_some_and(|action| {
            matches!(
                action,
                AnswerAction::Approve | AnswerAction::ApproveWithEdits
            )
        }),
        "drafts_agree" => drafts(state) == Drafts::Agree,
        "drafts_disagree" => drafts(state) == Drafts::Disagree,
        // Critique only where drafts disagree, and never past `critique.rounds`.
        "critique_done" => {
            matches!(drafts(state), Drafts::Agree | Drafts::Unmapped)
                || at.round > settings.critique.rounds
        }
        "reviewers_agree" => reviewer_verdicts(state).is_some_and(|verdicts| verdicts <= 1),
        "reviewers_disagree" => reviewer_verdicts(state).is_some_and(|verdicts| verdicts > 1),
        "understand_first" => settings.captain.understand_first,
        other => return Err(format!("`{other}` isn't a condition a workflow can use")),
    })
}

fn plan_answer(state: &RunState) -> Option<AnswerAction> {
    state
        .approvals
        .last_decided(ApprovalKind::Plan, None)
        .and_then(|record| record.decided.as_ref())
        .map(|decided| decided.action.clone())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Drafts {
    NoDraft,
    /// One draft, or a map with no topic disputed: the fast path takes it.
    Agree,
    Disagree,
    /// Several drafts and no map, such as when no mapping agent was free: judging picks one.
    Unmapped,
}

/// How the newest drafts stand. A lone draft agrees with itself, since `map` skips it.
fn drafts(state: &RunState) -> Drafts {
    let plan = &state.plan;
    match plan.latest().len() {
        0 => Drafts::NoDraft,
        1 => Drafts::Agree,
        _ if plan.latest_map().is_none() => Drafts::Unmapped,
        _ if plan.disputed().is_empty() => Drafts::Agree,
        _ => Drafts::Disagree,
    }
}

/// How many different verdicts the newest review step's reviewers gave in its newest round.
/// Earlier rounds, and earlier items of a `for_each`, have their own verdicts.
fn reviewer_verdicts(state: &RunState) -> Option<usize> {
    let mut reviews: Vec<(&SessionRecord, &Value)> = state
        .sessions
        .by_key
        .values()
        .filter(|record| record.started.role == RoleName::Reviewer)
        .filter_map(|record| {
            let structured = record.end.as_ref()?.structured.as_ref()?;
            Some((record, structured))
        })
        .collect();
    reviews.sort_by_key(|(record, _)| std::cmp::Reverse(record.since));
    let mut reviews = reviews.into_iter();
    let (newest, verdict) = reviews.next()?;
    let mut verdicts = vec![verdict.get("verdict")?.clone()];
    let Some(step) = newest.started.workflow_step.as_ref() else {
        return Some(verdicts.len());
    };
    let round_since = state
        .workflows
        .progress(step)
        .map_or(0, |round| round.since);
    for (record, answer) in reviews {
        if record.started.workflow_step.as_ref() != Some(step) || record.since < round_since {
            continue;
        }
        if let Some(verdict) = answer.get("verdict")
            && !verdicts.contains(verdict)
        {
            verdicts.push(verdict.clone());
        }
    }
    Some(verdicts.len())
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use argonauts_core::events::session::EndReason;
    use argonauts_core::names::{ModelRef, WorkflowId};
    use argonauts_core::workflow::{StandardPhase, StepKind, StepRef};

    use super::*;
    use crate::state::tests::{end, session_record};
    use crate::state::workflow::{StepProgress, WorkflowRecord};

    fn review_step() -> StepRef {
        StepRef {
            workflow: WorkflowId::numbered(1),
            phase: 0,
            path: vec![0, 0],
        }
    }

    fn round_started(state: &mut RunState, round: u32, since: u64) {
        let id = WorkflowId::numbered(1);
        let record = state
            .workflows
            .by_id
            .entry(id.clone())
            .or_insert_with(|| WorkflowRecord {
                id,
                from: None,
                version: "v1".to_owned(),
                parent: None,
                workflow: crate::workflow::builtin::load("captain").unwrap(),
                steps: Vec::new(),
                finished: false,
                changed_from_phase: None,
                since: 0,
                changes: Vec::new(),
                declared: None,
                phases_started: Default::default(),
                phases_ended: Default::default(),
                phases_skipped: Default::default(),
                left: Vec::new(),
            });
        record.steps.push(StepProgress {
            step: review_step(),
            kind: StepKind::Review,
            standard: StandardPhase::Check,
            round,
            since,
            end: None,
            summary: None,
            ended_at: None,
        });
    }

    fn reviewed(state: &mut RunState, key: &str, since: u64, verdict: &str) {
        let worker = ModelRef::new("codex", None);
        let mut record = session_record(key, &worker, RoleName::Reviewer);
        record.since = since;
        record.started.workflow_step = Some(review_step());
        let mut ended = end(EndReason::Completed, None);
        ended.structured = Some(json!({ "verdict": verdict }));
        record.end = Some(ended);
        state.sessions.by_key.insert(record.key.clone(), record);
    }

    fn holds_now(state: &RunState, condition: &str) -> bool {
        let at = StepAt {
            step: review_step(),
            round: 1,
        };
        holds(state, &Settings::default(), condition, &at).unwrap()
    }

    #[test]
    fn reviewer_verdicts_count_only_the_newest_round() {
        let mut state = RunState::default();
        round_started(&mut state, 1, 10);
        reviewed(&mut state, "s-001", 11, "approve");
        reviewed(&mut state, "s-002", 12, "request_changes");
        assert!(holds_now(&state, "reviewers_disagree"));

        // The next item's round: its one verdict agrees with itself.
        round_started(&mut state, 2, 20);
        reviewed(&mut state, "s-003", 21, "approve");
        assert!(holds_now(&state, "reviewers_agree"));
        assert!(!holds_now(&state, "reviewers_disagree"));
    }
}
