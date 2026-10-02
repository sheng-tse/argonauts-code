//! Solo: one card for the default agent. The prompt is the goal (`GoalSource::Prompt`) unless
//! `solo.restate_first`; `solo.attempts` above 1 runs independent attempts and keeps one that
//! passes the visible gates; the triage's `escalate_to_crew` switches to the crew if the card
//! escalates.

use std::collections::BTreeMap;

use argonauts_core::CardId;
use argonauts_core::events::card::{CardBudget, CardKind, CardSpec, CardState};
use argonauts_core::events::goal::GoalFile;
use argonauts_core::events::run::{Party, RunMode};
use argonauts_core::names::{RoleName, WorkerRef};
use argonauts_core::settings::protocol::ForcedMode;

use crate::engine::host::Host;
use crate::error::{EngineError, EngineResult};
use crate::goal::{checkpoint, draft, round};
use crate::roles;
use crate::state::RunState;
use crate::state::cards::CardRecord;

/// Records the prompt as the approved goal and writes it as a conductor commit. An approved
/// goal is returned as it is, so every caller may ask. With `solo.restate_first` the goal waits
/// for your approval instead.
pub async fn goal_from_prompt(host: &Host) -> EngineResult<GoalFile> {
    if let Some(approved) = host.read(|state| state.goal.approved.clone()) {
        return Ok(approved);
    }
    if host.settings().solo.restate_first {
        return Err(EngineError::Refused(
            "solo.restate_first is on, so the goal waits for your approval".to_owned(),
        ));
    }
    let request = host.read(round::request)?;
    round::begin(host)?;
    let goal = draft::from_prompt(host.run_id(), &request);
    let goal = round::write_draft(host, goal).await?;
    // The setting made your prompt the goal, so the approval is the configuration's.
    checkpoint::approve(host, goal.clone(), Party::Config)?;
    Ok(goal)
}

/// The one `Solo` card, owned by the default agent, serving every goal criterion. With
/// `solo.review` its reviewer comes from another model family than the owner.
pub fn card(state: &RunState, host: &Host) -> EngineResult<CardSpec> {
    let started = state
        .run
        .started
        .as_ref()
        .ok_or_else(|| EngineError::Refused("the run hasn't started".to_owned()))?;
    let settings = host.settings();
    let owner = settings.default_agent.clone();
    let reviewers = if settings.solo.review {
        vec![reviewer(host, &owner)?]
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
    Ok(CardSpec {
        id: CardId::numbered(1),
        title: started.title.clone(),
        task: started.request.clone(),
        kind: CardKind::Solo,
        owned_paths: Vec::new(),
        read_only_paths: Vec::new(),
        forbidden_paths: Vec::new(),
        hidden_paths: Vec::new(),
        depends_on: Vec::new(),
        criteria,
        owner: Some(owner),
        reviewers,
        budget: CardBudget::default(),
        predictions: None,
    })
}

/// The first reviewer candidate (the reviewer role's, else every available worker) from another
/// family than the owner, with no role conflict.
fn reviewer(host: &Host, owner: &WorkerRef) -> EngineResult<WorkerRef> {
    let family = |worker: &WorkerRef| roles::family(host, worker);
    let owner_family = family(owner);
    let mut pool = roles::candidates(host.settings(), RoleName::Reviewer);
    pool.extend(host.services().workers.available());
    pool.into_iter()
        .find(|worker| {
            worker != owner
                && family(worker) != owner_family
                && roles::conflict(RoleName::Reviewer, worker, Some(owner), &family).is_none()
        })
        .ok_or_else(|| {
            EngineError::Refused(format!(
                "solo.review needs a reviewer from another model family than {owner} \
                 ({owner_family}), and none is available"
            ))
        })
}

/// Best of n: the attempt to keep among those that passed the visible gates. The one that
/// needed the fewest tries wins, then the earliest.
pub fn pick_attempt(state: &RunState, attempts: &[CardId]) -> Option<CardId> {
    attempts
        .iter()
        .enumerate()
        .filter_map(|(order, id)| {
            let record = state.cards.get(id)?;
            passed_visible_gates(record).then_some((record.attempts, order, id))
        })
        .min_by_key(|(tries, order, _)| (*tries, *order))
        .map(|(_, _, id)| id.clone())
}

/// Every gate the card ran passed on its newest run, and the card is still in play.
fn passed_visible_gates(record: &CardRecord) -> bool {
    let out_of_play = matches!(
        record.state,
        CardState::Failed | CardState::Escalated | CardState::Cancelled | CardState::Deferred
    );
    if out_of_play || record.gates.is_empty() {
        return false;
    }
    let mut newest = BTreeMap::new();
    for gate in &record.gates {
        newest.insert(gate.name.as_str(), gate.passed);
    }
    newest.values().all(|passed| *passed)
}

/// Whether the solo card escalated and triage chose solo first, then crew. A forced solo never
/// escalates to the crew.
pub fn escalates_to_crew(state: &RunState, host: &Host) -> bool {
    let solo_first = state
        .run
        .started
        .as_ref()
        .and_then(|started| started.triage.as_ref())
        .is_some_and(|triage| triage.escalate_to_crew);
    let forced_solo = host.settings().triage.force == Some(ForcedMode::Solo);
    let escalated =
        state.cards.by_id.values().any(|record| {
            record.spec.kind == CardKind::Solo && record.state == CardState::Escalated
        });
    round::mode(state) == RunMode::Solo && solo_first && !forced_solo && escalated
}

#[cfg(test)]
mod tests {
    use argonauts_core::events::card::{GateKind, GateOutcome};

    use super::*;

    fn gate(name: &str, passed: bool) -> GateOutcome {
        GateOutcome {
            name: name.to_owned(),
            kind: GateKind::Command,
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

    fn record(n: u32, state: CardState, tries: u32, gates: Vec<GateOutcome>) -> CardRecord {
        CardRecord {
            spec: CardSpec {
                id: CardId::numbered(n),
                title: format!("attempt {n}"),
                task: "t".to_owned(),
                kind: CardKind::Solo,
                owned_paths: Vec::new(),
                read_only_paths: Vec::new(),
                forbidden_paths: Vec::new(),
                hidden_paths: Vec::new(),
                depends_on: Vec::new(),
                criteria: Vec::new(),
                owner: None,
                reviewers: Vec::new(),
                budget: CardBudget::default(),
                predictions: None,
            },
            state,
            attempts: tries,
            owner: None,
            reassigned: 0,
            worktree: None,
            branch: None,
            base: None,
            commits: Vec::new(),
            reports: Vec::new(),
            gates,
            reviews: Vec::new(),
            handoffs: 0,
            debt: None,
        }
    }

    fn state(records: Vec<CardRecord>) -> RunState {
        let mut state = RunState::default();
        for record in records {
            state.cards.by_id.insert(record.spec.id.clone(), record);
        }
        state
    }

    #[test]
    fn best_of_n_keeps_an_attempt_that_passed_the_gates() {
        let state = state(vec![
            record(1, CardState::InReview, 1, vec![gate("test", false)]),
            record(
                2,
                CardState::InReview,
                2,
                vec![gate("test", false), gate("test", true)],
            ),
            record(
                3,
                CardState::InReview,
                1,
                vec![gate("test", true), gate("lint", true)],
            ),
            record(4, CardState::Failed, 1, vec![gate("test", true)]),
            record(5, CardState::InReview, 1, Vec::new()),
        ]);
        let ids: Vec<CardId> = (1..=5).map(CardId::numbered).collect();
        assert_eq!(pick_attempt(&state, &ids), Some(CardId::numbered(3)));
        assert_eq!(pick_attempt(&state, &ids[..2]), Some(CardId::numbered(2)));
        assert_eq!(
            pick_attempt(&state, &[ids[0].clone(), ids[3].clone()]),
            None
        );
        assert_eq!(pick_attempt(&state, &[CardId::numbered(9)]), None);
    }
}
