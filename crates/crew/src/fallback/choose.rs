//! Who takes a stopped task. Your pick for the task (on its fallback question) comes first; the
//! role and `[fallback]` lists come from the planner's shortlist, and with no list set, the other
//! installed CLIs in [`defaults`](crate::fallback::defaults)' order. With `spread`, the stopped
//! agent's tasks go to whichever valid candidate holds the fewest tasks.

use argonauts_core::TaskId;
use argonauts_core::events::approval::{AnswerAction, ApprovalKind};
use argonauts_core::events::run::Party;
use argonauts_core::events::task::TaskState;
use argonauts_core::limits::{Candidate, CandidateQuery};
use argonauts_core::names::{ModelRef, RoleName};
use argonauts_core::settings::fallback::FallbackStrategy;

use crate::engine::host::Host;
use crate::fallback::{defaults, rules};
use crate::state::RunState;
use crate::state::tasks::TaskEntry;

/// The candidate you chose on this task's latest fallback question.
pub fn your_pick(state: &RunState, task: &TaskId) -> Option<ModelRef> {
    let approval = state
        .approvals
        .last_decided(ApprovalKind::Fallback, Some(task))?;
    let decided = approval.decided.as_ref()?;
    match (&decided.action, &decided.by) {
        (AnswerAction::Candidate { worker }, Party::User) => Some(worker.clone()),
        _ => None,
    }
}

/// The query the planner answers for this task, with the stopped worker.
pub fn query(
    host: &Host,
    record: &TaskEntry,
    stopped: &ModelRef,
    all_valid: bool,
) -> CandidateQuery {
    let rules = rules::applied_for(host, &record.spec, stopped);
    let fallback = if rules.candidates.is_empty() {
        defaults::candidates(host, &record.spec, stopped)
    } else {
        rules.candidates
    };
    let exclude_providers = record
        .spec
        .reviewers
        .iter()
        .map(|reviewer| host.services().registry.provider(reviewer))
        .collect();
    CandidateQuery {
        task: record.spec.clone(),
        role: RoleName::Worker,
        stopped: stopped.clone(),
        exclude_providers,
        fallback,
        all_valid,
    }
}

/// The new worker: your pick when it's usable, else the one proposed from the lists, balanced
/// across candidates when the strategy is `spread`.
pub fn choose(
    host: &Host,
    record: &TaskEntry,
    stopped: Option<&ModelRef>,
    proposed: ModelRef,
) -> ModelRef {
    let task = &record.spec.id;
    if let Some(pick) = host.read(|state| your_pick(state, task))
        && Some(&pick) != stopped
        && rules::agent_usable(host, &pick.agent_id())
    {
        return pick;
    }
    let Some(stopped) = stopped else {
        return proposed;
    };
    let shortlist = host
        .services()
        .fallback
        .shortlist(&query(host, record, stopped, false));
    if shortlist.strategy != FallbackStrategy::Spread {
        // The proposal came from the lists the settings name. With none, the defaults' first
        // valid CLI that needs no OK of yours stands.
        if shortlist.strategy == FallbackStrategy::First
            && defaults::apply(host, &record.spec, stopped)
            && let Some(first) = shortlist
                .candidates
                .iter()
                .find(|candidate| !candidate.needs_confirm && candidate.worker != *stopped)
        {
            return first.worker.clone();
        }
        return proposed;
    }
    let loads: Vec<usize> = host.read(|state| {
        shortlist
            .candidates
            .iter()
            .map(|candidate| tasks_held(state, &candidate.worker))
            .collect()
    });
    spread(&shortlist.candidates, &loads, &proposed).unwrap_or(proposed)
}

/// Tasks a worker owns that aren't finished.
pub fn tasks_held(state: &RunState, worker: &ModelRef) -> usize {
    state
        .tasks
        .by_id
        .values()
        .filter(|task| !matches!(task.state, TaskState::Merged | TaskState::Cancelled))
        .filter(|task| rules::worker(task).as_ref() == Some(worker))
        .count()
}

/// The candidate holding the fewest tasks. Ties keep the proposed one, then the planner's
/// order, which already weighs plan usage and track record.
pub fn spread(candidates: &[Candidate], loads: &[usize], proposed: &ModelRef) -> Option<ModelRef> {
    candidates
        .iter()
        .zip(loads)
        .enumerate()
        .min_by_key(|(index, (candidate, load))| (**load, &candidate.worker != proposed, *index))
        .map(|(_, (candidate, _))| candidate.worker.clone())
}

#[cfg(test)]
mod tests {
    use super::*;
    use argonauts_core::limits::CostEstimate;

    fn candidate(worker: &str) -> Candidate {
        Candidate {
            worker: worker.parse().unwrap(),
            plan_usage_left: None,
            plan_usage_text: String::new(),
            track_record: None,
            cost: CostEstimate::Unknown,
            needs_confirm: false,
            may_run_out: false,
        }
    }

    #[test]
    fn spread_goes_to_the_least_loaded_candidate() {
        let candidates = [candidate("codex/gpt-5.6"), candidate("agy/gemini-3.6")];
        let proposed: ModelRef = "codex/gpt-5.6".parse().unwrap();
        assert_eq!(
            spread(&candidates, &[0, 0], &proposed),
            Some(proposed.clone())
        );
        assert_eq!(
            spread(&candidates, &[1, 0], &proposed),
            Some("agy/gemini-3.6".parse().unwrap())
        );
        let other: ModelRef = "agy/gemini-3.6".parse().unwrap();
        assert_eq!(spread(&candidates, &[2, 2], &other), Some(other));
        assert_eq!(spread(&[], &[], &proposed), None);
    }
}
