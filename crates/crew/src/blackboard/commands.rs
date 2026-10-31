//! Your steering, mentions, guidance, and taking undelivered messages back.

use argonauts_core::agents::session_runner::{SessionControl, SteerSupport};
use argonauts_core::base::names::{MessageId, RoleName, SessionKey};
use argonauts_core::input::MentionTarget;
use argonauts_core::runs::api::SteerAck;
use argonauts_core::runs::events::RunEventKind;
use argonauts_core::runs::events::run::Party;
use argonauts_core::runs::events::session::{Delivery, QueuedMessage};
use argonauts_core::runs::events::shared::{BlackboardKind, Recipient};
use argonauts_core::{Note, NoteLevel};

use crate::blackboard::{Post, delivery, post};
use crate::engine::error::EngineResult;
use crate::engine::host::Host;
use crate::roles::consultant;
use crate::state::blackboard::TAKEN_BACK;

/// `SessionControl::Deliver` with `Delivery::Steer`; the reply says how the worker takes it.
/// `now`: where the session's CLI can, the model reads it at once, the running tool moving to the
/// background, as Claude Code's send now does.
pub async fn steer(
    host: &Host,
    session: &SessionKey,
    text: String,
    now: bool,
) -> Result<SteerAck, String> {
    if host.shared().live.get(session).is_none() {
        return Err(format!("{session} isn't running"));
    }
    let who = delivery::addressee(host, session).map_err(|error| error.to_string())?;
    let how = host
        .services()
        .workers
        .get(&who.worker)
        .map_err(|error| error.to_string())?
        .capabilities()
        .steer;
    let id = post(
        host,
        Post {
            kind: BlackboardKind::Mention,
            from: Party::User,
            to: Recipient::Session {
                session: session.clone(),
            },
            text: text.clone(),
            in_reply_to: None,
            refs: Vec::new(),
            from_task: None,
        },
    )
    .map_err(|error| error.to_string())?;
    let queued = QueuedMessage {
        id,
        from: Party::User,
        to: MentionTarget::Worker(who.worker.clone()),
        // Steering is your own words, so it goes as typed, to blind sessions too.
        text,
        queued_at: host.now(),
        delivery: match how {
            SteerSupport::Input | SteerSupport::InterruptResume => Delivery::Steer,
            SteerSupport::NextTurnOnly => Delivery::NextTurn,
        },
    };
    delivery::hand_over(host, &who, queued).map_err(|error| error.to_string())?;
    if now {
        // An empty uuid names whatever the session holds for the running tool call.
        let _ = host.control(
            session,
            SessionControl::SendNow {
                uuid: String::new(),
            },
        );
    }
    Ok(SteerAck {
        session: session.clone(),
        how,
    })
}

/// Posted as a `Mention`, queued on the target sessions for their next turn. `@consultant`
/// asks the consultant instead, whose answer comes back to you.
pub async fn mention(
    host: &Host,
    target: MentionTarget,
    text: String,
) -> Result<MessageId, String> {
    let to = recipient(host, target)?;
    let asks_consultant = to
        == Recipient::Role {
            role: RoleName::Consultant,
        };
    if !asks_consultant {
        return mention_to(host, to, text).map_err(|error| error.to_string());
    }
    let question = Post {
        kind: BlackboardKind::Question,
        ..from_you(to, text)
    };
    let id = post(host, question).map_err(|error| error.to_string())?;
    // The consultant's session takes minutes, and the loop never waits on a long command.
    let host = host.clone();
    let question = id.clone();
    tokio::spawn(async move {
        if let Err(error) = consultant::answer(&host, &question).await {
            let note = Note {
                level: NoteLevel::Warn,
                text: format!("The consultant couldn't answer {question}: {error}"),
            };
            if let Err(error) = host.record(None, None, RunEventKind::Note(note)) {
                tracing::error!(%question, %error, "recording the consultant's failure");
            }
        }
    });
    Ok(id)
}

/// A mention of yours for `to`: queued on its running sessions for their next turn, and put
/// in the first prompt of one that starts later.
pub fn mention_to(host: &Host, to: Recipient, text: String) -> EngineResult<MessageId> {
    let id = post(host, from_you(to, text))?;
    delivery::to_running(host, &id)?;
    Ok(id)
}

fn from_you(to: Recipient, text: String) -> Post {
    Post {
        kind: BlackboardKind::Mention,
        from: Party::User,
        to,
        text,
        in_reply_to: None,
        refs: Vec::new(),
        from_task: None,
    }
}

/// Posted as `Guidance` for every working agent's next turn.
pub async fn guidance(host: &Host, text: String) -> Result<MessageId, String> {
    let id = post(
        host,
        Post {
            kind: BlackboardKind::Guidance,
            from: Party::User,
            to: Recipient::All,
            text,
            in_reply_to: None,
            refs: Vec::new(),
            from_task: None,
        },
    )
    .map_err(|error| error.to_string())?;
    delivery::to_running(host, &id).map_err(|error| error.to_string())?;
    Ok(id)
}

/// Removes the undelivered ones and returns them. A message already passed to a session for its
/// next turn has left the blackboard and can't be taken back.
pub fn take_back(host: &Host, messages: &[MessageId]) -> Vec<QueuedMessage> {
    let mut taken = Vec::new();
    for id in messages {
        let message = host.read(|state| {
            let board = &state.blackboard;
            board
                .get(id)
                .filter(|message| {
                    message.from == Party::User
                        && matches!(
                            message.kind,
                            BlackboardKind::Guidance | BlackboardKind::Mention
                        )
                        && !board.delivered_anywhere(id)
                        && !board.taken_back.contains(id)
                })
                .cloned()
        });
        let Some(message) = message else {
            continue;
        };
        let mark = Post {
            kind: message.kind,
            from: Party::User,
            to: Recipient::User,
            text: String::new(),
            in_reply_to: Some(id.clone()),
            refs: vec![TAKEN_BACK.to_owned()],
            from_task: None,
        };
        match post(host, mark) {
            Ok(_) => taken.push(QueuedMessage {
                id: id.clone(),
                from: Party::User,
                to: typed_target(&message.to),
                text: message.text,
                queued_at: host.now(),
                delivery: Delivery::SessionStart,
            }),
            Err(error) => tracing::warn!(%id, %error, "taking a message back"),
        }
    }
    taken
}

fn recipient(host: &Host, target: MentionTarget) -> Result<Recipient, String> {
    Ok(match target {
        MentionTarget::Worker(worker) => Recipient::Worker { worker },
        MentionTarget::Task(task) => {
            if host.read(|state| state.tasks.get(&task).is_none()) {
                return Err(format!("There's no {task} in this run"));
            }
            Recipient::Task { task }
        }
        MentionTarget::Role(role) => Recipient::Role { role },
        MentionTarget::Number(key) => Recipient::Session {
            session: following(host, key)
                .ok_or_else(|| format!("No running agent has the key {key}"))?,
        },
        MentionTarget::All => Recipient::All,
    })
}

/// The running session the UI shows under this follow key.
fn following(host: &Host, key: u8) -> Option<SessionKey> {
    let running = host.shared().live.keys();
    host.read(|state| {
        running.into_iter().find(|session| {
            state
                .sessions
                .get(session)
                .is_some_and(|record| record.started.follow_key == Some(key))
        })
    })
}

/// The target as you typed it, as near as the recipient tells.
fn typed_target(to: &Recipient) -> MentionTarget {
    match to {
        Recipient::Worker { worker } => MentionTarget::Worker(worker.clone()),
        Recipient::Task { task } => MentionTarget::Task(task.clone()),
        Recipient::Role { role } => MentionTarget::Role(*role),
        Recipient::All | Recipient::Session { .. } | Recipient::User => MentionTarget::All,
    }
}
