//! Session runners: CLI agents (through the `Adapter` trait) and direct model endpoints,
//! behind one interface.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use tokio::sync::{mpsc, oneshot};

use crate::events::AgentStatusEvent;
use crate::events::context::ContextDetail;
use crate::events::session::{InputPart, QueuedMessage, SessionEnd, SessionEvent};
use crate::future::BoxFuture;
use crate::names::{AgentId, ModelRef, RoleName, SessionKey};
use crate::screen::ScreenRequest;
use crate::settings::limit::LimitAmount;
use crate::settings::sessions::{Compaction, NativeFanout, SessionLifetime};
use crate::steps::StepName;
use crate::{AgentEvent, CommandSpec, DEFAULT_SOFT_RATIO, RunId, TaskId};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunnerKind {
    /// A coding agent with its own tool loop: Claude Code, Codex, agy, OpenCode.
    Cli,
    /// An OpenAI-compatible model API with no tools.
    Endpoint,
}

/// What a session may do. Enforced by CLI flags and the OS sandbox, not the prompt.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Access {
    /// The only access an endpoint has.
    NoTools,
    ReadOnly,
    /// Edits its worktree and runs commands in the sandbox. Needs the goal hash.
    Build,
    /// Like build, for a time-boxed experiment worktree.
    Experiment,
    /// Direct Captain: your own checkout, the CLI's own tools, and its own asking, as when you run
    /// it yourself. Needs the goal hash, which for a direct turn is your message's.
    Direct,
}

impl Access {
    /// Whether the session may change files in its folder.
    pub fn writes(self) -> bool {
        matches!(self, Self::Build | Self::Experiment | Self::Direct)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct SessionSpec {
    /// `None` for conversation and narrator sessions.
    pub run: Option<RunId>,
    pub task: Option<TaskId>,
    pub key: SessionKey,
    pub role: RoleName,
    pub step: Option<StepName>,
    pub model: ModelRef,
    /// Such as `low`, from `roles.<role>.effort`, else `agents.<cli>.effort`. Unset uses the
    /// CLI's default.
    pub effort: Option<String>,
    pub access: Access,
    /// The task's worktree for build and experiment sessions, the run's base worktree for read-only
    /// sessions, and an empty scratch directory for no-tools sessions. Only conversation and the
    /// estimate read your main checkout.
    pub workdir: PathBuf,
    /// Private files outside the worktree: schema file, raw log, status file.
    pub session_dir: PathBuf,
    /// Sent concatenated and recorded as input.
    pub prompt: Vec<InputPart>,
    /// Enforced where the session runner supports it.
    pub json_schema: Option<serde_json::Value>,
    /// CLI session to resume. Every launch flag is passed again.
    pub resume: Option<String>,
    /// Resume it as a copy with an id of its own, which leaves the original as it was. Only a
    /// runner that [forks](RunnerCapabilities::fork) takes one.
    pub fork: bool,
    pub lifetime: SessionLifetime,
    /// Keep the input open so messages can be steered in (Claude Code, agy stream input).
    pub keep_input_open: bool,
    pub limits: SessionLimits,
    /// SHA-256 of the approved goal. Required for `Build` and `Experiment`.
    pub goal_sha256: Option<String>,
    pub sandbox: SandboxProfile,
    pub compaction: Compaction,
    /// Cap on the CLI's own subagents.
    pub native_fanout: NativeFanout,
    /// The CLI loads none of its own MCP servers, plugins, or project settings, as for a blind
    /// reviewer or the judge, where it can keep them out ([`Capabilities::isolates`]). A
    /// session on one that can't says so. Every other session gets what the CLI loads when run
    /// directly.
    ///
    /// [`Capabilities::isolates`]: crate::Capabilities::isolates
    pub isolated: bool,
    /// The session's run is on autopilot, so nobody answers a question it ends its turn with.
    /// A CLI with a style of its own for working without stopping gets it (Claude Code's
    /// "Proactive").
    pub autopilot: bool,
    /// Read by the agent's `argonauts_status` MCP tool, pre-filtered for the role. `None` turns
    /// the tool off.
    pub status_file: Option<PathBuf>,
    /// The session runner always sets `ARGONAUTS_SESSION=<key>`, which refuses approvals, over
    /// anything given here.
    pub env: Vec<(String, String)>,
    /// Where a direct session's questions go, such as Claude Code's permission prompts. `None`
    /// asks nothing: what the CLI would ask about is refused, or allowed by its flags.
    pub asking: Option<Asking>,
    /// What the CLI's own workflow does in this session, where it has one.
    pub cli_workflow: CliWorkflow,
    /// What it may run in a shell when it only reads.
    pub shell: SessionShell,
    /// What you set for the session you talk to that its CLI takes as settings of its own.
    pub preferences: SessionPreferences,
}

/// Thinking, fast mode, the language answers come in, and the workflow word, as you set them
/// for the session you talk to. A CLI takes each as a setting of its own where it has one, as
/// Claude Code does; the default leaves the CLI's own.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct SessionPreferences {
    /// No thinking before it answers.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub thinking_off: bool,
    /// The CLI's own fast mode.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub fast: bool,
    /// Such as `japanese` or `ja`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub language: Option<String>,
    /// The word `ultracode` in a prompt asks the CLI for nothing.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub workflow_word_off: bool,
}

impl SessionPreferences {
    pub fn is_default(&self) -> bool {
        *self == Self::default()
    }
}

/// A CLI's own workflow in one session. Only Claude Code has one.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CliWorkflow {
    /// Ultracode for the whole session: a workflow on every real task.
    pub ultracode: bool,
    /// You typed `ultracode` to ask for a workflow with this message. It goes out marked as
    /// typed by you, so the CLI's own keyword works.
    pub typed_keyword: bool,
    /// The session starts no workflow or background agent of its CLI's own. A crew agent's
    /// answer is taken when its turn ends, and what it left running would outlive that.
    pub background_off: bool,
}

/// What a read-only session may run in a shell. A session that builds has its CLI's own.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionShell {
    #[default]
    None,
    /// These commands, each as one line, such as `node --test`, with any arguments after.
    Only(Vec<String>),
    /// Any command, inside the session's sandbox.
    Any,
}

impl SessionShell {
    pub fn is_none(&self) -> bool {
        *self == Self::None
    }
}

/// A direct session's questions, and the app's listener that answers them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Asking {
    pub asks: crate::session::Asks,
    /// `127.0.0.1:<port>`, which the session's MCP server connects to with each question.
    pub endpoint: String,
    /// Sent with every question, so nothing else on this machine can ask in its name.
    pub token: String,
    /// The main session's tools this session is offered, by name, such as
    /// `argonauts_start_workflow`. The app picks them for each turn.
    pub tools: Vec<String>,
    /// What the session is told about those tools: a few lines at the end of its instructions,
    /// given through its CLI's own way of taking such a note. `None` says nothing.
    pub standing: Option<String>,
    /// Crew roles it may ask through Argonauts Code, each with a tool of its own, such as the
    /// reviewers from inside Claude Code's own workflow.
    pub roles: Vec<crate::settings::crew::CrewRole>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SessionLimits {
    /// `None` when limits are off.
    pub limit: Option<LimitAmount>,
    /// Share of any limit that sends the wrap-up message.
    pub soft_stop: f64,
    /// Wall-clock minutes.
    pub max_minutes: Option<f64>,
    pub max_tool_calls: Option<u64>,
    /// Idle minutes with no events, CPU, or output.
    pub inactivity_min: Option<f64>,
    /// The same call this many times in a row gets a nudge.
    pub repeat_limit: Option<u32>,
    /// The agent's native cap as a backstop, set a little above `limit`, such as
    /// `--max-budget-usd`.
    pub native_cap_usd: Option<f64>,
    /// What's left of the run's own limit as the session starts. Only the session's own
    /// spend counts against it: the run's already holds what the CLI session it resumes spent.
    pub run_left: Option<LimitAmount>,
    /// What the session spends can go on after its turn where the meter doesn't see it, as its
    /// CLI's own workflow or agents do in the background. The native cap is then the only stop
    /// there, so it holds to the limit itself. The runner sets it from the spec.
    pub spends_unseen: bool,
}

impl Default for SessionLimits {
    /// No limits, but the usual soft stop, so a wrap-up never goes out at the start.
    fn default() -> Self {
        Self {
            limit: None,
            soft_stop: DEFAULT_SOFT_RATIO,
            max_minutes: None,
            max_tool_calls: None,
            inactivity_min: None,
            repeat_limit: None,
            native_cap_usd: None,
            run_left: None,
            spends_unseen: false,
        }
    }
}

/// Every CLI session runs in an OS sandbox or a container, whatever its access. Read-only
/// and no-tools sessions may write only to temp and the CLI's own state directory. The
/// default is the OS sandbox with nothing writable, which fails closed.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct SandboxProfile {
    pub mode: SandboxMode,
    /// Such as the worktree, temp, and build caches.
    pub writable: Vec<PathBuf>,
    /// Extra readable paths inside denied areas, such as the session's status file.
    pub readable: Vec<PathBuf>,
    /// Paths that must stay unreadable: `~/.argonauts/state`, other worktrees, other agents'
    /// credential stores, `~/.ssh`.
    pub deny_read: Vec<PathBuf>,
    /// Network for commands the agent runs. The CLI still reaches its model API.
    pub network: bool,
    /// With `network` off, the one loopback port the session may connect to: the Argonauts Code
    /// proxy, which tunnels only to its CLI's API hosts. `None` leaves every loopback port
    /// open and nothing beyond, as checks have it.
    pub proxy_port: Option<u16>,
    /// With `network` off, the app's loopback port for a direct turn's questions, which the
    /// session reaches as it reaches the proxy, so a task can ask you before an edit.
    pub ask_port: Option<u16>,
    /// Paths no write may make or change even where they don't exist yet, inside a writable
    /// folder: a direct turn's protected paths, which every later session and your own CLI load.
    pub closed: Vec<PathBuf>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SandboxMode {
    /// No sandbox. Endpoint sessions only; `HardLimits::check_session` flags it on a CLI.
    None,
    /// Seatbelt on macOS, Landlock with user, mount, network, and PID namespaces on Linux.
    #[default]
    Os,
    /// Only the worktree mounted.
    Container,
}

#[derive(Debug, Clone, PartialEq)]
pub enum RunnerEvent {
    Agent(AgentEvent),
    Session(SessionEvent),
    AgentStatus(AgentStatusEvent),
}

/// Dropping `control` (with every clone of it) counts as `Stop`: the runner interrupts the
/// session and reaps its process group, so a cancelled turn or a dropped future never leaves a
/// agent process spending. Keep `control` for as long as the session should run.
pub struct SessionHandle {
    pub key: SessionKey,
    /// Closed when the session ends.
    pub events: mpsc::Receiver<RunnerEvent>,
    pub control: mpsc::Sender<SessionControl>,
    /// How it ended. Always sent, even when it never started, and only after the last event,
    /// once the events sender is dropped, so the final diff and result come first.
    pub ended: oneshot::Receiver<SessionEnd>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum SessionControl {
    /// Deliver a message now (after the current tool call) or at the next turn.
    Deliver(QueuedMessage),
    /// Send the fixed wrap-up message now.
    WrapUp,
    /// Let the current tool call finish, end the turn, then stop.
    StopAfterTool,
    /// Interrupt the turn, then kill the process tree.
    Stop,
    /// A message of yours, written to the open input at once with `uuid`, for a CLI that holds
    /// it until the model can read it. `MessageState` events say how far it got. `pictures`
    /// are the ones its `[Image #N]` chips name.
    Send {
        text: String,
        uuid: String,
        pictures: Vec<crate::pictures::Picture>,
    },
    /// What waits goes to the model now, and a running tool moves to the background.
    SendNow { uuid: String },
    /// Takes back a message the model hasn't read.
    TakeBack { uuid: String },
    /// A request of a screen that joins the session.
    Screen(ScreenRequest),
    /// Asks the CLI for its context by category now, as /context opening does. A CLI that
    /// can't say ignores it.
    AskContext(ContextDetail),
    /// Something shows this session's context: after each fill, ask for the breakdown, at most
    /// every 10 seconds. `None` stops asking.
    WatchContext(Option<ContextDetail>),
    /// Stops one subagent the session's own CLI runs, by the id its `Subagent` updates carry.
    /// The session and its other subagents go on. A CLI that can't stop one alone says so in
    /// a notice.
    StopSubagent { id: String },
    /// Esc: stops the running turn alone. What the session's own CLI runs in the background,
    /// its subagents and workflows, goes on, and the session stays open for them. A CLI that
    /// can't stop a turn alone is stopped whole, as by `Stop`.
    StopTurn,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct RunnerCapabilities {
    pub streams_text: bool,
    pub steer: SteerSupport,
    pub resume: bool,
    /// Resumes a session as a copy with an id of its own, which leaves the original as it was.
    #[serde(default)]
    pub fork: bool,
    /// Enforces an output schema natively.
    pub structured_output: bool,
    pub subagents: SubagentVisibility,
    pub native_limit_cap: bool,
    pub take_over: bool,
    /// Has tools and a sandbox path.
    pub can_build: bool,
    /// The `argonauts_status` MCP tool can be given to its sessions. Where it can't (agy, and
    /// Codex until a per-run route is verified), the summary is injected at session start and
    /// again with each delivered message and resume.
    #[serde(default)]
    pub status_tool: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SteerSupport {
    /// Through the open input stream, after the current tool call.
    Input,
    /// Interrupt, then resume the session with the message (Codex).
    InterruptResume,
    /// Only at the next turn; the input side says so.
    NextTurnOnly,
}

/// How much of a CLI's own subagents the run tree can show.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SubagentVisibility {
    /// Every child and its steps (Claude Code).
    Full,
    /// Each child with a link to its own log (agy).
    Linked,
    /// One row per thread with status and last message (Codex exec).
    ThreadsOnly,
    None,
}

/// Runs sessions of one model: a CLI adapter plus model, or an endpoint plus model.
pub trait SessionRunner: Send + Sync {
    fn model_ref(&self) -> &ModelRef;
    fn kind(&self) -> RunnerKind;
    fn agent_id(&self) -> AgentId;
    /// From the registry. Review and judging rules compare providers.
    fn provider(&self) -> String;
    fn capabilities(&self) -> RunnerCapabilities;
    /// An error means Argonauts Code couldn't start it at all, such as a build session with no
    /// goal hash or no working sandbox (fail closed). Agent failures are normal endings,
    /// reported through `SessionHandle::ended`.
    fn start(&self, spec: SessionSpec) -> BoxFuture<'_, Result<SessionHandle, RunnerError>>;
    /// Why [`start`](Self::start) would refuse any session of it now, known without starting
    /// one: such as OpenCode with no model set, whose own default runs on a hosted service, or
    /// an agent disabled after a broken update. A fallback never goes to one that would.
    fn start_refusal(&self) -> Option<String> {
        None
    }
    /// Opens the session in the CLI's own UI, for take-over.
    fn take_over_command(&self, cli_session: &str, workdir: &Path) -> Option<CommandSpec>;
    /// The cheapest model its CLI offers, for small read-only steps such as a fallback
    /// summary. `None` when it knows of none, and then its own model does them.
    fn cheapest_model(&self) -> Option<String> {
        None
    }

    /// The instruction files its CLI loads by itself in `workdir`, as
    /// [`Adapter::instruction_files`](crate::Adapter::instruction_files) says.
    fn instruction_files(&self, workdir: &Path, isolated: bool) -> Vec<String> {
        let _ = (workdir, isolated);
        Vec::new()
    }

    /// What its CLI's own login check says, the way the app's start check reads it: `Some(true)`
    /// logged in, `Some(false)` logged out, `None` when there's no check or it claims nothing.
    fn logged_in(&self) -> BoxFuture<'_, Option<bool>> {
        Box::pin(async { None })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RunnerError {
    #[error("unknown model {0}")]
    Unknown(ModelRef),
    #[error("{0} is not installed")]
    NotInstalled(String),
    /// Disabled for new work, such as after a broken CLI update.
    #[error("{model} is disabled: {reason}")]
    Disabled { model: ModelRef, reason: String },
    /// A rule refused the session, such as build without a goal hash or sandbox.
    #[error("refused: {0}")]
    Refused(String),
    /// Something local failed, such as writing the schema file.
    #[error("{0}")]
    Io(String),
}

pub trait RunnerDirectory: Send + Sync {
    fn get(&self, model: &ModelRef) -> Result<Arc<dyn SessionRunner>, RunnerError>;
    /// Every configured model that is installed and enabled.
    fn available(&self) -> Vec<ModelRef>;
    /// The CLI behind `model` keeps a read-only session from changing files with its own
    /// flags and sandbox. False when that isn't known, so callers keep their own sandbox.
    fn enforces_read_only(&self, model: &ModelRef) -> bool {
        let _ = model;
        false
    }
    /// The CLI behind `model` keeps your MCP servers, plugins, and hooks out of an isolated
    /// session and says when it couldn't, or `model` is an endpoint, which loads nothing.
    /// False when that isn't known, so a blind role never goes there.
    fn isolates(&self, model: &ModelRef) -> bool {
        let _ = model;
        false
    }
    /// The CLI behind `model` takes Argonauts Code's tools when Argonauts Code drives it, so your
    /// conversation can be that CLI itself and hand work over by calling one. False when that
    /// isn't known: such a CLI's replies end with a verdict that code reads instead.
    fn takes_session_tools(&self, model: &ModelRef) -> bool {
        let _ = model;
        false
    }
}
