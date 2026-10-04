//! Workers: harness agents (through the `Adapter` trait) and direct model endpoints, behind one
//! interface.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use tokio::sync::{mpsc, oneshot};

use crate::events::VendorEvent;
use crate::events::session::{InputPart, QueuedMessage, SessionEnd, SessionEvent};
use crate::future::BoxFuture;
use crate::names::{RoleName, SessionKey, VendorId, WorkerRef};
use crate::screen::ScreenRequest;
use crate::settings::budget::BudgetAmount;
use crate::settings::sessions::{Compaction, NativeFanout, SessionLifetime};
use crate::steps::StepName;
use crate::{AgentEvent, CardId, CommandSpec, DEFAULT_SOFT_RATIO, RunId};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkerKind {
    /// A coding agent with its own tool loop: Claude Code, Codex, agy, OpenCode.
    Harness,
    /// An OpenAI-compatible model API with no tools.
    Endpoint,
}

/// What a session may do. Enforced by harness flags and the OS sandbox, not the prompt.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Access {
    /// The only access an endpoint has.
    NoTools,
    ReadOnly,
    /// Edits its worktree and runs commands in the sandbox. Needs the goal hash.
    Build,
    /// Like build, for a time-boxed spike worktree.
    Spike,
    /// Direct solo: your own checkout, the CLI's own tools, and its own asking, as when you run
    /// it yourself. Needs the goal hash, which for a direct turn is your message's.
    Direct,
}

impl Access {
    /// Whether the session may change files in its folder.
    pub fn writes(self) -> bool {
        matches!(self, Self::Build | Self::Spike | Self::Direct)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct SessionSpec {
    /// `None` for chat and narrator sessions.
    pub run: Option<RunId>,
    pub card: Option<CardId>,
    pub key: SessionKey,
    pub role: RoleName,
    pub step: Option<StepName>,
    pub worker: WorkerRef,
    /// Such as `low`, from `roles.<role>.effort`, else `agents.<harness>.effort`. Unset uses the
    /// CLI's default.
    pub effort: Option<String>,
    pub access: Access,
    /// The card's worktree for build and spike sessions, the run's base worktree for read-only
    /// sessions, and an empty scratch directory for no-tools sessions. Only chat and triage read
    /// your main checkout.
    pub workdir: PathBuf,
    /// Private files outside the worktree: schema file, raw log, status file.
    pub session_dir: PathBuf,
    /// Sent concatenated and recorded as input.
    pub prompt: Vec<InputPart>,
    /// Enforced where the worker supports it.
    pub json_schema: Option<serde_json::Value>,
    /// Vendor session to resume. Every launch flag is passed again.
    pub resume: Option<String>,
    pub lifetime: SessionLifetime,
    /// Keep the input open so messages can be steered in (Claude Code, agy stream input).
    pub keep_input_open: bool,
    pub limits: SessionLimits,
    /// SHA-256 of the approved goal. Required for `Build` and `Spike`.
    pub goal_sha256: Option<String>,
    pub sandbox: SandboxProfile,
    pub compaction: Compaction,
    /// Cap on the harness's own subagents.
    pub native_fanout: NativeFanout,
    /// The CLI loads none of its own MCP servers, plugins, or project settings, as for a blind
    /// reviewer or the approver, where it can keep them out ([`Capabilities::isolates`]). A
    /// session on one that can't says so. Every other session gets what the CLI loads when run
    /// directly.
    ///
    /// [`Capabilities::isolates`]: crate::Capabilities::isolates
    pub isolated: bool,
    /// Read by the agent's `argonauts_status` MCP tool, pre-filtered for the role. `None` turns
    /// the tool off.
    pub status_file: Option<PathBuf>,
    /// The worker always sets `ARGONAUTS_SESSION=<key>`, which refuses approvals, over anything
    /// given here.
    pub env: Vec<(String, String)>,
    /// Where a direct session's questions go, such as Claude Code's permission prompts. `None`
    /// asks nothing: what the CLI would ask about is refused, or allowed by its flags.
    pub asking: Option<Asking>,
}

/// A direct session's questions, and the app's listener that answers them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Asking {
    pub asks: crate::session::Asks,
    /// `127.0.0.1:<port>`, which the session's MCP server connects to with each question.
    pub endpoint: String,
    /// Sent with every question, so nothing else on this machine can ask in its name.
    pub token: String,
    /// The `argonauts_start_run` tool is offered.
    pub start_run: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SessionLimits {
    /// `None` when budgets are off.
    pub budget: Option<BudgetAmount>,
    /// Share of any limit that sends the wrap-up message.
    pub soft_stop: f64,
    /// Wall-clock minutes.
    pub max_minutes: Option<f64>,
    pub max_tool_calls: Option<u64>,
    /// Idle minutes with no events, CPU, or output.
    pub inactivity_min: Option<f64>,
    /// The same call this many times in a row gets a nudge.
    pub repeat_limit: Option<u32>,
    /// The vendor's native cap as a backstop, set a little above `budget`, such as
    /// `--max-budget-usd`.
    pub native_cap_usd: Option<f64>,
}

impl Default for SessionLimits {
    /// No limits, but the usual soft stop, so a wrap-up never goes out at the start.
    fn default() -> Self {
        Self {
            budget: None,
            soft_stop: DEFAULT_SOFT_RATIO,
            max_minutes: None,
            max_tool_calls: None,
            inactivity_min: None,
            repeat_limit: None,
            native_cap_usd: None,
        }
    }
}

/// Every harness session runs in an OS sandbox or a container, whatever its access. Read-only
/// and no-tools sessions may write only to temp and the harness's own state directory. The
/// default is the OS sandbox with nothing writable, which fails closed.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct SandboxProfile {
    pub mode: SandboxMode,
    /// Such as the worktree, temp, and build caches.
    pub writable: Vec<PathBuf>,
    /// Extra readable paths inside denied areas, such as the session's status file.
    pub readable: Vec<PathBuf>,
    /// Paths that must stay unreadable: `~/.argonauts/state`, other worktrees, other vendors'
    /// credential stores, `~/.ssh`.
    pub deny_read: Vec<PathBuf>,
    /// Network for commands the agent runs. The harness still reaches its model API.
    pub network: bool,
    /// With `network` off, the one loopback port the session may connect to: the Argonauts
    /// proxy, which tunnels only to its harness's API hosts. `None` leaves every loopback port
    /// open and nothing beyond, as gates have it.
    pub proxy_port: Option<u16>,
    /// With `network` off, the app's loopback port for a direct turn's questions, which the
    /// session reaches as it reaches the proxy, so a card can ask you before an edit.
    pub ask_port: Option<u16>,
    /// Paths no write may make or change even where they don't exist yet, inside a writable
    /// folder: a direct turn's protected paths, which every later session and your own CLI load.
    pub closed: Vec<PathBuf>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SandboxMode {
    /// No sandbox. Endpoint sessions only; `HardLimits::check_session` flags it on a harness.
    None,
    /// Seatbelt on macOS, Landlock with user, mount, network, and PID namespaces on Linux.
    #[default]
    Os,
    /// Only the worktree mounted.
    Container,
}

#[derive(Debug, Clone, PartialEq)]
pub enum WorkerEvent {
    Agent(AgentEvent),
    Session(SessionEvent),
    Vendor(VendorEvent),
}

/// Dropping `control` (with every clone of it) counts as `Stop`: the worker interrupts the
/// session and reaps its process group, so a cancelled turn or a dropped future never leaves a
/// vendor process spending. Keep `control` for as long as the session should run.
pub struct SessionHandle {
    pub key: SessionKey,
    /// Closed when the session ends.
    pub events: mpsc::Receiver<WorkerEvent>,
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
    /// it until the model can read it. `MessageState` events say how far it got.
    Send { text: String, uuid: String },
    /// What waits goes to the model now, and a running tool moves to the background.
    SendNow { uuid: String },
    /// Takes back a message the model hasn't read.
    TakeBack { uuid: String },
    /// A request of a screen that joins the session.
    Screen(ScreenRequest),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct WorkerCapabilities {
    pub streams_text: bool,
    pub steer: SteerSupport,
    pub resume: bool,
    /// Enforces an output schema natively.
    pub structured_output: bool,
    pub subagents: SubagentVisibility,
    pub native_budget_cap: bool,
    pub takeover: bool,
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

/// How much of a harness's own subagents the run tree can show.
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

/// A worker: one harness adapter plus model, or one endpoint plus model.
pub trait Worker: Send + Sync {
    fn worker_ref(&self) -> &WorkerRef;
    fn kind(&self) -> WorkerKind;
    fn vendor(&self) -> VendorId;
    /// From the registry. Review and judging rules compare families.
    fn family(&self) -> String;
    fn capabilities(&self) -> WorkerCapabilities;
    /// An error means the conductor couldn't start it at all, such as a build session with no
    /// goal hash or no working sandbox (fail closed). Vendor failures are normal endings,
    /// reported through `SessionHandle::ended`.
    fn start(&self, spec: SessionSpec) -> BoxFuture<'_, Result<SessionHandle, WorkerError>>;
    /// Opens the session in the vendor's own UI, for take-over.
    fn takeover_command(&self, vendor_session: &str, workdir: &Path) -> Option<CommandSpec>;
    /// The cheapest model its harness offers, for small read-only steps such as a handoff
    /// summary. `None` when it knows of none, and then the worker's own model does them.
    fn cheapest_model(&self) -> Option<String> {
        None
    }

    /// The instruction files its harness loads by itself in `workdir`, as
    /// [`Adapter::instruction_files`](crate::Adapter::instruction_files) says.
    fn instruction_files(&self, workdir: &Path, isolated: bool) -> Vec<String> {
        let _ = (workdir, isolated);
        Vec::new()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum WorkerError {
    #[error("unknown worker {0}")]
    Unknown(WorkerRef),
    #[error("{0} is not installed")]
    NotInstalled(String),
    /// Disabled for new work, such as after a broken CLI update.
    #[error("{worker} is disabled: {reason}")]
    Disabled { worker: WorkerRef, reason: String },
    /// A rule refused the session, such as build without a goal hash or sandbox.
    #[error("refused: {0}")]
    Refused(String),
    /// Something local failed, such as writing the schema file.
    #[error("{0}")]
    Io(String),
}

pub trait WorkerDirectory: Send + Sync {
    fn get(&self, worker: &WorkerRef) -> Result<Arc<dyn Worker>, WorkerError>;
    /// Every configured worker that is installed and enabled.
    fn available(&self) -> Vec<WorkerRef>;
    /// The CLI behind `worker` keeps a read-only session from changing files with its own
    /// flags and sandbox. False when that isn't known, so callers keep their own sandbox.
    fn enforces_read_only(&self, worker: &WorkerRef) -> bool {
        let _ = worker;
        false
    }
    /// The CLI behind `worker` keeps your MCP servers, plugins, and hooks out of an isolated
    /// session and says when it couldn't, or `worker` is an endpoint, which loads nothing.
    /// False when that isn't known, so a blind role never goes there.
    fn isolates(&self, worker: &WorkerRef) -> bool {
        let _ = worker;
        false
    }
}
