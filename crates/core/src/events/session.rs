//! Sessions: exactly what was sent, queued messages, limits, subagents, and how they ended.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use time::OffsetDateTime;

use super::run::Party;
use crate::input::MentionTarget;
use crate::limits::PlanUsageReading;
use crate::names::{AgentId, MessageId, ModelRef, RoleName};
use crate::session_runner::{Access, RunnerKind};
use crate::settings::sessions::SessionLifetime;
use crate::steps::StepName;
use crate::workflow::StepRef;
use crate::{ErrorCategory, LimitDimension, Spend, SpendStatus};

/// A session's life. Agent output itself arrives as `RunEventKind::Agent`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum SessionEvent {
    Started(Box<SessionStarted>),
    /// Exactly what Argonauts Code sent. Emitted at start with every part, and after each delivered
    /// message with only the new parts, so the journal never repeats a prompt.
    Input(Box<SessionInput>),
    CliSession {
        /// The agent's id, used to resume and take over.
        id: String,
        transcript: Option<PathBuf>,
    },
    /// A message waits for delivery (an `@mention`, guidance, a blackboard message).
    Queued(QueuedMessage),
    Delivered {
        message: MessageId,
        via: Delivery,
    },
    /// A file changed; Argonauts Code diffed it against the last commit, for the live preview.
    FileDiff(FileDiff),
    Limit(LimitEvent),
    Ended(Box<SessionEnd>),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionStarted {
    pub worker: ModelRef,
    pub agent_id: AgentId,
    pub provider: String,
    pub kind: RunnerKind,
    pub role: RoleName,
    pub step: Option<StepName>,
    pub access: Access,
    pub lifetime: SessionLifetime,
    /// The earlier CLI session it resumes.
    pub resumed_from: Option<String>,
    /// The key number the UI shows in `[1]`, stable while the session runs.
    pub follow_key: Option<u8>,
    /// The workflow step it works for, which places it in the workflow view.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workflow_step: Option<StepRef>,
    /// Its name in the conversation, the panel, and the workflow view, such as `fix:slugify`,
    /// unique in the run. Task ids stay in Ctrl+O and commands.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    /// The model its step asked, when another runs it in that model's place because
    /// the asked one was out. The step finds the session again by it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub asked: Option<ModelRef>,
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

impl LaunchRecord {
    /// The model the command launched, as its `--model` or `-m` names it, which every CLI
    /// here takes. `None` where the CLI's own settings pick.
    pub fn model(&self) -> Option<&str> {
        let mut args = self.args.iter().map(String::as_str);
        while let Some(arg) = args.next() {
            let named = match arg.strip_prefix("--model=") {
                Some(model) => Some(model),
                None if matches!(arg, "--model" | "-m") => args.next(),
                None => None,
            };
            if let Some(model) = named.map(str::trim).filter(|model| !model.is_empty()) {
                return Some(model);
            }
        }
        None
    }
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
    Task,
    Goal,
    Notes,
    Decisions,
    /// Rules: AGENTS.md pointer, forbidden paths, dependency rules.
    Rules,
    /// The live status summary, filtered for the role.
    Status,
    Fallback,
    /// Check output for a retry.
    CheckOutput,
    Message,
    WrapUp,
    /// An evidence packet (judge) or digest (narrator).
    Evidence,
    /// The step's input, as JSON.
    StepInput,
    Schema,
    /// A file or folder you mentioned with `@`.
    File,
    /// The frame around an agent's instructions: its role, what it may touch, and the shape
    /// of its answer.
    Frame,
    /// What the agent is asked to do, by whoever wrote it: the orchestrator, a model you
    /// named, or Argonauts Code's own template.
    Instructions,
    /// Your own file for the role, such as `.argonauts/roles/reviewer.md`.
    RoleFile,
    /// What your request said for the role.
    Request,
    /// A picture sent with your message. Its `source` is the file; it adds no text.
    Picture,
    /// What earlier rounds of review found in the same work, each finding blocking or a note,
    /// and how the agent that asked answered it.
    EarlierFindings,
}

impl InputPartKind {
    /// Whether the part tells the agent what it is and what to do, rather than giving it
    /// something to work on. Endpoints send these as the system message.
    pub fn instructs(self) -> bool {
        matches!(
            self,
            Self::Prompt | Self::Frame | Self::Instructions | Self::RoleFile | Self::Request
        )
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct QueuedMessage {
    pub id: MessageId,
    pub from: Party,
    /// As typed.
    pub to: MentionTarget,
    /// A blind drafting agent or reviewer gets only this, nothing attached.
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
    /// At the agent's next turn, which for a worker can be the end of its task.
    NextTurn,
    /// At the next session start (blackboard delivery point).
    SessionStart,
}

/// One file's change, diffed by Argonauts Code, so previews look the same for every agent.
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
    Spend(SpendStatus),
    /// The fixed wrap-up message was sent at the soft stop.
    WrapUpSent {
        via: WrapUpVia,
        /// Which limit was at 85%.
        dimension: LimitDimension,
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
    /// Interrupt only; the CLI can't take more input.
    Interrupt,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionEnd {
    pub cli_session: Option<String>,
    /// CLIs only.
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
    /// Argonauts Code's raw copy of the agent's output lines, for `r`.
    pub raw_log: Option<PathBuf>,
}

/// Why a session ended. The adapter's error mapping and the meter decide this; the limits
/// service turns it into an action.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "why", rename_all = "snake_case")]
pub enum EndReason {
    Completed,
    /// The meter stopped it at 100% of a limit.
    LimitHit {
        dimension: LimitDimension,
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
    /// Rate limited for longer than the task's time limit allows.
    RateLimited,
    /// Overloaded or a server error that the CLI gave up on.
    CliError {
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
    /// The CLI produced output outside the adapter's tested range.
    AdapterBroken {
        detail: String,
    },
    /// An isolated session loaded your plugins or synced skills, which its role mustn't see.
    IsolationBroken {
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        plugins: Vec<String>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        skills: Vec<String>,
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

/// What an isolated session loaded, in words: `your plugins memory-keeper and lint-helper, and
/// 4 synced skills`. Skills are counted, since a synced set is often long.
pub fn loaded_words(plugins: &[String], skills: &[String]) -> String {
    let plugins = match plugins {
        [] => None,
        [one] => Some(format!("your plugin {one}")),
        [rest @ .., last] => Some(format!("your plugins {} and {last}", rest.join(", "))),
    };
    let skills = match skills.len() {
        0 => None,
        1 => Some(format!("your synced skill {}", skills[0])),
        n => Some(format!("{n} synced skills")),
    };
    match (plugins, skills) {
        (Some(plugins), Some(skills)) => format!("{plugins}, and {skills}"),
        (Some(only), None) | (None, Some(only)) => only,
        (None, None) => "your plugins".to_owned(),
    }
}

impl EndReason {
    /// The agent's side ended it: a login, a limit, an outage, a crash, or the network, or its
    /// CLI loaded your plugins into a session that must keep them out. Such an ending never
    /// ends a run; the step tries again or another agent takes the role.
    pub fn is_agent_failure(&self) -> bool {
        matches!(
            self,
            Self::Stuck { .. }
                | Self::UsageLimit { .. }
                | Self::RateLimited
                | Self::CliError { .. }
                | Self::Auth { .. }
                | Self::Billing { .. }
                | Self::ModelUnavailable { .. }
                | Self::AdapterBroken { .. }
                | Self::IsolationBroken { .. }
                | Self::StartFailed { .. }
                | Self::Crashed { .. }
        )
    }

    /// The agent's service refused its sign-in, which its CLI's own login check tells apart from a
    /// passing error.
    pub fn is_sign_in_error(&self) -> bool {
        match self {
            Self::Auth { .. } => true,
            Self::CliError { category, .. } => *category == ErrorCategory::Auth,
            _ => false,
        }
    }

    /// Whether the same agent may get past it a moment later: a rate limit, a service that's
    /// overloaded or failing, the network, or a process that died. Anything else comes back
    /// the same on a retry, so only another agent helps: a request the service rejects as
    /// invalid, such as an answer format it refuses, a sign-in error, a spent plan or credit, a
    /// model that's gone, a CLI that couldn't start or whose output changed, one that loads
    /// your plugins where it mustn't, and one the watchdog found stuck after its nudge.
    pub fn may_pass(&self) -> bool {
        match self {
            Self::RateLimited | Self::Crashed { .. } => true,
            Self::CliError { category, .. } => matches!(
                category,
                ErrorCategory::RateLimit
                    | ErrorCategory::Overloaded
                    | ErrorCategory::Server
                    | ErrorCategory::Network
                    | ErrorCategory::Timeout
                    | ErrorCategory::Interrupted
                    | ErrorCategory::AnswerMissed
                    | ErrorCategory::SessionLost
            ),
            _ => false,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Watchdog {
    /// No events, CPU, or output from the process tree.
    Inactivity,
    /// The same call repeated after a nudge.
    Repeat,
}

/// An agent's subagent, workflow, or thread changed state. Added to `AgentEvent` as
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
    /// The tokens it has used so far, where its CLI counts them as it goes, as Claude Code's
    /// task progress does. The list ends its row with them.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tokens: Option<u64>,
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
    /// The agent didn't say.
    Unknown,
}

/// A workflow an agent's own CLI runs, as its stream reports it: Claude Code's dynamic
/// workflows (`task_type: local_workflow`). Each update carries the whole state, replacing the
/// last one of the same id, so the views can draw it like one of Argonauts Code's own, with
/// whatever names it uses.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CliWorkflowRun {
    /// The agent's task id.
    pub id: String,
    /// The tool call that started it, which is also its subagent id.
    pub call: Option<String>,
    /// The script's `meta.name`, such as `fix-slugify-median`.
    pub name: String,
    /// The script's `meta.description`.
    pub description: String,
    /// In the order declared, from the script's `meta.phases` and then the progress reports.
    pub phases: Vec<CliPhase>,
    /// In the order they started.
    pub agents: Vec<CliAgent>,
    pub status: SubagentStatus,
    /// The whole workflow's totals, as last reported.
    pub tokens: u64,
    pub tool_calls: u32,
    pub duration_ms: u64,
    /// What it's doing now, or how it ended.
    pub summary: Option<String>,
    /// When Argonauts Code first saw it, in milliseconds since the epoch. Its time counts from here
    /// while it runs, on Argonauts Code's own clock, so a replayed stream reads right too.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_at_ms: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CliPhase {
    /// From 1, as the agent numbers them.
    pub index: u32,
    pub title: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub detail: String,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct CliAgent {
    /// From 1, in the order the agent started them.
    pub index: u32,
    /// The script's label, such as `fix:slugify`.
    pub label: String,
    /// The phase it belongs to, by the agent's index.
    pub phase: Option<u32>,
    pub model: Option<String>,
    pub state: CliAgentState,
    pub tokens: u64,
    pub tool_calls: u32,
    pub duration_ms: u64,
    /// Unix milliseconds.
    pub started_at_ms: Option<u64>,
    /// 1 the first time.
    pub try_number: u32,
    /// Shortened.
    pub prompt: Option<String>,
    /// The start of its answer, once done.
    pub result: Option<String>,
    /// The kind of agent its CLI runs it as, such as `Explore`, where the script names one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_type: Option<String>,
    /// Where it works apart from the session's own folder, such as `worktree`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub isolation: Option<String>,
    /// The model that took over from `model`, once one did.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fallback_model: Option<String>,
    /// The tool it called last. Its CLI counts the others and lists none of them.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_tool: Option<CliTool>,
    /// Why it failed or was blocked, in its CLI's words.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// A tool call as a CLI names it of one of its own agents: the tool, and what it was called
/// with in a few words, such as `Read` and `src/cli.js`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CliTool {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CliAgentState {
    #[default]
    Queued,
    Running,
    Done,
    Failed,
    /// Passed over before it finished, as Claude Code's skip does. Nothing went wrong.
    Skipped,
    /// Never started: its CLI refused its prompt, as a safety check does.
    Blocked,
    /// A state this version of Argonauts Code doesn't know yet.
    Other,
}

impl CliAgentState {
    /// Whether the agent is over, however it ended.
    pub fn ended(self) -> bool {
        matches!(
            self,
            Self::Done | Self::Failed | Self::Skipped | Self::Blocked
        )
    }
}

/// What a workflow's CLI names the group a child workflow's agents run in: `▸ date-cases`, and
/// `▸ date-cases #2` for its second call, as Claude Code and Argonauts Code's runner both name it.
pub const CHILD_GROUP: &str = "▸ ";

impl CliPhase {
    /// The name of the child workflow this phase is, when it is one.
    pub fn child(&self) -> Option<&str> {
        self.title
            .strip_prefix(CHILD_GROUP)
            .map(str::trim)
            .filter(|name| !name.is_empty())
    }
}

impl CliWorkflowRun {
    /// The agents of one phase, by the agent's index.
    pub fn agents_in(&self, phase: u32) -> impl Iterator<Item = &CliAgent> {
        self.agents
            .iter()
            .filter(move |agent| agent.phase == Some(phase))
    }

    /// Done of all, for a phase with agents. `None` for one that hasn't started.
    pub fn count(&self, phase: u32) -> Option<(u32, u32)> {
        let (done, total) = self.agents_in(phase).fold((0, 0), |(done, total), agent| {
            let finished = matches!(agent.state, CliAgentState::Done);
            (done + u32::from(finished), total + 1)
        });
        (total > 0).then_some((done, total))
    }

    /// Whether it's over, done or failed.
    pub fn ended(&self) -> bool {
        matches!(self.status, SubagentStatus::Done | SubagentStatus::Failed)
    }

    /// Whether `agent` was still going when the workflow ended, and so was stopped with it:
    /// its CLI reports no end of its own for it.
    pub fn stopped_with_it(&self, agent: &CliAgent) -> bool {
        self.ended() && !agent.state.ended()
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
            if !agent.state.ended() {
                agent.state = CliAgentState::Failed;
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
    /// the first agent the stream says it began, so the agent's clock never has to agree with
    /// ours. One still going when its workflow ended ran until then.
    pub fn agent_elapsed_ms(&self, agent: &CliAgent, now_ms: Option<u64>) -> u64 {
        let running =
            !self.ended() && matches!(agent.state, CliAgentState::Running | CliAgentState::Other);
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
            // Stopped with the workflow, it reported no total of its own: it ran from when
            // it began until the workflow ended.
            (false, _, Some(began), Some(first), _) if self.stopped_with_it(agent) => self
                .duration_ms
                .saturating_sub(began.saturating_sub(first))
                .max(agent.duration_ms),
            _ => agent.duration_ms,
        }
    }
}

/// A normalized plan usage reading, added to `AgentEvent` as `PlanUsage { reading }`.
pub type AgentPlanUsage = PlanUsageReading;

#[cfg(test)]
mod tests {
    use super::*;

    fn names(names: &[&str]) -> Vec<String> {
        names.iter().map(|name| (*name).to_owned()).collect()
    }

    /// The model a command launched, as each CLI's command names it.
    #[test]
    fn a_launch_names_the_model_its_command_ran() {
        let launch = |args: &[&str]| LaunchRecord {
            program: "cli".to_owned(),
            args: names(args),
            cwd: PathBuf::from("/w"),
            env_set: Vec::new(),
            env_removed: Vec::new(),
            prompt_via: PromptVia::Argument,
            sandbox: None,
        };
        let model = |args: &[&str]| launch(args).model().map(str::to_owned);
        assert_eq!(
            model(&["-p", "--model", "haiku", "<prompt>"]).as_deref(),
            Some("haiku")
        );
        assert_eq!(
            model(&["exec", "-m", "gpt-6-luna", "-c", "x=1"]).as_deref(),
            Some("gpt-6-luna")
        );
        assert_eq!(
            model(&["--model=gemini-3.6-flash-low"]).as_deref(),
            Some("gemini-3.6-flash-low")
        );
        assert_eq!(model(&["-p", "<prompt>"]), None, "the CLI's settings pick");
        assert_eq!(model(&["--model"]), None);
    }

    #[test]
    fn what_an_isolated_session_loaded_reads_as_a_list() {
        assert_eq!(
            loaded_words(&names(&["memory-keeper", "lint-helper"]), &[]),
            "your plugins memory-keeper and lint-helper"
        );
        assert_eq!(
            loaded_words(
                &names(&["a", "b", "c"]),
                &names(&["anthropic-skills:example-a"])
            ),
            "your plugins a, b and c, and your synced skill anthropic-skills:example-a"
        );
        assert_eq!(
            loaded_words(
                &[],
                &names(&["anthropic-skills:example-a", "anthropic-skills:example-b"])
            ),
            "2 synced skills"
        );
        let broken = EndReason::IsolationBroken {
            plugins: names(&["memory-keeper"]),
            skills: Vec::new(),
        };
        let json = serde_json::to_value(&broken).unwrap();
        assert_eq!(
            json,
            serde_json::json!({"why": "isolation_broken", "plugins": ["memory-keeper"]})
        );
        assert_eq!(serde_json::from_value::<EndReason>(json).unwrap(), broken);
    }
}
