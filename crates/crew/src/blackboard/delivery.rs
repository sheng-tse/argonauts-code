//! Delivering messages at delivery points: the next session start, or the next turn for queued
//! messages. A drafting agent or blind reviewer gets only your text. Records `Delivered`, which
//! marks the message passed to that session, so replaying a log never sends it twice.

use std::fmt::Write as _;
use std::sync::{Mutex, MutexGuard, PoisonError};

use argonauts_core::agents::session_runner::SessionControl;
use argonauts_core::base::names::{MessageId, RoleName, SessionKey};
use argonauts_core::input::MentionTarget;
use argonauts_core::runs::events::RunEventKind;
use argonauts_core::runs::events::run::Party;
use argonauts_core::runs::events::session::{
    Delivery, InputPart, InputPartKind, QueuedMessage, SessionEvent,
};
use argonauts_core::runs::events::shared::{
    BlackboardEvent, BlackboardKind, BlackboardMessage, Recipient,
};
use argonauts_core::settings::Settings;

use crate::engine::error::{EngineError, EngineResult};
use crate::engine::host::Host;
use crate::state::blackboard::Addressee;

/// Checking that a session hasn't had a message, recording that it has, and sending it are one
/// step, so two posters, or a poster and a session catching up as it goes live, never both
/// hand one message over. A hand-over is a check and two records, so one lock shared by every
/// run in the process is cheaper than keeping one per run.
static HANDING_OVER: Mutex<()> = Mutex::new(());

fn one_at_a_time() -> MutexGuard<'static, ()> {
    HANDING_OVER.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The parts a session that has already started gets: every pending message for it, recorded
/// delivered at once.
pub fn at_session_start(host: &Host, session: &SessionKey) -> EngineResult<Vec<InputPart>> {
    let (parts, deliveries) = for_session_start(host, session)?;
    deliveries.record(host)?;
    Ok(parts)
}

/// Every pending message for a session about to start, as prompt parts, and their deliveries
/// to record once it has started. Call it once the session's `Session::Started` is recorded,
/// since that says who the session is.
pub fn for_session_start(
    host: &Host,
    session: &SessionKey,
) -> EngineResult<(Vec<InputPart>, StartDeliveries)> {
    let who = addressee(host, session)?;
    let blind = is_blind(host, &who);
    let pending: Vec<BlackboardMessage> = host.read(|state| {
        state
            .blackboard
            .pending_for(&who)
            .into_iter()
            .cloned()
            .collect()
    });
    let mut parts = Vec::new();
    let mut messages = Vec::new();
    for message in pending {
        let Some(text) = text_for(&message, blind) else {
            continue;
        };
        parts.push(part(&message, text));
        messages.push(message.id);
    }
    let deliveries = StartDeliveries {
        to: Some(who),
        messages,
    };
    Ok((parts, deliveries))
}

/// The messages put in a session's first prompt. They count as delivered only once the session
/// has started, so a session that fails to start leaves them pending for the next one.
#[derive(Debug, Default)]
#[must_use = "record the deliveries once the session has started"]
pub struct StartDeliveries {
    to: Option<Addressee>,
    messages: Vec<MessageId>,
}

impl StartDeliveries {
    pub fn record(self, host: &Host) -> EngineResult<()> {
        let Some(who) = &self.to else {
            return Ok(());
        };
        let _one_at_a_time = one_at_a_time();
        for message in &self.messages {
            record_delivered(host, message, who)?;
        }
        Ok(())
    }
}

/// Queues a message on a running session for its next turn (`Delivery::NextTurn`). Queuing one
/// the session already has sends nothing.
pub fn queue(
    host: &Host,
    message: &MessageId,
    session: &SessionKey,
) -> EngineResult<QueuedMessage> {
    let who = addressee(host, session)?;
    let posted = host
        .read(|state| state.blackboard.get(message).cloned())
        .ok_or_else(|| EngineError::Refused(format!("There's no message {message}")))?;
    let text = text_for(&posted, is_blind(host, &who)).ok_or_else(|| {
        EngineError::Refused(format!(
            "{session} works blind, so it gets only your own guidance and mentions"
        ))
    })?;
    let queued = QueuedMessage {
        id: posted.id.clone(),
        from: posted.from.clone(),
        to: target(&posted.to, &who),
        text,
        queued_at: host.now(),
        delivery: Delivery::NextTurn,
    };
    hand_over(host, &who, queued)
}

/// Queues on a session that has just gone live every message posted for it while it started:
/// its first prompt was built before them, and no poster could reach it until it was live. Call
/// it once the session is live and its first prompt's messages are recorded delivered.
pub fn catch_up(host: &Host, session: &SessionKey) -> EngineResult<Vec<QueuedMessage>> {
    let who = addressee(host, session)?;
    let pending: Vec<MessageId> = host.read(|state| {
        state
            .blackboard
            .pending_for(&who)
            .into_iter()
            .map(|message| message.id.clone())
            .collect()
    });
    let mut queued = Vec::new();
    for message in pending {
        match queue(host, &message, session) {
            Ok(one) => queued.push(one),
            // Blind, or it ended meanwhile: it waits for a delivery point it can take.
            Err(EngineError::Refused(_)) => {}
            Err(error) => return Err(error),
        }
    }
    Ok(queued)
}

/// Queues the message on every running session it's for that hasn't had it. Sessions that
/// aren't running get it when they start.
pub fn to_running(host: &Host, message: &MessageId) -> EngineResult<Vec<QueuedMessage>> {
    let mut queued = Vec::new();
    for session in host.shared().live.keys() {
        let Ok(who) = addressee(host, &session) else {
            continue;
        };
        let wanted = host.read(|state| {
            state
                .blackboard
                .pending_for(&who)
                .iter()
                .any(|pending| &pending.id == message)
        });
        if !wanted {
            continue;
        }
        match queue(host, message, &session) {
            Ok(one) => queued.push(one),
            // Blind, or it ended meanwhile: it waits for a delivery point it can take.
            Err(EngineError::Refused(_)) => {}
            Err(error) => return Err(error),
        }
    }
    Ok(queued)
}

/// Records the message queued and delivered, then sends it, unless the session had it already.
/// The send takes a place in the session's control queue held before the records, so nothing
/// can fail once they're written, and a replay that finds them never sends the message again.
pub(crate) fn hand_over(
    host: &Host,
    who: &Addressee,
    queued: QueuedMessage,
) -> EngineResult<QueuedMessage> {
    let _one_at_a_time = one_at_a_time();
    if host.read(|state| state.blackboard.was_delivered(&queued.id, &who.session)) {
        return Ok(queued);
    }
    let slot = host
        .reserve_control(&who.session)
        .map_err(EngineError::Refused)?;
    host.record(
        who.task.as_ref(),
        Some(&who.session),
        RunEventKind::Session(SessionEvent::Queued(queued.clone())),
    )?;
    record_delivered(host, &queued.id, who)?;
    slot.send(SessionControl::Deliver(queued.clone()));
    Ok(queued)
}

fn record_delivered(host: &Host, message: &MessageId, who: &Addressee) -> EngineResult<()> {
    if host.read(|state| state.blackboard.was_delivered(message, &who.session)) {
        return Ok(());
    }
    host.record(
        who.task.as_ref(),
        Some(&who.session),
        RunEventKind::Blackboard(BlackboardEvent::Delivered {
            message: message.clone(),
            to: who.session.clone(),
        }),
    )?;
    Ok(())
}

/// Who a recorded session is, for addressing it.
pub fn addressee(host: &Host, session: &SessionKey) -> EngineResult<Addressee> {
    host.read(|state| {
        state.sessions.get(session).map(|record| Addressee {
            session: session.clone(),
            worker: record.started.worker.clone(),
            role: record.started.role,
            step: record.started.step,
            task: record.task.clone(),
        })
    })
    .ok_or_else(|| {
        EngineError::Refused(format!(
            "{session} has no recorded start, so no message can reach it yet"
        ))
    })
}

fn is_blind(host: &Host, who: &Addressee) -> bool {
    match host.shared().live.get(&who.session) {
        Some(live) => live.blind,
        None => blind_role(who.role, host.settings()),
    }
}

/// Drafting agents, judges, and blind reviewers work blind.
pub fn blind_role(role: RoleName, settings: &Settings) -> bool {
    match role {
        RoleName::Draft => settings.drafting.blind,
        RoleName::Judge => true,
        RoleName::Reviewer => settings
            .roles
            .get(&RoleName::Reviewer)
            .and_then(|reviewer| reviewer.blind)
            .unwrap_or(settings.review.blind),
        _ => false,
    }
}

/// What a session gets of a message. A blind session gets only your guidance and mentions,
/// as your bare text, and no agent's message at all.
pub fn text_for(message: &BlackboardMessage, blind: bool) -> Option<String> {
    if blind {
        let yours = message.from == Party::User
            && matches!(
                message.kind,
                BlackboardKind::Guidance | BlackboardKind::Mention
            );
        return yours.then(|| message.text.clone());
    }
    Some(framed(message))
}

fn framed(message: &BlackboardMessage) -> String {
    let mut text = format!(
        "{} {} from {}",
        kind_label(message.kind),
        message.id,
        party_label(&message.from)
    );
    if let Some(about) = &message.in_reply_to {
        let _ = write!(text, ", about {about}");
    }
    let _ = write!(text, ":\n{}", message.text);
    if !message.refs.is_empty() {
        let _ = write!(text, "\nCites: {}", message.refs.join(", "));
    }
    match message.kind {
        BlackboardKind::Question => {
            let _ = write!(
                text,
                "\nAnswer it in your report, on its own line in `concerns`: `answer {}: <your answer>`.",
                message.id
            );
        }
        BlackboardKind::ChangeRequest => {
            let _ = write!(
                text,
                "\nDecide it in your report, on its own line in `concerns`: `accept {id}: <why>` or `reject {id}: <why>`.",
                id = message.id
            );
        }
        _ => {}
    }
    text
}

fn part(message: &BlackboardMessage, text: String) -> InputPart {
    InputPart {
        kind: InputPartKind::Message,
        label: format!("{} {}", kind_label(message.kind), message.id),
        approx_tokens: u32::try_from(text.len().div_ceil(4)).unwrap_or(u32::MAX),
        text,
        source: Some(message.id.to_string()),
    }
}

/// The mention target a queued message shows, from whom the message was for.
pub fn target(to: &Recipient, who: &Addressee) -> MentionTarget {
    match to {
        Recipient::All => MentionTarget::All,
        Recipient::Task { task } => MentionTarget::Task(task.clone()),
        Recipient::Role { role } => MentionTarget::Role(*role),
        Recipient::Worker { worker } => MentionTarget::Worker(worker.clone()),
        Recipient::Session { .. } | Recipient::User => MentionTarget::Worker(who.worker.clone()),
    }
}

pub fn kind_label(kind: BlackboardKind) -> &'static str {
    match kind {
        BlackboardKind::Question => "Question",
        BlackboardKind::Answer => "Answer",
        BlackboardKind::ChangeRequest => "Change request",
        BlackboardKind::Decision => "Decision",
        BlackboardKind::ExperimentReport => "Experiment report",
        BlackboardKind::Fallback => "Fallback",
        BlackboardKind::Guidance => "Guidance",
        BlackboardKind::Mention => "Message",
    }
}

pub fn party_label(party: &Party) -> String {
    match party {
        Party::User | Party::Declined => "you".to_owned(),
        Party::Rules => "your approval rules".to_owned(),
        Party::Judge { model } => format!("your judge {model}"),
        Party::Config => "a setting".to_owned(),
        Party::Autopilot => "autopilot".to_owned(),
        Party::ArgonautsCode => "Argonauts Code".to_owned(),
        Party::Network => "the network".to_owned(),
        Party::Agent { worker, .. } => worker.to_string(),
        Party::OutsideAgent { client } => client.clone(),
    }
}

#[cfg(test)]
mod tests {
    use argonauts_core::base::names::ModelRef;

    use super::*;

    fn message(kind: BlackboardKind, from: Party) -> BlackboardMessage {
        BlackboardMessage {
            id: MessageId::numbered(4),
            kind,
            from,
            to: Recipient::All,
            text: "Keep the limiter in-process.".to_owned(),
            in_reply_to: None,
            refs: vec!["src/limiter/mod.rs".to_owned()],
        }
    }

    #[test]
    fn blind_sessions_get_your_text_only() {
        let yours = message(BlackboardKind::Guidance, Party::User);
        assert_eq!(
            text_for(&yours, true).as_deref(),
            Some("Keep the limiter in-process.")
        );
        let framed = text_for(&yours, false).unwrap();
        assert!(framed.starts_with("Guidance m-004 from you:\n"));
        assert!(framed.contains("Cites: src/limiter/mod.rs"));
        let agent = message(
            BlackboardKind::Question,
            Party::Agent {
                session: SessionKey::numbered(2),
                worker: ModelRef::new("codex", None),
            },
        );
        assert_eq!(text_for(&agent, true), None);
        assert!(text_for(&agent, false).unwrap().contains("`answer m-004: "));
    }

    #[test]
    fn drafting_agents_and_judges_are_blind() {
        let settings = Settings::default();
        assert!(blind_role(RoleName::Draft, &settings));
        assert!(blind_role(RoleName::Judge, &settings));
        assert!(!blind_role(RoleName::Worker, &settings));
    }
}
