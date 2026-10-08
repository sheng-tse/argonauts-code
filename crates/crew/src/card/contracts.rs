//! Contracts first, inside `build`: the interface and acceptance-test cards run first, as the
//! `Contracts` phase. Once every test card is integrated, a goal criterion no test card covers
//! holds the build cards back and escalates to you.

use argonauts_core::CardId;
use argonauts_core::PhaseStatus;
use argonauts_core::events::RunEventKind;
use argonauts_core::events::card::{CardKind, CardState};
use argonauts_core::events::checkpoint::{
    AnswerAction, CheckpointKind, CheckpointPacket, EscalationPacket, FreeText,
};
use argonauts_core::events::run::{PhaseChange, RunPhase};
use argonauts_core::policy::EvidenceFacts;

use crate::checkpoints::{self, CheckpointRequest};
use crate::divide;
use crate::engine::host::Host;
use crate::error::EngineResult;
use crate::state::RunState;
use crate::workflow::StepAt;

fn is_contract(kind: &CardKind) -> bool {
    matches!(kind, CardKind::Interface | CardKind::AcceptanceTests { .. })
}

/// Goal criteria no acceptance-test card covers, once every test card is integrated. Empty
/// when the run has no test cards.
pub(crate) fn uncovered(state: &RunState) -> Vec<String> {
    let tests: Vec<_> = state
        .cards
        .by_id
        .values()
        .filter(|r| matches!(r.spec.kind, CardKind::AcceptanceTests { .. }))
        .filter(|r| r.state != CardState::Cancelled)
        .collect();
    if tests.is_empty() || tests.iter().any(|t| t.state != CardState::Integrated) {
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

/// A build card whose dependencies are in, and so would start, if coverage allowed.
pub(crate) fn held_build_card(state: &RunState, ready: &[CardId]) -> Option<CardId> {
    ready
        .iter()
        .find(|id| {
            state
                .cards
                .get(id)
                .is_some_and(|r| r.spec.kind == CardKind::Build)
        })
        .cloned()
}

/// Asks you about the criteria no test covers. True when the build goes on anyway.
pub(crate) async fn criteria_gate(
    host: &Host,
    at: &StepAt,
    card: &CardId,
    uncovered: &[String],
) -> EngineResult<bool> {
    let list = uncovered.join(", ");
    let packet = EscalationPacket {
        card: card.clone(),
        title: format!("Goal criteria no acceptance test covers: {list}"),
        state: CardState::Waiting,
        attempts: Vec::new(),
        last_failure: format!("no acceptance test covers {list}, so the build tasks wait"),
    };
    let request = CheckpointRequest {
        at: Some(at.clone()),
        kind: CheckpointKind::Escalation,
        title: format!("Untested goal criteria · {list}"),
        card: Some(card.clone()),
        packet: CheckpointPacket::Escalation(packet),
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
    let decided = checkpoints::open_and_wait(host, request).await?;
    Ok(!matches!(
        decided.action,
        AnswerAction::Cancel | AnswerAction::Stop | AnswerAction::Abandon
    ))
}

/// Which phase the build is in: `Contracts` while interface or test cards remain, then
/// `Build`.
pub(crate) fn phase_now(state: &RunState) -> RunPhase {
    let open_contract = state.cards.by_id.values().any(|r| {
        is_contract(&r.spec.kind)
            && !matches!(r.state, CardState::Integrated | CardState::Cancelled)
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
    use crate::state::cards::CardRecord;
    use crate::testutil::card_spec;

    fn add(
        state: &mut RunState,
        id: &str,
        kind: CardKind,
        card_state: CardState,
        criteria: &[&str],
    ) {
        let mut spec = card_spec(id, &[], None);
        spec.kind = kind;
        spec.criteria = criteria.iter().map(|c| (*c).to_owned()).collect();
        let mut record = CardRecord::new(spec);
        record.state = card_state;
        state.cards.by_id.insert(record.spec.id.clone(), record);
    }

    #[test]
    fn contracts_come_first_then_build() {
        let mut state = RunState::default();
        add(
            &mut state,
            "task-001",
            CardKind::Build,
            CardState::Waiting,
            &[],
        );
        assert_eq!(phase_now(&state), RunPhase::Build);
        let for_card = CardId::new("task-001").unwrap();
        add(
            &mut state,
            "test-001",
            CardKind::AcceptanceTests { for_card },
            CardState::Running,
            &["G1"],
        );
        assert_eq!(phase_now(&state), RunPhase::Contracts);
        state
            .cards
            .by_id
            .get_mut(&CardId::new("test-001").unwrap())
            .unwrap()
            .state = CardState::Integrated;
        assert_eq!(phase_now(&state), RunPhase::Build);
    }

    #[test]
    fn coverage_waits_for_every_test_card() {
        let mut state = RunState::default();
        assert!(
            uncovered(&state).is_empty(),
            "no test tasks, nothing to check"
        );
        let for_card = CardId::new("task-001").unwrap();
        add(
            &mut state,
            "test-001",
            CardKind::AcceptanceTests { for_card },
            CardState::Running,
            &["G1"],
        );
        assert!(uncovered(&state).is_empty(), "not merged into the run yet");
        let ready = [CardId::new("task-001").unwrap()];
        add(
            &mut state,
            "task-001",
            CardKind::Build,
            CardState::Waiting,
            &[],
        );
        assert_eq!(held_build_card(&state, &ready), Some(ready[0].clone()));
    }
}
