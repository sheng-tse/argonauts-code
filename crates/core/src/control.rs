//! JSON lines between a client and a run's supervisor, on `runs/<run>/control.sock` (mode 0600).
//!
//! Agent sandboxes can't read the state directory, so they can't reach the socket. Approval
//! requests (`Answer`, `ApprovalMode`, `TakeOver`, `TakeOverEnded`, and a rule-overriding
//! `Reassign`) also carry the token from `runs/<run>/approve.token`, and are refused when the
//! peer descends from an agent process or has `ARGONAUTS_SESSION` set.

use serde::{Deserialize, Serialize};

use crate::api::{
    CheckpointAnswer, DiffScope, DiffView, RawView, StatusSummary, SteerAck, TakeOverTicket,
};
use crate::events::RunEvent;
use crate::events::session::QueuedMessage;
use crate::input::MentionTarget;
use crate::names::{MessageId, SessionKey, WorkerRef};
use crate::settings::approvals::ApprovalMode;
use crate::{CardId, RunId};

/// Replies echo the request's `id`; a `Subscribe` reply streams `Event` frames with that id.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Frame<T> {
    pub id: u64,
    pub body: T,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClientKind {
    App,
    Mcp,
    Cli,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "request", rename_all = "snake_case")]
pub enum ControlRequest {
    /// Must be the first frame.
    Hello {
        client: ClientKind,
        version: String,
    },
    Subscribe {
        from_seq: u64,
        /// Start at the latest `Run::Snapshot` instead, for a reattach.
        #[serde(default)]
        from_snapshot: bool,
    },
    Steer {
        session: SessionKey,
        text: String,
    },
    Mention {
        target: MentionTarget,
        text: String,
    },
    /// For every working agent.
    Guidance {
        text: String,
        /// The MCP client's own name, logged as `Party::FrontEnd`. `None` is you.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        front_end: Option<String>,
    },
    /// Pause at the next safe point, or stop the agents now.
    Pause {
        now: bool,
        /// As for `Guidance`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        front_end: Option<String>,
    },
    CancelPause,
    Stop,
    StopSession {
        session: SessionKey,
    },
    /// Take undelivered messages back.
    TakeBack {
        messages: Vec<MessageId>,
    },
    /// App clients only.
    ApprovalMode {
        mode: ApprovalMode,
        token: String,
    },
    Reassign {
        card: CardId,
        to: WorkerRef,
        /// A role rule to override for this card only. Needs the token.
        #[serde(default)]
        rule_override: Option<String>,
        #[serde(default)]
        token: Option<String>,
    },
    /// App clients only.
    TakeOver {
        card: CardId,
        token: String,
    },
    /// App clients only.
    TakeOverEnded {
        card: CardId,
        token: String,
    },
    /// App clients only.
    Answer {
        answer: CheckpointAnswer,
        token: String,
    },
    Status,
    Diff {
        card: CardId,
        scope: DiffScope,
    },
    Raw {
        session: SessionKey,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "reply", rename_all = "snake_case")]
pub enum ControlReply {
    Welcome {
        run: RunId,
        /// The supervisor's process id.
        pid: u32,
        version: String,
    },
    Ok,
    Error {
        message: String,
    },
    Event {
        event: Box<RunEvent>,
    },
    /// The reply to `Mention` and `Guidance`; the id is what `TakeBack` takes.
    Queued {
        id: MessageId,
    },
    TakenBack {
        messages: Vec<QueuedMessage>,
    },
    Steered {
        ack: SteerAck,
    },
    TakeOver {
        ticket: TakeOverTicket,
    },
    Status {
        summary: Box<StatusSummary>,
    },
    Diff {
        diff: Box<DiffView>,
    },
    Raw {
        raw: Box<RawView>,
    },
}
