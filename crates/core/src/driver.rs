//! Session drivers: code that speaks a two-way protocol with an agent, such as the Agent Client
//! Protocol, where a line parser can't work because permission requests need answers while
//! updates stream. The harness worker still spawns and sandboxes the process, and keeps its
//! refusal rules, meter, watchdogs, raw logs, file diffs, and stop plan.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::sync::{mpsc, oneshot};

use crate::ErrorCategory;
use crate::event::AgentEvent;
use crate::events::session::Delivery;
use crate::future::BoxFuture;
use crate::worker::Access;

pub trait SessionDriver: Send + Sync {
    /// Runs one session to its end. It returns when the agent's turn ends with nothing left to
    /// send, when it's cancelled, or when the pipes close.
    fn run(&self, io: DriverIo, start: DriverStart) -> BoxFuture<'static, DriverEnd>;
}

/// The child's pipes and the worker's channels.
pub struct DriverIo {
    pub stdin: Box<dyn AsyncWrite + Send + Unpin>,
    pub stdout: Box<dyn AsyncRead + Send + Unpin>,
    /// Lines the worker has already logged.
    pub stderr_lines: mpsc::Receiver<String>,
    /// Unbounded, so the driver never stops reading the agent's pipe while the worker's meter,
    /// watchdogs, or journal are busy.
    pub events: mpsc::UnboundedSender<AgentEvent>,
    pub control: mpsc::Receiver<DriverControl>,
    /// Every frame both ways, for `raw.stdin.jsonl` and `raw.stdout.jsonl`.
    pub frames: mpsc::UnboundedSender<RawFrame>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DriverStart {
    /// The card's worktree, or the run's base worktree for read-only sessions.
    pub cwd: PathBuf,
    pub prompt: String,
    pub model: Option<String>,
    pub effort: Option<String>,
    pub access: Access,
    /// The vendor session to resume.
    pub resume: Option<String>,
    pub mcp_servers: Vec<McpServerSpec>,
    pub scope: PermissionScope,
    /// The step's output schema, which the report tool enforces.
    pub json_schema: Option<Value>,
    /// Where the report tool writes the last valid report, `<session_dir>/report.json`.
    pub report_file: Option<PathBuf>,
}

#[derive(Debug)]
pub enum DriverControl {
    /// `now` delivers during the turn where the agent allows it; otherwise the message waits
    /// for the turn to end. `done` says how it went out, and is dropped if it never did.
    Deliver {
        text: String,
        now: bool,
        done: oneshot::Sender<Delivery>,
    },
    /// Let the running tool call finish, then cancel the turn.
    StopAfterTool,
    /// Cancel now and answer every pending permission request `cancelled`.
    Cancel,
}

/// What a session may touch, computed by the worker from the session's access, workdir, and
/// sandbox, plus the paths no card may ever touch. The driver answers permission requests from
/// it, and the OS sandbox enforces the same bounds for whatever the agent does without asking.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct PermissionScope {
    pub roots: Vec<PathBuf>,
    pub writable: Vec<PathBuf>,
    pub deny: Vec<PathBuf>,
    /// Glob patterns, relative to the worktree.
    pub forbidden: Vec<String>,
    pub network: bool,
}

/// A stdio MCP server the agent starts for this session, such as `argonauts mcp session`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct McpServerSpec {
    pub name: String,
    /// Absolute.
    pub command: PathBuf,
    pub args: Vec<String>,
}

/// One protocol frame, as sent or received, without its newline.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RawFrame {
    /// Sent to the agent, rather than received from it.
    pub outgoing: bool,
    pub line: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DriverEnd {
    /// For resume and take-over.
    pub vendor_session: Option<String>,
    pub stop: DriverStop,
}

/// Why the session ended. The worker turns it into an `EndReason`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "stop", rename_all = "snake_case")]
pub enum DriverStop {
    EndTurn,
    Cancelled,
    MaxTokens,
    /// The agent's own turn cap, set from the session's tool-call limit where it allows one.
    MaxTurnRequests,
    Refusal,
    Failed {
        category: ErrorCategory,
        message: String,
    },
    /// The pipes closed mid-turn.
    Disconnected,
}
