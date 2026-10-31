//! Each control request as the engine command that carries it, and the reply to wait for.

use argonauts_core::base::names::MessageId;
use argonauts_core::runs::api::{DiffView, RawView, StatusSummary, SteerAck, TakeOverTicket};
use argonauts_core::runs::engine::EngineCommand;
use argonauts_core::runs::events::session::QueuedMessage;
use tokio::sync::oneshot;

use super::error;
use crate::control::{ControlReply, ControlRequest};

/// What the engine will answer for one command.
pub(crate) enum EngineReply {
    /// Commands the engine doesn't answer. Taken is done.
    Queued,
    Unit(oneshot::Receiver<Result<(), String>>),
    Message(oneshot::Receiver<Result<MessageId, String>>),
    Steered(oneshot::Receiver<Result<SteerAck, String>>),
    TakenBack(oneshot::Receiver<Vec<QueuedMessage>>),
    TakeOver(oneshot::Receiver<Result<TakeOverTicket, String>>),
    Status(oneshot::Receiver<StatusSummary>),
    Diff(oneshot::Receiver<Result<DiffView, String>>),
    Raw(oneshot::Receiver<Result<RawView, String>>),
}

impl EngineReply {
    /// The engine already answered, so it surely took the command.
    pub(crate) fn answered(&self) -> bool {
        match self {
            Self::Queued => false,
            Self::Unit(reply) => !reply.is_empty(),
            Self::Message(reply) => !reply.is_empty(),
            Self::Steered(reply) => !reply.is_empty(),
            Self::TakenBack(reply) => !reply.is_empty(),
            Self::TakeOver(reply) => !reply.is_empty(),
            Self::Status(reply) => !reply.is_empty(),
            Self::Diff(reply) => !reply.is_empty(),
            Self::Raw(reply) => !reply.is_empty(),
        }
    }

    pub(crate) async fn wait(self) -> ControlReply {
        match self {
            Self::Queued => Ok(ControlReply::Ok),
            Self::Unit(reply) => answered(reply.await, |()| ControlReply::Ok),
            Self::Message(reply) => answered(reply.await, |id| ControlReply::Queued { id }),
            Self::Steered(reply) => answered(reply.await, |ack| ControlReply::Steered { ack }),
            Self::TakenBack(reply) => reply
                .await
                .map(|messages| ControlReply::TakenBack { messages }),
            Self::TakeOver(reply) => {
                answered(reply.await, |ticket| ControlReply::TakeOver { ticket })
            }
            Self::Status(reply) => reply.await.map(|summary| ControlReply::Status {
                summary: Box::new(summary),
            }),
            Self::Diff(reply) => answered(reply.await, |diff| ControlReply::Diff {
                diff: Box::new(diff),
            }),
            Self::Raw(reply) => {
                answered(reply.await, |raw| ControlReply::Raw { raw: Box::new(raw) })
            }
        }
        .unwrap_or_else(|_closed| error("the run stopped before it answered; send it again"))
    }
}

fn answered<T>(
    reply: Result<Result<T, String>, oneshot::error::RecvError>,
    ok: impl FnOnce(T) -> ControlReply,
) -> Result<ControlReply, oneshot::error::RecvError> {
    reply.map(|answer| match answer {
        Ok(value) => ok(value),
        Err(message) => ControlReply::Error { message },
    })
}

pub(crate) fn command(request: ControlRequest) -> Result<(EngineCommand, EngineReply), String> {
    let made = match request {
        ControlRequest::Hello { .. } | ControlRequest::Subscribe { .. } => {
            return Err("hello and subscribe belong to a connection".to_owned());
        }
        ControlRequest::Steer { session, text, now } => {
            let (reply, wait) = oneshot::channel();
            let command = EngineCommand::Steer {
                session,
                text,
                now,
                reply,
            };
            (command, EngineReply::Steered(wait))
        }
        ControlRequest::Mention { target, text } => {
            let (reply, wait) = oneshot::channel();
            let command = EngineCommand::Mention {
                target,
                text,
                reply,
            };
            (command, EngineReply::Message(wait))
        }
        ControlRequest::Guidance {
            text,
            outside_agent: _,
        } => {
            let (reply, wait) = oneshot::channel();
            (
                EngineCommand::Guidance { text, reply },
                EngineReply::Message(wait),
            )
        }
        ControlRequest::Pause {
            now,
            outside_agent: _,
        } => (EngineCommand::Pause { now }, EngineReply::Queued),
        ControlRequest::CancelPause => (EngineCommand::CancelPause, EngineReply::Queued),
        ControlRequest::Stop => (EngineCommand::Stop, EngineReply::Queued),
        ControlRequest::StopSession { session, again } => (
            EngineCommand::StopSession { session, again },
            EngineReply::Queued,
        ),
        ControlRequest::TakeBack { messages } => {
            let (reply, wait) = oneshot::channel();
            (
                EngineCommand::TakeBack { messages, reply },
                EngineReply::TakenBack(wait),
            )
        }
        ControlRequest::ApprovalMode { mode, token: _ } => {
            (EngineCommand::ApprovalMode { mode }, EngineReply::Queued)
        }
        ControlRequest::Reassign {
            task,
            to,
            rule_override,
            token: _,
        } => {
            let (reply, wait) = oneshot::channel();
            let command = EngineCommand::Reassign {
                task,
                to,
                rule_override,
                reply,
            };
            (command, EngineReply::Unit(wait))
        }
        ControlRequest::TakeOver { task, token: _ } => {
            let (reply, wait) = oneshot::channel();
            (
                EngineCommand::TakeOver { task, reply },
                EngineReply::TakeOver(wait),
            )
        }
        ControlRequest::TakeOverEnded { task, token: _ } => {
            (EngineCommand::TakeOverEnded { task }, EngineReply::Queued)
        }
        ControlRequest::Answer { answer, token: _ } => {
            let (reply, wait) = oneshot::channel();
            (
                EngineCommand::Answer { answer, reply },
                EngineReply::Unit(wait),
            )
        }
        ControlRequest::Status => {
            let (reply, wait) = oneshot::channel();
            (EngineCommand::Status { reply }, EngineReply::Status(wait))
        }
        ControlRequest::Diff { task, scope } => {
            let (reply, wait) = oneshot::channel();
            (
                EngineCommand::Diff { task, scope, reply },
                EngineReply::Diff(wait),
            )
        }
        ControlRequest::Raw { session } => {
            let (reply, wait) = oneshot::channel();
            (
                EngineCommand::Raw { session, reply },
                EngineReply::Raw(wait),
            )
        }
        ControlRequest::Rewind { task, checkpoint } => {
            let (reply, wait) = oneshot::channel();
            let command = EngineCommand::Rewind {
                task,
                checkpoint,
                reply,
            };
            (command, EngineReply::Unit(wait))
        }
        ControlRequest::SpotCheck { id, correct } => {
            let (reply, wait) = oneshot::channel();
            let command = EngineCommand::SpotCheck { id, correct, reply };
            (command, EngineReply::Unit(wait))
        }
        ControlRequest::Goal { action } => {
            let (reply, wait) = oneshot::channel();
            (
                EngineCommand::Goal { action, reply },
                EngineReply::Unit(wait),
            )
        }
        ControlRequest::Council { action } => {
            let (reply, wait) = oneshot::channel();
            (
                EngineCommand::Council { action, reply },
                EngineReply::Unit(wait),
            )
        }
    };
    Ok(made)
}
