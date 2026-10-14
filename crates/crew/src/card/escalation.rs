//! Decisions a card waits on: an escalated card (retry, a new owner, take over, defer, split,
//! or drop), and a card stopped for a decision because you stopped its session or its owner
//! asked for one. Every answer moves the card, so a decided checkpoint is never asked again.

use std::collections::BTreeSet;

use argonauts_core::CardId;
use argonauts_core::events::card::{AttemptCause, CardEvent, CardSpec, CardState};
use argonauts_core::events::checkpoint::{
    AnswerAction, CheckpointDecided, CheckpointKind, CheckpointPacket, EscalationPacket, FreeText,
    TakeOverReturnPacket,
};
use argonauts_core::events::run::Party;
use argonauts_core::events::session::InputPart;
use argonauts_core::names::{RoleName, WorkerRef};
use argonauts_core::policy::{EscalationFacts, EvidenceFacts};
use argonauts_core::steps::StepName;
use argonauts_core::steps::plan::{GoalBrief, PlanDraft, PlanToCards, PlanToCardsInput};

use crate::card::budget;
use crate::card::machine::Trigger;
use crate::card::pipeline::transition;
use crate::card::prompt;
use crate::card::recovery;
use crate::card::session::Next;
use crate::card::support::{owner_of, progress_of, record_card, record_of, wait_until};
use crate::checkpoints::{self, CheckpointRequest};
use crate::divide::graph;
use crate::engine::host::{Ask, Host};
use crate::error::EngineResult;
use crate::handoff::rules;
use crate::lead;
use crate::prompts;
use crate::roles;
use crate::state::RunState;
use crate::state::cards::CardRecord;
use crate::workflow::StepAt;

/// What a decision did to the card.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Decision {
    /// It moved; a following session starts as `next` says when given.
    Moved(Option<Next>),
    /// It moved while the checkpoint was open (you reassigned it or took it over).
    MovedMeanwhile,
}

/// Opens the escalation checkpoint for a card in `from` (`Escalated`, or `NeedsDecision` for
/// an escalation-style stop) and applies your answer.
pub(crate) async fn decide(
    host: &Host,
    card: &CardId,
    at: Option<&StepAt>,
    from: CardState,
    why: &str,
) -> EngineResult<Decision> {
    let record = record_of(host, card)?;
    let request = request(host, &record, at, from, why);
    let Some(decided) = wait_decision(host, card, from, request).await? else {
        return Ok(Decision::MovedMeanwhile);
    };
    apply_answer(host, card, at, from, &decided).await
}

/// The checkpoint's decision, or `None` when the card left `from` first.
pub(crate) async fn wait_decision(
    host: &Host,
    card: &CardId,
    from: CardState,
    request: CheckpointRequest,
) -> EngineResult<Option<CheckpointDecided>> {
    let moved = |state: &RunState| state.cards.get(card).map(|r| r.state) != Some(from);
    tokio::select! {
        decided = checkpoints::open_and_wait(host, request) => decided.map(Some),
        moved = wait_until(host, moved) => moved.map(|()| None),
    }
}

fn request(
    host: &Host,
    record: &CardRecord,
    at: Option<&StepAt>,
    from: CardState,
    why: &str,
) -> CheckpointRequest {
    let card = record.spec.id.clone();
    let title = match from {
        CardState::Escalated => format!("{card} needs you · {}", record.spec.title),
        _ => format!("{card} needs a decision · {}", record.spec.title),
    };
    let packet = EscalationPacket {
        card: card.clone(),
        title: record.spec.title.clone(),
        state: from,
        attempts: recovery::attempt_lines(record),
        last_failure: why.to_owned(),
    };
    CheckpointRequest {
        at: at.cloned(),
        kind: CheckpointKind::Escalation,
        title,
        card: Some(card),
        packet: CheckpointPacket::Escalation(packet),
        facts: EvidenceFacts {
            escalation: escalation_facts(host, record),
            ..EvidenceFacts::default()
        },
        agent_outputs: Vec::new(),
        documents: Vec::new(),
        options: None,
        free_text: FreeText::Guidance,
        authors: vec![owner_of(host, record)],
    }
}

/// What auto needs to grant one more attempt, by the fallback "Hand it off" would pick. `None`,
/// which asks you, when whether it fits the reserve isn't known.
fn escalation_facts(host: &Host, record: &CardRecord) -> Option<EscalationFacts> {
    let card = &record.spec.id;
    let owner = owner_of(host, record);
    let handoff_mode = rules::applied_for(host, &record.spec, &owner).mode;
    let next_family =
        recovery::other_provider(host, record).map(|worker| roles::family(host, &worker));
    let (mut families_tried, extra_attempts, fits_reserve) = host.read(|state| {
        let families: Vec<String> = state
            .sessions
            .by_key
            .values()
            .filter(|s| s.card.as_ref() == Some(card) && s.started.step == Some(StepName::Build))
            .map(|s| s.started.family.clone())
            .collect();
        (
            families,
            state.cards.progress(card).extra_attempts,
            budget::fits_reserve(state, record),
        )
    });
    families_tried.push(roles::family(host, &owner));
    families_tried.sort();
    families_tried.dedup();
    Some(EscalationFacts {
        handoff_mode,
        families_tried,
        next_family,
        extra_attempts,
        fits_reserve: fits_reserve?,
    })
}

/// Moves the card by your answer. From `NeedsDecision`, the moves the table only allows from
/// `Escalated` escalate first.
pub(crate) async fn apply_answer(
    host: &Host,
    card: &CardId,
    at: Option<&StepAt>,
    from: CardState,
    decided: &CheckpointDecided,
) -> EngineResult<Decision> {
    let note = decided.text.clone().filter(|t| !t.trim().is_empty());
    let extra: Vec<_> = note
        .iter()
        .map(|text| prompt::message_part("your note", text.clone()))
        .collect();
    let to_ready = if from == CardState::Escalated {
        Trigger::NewOwner
    } else {
        Trigger::Decided
    };
    let escalate_first = |reason: &str| -> EngineResult<()> {
        if from == CardState::NeedsDecision {
            transition(host, card, Trigger::Escalate, reason)?;
        }
        Ok(())
    };
    match &decided.action {
        AnswerAction::Candidate { worker } => {
            reassign(host, card, worker.clone(), &decided.by)?;
            transition(
                host,
                card,
                to_ready,
                &format!("you picked {}", crate::roles::display_name(worker)),
            )?;
            Ok(fresh(
                AttemptCause::Reassigned,
                "you picked a new worker",
                extra,
            ))
        }
        AnswerAction::HandOff | AnswerAction::OtherCandidates => {
            let record = record_of(host, card)?;
            match recovery::other_provider(host, &record) {
                Some(worker) => {
                    reassign(host, card, worker.clone(), &decided.by)?;
                    transition(
                        host,
                        card,
                        to_ready,
                        &format!("handed to {}", crate::roles::display_name(&worker)),
                    )?;
                    Ok(fresh(AttemptCause::Handoff, "you handed it off", extra))
                }
                None => {
                    let reason = "no other agent can take it; one more attempt";
                    transition(host, card, to_ready, reason)?;
                    Ok(fresh(AttemptCause::AfterFailure, reason, extra))
                }
            }
        }
        AnswerAction::TakeOver => {
            escalate_first("you took it over")?;
            transition(host, card, Trigger::TakeOver, "you took it over")?;
            record_card(host, card, CardEvent::TakenOver { command: None })?;
            Ok(Decision::Moved(None))
        }
        AnswerAction::Defer | AnswerAction::Wait => {
            escalate_first("you deferred it")?;
            transition(host, card, Trigger::Defer, "you deferred it")?;
            Ok(Decision::Moved(None))
        }
        AnswerAction::Split => {
            let how = note.clone().unwrap_or_default();
            match split(host, card, at, &how).await? {
                Ok(specs) => {
                    escalate_first("you split it")?;
                    let ids: Vec<String> = specs.iter().map(|s| s.id.to_string()).collect();
                    let reason = format!("split into {}", ids.join(", "));
                    transition(host, card, Trigger::Split, &reason)?;
                    for spec in specs {
                        record_card(host, &spec.id.clone(), CardEvent::Created(Box::new(spec)))?;
                    }
                    rewire_dependents(host, card, &ids)?;
                    Ok(Decision::Moved(None))
                }
                Err(why) => {
                    // A split the graph rules refuse can't be asked again from the same
                    // decided checkpoint, so the card waits deferred with the reason.
                    escalate_first("the split was refused")?;
                    let reason = format!("the split was refused: {why}");
                    transition(host, card, Trigger::Defer, &reason)?;
                    Ok(Decision::Moved(None))
                }
            }
        }
        AnswerAction::Cancel | AnswerAction::Stop | AnswerAction::Abandon => {
            escalate_first("you dropped it")?;
            transition(host, card, Trigger::Drop, "you dropped it")?;
            Ok(Decision::Moved(None))
        }
        AnswerAction::Escalate if from == CardState::NeedsDecision => {
            transition(host, card, Trigger::Escalate, "you asked to decide it")?;
            Ok(Decision::Moved(None))
        }
        _ if from == CardState::NeedsDecision => {
            transition(host, card, to_ready, "you decided it continues")?;
            let mut parts = extra;
            if parts.is_empty() {
                parts.push(prompt::message_part(
                    "continue",
                    "You decided it continues. Carry on with the task.",
                ));
            }
            Ok(Decision::Moved(Some(Next::resume(
                AttemptCause::Resumed,
                parts,
            ))))
        }
        _ => {
            transition(host, card, to_ready, "you asked for another attempt")?;
            Ok(fresh(
                AttemptCause::AfterFailure,
                "you asked for another attempt",
                extra,
            ))
        }
    }
}

/// You're done with a card you took over: let the owner continue, told what changed, or keep
/// it, so its gates and review run on your work.
pub(crate) async fn take_over_return(
    host: &Host,
    card: &CardId,
    at: Option<&StepAt>,
) -> EngineResult<Decision> {
    let record = record_of(host, card)?;
    let Some((changed, commit)) = progress_of(host, card).take_over_end else {
        return Ok(Decision::MovedMeanwhile);
    };
    let owner = owner_of(host, &record);
    let packet = TakeOverReturnPacket {
        card: card.clone(),
        owner: owner.clone(),
        changed,
        commit: commit.clone(),
    };
    let request = CheckpointRequest {
        at: at.cloned(),
        kind: CheckpointKind::TakeOverReturn,
        title: format!("{card} is back from you · {}", record.spec.title),
        card: Some(card.clone()),
        packet: CheckpointPacket::TakeOverReturn(packet),
        facts: EvidenceFacts::default(),
        agent_outputs: Vec::new(),
        documents: Vec::new(),
        options: None,
        free_text: FreeText::Guidance,
        authors: Vec::new(),
    };
    let Some(decided) = wait_decision(host, card, CardState::TakenOver, request).await? else {
        return Ok(Decision::MovedMeanwhile);
    };
    match &decided.action {
        AnswerAction::LetContinue | AnswerAction::HandBack => {
            transition(
                host,
                card,
                Trigger::HandBack,
                &format!("you handed it back to {owner}"),
            )?;
            let commit = commit.map_or_else(String::new, |sha| format!(" in commit {sha}"));
            let mut text = format!(
                "The user took this task over while you were paused and changed {} files                  (+{} -{}){commit}. Read what changed before you go on, then continue the task.",
                changed.files_changed, changed.insertions, changed.deletions
            );
            if let Some(note) = decided.text.as_deref().filter(|t| !t.trim().is_empty()) {
                text.push_str(&format!("\n\nTheir note: {note}"));
            }
            let parts = vec![prompt::message_part("your changes", text)];
            Ok(Decision::Moved(Some(Next::resume(
                AttemptCause::Resumed,
                parts,
            ))))
        }
        // Back from your own session, keeping the card says it's done: the work is checked as
        // yours.
        _ => {
            transition(
                host,
                card,
                Trigger::SaidDone,
                "you kept it, so its checks run",
            )?;
            Ok(Decision::Moved(None))
        }
    }
}

fn fresh(cause: AttemptCause, why: &str, extra: Vec<InputPart>) -> Decision {
    Decision::Moved(Some(Next::Fresh {
        cause,
        why: why.to_owned(),
        packet: None,
        extra,
    }))
}

fn reassign(host: &Host, card: &CardId, to: WorkerRef, by: &Party) -> EngineResult<()> {
    let record = record_of(host, card)?;
    let event = CardEvent::Reassigned {
        from: Some(owner_of(host, &record)),
        to,
        by: by.clone(),
        rule_override: None,
    };
    record_card(host, card, event)?;
    Ok(())
}

/// Your note on how to split the card becomes new cards through the `PlanToCards` step and the
/// graph validator. `Err` carries the validator's reasons.
async fn split(
    host: &Host,
    card: &CardId,
    at: Option<&StepAt>,
    how: &str,
) -> EngineResult<Result<Vec<CardSpec>, String>> {
    let record = record_of(host, card)?;
    let (goal, decisions, lead, next_number) = host.read(|state| {
        (
            state.goal.approved.clone(),
            state.plan.decisions.clone(),
            lead::phase_lead(state, host.settings()),
            next_card_number(state),
        )
    });
    let settings = host.settings();
    let brief = match &goal {
        Some(goal) => GoalBrief {
            summary: goal.summary.clone(),
            criteria: goal
                .criteria
                .iter()
                .map(|c| format!("{}: {}", c.id, c.text))
                .collect(),
            assumptions: goal.assumptions.iter().map(|a| a.text.clone()).collect(),
            sha256: goal.sha256.clone(),
        },
        None => GoalBrief {
            summary: record.spec.task.clone(),
            criteria: Vec::new(),
            assumptions: Vec::new(),
            sha256: String::new(),
        },
    };
    let mut forbidden = vec![".argonauts/**".to_owned()];
    forbidden.extend(settings.limits.protected_paths.iter().cloned());
    let input = PlanToCardsInput {
        goal: brief,
        plan: PlanDraft {
            summary: format!(
                "Split {card} ({}) as the user says: {how}",
                record.spec.title
            ),
            approach: format!(
                "The task:\n\n{}\n\nIts paths: {}\n\nHow to split it: {how}",
                record.spec.task,
                record.spec.owned_paths.join(", ")
            ),
            pieces: Vec::new(),
            choices: Vec::new(),
            risks: Vec::new(),
            test_strategy: String::new(),
            new_dependencies: Vec::new(),
        },
        decisions: decisions
            .iter()
            .map(|d| format!("{}: {}", d.title, d.choice))
            .collect(),
        forbidden_paths: forbidden,
        max_tasks: settings.divide.max_tasks,
    };
    let worker = lead.unwrap_or_else(|| settings.main_agent.clone());
    let mut ask = Ask::new(
        at.cloned(),
        RoleName::PhaseLead,
        worker,
        prompts::divide::PLAN_TO_CARDS,
        input,
    );
    ask.card = Some(card.clone());
    let answered = host.ask::<PlanToCards>(ask).await?;
    let specs = match graph::validate(host.run_id(), &answered.output, settings) {
        Ok(specs) => specs,
        Err(reasons) => return Ok(Err(reasons.join("; "))),
    };
    Ok(Ok(renumbered(specs, &record.spec, next_number)))
}

/// The validator numbers cards from 1; the split's cards take the numbers after the run's
/// highest, keep the split card's dependencies, owner, and reviewers, and point at each other
/// by their new ids.
fn renumbered(specs: Vec<CardSpec>, split: &CardSpec, first: u32) -> Vec<CardSpec> {
    let renames: Vec<(CardId, CardId)> = specs
        .iter()
        .zip(first..)
        .map(|(spec, n)| (spec.id.clone(), CardId::numbered(n)))
        .collect();
    let rename = |id: &CardId| {
        renames
            .iter()
            .find(|(old, _)| old == id)
            .map(|(_, new)| new.clone())
    };
    specs
        .into_iter()
        .map(|mut spec| {
            spec.id = rename(&spec.id).unwrap_or(spec.id);
            let mut deps: Vec<CardId> = spec.depends_on.iter().filter_map(rename).collect();
            deps.extend(split.depends_on.iter().cloned());
            deps.sort();
            deps.dedup();
            spec.depends_on = deps;
            if spec.owner.is_none() {
                spec.owner = split.owner.clone();
            }
            if spec.reviewers.is_empty() {
                spec.reviewers = split.reviewers.clone();
            }
            spec
        })
        .collect()
}

/// Cards that waited on the split card now wait on all of its replacements.
fn rewire_dependents(host: &Host, split: &CardId, new_ids: &[String]) -> EngineResult<()> {
    let dependents: Vec<CardSpec> = host.read(|state| {
        state
            .cards
            .by_id
            .values()
            .filter(|r| r.spec.depends_on.contains(split))
            .map(|r| r.spec.clone())
            .collect()
    });
    let new: Vec<CardId> = new_ids
        .iter()
        .filter_map(|id| CardId::new(id.as_str()).ok())
        .collect();
    for mut spec in dependents {
        let mut deps: BTreeSet<CardId> = spec.depends_on.drain(..).filter(|d| d != split).collect();
        deps.extend(new.iter().cloned());
        spec.depends_on = deps.into_iter().collect();
        let id = spec.id.clone();
        record_card(host, &id, CardEvent::Created(Box::new(spec)))?;
    }
    Ok(())
}

/// One past the highest `card-NNN` number in the run.
fn next_card_number(state: &RunState) -> u32 {
    state
        .cards
        .by_id
        .keys()
        .filter_map(|id| id.as_str().strip_prefix("task-")?.parse::<u32>().ok())
        .max()
        .unwrap_or(0)
        + 1
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::card_spec;

    #[test]
    fn split_cards_take_new_numbers_and_the_old_dependencies() {
        let mut split = card_spec("task-002", &["task-001"], Some("codex"));
        split.reviewers = vec!["agy".parse().unwrap()];
        let a = card_spec("task-001", &[], None);
        let b = card_spec("task-002", &["task-001"], None);
        let cards = renumbered(vec![a, b], &split, 7);
        let ids: Vec<&str> = cards.iter().map(|c| c.id.as_str()).collect();
        assert_eq!(ids, ["task-007", "task-008"]);
        assert_eq!(cards[0].depends_on, [CardId::new("task-001").unwrap()]);
        assert_eq!(
            cards[1].depends_on,
            [
                CardId::new("task-001").unwrap(),
                CardId::new("task-007").unwrap()
            ]
        );
        assert_eq!(cards[1].owner, Some("codex".parse().unwrap()));
        assert_eq!(cards[1].reviewers, split.reviewers);
    }

    #[test]
    fn new_card_numbers_follow_the_highest() {
        let mut state = RunState::default();
        for id in ["task-003", "task-010", "spike-001"] {
            let spec = card_spec(id, &[], None);
            state
                .cards
                .by_id
                .insert(spec.id.clone(), CardRecord::new(spec));
        }
        assert_eq!(next_card_number(&state), 11);
        assert_eq!(next_card_number(&RunState::default()), 1);
    }
}
