//! File and folder names for everything Argonauts Code stores. Build paths from these and `Paths`,
//! never by hand.

use std::path::{Path, PathBuf};

use crate::base::names::{ConversationId, SessionKey};
use crate::{RunId, TaskId};

/// Files in a run's directory, `~/.argonauts/state/<repo>/runs/<run>/`.
pub mod run_files {
    pub const EVENTS: &str = "events.jsonl";
    /// Narrator lines. Never read into any agent's context.
    pub const NARRATOR: &str = "narrator.jsonl";
    pub const SETTINGS: &str = "settings.json";
    /// What `ConductorApi::start` asked for. The run host writes `RunStarted` itself, under
    /// the lock, so a run is never seen without its run host.
    pub const START: &str = "start.json";
    pub const LOCK: &str = "run_host.lock";
    pub const PID: &str = "run_host.pid";
    pub const SOCKET: &str = "control.sock";
    pub const RUN_HOST_LOG: &str = "run_host.log";
    /// A random token `Answer` requests must carry, mode 0600.
    pub const APPROVE_TOKEN: &str = "approve.token";
    /// `ControlRequest`s sent while no run host was running. The next run host feeds them to
    /// the engine first, so the engine stays the only writer of `Decided` events.
    pub const PENDING_ANSWERS: &str = "pending-answers.jsonl";
    /// Sealed drafts, one file per draft, unreadable to every session until all are sealed.
    pub const DRAFTS: &str = "drafts";
    pub const TASKS: &str = "tasks";
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
    /// orphan left by a crashed run host before it resumes the CLI session.
    pub const PROCESS: &str = "process.json";
    /// Every command the session was launched with, one JSON line each in order, with the
    /// prompt and the variables' values left out, so whether a launch resumed shows later.
    pub const LAUNCHES: &str = "launches.jsonl";
    /// The MCP config passed to the CLI: only the `argonauts_status` server.
    pub const MCP_CONFIG: &str = "mcp.json";
    /// The last valid report from the `argonauts_report` tool. Argonauts Code validates it
    /// again, since the agent could write the file itself.
    pub const REPORT: &str = "report.json";
    /// The prompt, for a manifest CLI with `prompt = "file"`.
    pub const PROMPT: &str = "prompt.md";
    /// A per-session home made from a CLI's template.
    pub const HOME: &str = "home";
    /// The frames sent to an ACP agent. What it sends back goes to `RAW_STDOUT`.
    pub const RAW_STDIN: &str = "raw.stdin.jsonl";
    /// The session's last count by its CLI, whole, written over at each count. The run's
    /// journal keeps only its rows.
    pub const CONTEXT: &str = "context.json";
}

/// Per-user files under `~/.argonauts/` (`ARGONAUTS_HOME`).
pub mod user_files {
    pub const CONFIG: &str = "config.toml";
    pub const MODELS: &str = "models.toml";
    /// What model discovery last found, so `/model` lists it before the next refresh lands.
    pub const MODELS_CACHE: &str = "models-cache.json";
    pub const AGENTS_FILE: &str = "agents.json";
    /// Live run hosts and their session counts, for global concurrency caps.
    pub const REGISTRY: &str = "registry.json";
    pub const TERMS_SHOWN: &str = "terms-note-shown";
    /// One file per agent process group, named by its pgid. Approval paths refuse a caller
    /// descended from one.
    pub const AGENTS: &str = "agents";
    /// Present after `argonauts auto off`: every approval is supervised until it's removed.
    pub const AUTO_OFF: &str = "auto-off";
    /// Auto-merges and spend per day across every repo, kept under a file lock.
    pub const DAILY: &str = "daily.json";
    /// Your workflow files, `<name>.yaml`.
    pub const WORKFLOWS: &str = "workflows";
    /// Home templates for manifest CLIs, `<template>/`. Never taken from a repo.
    pub const CLIS: &str = "clis";
    /// The deeper Behaviors map: every summary, vector and position, kept so none is made twice.
    pub const BEHAVIORS: &str = "behaviors";
}

/// Folders in a repo's state directory, `~/.argonauts/state/<repo>/`.
pub mod repo_state {
    pub const RUNS: &str = "runs";
    /// Conversations with the main agent, `<conversation>.jsonl` of `ConversationEvent`s plus a
    /// `<conversation>.json` with the CLI session id to resume.
    pub const CONVERSATION: &str = "conversation";
    pub const MAIN_SESSIONS: &str = "sessions";
    /// Hashes of the project settings files you trusted. Until you trust the current ones,
    /// their safety keys are ignored with a warning.
    pub const TRUSTED: &str = "trusted.json";
    /// Auto preview records and spot checks.
    pub const APPROVALS: &str = "approvals.jsonl";
    /// Cross-run engine state, such as the critic rotation.
    pub const CREW: &str = "crew.json";
    /// The outcome log, one JSONL file per run.
    pub const OUTCOMES: &str = "outcomes";
    pub const HISTORY: &str = "history.jsonl";
    /// The replay set: its tuning and holdout lists and the hidden tests, outside every
    /// sandbox's read scope.
    pub const REPLAY: &str = "replay";
    /// The evolution loop's candidates, canaries, and spend.
    pub const EVOLUTION: &str = "evolution";
    /// Skills staged for import, never inside the repo.
    pub const IMPORTS: &str = "imports";
    /// The history Argonauts Code keeps of a folder that has no git of its own: a git repository
    /// whose work tree is your folder, so your folder never gets a `.git`.
    pub const HIDDEN_GIT: &str = "history.git";
    /// Each run's approved goal, `<run>.md`. Argonauts Code's own files stay here, never in your
    /// repository, so nothing of theirs is ever committed or merged.
    pub const GOALS: &str = "goals";
    /// The crew's notes, one Markdown file per topic, read by later runs.
    pub const NOTES: &str = "notes";
    /// The plans' decision records, `D-003-<slug>.md`.
    pub const DECISIONS: &str = "decisions";
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
    /// A workflow here overrides yours and the built-in of the same name.
    pub const WORKFLOWS: &str = "workflows";
    /// Learned workflows, `<name>/v<N>.yaml`, with `<name>/current` naming the version that runs.
    pub const LEARNED_WORKFLOWS: &str = "learned";
    /// Finished runs' outcome logs, when `outcomes.share` is on.
    pub const OUTCOMES: &str = "outcomes";
}

pub fn session_dir(run_dir: &Path, session: &SessionKey) -> PathBuf {
    run_dir.join(run_files::SESSIONS).join(session.as_str())
}

pub fn task_dir(run_dir: &Path, task: &TaskId) -> PathBuf {
    run_dir.join(run_files::TASKS).join(task.as_str())
}

pub fn conversation_journal(state_dir: &Path, conversation: &ConversationId) -> PathBuf {
    state_dir
        .join(repo_state::CONVERSATION)
        .join(format!("{}.jsonl", conversation.as_str()))
}

/// The goal file's path inside the repo's state directory, `goals/<run>.md`.
pub fn goal_file(run: &RunId) -> PathBuf {
    Path::new(repo_state::GOALS).join(format!("{}.md", run.as_str()))
}
