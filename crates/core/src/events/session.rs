//! Sessions: exactly what was sent, queued messages, limits, subagents, and how they ended.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use time::OffsetDateTime;

use super::run::Party;
use crate::input::MentionTarget;
use crate::limits::QuotaReading;
use crate::names::{MessageId, RoleName, VendorId, WorkerRef};
use crate::settings::sessions::SessionLifetime;
use crate::steps::StepName;
use crate::worker::{Access, WorkerKind};
use crate::workflow::StepRef;
use crate::{BudgetDimension, BudgetStatus, ErrorCategory, Spend};

/// A session's life. Agent output itself arrives as `RunEventKind::Agent`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum SessionEvent {
    Started(Box<SessionStarted>),
    /// Exactly what Argonauts sent. Emitted at start with every part, and after each delivered
    /// message with only the new parts, so the journal never repeats a prompt.
    Input(Box<SessionInput>),
    VendorSession {
        /// The vendor's id, used to resume and take over.
        id: String,
        transcript: Option<PathBuf>,
    },
    /// A message waits for delivery (an `@mention`, guidance, a blackboard message).
    Queued(QueuedMessage),
    Delivered {
        message: MessageId,
        via: Delivery,
    },
    /// A file changed; the conductor diffed it against the last commit, for the live preview.
    FileDiff(FileDiff),
    Limit(LimitEvent),
    Ended(Box<SessionEnd>),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionStarted {
    pub worker: WorkerRef,
    pub vendor: VendorId,
    pub family: String,
    pub kind: WorkerKind,
    pub role: RoleName,
    pub step: Option<StepName>,
    pub access: Access,
    pub lifetime: SessionLifetime,
    /// The earlier vendor session it resumes.
    pub resumed_from: Option<String>,
    /// The key number the UI shows in `[1]`, stable while the session runs.
    pub follow_key: Option<u8>,
    /// The workflow step it works for, which places it in the workflow view.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workflow_step: Option<StepRef>,
    /// Its name in the chat, the panel, and the workflow view, such as `fix:slugify`, unique
    /// in the run. Card ids stay in Ctrl+O and commands.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionInput {
    pub launch: LaunchRecord,
    /// Each injected part, in order. At start their concatenation is the prompt, byte for
    /// byte; after a delivery they are only what that delivery added.
    pub parts: Vec<InputPart>,
    pub queued: Vec<QueuedMessage>,
}

/// The command line, with secrets removed.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LaunchRecord {
    pub program: String,
    /// The prompt argument is replaced by `<prompt>`.
    pub args: Vec<String>,
    pub cwd: PathBuf,
    /// Names of environment variables set. Values are never recorded.
    pub env_set: Vec<String>,
    pub env_removed: Vec<String>,
    pub prompt_via: PromptVia,
    /// The sandbox profile, in one line, such as "seatbelt: write worktree+tmp, no network".
    pub sandbox: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PromptVia {
    Argument,
    /// On stdin, once.
    Stdin,
    /// As JSON lines on a stdin that stays open for steering.
    StreamInput,
    /// In an HTTP request body (endpoints).
    Request,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InputPart {
    pub kind: InputPartKind,
    /// The label the input side shows, such as `notes`.
    pub label: String,
    /// The text, exactly as sent.
    pub text: String,
    /// Estimated tokens, about 4 characters each.
    pub approx_tokens: u32,
    /// Where it came from, such as `.argonauts/notes/redis.md` or `D-003`.
    pub source: Option<String>,
}

/// Kinds of prompt part, in the order the session view lists them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InputPartKind {
    /// The role's instructions.
    Prompt,
    Card,
    Goal,
    Notes,
    Decisions,
    /// Rules: AGENTS.md pointer, forbidden paths, dependency rules.
    Rules,
    /// The live status summary, filtered for the role.
    Status,
    Handoff,
    /// Gate output for a retry.
    GateOutput,
    Message,
    WrapUp,
    /// An evidence packet (approver) or digest (narrator).
    Evidence,
    /// The step's input, as JSON.
    StepInput,
    Schema,
    /// A file or folder you mentioned with `@`.
    File,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct QueuedMessage {
    pub id: MessageId,
    pub from: Party,
    /// As typed.
    pub to: MentionTarget,
    /// A blind drafter or reviewer gets only this, nothing attached.
    pub text: String,
    #[serde(with = "time::serde::rfc3339")]
    pub queued_at: OffsetDateTime,
    pub delivery: Delivery,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Delivery {
    /// `s` in the session view: after the current tool call, in the same session.
    Steer,
    /// At the agent's next turn, which for an owner can be the end of its card.
    NextTurn,
    /// At the next session start (blackboard delivery point).
    SessionStart,
}

/// One file's change, diffed by the conductor, so previews look the same for every vendor.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FileDiff {
    /// Relative to the worktree.
    pub path: String,
    pub added: u32,
    pub removed: u32,
    /// The unified hunks, cut to 40 lines.
    pub hunk: String,
    pub truncated: bool,
}

/// Tagged with `kind`, not `event`: it sits inside `SessionEvent::Limit`, whose own tag is
/// `event`, and one JSON object can't carry the same key twice.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum LimitEvent {
    /// The meter's state, at most once a second per session.
    Budget(BudgetStatus),
    /// The fixed wrap-up message was sent at the soft stop.
    WrapUpSent {
        via: WrapUpVia,
        /// Which limit was at 85%.
        dimension: BudgetDimension,
    },
    /// The same tool call repeated; the agent got a note.
    Nudged {
        call: String,
        /// How many times in a row.
        times: u32,
    },
    /// A native cap was set as a backstop, such as Claude Code's `--max-budget-usd`.
    NativeCap { usd: f64 },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WrapUpVia {
    /// Through the session's open input stream.
    Input,
    /// Interrupt, then resume the same session with the message (Codex).
    InterruptResume,
    /// Interrupt only; the harness can't take more input.
    Interrupt,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionEnd {
    pub vendor_session: Option<String>,
    /// Harnesses only.
    pub exit_code: Option<i32>,
    pub signal: Option<i32>,
    /// Wall time.
    pub duration_ms: u64,
    pub reason: EndReason,
    pub spend: Spend,
    /// Tool calls started, subagents' included where visible.
    pub tool_calls: u64,
    pub result_text: Option<String>,
    /// Structured output, when a schema was enforced.
    pub structured: Option<Value>,
    /// The last complete top-level message.
    pub last_message: Option<String>,
    /// Argonauts' raw copy of the vendor's output lines, for `r`.
    pub raw_log: Option<PathBuf>,
}

/// Why a session ended. The adapter's error mapping and the meter decide this; the limits
/// service turns it into an action.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "why", rename_all = "snake_case")]
pub enum EndReason {
    Completed,
    /// The meter stopped it at 100% of a limit.
    BudgetHit {
        dimension: BudgetDimension,
    },
    /// Stopped after its tool call because the run paused.
    Paused,
    /// You stopped it, or `/stop`, or `/reassign`.
    Stopped,
    Stuck {
        watchdog: Watchdog,
    },
    /// The plan's usage limit.
    UsageLimit {
        #[serde(with = "time::serde::rfc3339::option")]
        resets_at: Option<OffsetDateTime>,
    },
    /// Rate limited for longer than the card's time budget allows.
    RateLimited,
    /// Overloaded or a server error that the harness gave up on.
    VendorError {
        category: ErrorCategory,
        message: String,
    },
    /// Logged out or the token expired.
    Auth {
        /// Which login to fix.
        message: String,
    },
    /// Credits or a spend cap ran out.
    Billing {
        message: String,
    },
    /// The context window is full. The session rotates.
    ContextFull,
    ModelUnavailable {
        model: String,
    },
    /// The harness produced output outside the adapter's tested range.
    AdapterBroken {
        detail: String,
    },
    StartFailed {
        message: String,
    },
    /// Anything else: a crash or an exit without a result.
    Crashed {
        /// Why, from the last error or stderr.
        message: String,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Watchdog {
    /// No events, CPU, or output from the process tree.
    Inactivity,
    /// The same call repeated after a nudge.
    Repeat,
}

/// A vendor subagent, workflow, or thread changed state. Added to `AgentEvent` as
/// `Subagent { update }`, emitted by adapters.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SubagentUpdate {
    /// The child's id: a tool call id (Claude Code), a thread id (Codex), a conversation id (agy).
    pub id: String,
    /// The parent's id, `None` for children of the main agent.
    pub parent: Option<String>,
    pub kind: SubagentKind,
    /// Its name or type, such as `explorer` or `research`.
    pub label: Option<String>,
    /// Shortened.
    pub prompt: Option<String>,
    pub status: SubagentStatus,
    pub last_message: Option<String>,
    /// A link to its own log (agy).
    pub log_uri: Option<String>,
    /// Whether its own tool calls stream to us. False for Codex threads, and the UI says so.
    pub steps_visible: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SubagentKind {
    Subagent,
    Workflow,
    /// A Codex thread.
    Thread,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SubagentStatus {
    Started,
    Running,
    Done,
    Failed,
    /// The vendor didn't say.
    Unknown,
}

/// A workflow an agent's own CLI runs, as its stream reports it: Claude Code's dynamic
/// workflows (`task_type: local_workflow`). Each update carries the whole state, replacing the
/// last one of the same id, so the views can draw it like one of Argonauts' own, with whatever
/// names it uses.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct VendorWorkflow {
    /// The vendor's task id.
    pub id: String,
    /// The tool call that started it, which is also its subagent id.
    pub call: Option<String>,
    /// The script's `meta.name`, such as `fix-slugify-median`.
    pub name: String,
    /// The script's `meta.description`.
    pub description: String,
    /// In the order declared, from the script's `meta.phases` and then the progress reports.
    pub phases: Vec<VendorPhase>,
    /// In the order they started.
    pub agents: Vec<VendorAgent>,
    pub status: SubagentStatus,
    /// The whole workflow's totals, as last reported.
    pub tokens: u64,
    pub tool_calls: u32,
    pub duration_ms: u64,
    /// What it's doing now, or how it ended.
    pub summary: Option<String>,
    /// When Argonauts first saw it, in milliseconds since the epoch. Its time counts from here
    /// while it runs, on Argonauts' own clock, so a replayed stream reads right too.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_at_ms: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VendorPhase {
    /// From 1, as the vendor numbers them.
    pub index: u32,
    pub title: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub detail: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct VendorAgent {
    /// From 1, in the order the vendor started them.
    pub index: u32,
    /// The script's label, such as `fix:slugify`.
    pub label: String,
    /// The phase it belongs to, by the vendor's index.
    pub phase: Option<u32>,
    pub model: Option<String>,
    pub state: VendorAgentState,
    pub tokens: u64,
    pub tool_calls: u32,
    pub duration_ms: u64,
    /// Unix milliseconds.
    pub started_at_ms: Option<u64>,
    /// 1 the first time.
    pub attempt: u32,
    /// Shortened.
    pub prompt: Option<String>,
    /// The start of its answer, once done.
    pub result: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VendorAgentState {
    Queued,
    Running,
    Done,
    Failed,
    /// A state this version of Argonauts doesn't know yet.
    Other,
}

impl VendorWorkflow {
    /// The agents of one phase, by the vendor's index.
    pub fn agents_in(&self, phase: u32) -> impl Iterator<Item = &VendorAgent> {
        self.agents
            .iter()
            .filter(move |agent| agent.phase == Some(phase))
    }

    /// Done of all, for a phase with agents. `None` for one that hasn't started.
    pub fn count(&self, phase: u32) -> Option<(u32, u32)> {
        let (done, total) = self.agents_in(phase).fold((0, 0), |(done, total), agent| {
            let finished = matches!(agent.state, VendorAgentState::Done);
            (done + u32::from(finished), total + 1)
        });
        (total > 0).then_some((done, total))
    }

    /// Whether it's over, done or failed.
    pub fn ended(&self) -> bool {
        matches!(self.status, SubagentStatus::Done | SubagentStatus::Failed)
    }

    /// Ends it with its session, which stopped before the workflow said it ended: it and every
    /// agent still going count as failed, and `why` says so.
    pub fn end_with_session(&mut self, why: &str) {
        if self.ended() {
            return;
        }
        self.status = SubagentStatus::Failed;
        self.summary = Some(why.to_owned());
        for agent in &mut self.agents {
            if !matches!(
                agent.state,
                VendorAgentState::Done | VendorAgentState::Failed
            ) {
                agent.state = VendorAgentState::Failed;
            }
        }
    }

    /// How long it has run by `now_ms` (milliseconds since the epoch): from its start while it
    /// runs, else its own total.
    pub fn elapsed_ms(&self, now_ms: Option<u64>) -> u64 {
        match (self.ended(), self.started_at_ms, now_ms) {
            (false, Some(started), Some(now)) => now.saturating_sub(started).max(self.duration_ms),
            _ => self.duration_ms,
        }
    }

    /// How long `agent` has run by `now_ms`: its own total once it ends, which is all the
    /// stream reports. While it runs, from the workflow's start here plus how much later than
    /// the first agent the stream says it began, so the vendor's clock never has to agree with
    /// ours.
    pub fn agent_elapsed_ms(&self, agent: &VendorAgent, now_ms: Option<u64>) -> u64 {
        let running = matches!(
            agent.state,
            VendorAgentState::Running | VendorAgentState::Other
        );
        let first = self
            .agents
            .iter()
            .filter_map(|agent| agent.started_at_ms)
            .min();
        match (
            running,
            self.started_at_ms,
            agent.started_at_ms,
            first,
            now_ms,
        ) {
            (true, Some(start), Some(began), Some(first), Some(now)) => now
                .saturating_sub(start + began.saturating_sub(first))
                .max(agent.duration_ms),
            _ => agent.duration_ms,
        }
    }
}

/// A normalized quota reading, added to `AgentEvent` as `Quota { reading }`.
pub type AgentQuota = QuotaReading;
