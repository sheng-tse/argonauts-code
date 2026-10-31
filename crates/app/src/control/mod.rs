//! JSON lines between a client and a run's run host, on `runs/<run>/control.sock` (mode 0600).
//!
//! Agent sandboxes can't read the state directory, so they can't reach the socket. Approval
//! requests (`Answer`, `ApprovalMode`, `TakeOver`, `TakeOverEnded`, and a rule-overriding
//! `Reassign`) also carry the token from `runs/<run>/approve.token`, and are refused when the
//! caller descends from an agent process or has `ARGONAUTS_SESSION` set.

use serde::{Deserialize, Serialize};

use argonauts_core::base::names::{ApprovalId, MessageId, ModelRef, SessionKey};
use argonauts_core::input::MentionTarget;
use argonauts_core::runs::api::{
    ApprovalAnswer, DiffScope, DiffView, RawView, StatusSummary, SteerAck, TakeOverTicket,
};
use argonauts_core::runs::events::RunEvent;
use argonauts_core::runs::events::council::CouncilAction;
use argonauts_core::runs::events::goal::GoalAction;
use argonauts_core::runs::events::session::QueuedMessage;
use argonauts_core::settings::approvals::ApprovalMode;
use argonauts_core::{RunId, TaskId};

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
        /// Hand it to the model now, where the session's CLI can.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        now: bool,
    },
    Mention {
        target: MentionTarget,
        text: String,
    },
    /// For every working agent.
    Guidance {
        text: String,
        /// The MCP client's own name, logged as `Party::OutsideAgent`. `None` is you.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        outside_agent: Option<String>,
    },
    /// Pause at the next safe point, or stop the agents now.
    Pause {
        now: bool,
        /// As for `Guidance`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        outside_agent: Option<String>,
    },
    CancelPause,
    Stop,
    StopSession {
        session: SessionKey,
        /// Starts it over with the prompt it had once it has stopped.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        again: bool,
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
        task: TaskId,
        to: ModelRef,
        /// A role rule to override for this task only. Needs the token.
        #[serde(default)]
        rule_override: Option<String>,
        #[serde(default)]
        token: Option<String>,
    },
    /// App clients only.
    TakeOver {
        task: TaskId,
        token: String,
    },
    /// App clients only.
    TakeOverEnded {
        task: TaskId,
        token: String,
    },
    /// App clients only.
    Answer {
        answer: ApprovalAnswer,
        token: String,
    },
    Status,
    Diff {
        task: TaskId,
        scope: DiffScope,
    },
    Raw {
        session: SessionKey,
    },
    /// Reset a task to checkpoint `fallback-<checkpoint>`. The work after it stays on a side
    /// branch.
    Rewind {
        task: TaskId,
        checkpoint: u32,
    },
    /// Your verdict on an auto-approval from the spot-check queue.
    SpotCheck {
        id: ApprovalId,
        correct: bool,
    },
    /// Clear the goal, turn the workflow into one, or change an assumption.
    Goal {
        action: GoalAction,
    },
    /// Write to the council that talks, to one of its seats alone, or have a seat sit out.
    Council {
        action: CouncilAction,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "reply", rename_all = "snake_case")]
pub enum ControlReply {
    Welcome {
        run: RunId,
        /// The run host's process id.
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

#[cfg(test)]
mod tests;
pub(crate) mod wire;
