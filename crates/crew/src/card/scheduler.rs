//! Which cards run next: Waiting cards whose dependencies are all integrated become Ready; Ready
//! cards start while `concurrency` and the budget allow and their owner is free.

use std::collections::{BTreeMap, BTreeSet};

use tokio::task::JoinSet;

use argonauts_core::CardId;
use argonauts_core::PhaseStatus;
use argonauts_core::events::card::{CardKind, CardState};
use argonauts_core::events::run::{RunMode, RunPhase};
use argonauts_core::events::workflow::StepEnd;
use argonauts_core::names::WorkerRef;
use argonauts_core::settings::Settings;
use argonauts_core::settings::budget::BudgetUnit;

use crate::card::contracts;
use crate::card::machine::{self, Trigger};
use crate::card::pipeline::{self, Until, dependencies_done, transition};
use crate::card::support::changed_since;
use crate::engine::host::Host;
use crate::error::{EngineError, EngineResult};
use crate::solo;
use crate::state::RunState;
use crate::state::cards::CardRecord;
use crate::workflow::{Flow, StepAt, StepCall, StepDone, runner};

pub fn unblocked(state: &RunState) -> Vec<CardId> {
    state
        .cards
        .by_id
        .values()
        .filter(|record| record.state == CardState::Waiting)
        .map(|record| record.spec.id.clone())
        .filter(|card| dependencies_done(state, card))
        .collect()
}

/// Ready cards that may start now, given how many are running.
pub fn startable(state: &RunState, settings: &Settings, running: usize) -> Vec<CardId> {
    if budget_low(state) {
        return Vec::new();
    }
    let owner = |record: &CardRecord| -> WorkerRef {
        record
            .owner
            .clone()
            .or_else(|| record.spec.owner.clone())
            .unwrap_or_else(|| settings.main_agent.clone())
    };
    let active: Vec<&CardRecord> = state
        .cards
        .by_id
        .values()
        .filter(|record| record.state == CardState::Running)
        .collect();
    let mut busy: BTreeSet<WorkerRef> = active.iter().map(|record| owner(record)).collect();
    let mut per_vendor: BTreeMap<String, u32> = BTreeMap::new();
    for record in &active {
        *per_vendor.entry(owner(record).worker).or_default() += 1;
    }
    let max_sessions = usize::try_from(settings.concurrency.max_sessions).unwrap_or(usize::MAX);
    let mut slots = max_sessions.saturating_sub(running);
    let mut picked = Vec::new();
    for record in state.cards.by_id.values() {
        if slots == 0 {
            break;
        }
        if record.state != CardState::Ready {
            continue;
        }
        let owner = owner(record);
        let vendor_count = per_vendor.entry(owner.worker.clone()).or_default();
        if busy.contains(&owner) || *vendor_count >= settings.concurrency.max_per_vendor {
            continue;
        }
        *vendor_count += 1;
        busy.insert(owner);
        slots -= 1;
        picked.push(record.spec.id.clone());
    }
    picked
}

/// The run's budget is spent and its reserve with it, so cards that haven't started wait.
pub(crate) fn budget_low(state: &RunState) -> bool {
    let Some(budget) = &state.run.budget else {
        return false;
    };
    let Some(limit) = &budget.limit else {
        return false;
    };
    let spent = match limit.unit {
        BudgetUnit::Usd => budget.spent.cost_usd,
        BudgetUnit::Tokens => budget.spent.billable_tokens() as f64,
        BudgetUnit::Quota | BudgetUnit::Minutes => return false,
    };
    let reserve = budget.reserve_left.as_ref().map_or(0.0, |r| r.amount);
    spent >= limit.amount && reserve <= 0.0
}

/// Runs the cards until every one is integrated, cancelled, or deferred, or nothing can move
/// without you.
pub async fn run_cards(host: &Host, call: &StepCall) -> EngineResult<StepDone> {
    let at = call.at.clone();
    let mut tasks: JoinSet<(CardId, EngineResult<CardState>)> = JoinSet::new();
    let mut active: BTreeSet<CardId> = BTreeSet::new();
    // Cards whose task returned waiting, with their change count then; they run again once
    // anything about them changes.
    let mut idle: BTreeMap<CardId, u64> = BTreeMap::new();
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
        for card in cards_to_drive(host, &active, &idle) {
            idle.remove(&card);
            active.insert(card.clone());
            let (host, at) = (host.clone(), at.clone());
            tasks.spawn(async move {
                let state = pipeline::drive(&host, &card, &at, Until::Integrated).await;
                (card, state)
            });
        }
        let seq = host.read(|state| state.seq);
        if tasks.is_empty() {
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
            joined = tasks.join_next() => {
                let Some(joined) = joined else { continue };
                let (card, result) = joined.map_err(|err| {
                    EngineError::Refused(format!("a task stopped: {err}"))
                })?;
                active.remove(&card);
                let state = result?;
                if !machine::is_final(state) {
                    let changes = host.read(|s| s.cards.progress(&card).changes);
                    idle.insert(card, changes);
                }
            }
            changed = changed_since(host, seq) => changed?,
        }
    }
}

/// Moves that need no session: dependencies in, budget out or back, and the coverage check.
/// `Some` when the step ends here.
async fn settle_without_sessions(
    host: &Host,
    at: &StepAt,
    coverage_accepted: &mut bool,
) -> EngineResult<Option<StepDone>> {
    let ready = host.read(unblocked);
    let uncovered = host.read(contracts::uncovered);
    let held = host.read(|state| contracts::held_build_card(state, &ready));
    let hold_builds = !uncovered.is_empty() && !*coverage_accepted;
    if let (true, Some(card)) = (hold_builds, held) {
        if !contracts::criteria_gate(host, at, &card, &uncovered).await? {
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
    for card in ready {
        transition(
            host,
            &card,
            Trigger::DependenciesIntegrated,
            "its dependencies are merged into the run",
        )?;
    }
    let (low, deferred, ready_now) = host.read(|state| {
        (
            budget_low(state),
            state
                .cards
                .by_id
                .values()
                .filter(|r| {
                    r.state == CardState::Deferred
                        && state.cards.progress(&r.spec.id).deferred_for_budget
                })
                .map(|r| r.spec.id.clone())
                .collect::<Vec<_>>(),
            state.cards.in_state(CardState::Ready),
        )
    });
    if low {
        for card in ready_now {
            transition(host, &card, Trigger::BudgetLow, "the run's limit is low")?;
        }
    } else {
        for card in deferred {
            transition(host, &card, Trigger::MoreBudget, "the run has limit again")?;
        }
    }
    Ok(None)
}

/// Cards to start now, and cards to drive again after a restart or after something outside
/// moved them (an answer, a hand-back, a vendor coming back).
fn cards_to_drive(
    host: &Host,
    active: &BTreeSet<CardId>,
    idle: &BTreeMap<CardId, u64>,
) -> Vec<CardId> {
    host.read(|state| {
        let running = state.cards.in_state(CardState::Running).len()
            + active
                .iter()
                .filter(|card| state.cards.get(card).map(|r| r.state) == Some(CardState::Ready))
                .count();
        let mut cards: Vec<CardId> = startable(state, host.settings(), running)
            .into_iter()
            .filter(|card| !active.contains(card))
            .collect();
        for record in state.cards.by_id.values() {
            let card = &record.spec.id;
            let resumable = matches!(
                record.state,
                CardState::Running
                    | CardState::Gating
                    | CardState::InReview
                    | CardState::Integrating
                    | CardState::Failed
                    | CardState::Escalated
                    | CardState::NeedsDecision
                    | CardState::TakenOver
            );
            let unchanged = idle.get(card) == Some(&state.cards.progress(card).changes);
            if resumable && !unchanged && !active.contains(card) && !cards.contains(card) {
                cards.push(card.clone());
            }
        }
        cards
    })
}

/// Cards the step ends without: deferred ones, and waiting ones that depend on a card left
/// out, directly or through others, since they can't start until it comes back.
pub(crate) fn left_out(state: &RunState) -> BTreeSet<CardId> {
    let mut out: BTreeSet<CardId> = state
        .cards
        .in_state(CardState::Deferred)
        .into_iter()
        .collect();
    loop {
        let held: Vec<CardId> = state
            .cards
            .by_id
            .values()
            .filter(|record| record.state == CardState::Waiting && !out.contains(&record.spec.id))
            .filter(|record| record.spec.depends_on.iter().any(|dep| out.contains(dep)))
            .map(|record| record.spec.id.clone())
            .collect();
        if held.is_empty() {
            return out;
        }
        out.extend(held);
    }
}

/// The step's end once no card can move: every card integrated, cancelled, or left out; or,
/// in solo, the card escalated with `escalate_to_crew` set.
fn finished(host: &Host) -> Option<StepDone> {
    let snapshot = host.read(Clone::clone);
    let solo = snapshot.run.mode == Some(RunMode::Solo);
    let escalated = !snapshot.cards.in_state(CardState::Escalated).is_empty();
    if solo && escalated && solo::escalates_to_crew(&snapshot, host) {
        return Some(StepDone {
            end: StepEnd::Done,
            summary: Some("the solo task needed you, so the crew takes over".to_owned()),
            flow: Flow::Switch(runner::crew_workflow(host)),
        });
    }
    let left_out = left_out(&snapshot);
    let settled = snapshot
        .cards
        .by_id
        .values()
        .all(|record| machine::is_final(record.state) || left_out.contains(&record.spec.id));
    settled.then(|| StepDone::done(summary(&snapshot)))
}

/// Such as "3 cards integrated, 1 deferred, 2 waiting on deferred cards".
pub(crate) fn summary(state: &RunState) -> String {
    let count = |wanted: CardState| {
        state
            .cards
            .by_id
            .values()
            .filter(|r| r.state == wanted && !matches!(r.spec.kind, CardKind::Spike { .. }))
            .count()
    };
    let integrated = count(CardState::Integrated);
    let mut text = match integrated {
        1 => "1 task merged into the run".to_owned(),
        n => format!("{n} tasks merged into the run"),
    };
    for (state, word) in [
        (CardState::Deferred, "deferred"),
        (CardState::Cancelled, "cancelled"),
    ] {
        let n = count(state);
        if n > 0 {
            text.push_str(&format!(", {n} {word}"));
        }
    }
    let held = left_out(state)
        .iter()
        .filter(|card| state.cards.get(card).map(|r| r.state) == Some(CardState::Waiting))
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
    use argonauts_core::events::vendor::RunBudget;
    use argonauts_core::settings::budget::BudgetAmount;

    use crate::testutil::card_spec;

    fn state_with(cards: &[(&str, &[&str], Option<&str>, CardState)]) -> RunState {
        let mut state = RunState::default();
        for (id, deps, owner, card_state) in cards {
            let mut record = CardRecord::new(card_spec(id, deps, *owner));
            record.state = *card_state;
            state.cards.by_id.insert(record.spec.id.clone(), record);
        }
        state
    }

    fn ids(cards: &[CardId]) -> Vec<&str> {
        cards.iter().map(CardId::as_str).collect()
    }

    #[test]
    fn waiting_cards_unblock_once_their_dependencies_are_in() {
        let state = state_with(&[
            ("task-001", &[], None, CardState::Integrated),
            ("task-002", &["task-001"], None, CardState::Waiting),
            ("task-003", &["task-002"], None, CardState::Waiting),
            ("task-004", &[], None, CardState::Ready),
        ]);
        assert_eq!(ids(&unblocked(&state)), ["task-002"]);
    }

    #[test]
    fn a_busy_owner_or_a_full_vendor_waits() {
        let mut settings = Settings::default();
        settings.concurrency.max_sessions = 6;
        settings.concurrency.max_per_vendor = 1;
        let state = state_with(&[
            (
                "task-001",
                &[],
                Some("claude-code/opus"),
                CardState::Running,
            ),
            ("task-002", &[], Some("claude-code/opus"), CardState::Ready),
            (
                "task-003",
                &[],
                Some("claude-code/sonnet"),
                CardState::Ready,
            ),
            ("task-004", &[], Some("codex"), CardState::Ready),
            ("task-005", &[], Some("codex"), CardState::Ready),
            ("task-006", &[], Some("agy"), CardState::Ready),
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
            ("task-001", &[], Some("codex"), CardState::Ready),
            ("task-002", &[], Some("agy"), CardState::Ready),
            ("task-003", &[], Some("claude-code"), CardState::Ready),
        ]);
        assert_eq!(
            ids(&startable(&state, &settings, 0)),
            ["task-001", "task-002"]
        );
        assert!(startable(&state, &settings, 2).is_empty());
    }

    #[test]
    fn a_spent_budget_holds_every_start() {
        let settings = Settings::default();
        let mut state = state_with(&[("task-001", &[], Some("codex"), CardState::Ready)]);
        let limit = BudgetAmount {
            unit: BudgetUnit::Usd,
            amount: 5.0,
        };
        state.run.budget = Some(RunBudget {
            spent: Spend {
                cost_usd: 5.5,
                ..Spend::default()
            },
            limit: Some(limit),
            lines: Vec::new(),
            reserve_left: None,
        });
        assert!(budget_low(&state));
        assert!(startable(&state, &settings, 0).is_empty());
        state.run.budget.as_mut().unwrap().reserve_left = Some(BudgetAmount {
            unit: BudgetUnit::Usd,
            amount: 1.0,
        });
        assert!(!budget_low(&state), "the reserve covers it");
    }

    #[test]
    fn the_summary_counts_integrated_deferred_and_cancelled() {
        let state = state_with(&[
            ("task-001", &[], None, CardState::Integrated),
            ("task-002", &[], None, CardState::Integrated),
            ("task-003", &[], None, CardState::Deferred),
        ]);
        assert_eq!(summary(&state), "2 tasks merged into the run, 1 deferred");
    }

    #[test]
    fn waiting_on_a_deferred_card_leaves_a_card_out() {
        let state = state_with(&[
            ("task-001", &[], None, CardState::Integrated),
            ("task-002", &[], None, CardState::Deferred),
            ("task-003", &["task-002"], None, CardState::Waiting),
            (
                "task-004",
                &["task-003", "task-001"],
                None,
                CardState::Waiting,
            ),
            ("task-005", &["task-001"], None, CardState::Waiting),
        ]);
        assert_eq!(
            left_out(&state)
                .iter()
                .map(CardId::as_str)
                .collect::<Vec<_>>(),
            ["task-002", "task-003", "task-004"]
        );
        let settled = state_with(&[
            ("task-001", &[], None, CardState::Integrated),
            ("task-002", &[], None, CardState::Deferred),
            ("task-003", &["task-002"], None, CardState::Waiting),
        ]);
        assert_eq!(
            summary(&settled),
            "1 task merged into the run, 1 deferred, 1 waiting on deferred tasks"
        );
    }
}
