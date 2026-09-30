//! File and folder names for everything Argonauts stores. Build paths from these and `Paths`,
//! never by hand.

use std::path::{Path, PathBuf};

use crate::names::{ChatId, SessionKey};
use crate::{CardId, RunId};

/// Files in a run's directory, `~/.argonauts/state/<repo>/runs/<run>/`.
pub mod run_files {
    pub const EVENTS: &str = "events.jsonl";
    /// Narrator lines. Never read into any agent's context.
    pub const NARRATOR: &str = "narrator.jsonl";
    pub const SETTINGS: &str = "settings.json";
    /// What `ConductorApi::start` asked for. The supervisor writes `RunStarted` itself, under
    /// the lock, so a run is never seen without an owner.
    pub const START: &str = "start.json";
    pub const LOCK: &str = "supervisor.lock";
    pub const PID: &str = "supervisor.pid";
    pub const SOCKET: &str = "control.sock";
    pub const SUPERVISOR_LOG: &str = "supervisor.log";
    /// A random token `Answer` requests must carry, mode 0600.
    pub const APPROVE_TOKEN: &str = "approve.token";
    /// `ControlRequest`s sent while no supervisor was running. The next supervisor feeds them to
    /// the engine first, so the engine stays the only writer of `Decided` events.
    pub const PENDING_ANSWERS: &str = "pending-answers.jsonl";
    /// Sealed drafts, one file per draft, unreadable to every session until all are sealed.
    pub const DRAFTS: &str = "drafts";
    pub const CARDS: &str = "cards";
    pub const SESSIONS: &str = "sessions";
    pub const EVIDENCE: &str = "evidence";
}

pub mod session_files {
    pub const RAW_STDOUT: &str = "raw.stdout.jsonl";
    pub const RAW_STDERR: &str = "raw.stderr.txt";
    pub const SCHEMA: &str = "schema.json";
    /// The status summary filtered for this session's role, for its MCP status tool.
    pub const STATUS: &str = "status.json";
    /// The session's process group, pid, and process start time, so recovery can kill an
    /// orphan left by a crashed supervisor before it resumes the vendor session.
    pub const PROCESS: &str = "process.json";
    /// The MCP config handed to the harness: only the `argonauts_status` server.
    pub const MCP_CONFIG: &str = "mcp.json";
    /// The last valid report from the `argonauts_report` tool. The conductor validates it
    /// again, since the agent could write the file itself.
    pub const REPORT: &str = "report.json";
    /// The prompt, for a manifest harness with `prompt = "file"`.
    pub const PROMPT: &str = "prompt.md";
    /// A per-session home made from a harness's template.
    pub const HOME: &str = "home";
    /// The frames sent to an ACP agent. What it sends back goes to `RAW_STDOUT`.
    pub const RAW_STDIN: &str = "raw.stdin.jsonl";
}

/// Per-user files under `~/.argonauts/` (`ARGONAUTS_HOME`).
pub mod user_files {
    pub const CONFIG: &str = "config.toml";
    pub const MODELS: &str = "models.toml";
    pub const PRESETS: &str = "presets";
    pub const VENDORS: &str = "vendors.json";
    /// Live supervisors and their session counts, for global concurrency caps.
    pub const REGISTRY: &str = "registry.json";
    pub const TERMS_SHOWN: &str = "terms-note-shown";
    /// One file per agent process group, named by its pgid. Approval paths refuse a caller
    /// descended from one.
    pub const AGENTS: &str = "agents";
    /// Present after `argonauts auto off`: every checkpoint is supervised until it's removed.
    pub const AUTO_OFF: &str = "auto-off";
    /// Auto-merges and spend per day across every repo, kept under a file lock.
    pub const DAILY: &str = "daily.json";
    /// Your workflow files, `<name>.yaml`.
    pub const WORKFLOWS: &str = "workflows";
    /// Home templates for manifest harnesses, `<template>/`. Never taken from a repo.
    pub const HARNESSES: &str = "harnesses";
}

/// Folders in a repo's state directory, `~/.argonauts/state/<repo>/`.
pub mod repo_state {
    pub const RUNS: &str = "runs";
    /// Chats with the default agent, `<chat>.jsonl` of `ChatEvent`s plus a `<chat>.json` with
    /// the vendor session id to resume.
    pub const CHAT: &str = "chat";
    pub const MAIN_SESSIONS: &str = "sessions";
    /// Hashes of the project settings files you trusted. Until you trust the current ones,
    /// their safety keys are ignored with a warning.
    pub const TRUSTED: &str = "trusted.json";
    /// Shadow records and spot checks.
    pub const APPROVALS: &str = "approvals.jsonl";
    /// Cross-run engine state, such as the critic rotation.
    pub const CREW: &str = "crew.json";
    /// The outcome log, one JSONL file per run.
    pub const OUTCOMES: &str = "outcomes";
    pub const HISTORY: &str = "history.jsonl";
}

/// Reviewable project files in the repo's `.argonauts/`.
pub mod project_files {
    pub const CONFIG: &str = "config.toml";
    /// Just for you. The first write adds it to `.git/info/exclude`.
    pub const CONFIG_LOCAL: &str = "config.local.toml";
    pub const MODELS: &str = "models.toml";
    pub const APPROVAL_RULES: &str = "approval-rules.md";
    pub const CONVENTIONS: &str = "conventions.md";
    /// Approved goals, `<run>.md`.
    pub const GOAL: &str = "goal";
    pub const NOTES: &str = "notes";
    /// `D-003-<slug>.md`.
    pub const DECISIONS: &str = "decisions";
    /// A preset here overrides yours and the built-in of the same name.
    pub const PRESETS: &str = "presets";
    /// A workflow here overrides yours and the built-in of the same name.
    pub const WORKFLOWS: &str = "workflows";
}

pub fn session_dir(run_dir: &Path, session: &SessionKey) -> PathBuf {
    run_dir.join(run_files::SESSIONS).join(session.as_str())
}

pub fn card_dir(run_dir: &Path, card: &CardId) -> PathBuf {
    run_dir.join(run_files::CARDS).join(card.as_str())
}

pub fn chat_journal(state_dir: &Path, chat: &ChatId) -> PathBuf {
    state_dir
        .join(repo_state::CHAT)
        .join(format!("{}.jsonl", chat.as_str()))
}

/// `tree_root` is the run's integration worktree or a solo card's worktree, never your main
/// checkout.
pub fn goal_file(tree_root: &Path, run: &RunId) -> PathBuf {
    tree_root
        .join(".argonauts")
        .join(project_files::GOAL)
        .join(format!("{}.md", run.as_str()))
}
