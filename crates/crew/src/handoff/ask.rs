//! The handoff card: a card's agent stopped and you pick who continues, take it over, or wait.
//! When an outage has the vendor marked down, the vendor-down card asks first.

use time::OffsetDateTime;

use argonauts_core::CardId;
use argonauts_core::display_names::DisplayNames;
use argonauts_core::events::RunEventKind;
use argonauts_core::events::card::CardState;
use argonauts_core::events::checkpoint::{
    AnswerAction, CheckpointKind, CheckpointPacket, FreeText, PausedCard, VendorDownPacket,
};
use argonauts_core::events::session::EndReason;
use argonauts_core::events::vendor::{HandoffEvent, HandoffPlan};
use argonauts_core::limits::HealthState;
use argonauts_core::names::{RoleName, VendorId};
use argonauts_core::policy::EvidenceFacts;
use argonauts_core::{Note, NoteLevel};

use crate::card::machine::Trigger;
use crate::card::support::newest_owner_session;
use crate::card::{pipeline, takeover};
use crate::checkpoints::options::{self, HandoffCause};
use crate::checkpoints::{self, CheckpointRequest, autopilot};
use crate::engine::host::Host;
use crate::engine::stand_in;
use crate::error::{EngineError, EngineResult, Stall};
use crate::handoff::{assign, choose, rules};
use crate::roles::display_name;
use crate::state::RunState;

/// Opens the handoff card and acts on your answer. Full auto never asks: with nobody to take
/// the card, it waits for its agent and tries again on its own.
pub async fn ask(host: &Host, card: &CardId, plan: HandoffPlan) -> EngineResult<()> {
    if vendor_down(host, card, &plan).await? == Asked::Settled {
        return Ok(());
    }
    if plan.candidates.is_empty() && autopilot::applies_now(host, CheckpointKind::Handoff) {
        return wait_to_retry(host, card, &plan).await;
    }
    let mut plan = plan;
    let mut seen = Vec::new();
    loop {
        let mut cause = host.read(|state| cause(state, card));
        // "I logged in, retry" only where the CLI's own check says it's logged out.
        if cause == HandoffCause::LoggedOut && !says_logged_out(host, card, &plan).await {
            cause = HandoffCause::Outage;
        }
        let request = CheckpointRequest {
            at: host.read(rules::current_step),
            kind: CheckpointKind::Handoff,
            // Titles reach the transcript's notices, so they name the agent and the card as
            // people read them, not by their ids.
            title: format!(
                "{} stopped · {}",
                DisplayNames::default().worker(&plan.stopped),
                plan.title.trim()
            ),
            card: Some(card.clone()),
            packet: CheckpointPacket::Handoff(plan.clone()),
            facts: EvidenceFacts::default(),
            // The candidates are part of what you decide on, so they're in the evidence.
            agent_outputs: plan
                .candidates
                .iter()
                .map(|candidate| format!("candidate {}", candidate.worker))
                .collect(),
            documents: Vec::new(),
            options: Some(options::handoff(&plan, cause)),
            free_text: FreeText::None,
            authors: Vec::new(),
        };
        let decided = checkpoints::open_and_wait(host, request).await?;
        if seen.contains(&decided.id) {
            return Err(EngineError::Refused(format!(
                "the fallback question for {card} could not open again"
            )));
        }
        seen.push(decided.id.clone());
        let record = rules::record(host, card)?;
        match decided.action {
            AnswerAction::Candidate { worker } => {
                let packet = host.services().handoff.packet(&record.spec, &[]);
                let tag = rules::applied_for(host, &record.spec, &plan.stopped).checkpoint;
                assign::hand_off(host, card, &worker, &packet, tag, decided.by).await?;
                return Ok(());
            }
            AnswerAction::OtherCandidates => {
                let query = choose::query(host, &record, &plan.stopped, true);
                plan.candidates = host.services().handoff.candidates(&query);
            }
            AnswerAction::TakeOver => {
                takeover::take_over(host, card)
                    .await
                    .map_err(EngineError::Refused)?;
                return Ok(());
            }
            AnswerAction::Wait => {
                // Without a reset to wait for, it tries again after a while.
                let until = plan.resets_at.or_else(|| retry_at(host, card));
                return wait(host, card, plan.stopped.vendor(), until);
            }
            AnswerAction::Retry => {
                return rules::back_in_line(host, card, "retry after you fixed the account");
            }
            AnswerAction::Escalate | AnswerAction::Stop | AnswerAction::Cancel => {
                pipeline::transition(
                    host,
                    card,
                    Trigger::Escalate,
                    "sent to you from the fallback question",
                )?;
                return Ok(());
            }
            other => {
                return Err(EngineError::Refused(format!(
                    "the fallback question doesn't offer {other:?}"
                )));
            }
        }
    }
}

#[derive(Debug, PartialEq)]
enum Asked {
    Settled,
    /// The handoff card asks next: the vendor isn't down, or you chose per card.
    PerCard,
}

/// An outage that has the vendor marked down asks about the vendor: one agent takes every
/// paused card (the first valid candidate), you choose per card, or the card waits for it.
async fn vendor_down(host: &Host, card: &CardId, plan: &HandoffPlan) -> EngineResult<Asked> {
    let vendor = plan.stopped.vendor();
    let health = host.services().health.state(&vendor);
    let down = health.state == HealthState::Down || health.disabled.is_some();
    if !down || host.read(|state| cause(state, card)) != HandoffCause::Outage {
        return Ok(Asked::PerCard);
    }
    let packet = VendorDownPacket {
        vendor: vendor.clone(),
        evidence: health.reason.unwrap_or_else(|| plan.reason.clone()),
        test_at: health.next_test_at,
        paused: vec![PausedCard {
            card: card.clone(),
            title: plan.title.clone(),
            role: RoleName::Worker,
        }],
    };
    let request = CheckpointRequest {
        at: host.read(rules::current_step),
        kind: CheckpointKind::VendorDown,
        title: format!(
            "{} is down · {} paused",
            DisplayNames::default().harness(&vendor.0),
            plan.title.trim()
        ),
        card: Some(card.clone()),
        packet: CheckpointPacket::VendorDown(packet),
        facts: EvidenceFacts::default(),
        agent_outputs: Vec::new(),
        documents: Vec::new(),
        options: None,
        free_text: FreeText::None,
        authors: Vec::new(),
    };
    let decided = checkpoints::open_and_wait(host, request).await?;
    match decided.action {
        AnswerAction::OneForAll => {
            let Some(first) = plan.candidates.first() else {
                return Ok(Asked::PerCard);
            };
            let record = rules::record(host, card)?;
            let packet = host.services().handoff.packet(&record.spec, &[]);
            let tag = rules::applied_for(host, &record.spec, &plan.stopped).checkpoint;
            assign::hand_off(host, card, &first.worker, &packet, tag, decided.by).await?;
            Ok(Asked::Settled)
        }
        AnswerAction::Wait => {
            wait(host, card, vendor, health.next_test_at)?;
            Ok(Asked::Settled)
        }
        AnswerAction::PerTask => Ok(Asked::PerCard),
        other => Err(EngineError::Refused(format!(
            "the agent-down question doesn't offer {other:?}"
        ))),
    }
}

/// Full auto, and nobody may take the card: it waits for its agent, until the reset when one
/// is known, else for a retry after a wait that grows with each, and one line says why and what
/// fixes it.
async fn wait_to_retry(host: &Host, card: &CardId, plan: &HandoffPlan) -> EngineResult<()> {
    let ended = ended(host, card);
    let stopped = display_name(&plan.stopped);
    let logged_out = says_logged_out(host, card, plan).await;
    let why = ended.as_ref().map_or_else(
        || "it stopped".to_owned(),
        |reason| format!("its session ended early ({})", stand_in::in_words(reason)),
    );
    let stall = Stall {
        detail: format!("nobody could take over {}", plan.title.trim()),
        line: format!(
            "Nobody could take over \"{}\" from {stopped}: {why}, and no other agent can take \
             the task.",
            plan.title.trim()
        ),
        logged_out: if logged_out {
            vec![stopped]
        } else {
            Vec::new()
        },
        resets_at: plan.resets_at,
    };
    let waits = host.read(|state| state.cards.progress(card).vendor_waits);
    let retry = stand_in::retry_after(host.settings(), waits);
    let until = plan
        .resets_at
        .or_else(|| retry.map(|retry| host.now() + retry));
    let line = stand_in::park_line(&stall, retry.filter(|_| plan.resets_at.is_none()));
    host.record(
        Some(card),
        None,
        RunEventKind::Note(Note {
            level: NoteLevel::Warn,
            text: line,
        }),
    )?;
    wait(host, card, plan.stopped.vendor(), until)
}

/// When a card with no reset to wait for tries again: after the breaker's cool-down, doubled for
/// each wait it had. `None` once it stopped trying on its own.
fn retry_at(host: &Host, card: &CardId) -> Option<OffsetDateTime> {
    let waits = host.read(|state| state.cards.progress(card).vendor_waits);
    stand_in::retry_after(host.settings(), waits).map(|retry| host.now() + retry)
}

/// Whether the stopped agent's CLI says it's logged out, after the sign-in error that stopped
/// the card.
async fn says_logged_out(host: &Host, card: &CardId, plan: &HandoffPlan) -> bool {
    match ended(host, card) {
        Some(reason) => stand_in::login_check(host, &plan.stopped, &reason).await == Some(false),
        None => false,
    }
}

/// How the card's newest owner session ended.
fn ended(host: &Host, card: &CardId) -> Option<EndReason> {
    host.read(|state| {
        newest_owner_session(state, card)
            .and_then(|session| session.end.as_ref())
            .map(|end| end.reason.clone())
    })
}

/// The card waits for its vendor, in `NeedsDecision`.
pub fn wait(
    host: &Host,
    card: &CardId,
    vendor: VendorId,
    until: Option<OffsetDateTime>,
) -> EngineResult<()> {
    host.record(
        Some(card),
        None,
        RunEventKind::Handoff(HandoffEvent::Waiting {
            card: card.clone(),
            vendor,
            until,
        }),
    )?;
    Ok(())
}

/// `vendor` is back: each card that waits for it goes back in line with its owner, which
/// carries on in its own session. The card's own progress says what it waits for, since the
/// run's waits are cleared as the run wakes.
pub fn vendor_back(host: &Host, vendor: &VendorId) -> EngineResult<()> {
    let waiting: Vec<CardId> = host.read(|state| {
        state
            .cards
            .by_id
            .values()
            .filter(|record| record.state == CardState::NeedsDecision)
            .map(|record| record.spec.id.clone())
            .filter(|card| state.cards.progress(card).waiting_for.as_ref() == Some(vendor))
            .collect()
    });
    for card in waiting {
        pipeline::transition(host, &card, Trigger::Decided, &format!("{vendor} is back"))?;
    }
    Ok(())
}

/// Why the card's last session stopped, which decides the card's first option.
pub fn cause(state: &RunState, card: &CardId) -> HandoffCause {
    let last = state
        .sessions
        .by_key
        .values()
        .filter(|session| session.card.as_ref() == Some(card))
        .filter_map(|session| Some((session.since, &session.end.as_ref()?.reason)))
        .max_by_key(|(since, _)| *since);
    match last.map(|(_, reason)| reason) {
        Some(EndReason::Auth { .. }) => HandoffCause::LoggedOut,
        Some(EndReason::Billing { .. }) => HandoffCause::BillingCap,
        Some(EndReason::VendorError { .. }) => HandoffCause::Outage,
        _ => HandoffCause::UsageLimit,
    }
}
