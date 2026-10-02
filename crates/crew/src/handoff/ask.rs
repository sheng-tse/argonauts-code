//! The handoff card: a card's agent stopped and you pick who continues, take it over, or wait.
//! When an outage has the vendor marked down, the vendor-down card asks first.

use time::OffsetDateTime;

use argonauts_core::CardId;
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

use crate::card::machine::Trigger;
use crate::card::{pipeline, takeover};
use crate::checkpoints::options::{self, HandoffCause};
use crate::checkpoints::{self, CheckpointRequest};
use crate::engine::host::Host;
use crate::error::{EngineError, EngineResult};
use crate::handoff::{assign, choose, rules};
use crate::state::RunState;

/// Opens the handoff card and acts on your answer.
pub async fn ask(host: &Host, card: &CardId, plan: HandoffPlan) -> EngineResult<()> {
    if vendor_down(host, card, &plan).await? == Asked::Settled {
        return Ok(());
    }
    let mut plan = plan;
    let mut seen = Vec::new();
    loop {
        let cause = host.read(|state| cause(state, card));
        let request = CheckpointRequest {
            at: host.read(rules::current_step),
            kind: CheckpointKind::Handoff,
            title: format!("{} stopped · {card}", plan.stopped),
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
                "the handoff card for {card} could not open again"
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
            AnswerAction::Wait => return wait(host, card, plan.stopped.vendor(), plan.resets_at),
            AnswerAction::Retry => {
                return rules::back_in_line(host, card, "retry after you fixed the account");
            }
            AnswerAction::Escalate | AnswerAction::Stop | AnswerAction::Cancel => {
                pipeline::transition(
                    host,
                    card,
                    Trigger::Escalate,
                    "escalated from the handoff card",
                )?;
                return Ok(());
            }
            other => {
                return Err(EngineError::Refused(format!(
                    "the handoff card doesn't offer {other:?}"
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
            role: RoleName::Owner,
        }],
    };
    let request = CheckpointRequest {
        at: host.read(rules::current_step),
        kind: CheckpointKind::VendorDown,
        title: format!("{vendor} is down · {card} paused"),
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
        AnswerAction::PerCard => Ok(Asked::PerCard),
        other => Err(EngineError::Refused(format!(
            "the vendor-down card doesn't offer {other:?}"
        ))),
    }
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
