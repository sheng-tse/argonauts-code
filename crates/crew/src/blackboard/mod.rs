//! The blackboard: typed messages only. Questions go to the owner of the piece they concern or
//! to you; change requests go through fixed decision rules; every decision becomes a record;
//! messages are delivered at delivery points (a session's start, or its next turn).

use argonauts_core::CardId;
use argonauts_core::events::RunEventKind;
use argonauts_core::events::run::Party;
use argonauts_core::events::shared::{
    BlackboardEvent, BlackboardKind, BlackboardMessage, Recipient,
};
use argonauts_core::names::{MessageId, RoleName};
use argonauts_core::steps::card::CardReport;

use crate::engine::host::Host;
use crate::error::EngineResult;
use crate::state::RunState;
use crate::state::ids::IdKind;

pub mod commands;
pub mod delivery;
pub mod questions;
pub mod records;
pub mod refs;
pub mod replies;
pub mod requests;
pub mod route;
pub mod rules;

/// A message to post, before it has an id.
#[derive(Debug, Clone, PartialEq)]
pub struct Post {
    pub kind: BlackboardKind,
    pub from: Party,
    pub to: Recipient,
    pub text: String,
    pub in_reply_to: Option<MessageId>,
    /// Paths, cards, tests, or records it cites.
    pub refs: Vec<String>,
    /// The card it comes from, so replies find their way back.
    pub from_card: Option<CardId>,
}

/// Records `Blackboard::Posted` with a new message id.
pub fn post(host: &Host, post: Post) -> EngineResult<MessageId> {
    let id = MessageId::numbered(host.allocate(IdKind::Message)?);
    let session = match &post.from {
        Party::Agent { session, .. } => Some(session.clone()),
        _ => None,
    };
    let card = post.from_card.clone().or_else(|| {
        let session = session.as_ref()?;
        host.read(|state| state.sessions.get(session).and_then(|s| s.card.clone()))
    });
    host.record(
        card.as_ref(),
        session.as_ref(),
        RunEventKind::Blackboard(BlackboardEvent::Posted(BlackboardMessage {
            id: id.clone(),
            kind: post.kind,
            from: post.from,
            to: post.to,
            text: post.text,
            in_reply_to: post.in_reply_to,
            refs: post.refs,
        })),
    )?;
    Ok(id)
}

/// Posts unless the same message is already on the board, which is how a step re-entered after
/// a crash doesn't post its report's questions twice.
pub fn post_once(host: &Host, post: Post) -> EngineResult<MessageId> {
    let posted = host.read(|state| {
        state
            .blackboard
            .messages
            .iter()
            .find(|message| {
                message.kind == post.kind
                    && message.from == post.from
                    && message.to == post.to
                    && message.text == post.text
                    && message.in_reply_to == post.in_reply_to
            })
            .map(|message| message.id.clone())
    });
    match posted {
        Some(id) => Ok(id),
        None => self::post(host, post),
    }
}

/// A card's report after its session ended: questions routed, change requests through the
/// rules (dependents first move to `Blocked`), decisions counted as votes.
pub async fn on_report(host: &Host, card: &CardId, report: &CardReport) -> EngineResult<()> {
    let from = reporter(host, card);
    for reply in replies::in_report(report) {
        requests::take_reply(host, card, &from, reply).await?;
    }
    for question in &report.questions {
        questions::ask(host, card, &from, question).await?;
    }
    for request in &report.change_requests {
        requests::file(host, card, &from, request).await?;
    }
    Ok(())
}

/// The card's newest session, which wrote the report.
fn reporter(host: &Host, card: &CardId) -> Party {
    host.read(|state| {
        state
            .sessions
            .by_key
            .values()
            .filter(|session| session.card.as_ref() == Some(card))
            .max_by_key(|session| session.since)
            .map(|session| Party::Agent {
                session: session.key.clone(),
                worker: session.started.worker.clone(),
            })
    })
    .unwrap_or(Party::Conductor)
}

/// Where the reply to a message goes: back to the card it came from, else its session, else you.
pub fn reply_to(state: &RunState, message: &BlackboardMessage) -> Recipient {
    if let Some(card) = state.blackboard.posted_from.get(&message.id) {
        let session_of_card = match &message.from {
            Party::Agent { session, .. } => state
                .sessions
                .get(session)
                .is_none_or(|record| owns_card(record.started.role)),
            _ => true,
        };
        if session_of_card {
            return Recipient::Card { card: card.clone() };
        }
    }
    match &message.from {
        Party::Agent { session, .. } => Recipient::Session {
            session: session.clone(),
        },
        _ => Recipient::User,
    }
}

fn owns_card(role: RoleName) -> bool {
    matches!(role, RoleName::Owner | RoleName::Default | RoleName::Spike)
}
