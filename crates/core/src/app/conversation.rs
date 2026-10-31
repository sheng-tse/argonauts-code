//! What the conversation with the main agent sends the screen: each turn as it starts, works
//! and ends, and what the turn turned out to be.

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

use crate::base::names::{ModelRef, TurnId};
use crate::runs::events::context::ContextUsage;
use crate::runs::events::run::{CaptainOrCrewDecision, CheckedInstruction, RunMode};
use crate::workflows::steps::intake::ChangeEstimate;
use crate::{AgentEvent, Spend};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "conversation", rename_all = "snake_case")]
pub enum ConversationEvent {
    TurnStarted {
        turn: TurnId,
        text: String,
        worker: ModelRef,
        /// False only for a direct turn, which may change your files.
        read_only: bool,
        commit: String,
        /// Uncommitted paths left out.
        uncommitted: Vec<String>,
        #[serde(default)]
        settings_sha256: Option<String>,
        /// The turn goes on from the one before, as a direct turn does after its estimate, so
        /// your message isn't shown again.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        continues: bool,
        /// A change it finds goes to a workflow that talks, whose conversation opens with your
        /// message, so the screen holds the message until the reply says what it is.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        opens_a_talk: bool,
    },
    Agent {
        turn: TurnId,
        event: AgentEvent,
    },
    TurnFinished {
        turn: TurnId,
        reply: TurnReply,
        spend: Spend,
        elapsed_ms: u64,
    },
    /// The fallback agent may retry it.
    TurnFailed {
        turn: TurnId,
        error: String,
    },
    TurnCancelled {
        turn: TurnId,
    },
    /// A watchdog stopped the turn, since it repeated one call or went quiet. Nothing falls
    /// back: another agent would get the same request.
    TurnStopped {
        turn: TurnId,
        /// The line under the turn: `Stopped: Qwen3 Coder 30B repeated the same execute call
        /// 12 times.`
        why: String,
    },
    /// A turn's agent ran out, and `to` carries on the same request in the same folder.
    FellBack {
        turn: TurnId,
        from: ModelRef,
        to: ModelRef,
        /// Such as "hit its usage limit".
        why: String,
        /// What `to` was told, such as "your request, the transcript so far, and that
        /// invoice/money.py changed".
        told: String,
        /// Set when `from`'s CLI turned out to be signed out, as its error said.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        signed_out: Option<SignedOut>,
    },
    /// A message of yours that went into the running turn, once the model read it, as Claude
    /// Code's send now shows it.
    Joined {
        turn: TurnId,
        text: String,
    },
    /// The answer didn't fit the format the turn asked for, so its agent is asked for it again.
    /// What it said so far is set aside, and only the answer it ends with prints.
    AnswerRetried {
        turn: TurnId,
    },
    /// How the turn left its session's context, kept in the record as the turn ends so a
    /// resumed conversation counts it at once. The screen has it already while the turn runs.
    ContextLeft {
        turn: TurnId,
        /// When the turn ended.
        #[serde(with = "time::serde::rfc3339")]
        at: OffsetDateTime,
        /// In tokens.
        used: u64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        window: Option<u64>,
        /// The CLI's own last count of what fills it, where its CLI counts one.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        usage: Option<Box<ContextUsage>>,
    },
    /// A slash command you typed, kept in the record where you typed it, so the conversation
    /// drawn again prints it. Live, the screen has printed it already.
    CommandTyped {
        text: String,
        /// When you typed it, so a run's record drawn again goes around it as it printed.
        /// Older records have none.
        #[serde(
            default,
            skip_serializing_if = "Option::is_none",
            with = "time::serde::rfc3339::option"
        )]
        at: Option<OffsetDateTime>,
    },
    /// A line a command you typed printed back, kept after the command so the conversation
    /// drawn again prints it under it. Live, the screen has printed it already.
    CommandReplied {
        reply: CommandReply,
    },
}

/// One line a command you typed printed back under its echo: `Kept model as Opus 5.5`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommandReply {
    pub text: String,
    /// Drawn after `text` in the accent color: `Opus 5.5`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<String>,
    /// Dim words after the value, such as who the change is for.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub after: Option<String>,
    /// The whole line is dim, as the line a closing panel leaves is.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub dim: bool,
    /// The whole line is in the error colour, as a command's refusal is.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub error: bool,
}

/// A CLI whose error said its sign-in is gone, and how to sign it in again.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SignedOut {
    /// What became of its sign-in, such as "its sign-in was revoked". Empty when its CLI only
    /// asked to log in again.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub why: String,
    /// The command that signs it in again, such as `claude /login`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub login: Option<String>,
}

/// What a conversation turn turned out to be.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "reply", rename_all = "snake_case")]
pub enum TurnReply {
    Answer {
        markdown: String,
    },
    /// The Captain-or-crew rules picked a mode and the run starts, or for direct Captain the
    /// session goes ahead in your checkout.
    Change {
        /// The agent's short plan of attack.
        markdown: String,
        estimate: ChangeEstimate,
        captain_or_crew: CaptainOrCrewDecision,
        /// What the request said about how to run it, checked, for the start line of a Captain
        /// that works in your checkout. A run's own start line has them from its start.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        instructions: Vec<CheckedInstruction>,
    },
    /// Could be either; a choice question follows.
    Unclear {
        markdown: String,
        estimate: Option<ChangeEstimate>,
        would_pick: Option<RunMode>,
    },
}
