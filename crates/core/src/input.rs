//! Parses what you type: slash commands, `@mentions`, and plain text. The UI's menus and the
//! controller's routing share this parser, so they never disagree.

mod at_words;
mod cli;
mod draft;
mod mods;
mod prompts;
mod shared;

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::RunId;
use crate::TaskId;
use crate::Verbosity;
use crate::agent_sessions::AgentSessionRef;
use crate::cli_sessions::CliSessionRef;
use crate::names::{AgentId, ConversationId, ModelRef, RoleName};
use crate::settings::SettingScope;
use crate::settings::ui::NarratorMode;

pub use at_words::{AtWord, at_word, at_words};
pub use cli::parse_input_with;
pub use draft::{DraftCommand, NOT_A_COMMAND, draft_command, is_command_word};
pub use mods::{ModPlaceChange, ModsAction, ViewAction};
pub use prompts::PromptsAction;
pub use shared::SHARED_COMMANDS;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "input", rename_all = "snake_case")]
pub enum ParsedInput {
    Empty,
    Slash {
        command: SlashCommand,
    },
    Mention {
        target: MentionTarget,
        text: String,
    },
    /// Its meaning depends on the app's mode.
    Plain {
        text: String,
    },
}

/// Every slash command, in the `/` menu's order. `value` fields hold TOML text.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "command", rename_all = "snake_case")]
pub enum SlashCommand {
    /// A fresh conversation with the main agent. Runs keep going.
    Clear,
    /// No target opens the picker of conversations and runs, newest first.
    Resume {
        target: Option<ResumeTarget>,
    },
    /// Close the screen. Runs keep going.
    Exit,
    /// An answer, never a run.
    Ask {
        question: String,
    },
    Captain {
        task: String,
    },
    Crew {
        task: String,
    },
    /// The crew does it as a workflow, the main agent's phases. No task opens the workflow view, as
    /// `/workflows` does. Nothing typed makes it now that `/workflow` picks the workflow; a
    /// request that asks for one reaches it.
    WorkflowRun {
        task: Option<String>,
    },
    /// A new run that builds on a finished one.
    FollowUp {
        /// The latest merged run when omitted.
        run: Option<RunId>,
        request: String,
    },
    /// Where each run stands. Zero tokens; `s` in its view asks the narrator for a summary.
    Status {
        #[serde(default, skip_serializing_if = "ViewAction::is_open")]
        action: ViewAction,
    },
    /// The workflow view, as Ctrl+T opens it. The screen handles it; an outside agent without one
    /// gets the run's workflow, or the list of workflows when no run is on screen.
    Workflows,
    /// Every live session's context, cap, tool calls, and spend. Zero tokens.
    Context {
        /// The system prompt's sections and each tool too, as Claude Code's `/context all`.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        all: bool,
        #[serde(default, skip_serializing_if = "ViewAction::is_open")]
        action: ViewAction,
    },
    /// Go back to one of `task`'s checkpoints, or any task's. The work after it is kept on a side
    /// branch.
    Rewind {
        task: Option<TaskId>,
    },
    Reassign {
        /// The task, or every task of `from`.
        task: Option<TaskId>,
        from: Option<AgentId>,
        to: ModelRef,
    },
    /// Stop `run`, or the run on screen. Its work is committed.
    Stop {
        run: Option<RunId>,
    },
    /// Neither opens the picker. A model without a role is the main agent's.
    Model {
        role: Option<RoleName>,
        worker: Option<ModelRef>,
        /// Saved with the model, as the models level's effort line sets it.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        effort: Option<String>,
    },
    /// Reasoning effort for the main agent or a role. No level opens the picker.
    Effort {
        role: Option<RoleName>,
        /// One of [`EFFORT_LEVELS`].
        level: Option<String>,
        /// `ultracode`, as the slider's toggle set it, saved with the level. Alone, it's
        /// saved without one.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        ultracode: Option<bool>,
    },
    Agents {
        #[serde(default)]
        action: AgentsAction,
    },
    /// `/workflow`: no name opens the picker. `show`, `copy`, `diff`, `new`, `try` and `edit`
    /// are actions, so they can't be names.
    Workflow {
        name: Option<String>,
        #[serde(default)]
        action: WorkflowVerb,
    },
    /// `/workflow new <name> [from <built-in>]`: writes a workflow of your own from the
    /// template, or from a built-in one.
    WorkflowNew {
        name: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        from: Option<String>,
    },
    /// `/workflow try <name> <request>`: what it would run for the request. Nothing runs.
    WorkflowTry {
        name: String,
        request: String,
    },
    /// `/workflow edit <name>`: its page, where each step's keys change with a check on every
    /// change. A script goes to your editor.
    WorkflowEdit {
        name: String,
    },
    /// `/<name> <request>`: one request with the workflow of that name. The workflow in use
    /// stays as it was.
    WorkflowRequest {
        workflow: String,
        request: String,
    },
    /// `/prompts`: who writes what each agent reads, and the templates you may change.
    Prompts {
        #[serde(default)]
        action: PromptsAction,
    },
    /// `/templates`: the built-in workflows and prompts, which is in use and which you changed.
    /// With a `name`, a copy of that one goes into this folder's `.argonauts/`, or into
    /// `~/.argonauts/` with `global`.
    Templates {
        name: Option<String>,
        #[serde(default)]
        global: bool,
    },
    /// `/fallback <role> <key> <value>` sets one key under `fallback.<role>.`, the table of
    /// the crew role that `role` follows.
    Fallback {
        role: Option<RoleName>,
        key: Option<String>,
        value: Option<String>,
    },
    /// No argument opens the picker; `/approvals <key> <value>` sets one key under `approvals.`.
    Approvals {
        /// Without the `approvals.` prefix, such as `merge.mode`.
        #[serde(default)]
        key: Option<String>,
        #[serde(default)]
        value: Option<String>,
    },
    /// `/limits <key> <value>` sets one key under `limits.`.
    Limits {
        key: Option<String>,
        value: Option<String>,
        #[serde(default, skip_serializing_if = "ViewAction::is_open")]
        action: ViewAction,
    },
    /// `/stats`: the usage panel's Stats tab, or its card pinned above the input. No period
    /// takes the one `mods/stats.toml` sets.
    Stats {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        period: Option<String>,
        #[serde(default, skip_serializing_if = "ViewAction::is_open")]
        action: ViewAction,
    },
    /// No argument opens the picker; `/permissions <key> <value>` sets one of [`PERMISSION_KEYS`]
    /// for every agent. With a role first, the picker shows that role's own sandbox, and
    /// `/permissions <role> <key> <value>` sets one of [`ROLE_SANDBOX_KEYS`] under
    /// `roles.<role>.sandbox.`.
    Permissions {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        role: Option<RoleName>,
        #[serde(default)]
        key: Option<String>,
        #[serde(default)]
        value: Option<String>,
    },
    /// Who is good at what. An agent opens the picker on its rows.
    Records {
        #[serde(default)]
        agent: Option<String>,
    },
    /// Learned workflows and how they evolve. A name opens the picker on that learned workflow.
    LearnedWorkflows {
        #[serde(default)]
        name: Option<String>,
    },
    /// Exported and imported skills, and any drift.
    Skills,
    /// The crew's notes, flagged ones waiting for you, and the conventions.
    Memory,
    /// No level opens the picker. Saved as `ui.verbosity` in your global config.
    Verbosity {
        level: Option<Verbosity>,
    },
    Narrator {
        change: Option<NarratorChange>,
    },
    /// No name opens the picker.
    Theme {
        /// `auto`, `dark`, or `light`.
        name: Option<String>,
    },
    /// Toggle vim keys; saved as `ui.keys` in your global config.
    Vim,
    /// Where each mod shows and how it draws. `/pin` and `/unpin` are its changes above
    /// the input.
    Mods {
        action: ModsAction,
        /// Where the change is saved; unset is the layer its value came from.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        scope: Option<SettingScope>,
    },
    /// `/mods <name>` for a mod that is a file: its page, or with `pin` or `unpin` one change
    /// to where it shows.
    ModFile {
        name: String,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        words: Vec<String>,
    },
    /// Show settings, set one, or reset one (`/config <key> --reset`).
    Config {
        /// A dotted key.
        key: Option<String>,
        value: Option<String>,
        /// Drop `key` from the layer that sets it now, so the layer below shows through.
        #[serde(default)]
        reset: bool,
    },
    /// The guided setup again.
    Setup,
    /// Shows the MCP config `outside agent` needs, and writes it only after you confirm.
    Connect {
        outside_agent: OutsideAgent,
    },
    /// How this folder keeps history: with no git, off, hidden, or made a repository; in a
    /// repository, what a merged run leaves on your branch.
    Git,
    /// `/git clean`: removes what ended runs left behind in this folder's history and packs
    /// the rest. Never run by itself.
    GitClean,
    /// `/runs archive`: moves a run that can't go on out of the way, the one named or every
    /// run an older Argonauts Code wrote.
    Runs {
        archive: Option<RunId>,
    },
    /// The agent CLIs, their versions, and logins.
    Doctor,
    /// Keeps the computer awake: until you quit with no `time`, for a time such as `2h`,
    /// and `off` lets it sleep again, also while agents work.
    Awake {
        time: Option<String>,
    },
    /// Each installed agent CLI updated in turn the way it was installed, then Doctor.
    Update,
    Help,
    /// The pages this conversation published on claude.ai with Claude Code's Artifact tool:
    /// open one in the browser, or copy its link.
    Artifacts,
    /// Free the main agent's context by summarizing the conversation, whatever the CLI.
    Compact {
        instructions: Option<String>,
    },
    /// No name lets the main agent suggest one.
    Rename {
        name: Option<String>,
    },
    /// A new conversation that starts from here. This one stays as it is.
    Branch {
        name: Option<String>,
    },
    /// A copy of the conversation that carries on in the background.
    Fork {
        prompt: Option<String>,
    },
    /// A side question whose answer stays out of the conversation's history.
    Btw {
        question: Option<String>,
    },
    /// One line on where the conversation stands.
    Recap,
    /// The last answer to the clipboard, or the `nth` counting back from it.
    Copy {
        nth: Option<u32>,
    },
    /// The conversation as plain text.
    Export {
        file: Option<PathBuf>,
    },
    /// Work in another folder. No path shows the current one.
    Cd {
        path: Option<PathBuf>,
    },
    /// One more folder the agents may work in.
    AddDir {
        path: PathBuf,
    },
    /// Plan first, and change nothing before you agree.
    Plan {
        task: Option<String>,
    },
    /// A goal the main agent works toward until it's met. No text shows the goal; `clear`,
    /// `pause`, and `resume` act on it.
    Goal {
        text: Option<String>,
    },
    /// No value toggles it.
    Fast {
        on: Option<bool>,
    },
    OutputStyle {
        style: Option<String>,
    },
    /// The working tree's changes.
    Diff,
    /// A review of the changes: the working tree when no target is named, else a branch, a
    /// commit range, or a pull request.
    Review {
        target: Option<String>,
    },
    SecurityReview {
        target: Option<String>,
    },
    /// Spend and plan usage for each agent.
    Usage {
        /// `day`, `week`, `month`, or `all`, or a CLI's own word such as `weekly`; the
        /// current windows when `None`.
        period: Option<String>,
        #[serde(default, skip_serializing_if = "ViewAction::is_open")]
        action: ViewAction,
    },
    /// Run an agent CLI's own login. No agent means the main agent's.
    Login {
        agent: Option<String>,
    },
    Logout {
        agent: Option<String>,
    },
    ReleaseNotes,
    Feedback {
        text: Option<String>,
    },
    /// A command of the main agent's CLI, sent to its session as the command itself.
    Cli {
        /// Without the slash, as the CLI names it.
        name: String,
        #[serde(default, skip_serializing_if = "String::is_empty")]
        args: String,
    },
}

/// What `/resume` carries on.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "id", rename_all = "snake_case")]
pub enum ResumeTarget {
    /// A conversation with the main agent, `conversation-<n>`.
    Conversation(ConversationId),
    Run(RunId),
    /// A session of the main agent's CLI, `<adapter>:<session id>`, carried on through
    /// the CLI's own resume.
    CliSession(CliSessionRef),
    /// One agent's session in a run, `<run>/<session>`, carried on through its CLI's own resume
    /// in the folder it ran in.
    Agent(AgentSessionRef),
    /// A new conversation that starts from a conversation's turns, `conversation-<n> branch`.
    /// The original stays as it was.
    BranchConversation(ConversationId),
    /// A new session that starts from a CLI session's turns, `<adapter>:<session id> branch`.
    /// The original stays as it was.
    BranchCliSession(CliSessionRef),
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AgentsAction {
    /// Every agent Argonauts Code can use: CLIs, endpoints, and ACP agents.
    #[default]
    List,
    /// Hermes, OpenClaw, OpenCode, Ollama, or the name of your own CLI.
    Add { name: String },
}

/// An outside agent that drives Argonauts Code over MCP.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum OutsideAgent {
    Hermes,
    OpenClaw,
}

impl OutsideAgent {
    /// The word `/connect` takes.
    pub fn as_str(self) -> &'static str {
        match self {
            OutsideAgent::Hermes => "hermes",
            OutsideAgent::OpenClaw => "openclaw",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "change", rename_all = "snake_case")]
pub enum NarratorChange {
    Mode { mode: NarratorMode },
    Model { model: ModelRef },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "target", content = "value", rename_all = "snake_case")]
pub enum MentionTarget {
    /// `@codex`, `@claude-code`: every session of that agent in the run.
    Worker(ModelRef),
    /// `@task-002`: the task's worker.
    Task(TaskId),
    /// `@reviewer`: everyone in the role.
    Role(RoleName),
    /// `@2`: the agent with follow key 2.
    Number(u8),
    All,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Handler {
    Ui,
    /// The UI and the controller: the UI changes its display and tells the controller.
    Both,
    App,
}

/// The groups of the `/` menu and `/help`, in the order they show.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum CommandCategory {
    Session,
    Work,
    Runs,
    Crew,
    ApprovalsAndLimits,
    Learning,
    Display,
    SetupAndHelp,
}

impl CommandCategory {
    /// The dim header above the group.
    pub fn title(self) -> &'static str {
        match self {
            CommandCategory::Session => "Session",
            CommandCategory::Work => "Work",
            CommandCategory::Runs => "Runs",
            CommandCategory::Crew => "Crew",
            CommandCategory::ApprovalsAndLimits => "Approvals and limits",
            CommandCategory::Learning => "Learning",
            CommandCategory::Display => "Display",
            CommandCategory::SetupAndHelp => "Setup and help",
        }
    }
}

/// One row of the `/` menu.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SlashSpec {
    /// Without the slash.
    pub name: &'static str,
    /// Argument hint, such as `<task>`.
    pub args: &'static str,
    pub help: &'static str,
    pub handler: Handler,
    pub category: CommandCategory,
}

impl SlashSpec {
    /// Whether an argument outside `[...]` is required, as in `/follow-up [run] <request>` or
    /// the word of `/runs archive [run]`, so picking the command from the menu waits for it.
    pub fn needs_argument(&self) -> bool {
        let mut optional = 0_usize;
        for c in self.args.chars() {
            match c {
                '[' => optional += 1,
                ']' => optional = optional.saturating_sub(1),
                c if optional == 0 && !c.is_whitespace() => return true,
                _ => {}
            }
        }
        false
    }
}

/// The `/` menu, in display order: by category, then in the order written here.
#[rustfmt::skip]
pub const SLASH_COMMANDS: &[SlashSpec] = &[
    SlashSpec { name: "clear", args: "", help: "Start a fresh conversation with the main agent. Runs keep going", handler: Handler::App, category: CommandCategory::Session },
    SlashSpec { name: "resume", args: "", help: "Pick an earlier conversation or a run to carry on", handler: Handler::App, category: CommandCategory::Session },
    SlashSpec { name: "artifacts", args: "", help: "The pages this conversation published: open one or copy its link", handler: Handler::Ui, category: CommandCategory::Session },
    SlashSpec { name: "exit", args: "", help: "Close the screen. Runs keep going", handler: Handler::Ui, category: CommandCategory::Session },
    SlashSpec { name: "ask", args: "<question>", help: "Answer only, never a run", handler: Handler::App, category: CommandCategory::Work },
    SlashSpec { name: "captain", args: "<task>", help: "The CLI you talk to does it itself", handler: Handler::App, category: CommandCategory::Work },
    SlashSpec { name: "crew", args: "<task>", help: "The crew does it", handler: Handler::App, category: CommandCategory::Work },
    SlashSpec { name: "follow-up", args: "[run] <request>", help: "Follow up on a finished run", handler: Handler::App, category: CommandCategory::Work },
    SlashSpec { name: "status", args: "[print|pin]", help: "Where each run stands: goal, plan, tasks, limit, plan usage. Costs nothing. s in it asks the narrator for a summary", handler: Handler::App, category: CommandCategory::Runs },
    SlashSpec { name: "workflows", args: "", help: "The workflow view (Ctrl+T). Its Library tab shows, copies, and diffs workflow files", handler: Handler::Ui, category: CommandCategory::Runs },
    SlashSpec { name: "context", args: "[all] [print|pin]", help: "Each session's context, cap, calls, and spend", handler: Handler::App, category: CommandCategory::Runs },
    SlashSpec { name: "rewind", args: "[task]", help: "Go back to a checkpoint", handler: Handler::App, category: CommandCategory::Runs },
    SlashSpec { name: "reassign", args: "<task> <agent>", help: "Move a task to another agent", handler: Handler::App, category: CommandCategory::Runs },
    SlashSpec { name: "stop", args: "[run]", help: "Stop a run. Its work is committed", handler: Handler::App, category: CommandCategory::Runs },
    SlashSpec { name: "runs", args: "archive [run]", help: "Move a run that can't go on out of the way: the one named, or every run an older Argonauts Code wrote", handler: Handler::App, category: CommandCategory::Runs },
    SlashSpec { name: "model", args: "[role] [model]", help: "The main agent or a role, then its model, from each CLI's live list", handler: Handler::App, category: CommandCategory::Crew },
    SlashSpec { name: "effort", args: "[role] [level]", help: "Reasoning effort, from the levels that model supports", handler: Handler::App, category: CommandCategory::Crew },
    SlashSpec { name: "agents", args: "[add <name>]", help: "The agents Argonauts Code can use (CLIs, endpoints, ACP agents): add one (Hermes, OpenClaw, OpenCode, Ollama, your own CLI), turn one on or off, check its login", handler: Handler::App, category: CommandCategory::Crew },
    SlashSpec { name: "workflow", args: "[name|new|try|edit]", help: "How the crew works: a built-in workflow or one of your own. new <name> writes one, try <name> <request> shows what it would run", handler: Handler::App, category: CommandCategory::Crew },
    SlashSpec { name: "prompts", args: "[show|copy|diff|reset <name>]", help: "Who writes what each agent reads, and the templates you can change", handler: Handler::App, category: CommandCategory::Crew },
    SlashSpec { name: "templates", args: "[name] [--global]", help: "The built-in workflows and prompts: the one in use, the ones you changed, and a copy of one to make your own", handler: Handler::App, category: CommandCategory::Crew },
    SlashSpec { name: "fallback", args: "", help: "Who an agent falls back to when it stops, and what they're told", handler: Handler::App, category: CommandCategory::Crew },
    SlashSpec { name: "approvals", args: "", help: "Each approval's mode, and what waits for you", handler: Handler::App, category: CommandCategory::ApprovalsAndLimits },
    SlashSpec { name: "limits", args: "[print|pin]", help: "Run and task limits", handler: Handler::App, category: CommandCategory::ApprovalsAndLimits },
    SlashSpec { name: "stats", args: "[today|week|month|all] [pin]", help: "What each CLI and model did, and all together: tasks, reviews, tokens, spend", handler: Handler::App, category: CommandCategory::ApprovalsAndLimits },
    SlashSpec { name: "permissions", args: "", help: "Whose rules guard the agents, internet, commits, protected paths", handler: Handler::App, category: CommandCategory::ApprovalsAndLimits },
    SlashSpec { name: "records", args: "[agent]", help: "Who is good at what: each agent's track record and prediction score by kind of work. Pin, reset, or rebuild one", handler: Handler::App, category: CommandCategory::Learning },
    SlashSpec { name: "learned", args: "[name]", help: "Workflows learned from your runs, and how they evolve: versions, candidates, replays, canaries. Promote, roll back, or export one as a skill", handler: Handler::App, category: CommandCategory::Learning },
    SlashSpec { name: "skills", args: "", help: "Exported and imported skills, and any drift. Import one as a branch for you to review", handler: Handler::App, category: CommandCategory::Learning },
    SlashSpec { name: "memory", args: "", help: "Notes the crew keeps, flagged ones waiting for you, and the conventions. Sync AGENTS.md and CLAUDE.md", handler: Handler::App, category: CommandCategory::Learning },
    SlashSpec { name: "verbosity", args: "[full|normal|quiet]", help: "How much of each agent's work you see", handler: Handler::Both, category: CommandCategory::Display },
    SlashSpec { name: "narrator", args: "", help: "The narrator line", handler: Handler::App, category: CommandCategory::Display },
    SlashSpec { name: "theme", args: "", help: "Light, dark, or auto", handler: Handler::Both, category: CommandCategory::Display },
    SlashSpec { name: "vim", args: "", help: "Vim keys on or off", handler: Handler::Both, category: CommandCategory::Display },
    SlashSpec { name: "mods", args: "[mod] [change]", help: "Where each mod shows, above the input, on the bottom line, or on a tab, and how it draws", handler: Handler::App, category: CommandCategory::Display },
    SlashSpec { name: "pin", args: "<mod>", help: "Show a mod above the input, such as context", handler: Handler::App, category: CommandCategory::Display },
    SlashSpec { name: "unpin", args: "<mod>", help: "Take a mod from above the input", handler: Handler::App, category: CommandCategory::Display },
    SlashSpec { name: "config", args: "[key=value|all]", help: "Settings, each changed in place. key=value sets one; all lists every key and where it came from", handler: Handler::App, category: CommandCategory::SetupAndHelp },
    SlashSpec { name: "settings", args: "[key=value|all]", help: "Settings, as /config opens them", handler: Handler::App, category: CommandCategory::SetupAndHelp },
    SlashSpec { name: "setup", args: "", help: "The guided setup again", handler: Handler::App, category: CommandCategory::SetupAndHelp },
    SlashSpec { name: "git", args: "[clean]", help: "How this folder keeps history: off, hidden, or made a git repository. In a repository, what a merged run leaves on your branch. clean removes what ended runs left behind", handler: Handler::App, category: CommandCategory::SetupAndHelp },
    SlashSpec { name: "connect", args: "<hermes|openclaw>", help: "Let an outside agent drive Argonauts Code over MCP: shows the config it needs, and writes it only after you confirm", handler: Handler::App, category: CommandCategory::SetupAndHelp },
    SlashSpec { name: "doctor", args: "", help: "The agent CLIs, their versions, and logins", handler: Handler::App, category: CommandCategory::SetupAndHelp },
    SlashSpec { name: "awake", args: "[time|off]", help: "Keep the computer awake until you quit, or for a time such as 2h. It stays awake by itself while anything works", handler: Handler::App, category: CommandCategory::SetupAndHelp },
    SlashSpec { name: "update", args: "", help: "Update each agent CLI the way it was installed, then run Doctor", handler: Handler::App, category: CommandCategory::SetupAndHelp },
    SlashSpec { name: "help", args: "", help: "Keys and commands, by category", handler: Handler::Ui, category: CommandCategory::SetupAndHelp },
];

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkflowVerb {
    #[default]
    Use,
    /// `/workflow show <name>`: print it, from its file or the built-in.
    Show,
    /// `/workflow copy <name>`: write the built-in out to `~/.argonauts/workflows/` to edit.
    Copy,
    /// `/workflow copy <name> --project`: write it to `.argonauts/workflows/`, to commit and share.
    CopyToProject,
    /// `/workflow diff <name>`: compare your file with the current built-in.
    Diff,
}

/// What the Library tab of the workflow view does with a workflow file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkflowAction {
    /// Print it, from its file or the built-in.
    Show,
    /// Write it out to `~/.argonauts/workflows/` to edit.
    Copy,
    /// Write it to `.argonauts/workflows/`, to commit and share.
    CopyToProject,
    /// Compare your file with the current built-in.
    Diff,
}

/// Reasoning effort levels `/effort` accepts. Pickers offer only the ones the model supports.
pub const EFFORT_LEVELS: &[&str] = &["minimal", "low", "medium", "high", "xhigh", "max", "ultra"];

/// The keys `/permissions` covers, in picker order.
pub const PERMISSION_KEYS: &[&str] = &[
    "sandbox.mode",
    "sandbox.network",
    "sandbox.agents_see_other_worktrees",
    "sandbox.agents_commit",
    "limits.protected_paths",
    "limits.dependency_allowlist",
    "limits.spend_per_day",
    // Where Captain works and what a direct turn asks you, which a request can't loosen.
    "captain.kind",
    "captain.asks",
    "captain_or_crew.captain_kind",
];

/// Whether `/permissions <key> <value>` sets `key`: one of [`PERMISSION_KEYS`], or any other
/// key under `sandbox.`, which the app checks against the catalog.
pub fn is_permission_key(key: &str) -> bool {
    PERMISSION_KEYS.contains(&key)
        || key
            .strip_prefix("sandbox.")
            .is_some_and(|rest| !rest.is_empty())
}

/// The sandbox keys a role can set for its own agents, under `roles.<role>.sandbox.`. A key
/// the role leaves unset comes from `[sandbox]`.
pub const ROLE_SANDBOX_KEYS: &[&str] = &[
    "mode",
    "network",
    "agents_see_other_worktrees",
    "agents_commit",
];

/// The menu row for `name`, from [`SLASH_COMMANDS`] or [`SHARED_COMMANDS`].
pub fn slash_spec(name: &str) -> Option<&'static SlashSpec> {
    SLASH_COMMANDS
        .iter()
        .chain(SHARED_COMMANDS)
        .find(|spec| spec.name == name)
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum InputError {
    #[error("unknown command /{0}")]
    UnknownCommand(String),
    #[error("/{command}: {problem}")]
    BadArguments { command: String, problem: String },
    #[error("@ needs a target, such as @codex or @task-002")]
    EmptyMention,
    /// One of the main agent's commands that only its own screen runs.
    #[error("/{command} works only in {agent_id} itself")]
    CliScreenOnly { command: String, agent_id: String },
}

/// `/` and `@` count only at the start. An `@` followed by something that isn't a valid target
/// stays plain text, like an email address, and so does a `/` followed by something that can't
/// name a command, like the path `/Users/me/notes.md`.
pub fn parse_input(text: &str) -> Result<ParsedInput, InputError> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Ok(ParsedInput::Empty);
    }
    // The command or target must follow the `/` or `@` directly.
    if let Some(rest) = trimmed.strip_prefix('/') {
        let (name, args) = if rest.starts_with(char::is_whitespace) {
            ("", rest)
        } else {
            split_word(rest)
        };
        if name.is_empty() || is_command_word(name) {
            return parse_slash(name, args).map(|command| ParsedInput::Slash { command });
        }
        return Ok(ParsedInput::Plain {
            text: trimmed.to_owned(),
        });
    }
    if let Some(rest) = trimmed.strip_prefix('@') {
        let (target, message) = split_word(rest);
        if target.is_empty() || rest.starts_with(char::is_whitespace) {
            return Err(InputError::EmptyMention);
        }
        if let Some(target) = parse_mention_target(target) {
            return Ok(ParsedInput::Mention {
                target,
                text: message.to_owned(),
            });
        }
    }
    Ok(ParsedInput::Plain {
        text: trimmed.to_owned(),
    })
}

fn split_word(text: &str) -> (&str, &str) {
    let text = text.trim_start();
    match text.find(char::is_whitespace) {
        Some(at) => (&text[..at], text[at..].trim()),
        None => (text, ""),
    }
}

fn bad(command: &str, problem: impl Into<String>) -> InputError {
    InputError::BadArguments {
        command: command.to_owned(),
        problem: problem.into(),
    }
}

fn parse_slash(name: &str, args: &str) -> Result<SlashCommand, InputError> {
    let no_args = |command: SlashCommand| {
        if args.is_empty() {
            Ok(command)
        } else {
            Err(bad(name, "takes no arguments"))
        }
    };
    let text_arg = |what: &str| {
        if args.is_empty() {
            Err(bad(name, format!("needs {what}")))
        } else {
            Ok(args.to_owned())
        }
    };
    match name {
        "clear" => no_args(SlashCommand::Clear),
        "resume" => parse_resume(name, args),
        "exit" => no_args(SlashCommand::Exit),
        "ask" => Ok(SlashCommand::Ask {
            question: text_arg("a question")?,
        }),
        "captain" => Ok(SlashCommand::Captain {
            task: text_arg("a task")?,
        }),
        "crew" => Ok(SlashCommand::Crew {
            task: text_arg("a task")?,
        }),
        "follow-up" => parse_follow_up(name, args),
        "status" => mods::parse_status(name, args),
        "workflows" => no_args(SlashCommand::Workflows),
        "context" => mods::parse_context(name, args),
        "rewind" => parse_rewind(name, args),
        "reassign" => parse_reassign(name, args),
        "stop" => parse_stop(name, args),
        "model" => parse_model(name, args),
        "effort" => parse_effort(name, args),
        "agents" => parse_agents(name, args),
        "workflow" => parse_workflow(name, args),
        "prompts" => prompts::parse_prompts(name, args),
        "templates" => parse_templates(name, args),
        "fallback" => parse_fallback(name, args),
        "approvals" => parse_approvals(name, args),
        "limits" => mods::parse_limits(name, args),
        "stats" => mods::parse_stats(name, args),
        "permissions" => parse_permissions(name, args),
        "records" => parse_one_word(name, args).map(|agent| SlashCommand::Records { agent }),
        "learned" => parse_one_word(name, args).map(|name| SlashCommand::LearnedWorkflows { name }),
        "skills" => no_args(SlashCommand::Skills),
        "memory" => no_args(SlashCommand::Memory),
        "verbosity" => parse_verbosity(name, args),
        "narrator" => parse_narrator(name, args),
        "theme" => parse_one_word(name, args).map(|name| SlashCommand::Theme { name }),
        "vim" => no_args(SlashCommand::Vim),
        "mods" => mods::parse_mods(name, args),
        "pin" => mods::parse_pin(name, args, true),
        "unpin" => mods::parse_pin(name, args, false),
        "config" | "settings" => parse_config(name, args),
        "setup" => no_args(SlashCommand::Setup),
        "connect" => parse_connect(name, args),
        "git" => match args {
            "" => Ok(SlashCommand::Git),
            "clean" => Ok(SlashCommand::GitClean),
            _ => Err(bad(name, "takes clean or nothing")),
        },
        "runs" => parse_runs(name, args),
        "doctor" => no_args(SlashCommand::Doctor),
        "awake" => parse_awake(name, args),
        "update" => no_args(SlashCommand::Update),
        "help" => no_args(SlashCommand::Help),
        "artifacts" => no_args(SlashCommand::Artifacts),
        other => shared::parse(other, args)
            .unwrap_or_else(|| Err(InputError::UnknownCommand(other.to_owned()))),
    }
}

/// `/awake`, `/awake off`, or `/awake <time>` with a time such as `2h` or `45m`.
fn parse_awake(name: &str, args: &str) -> Result<SlashCommand, InputError> {
    let usage = "use /awake, /awake <time> such as 2h or 45m, or /awake off";
    let time = match optional_word(name, args, "/awake [time|off]")? {
        None => None,
        Some("off") => Some("off"),
        Some(time) => match crate::settings::Span::new(time).parse() {
            Ok(Some(span)) if !span.is_zero() => Some(time),
            _ => return Err(bad(name, usage)),
        },
    };
    Ok(SlashCommand::Awake {
        time: time.map(str::to_owned),
    })
}

/// `/templates`, `/templates council`, `/templates frames/reviewer --global`. A name becomes
/// a file's path, so it must not reach outside its folder.
fn parse_templates(name: &str, args: &str) -> Result<SlashCommand, InputError> {
    let usage = "use /templates, or /templates <name> [--global] to copy one";
    let mut words: Vec<&str> = args.split_whitespace().collect();
    let global = words.last() == Some(&"--global");
    if global {
        words.pop();
    }
    let template = match words.as_slice() {
        [] if !global => None,
        [word] => {
            let safe = word.split('/').all(|piece| {
                !piece.is_empty()
                    && piece
                        .chars()
                        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
            });
            if !safe {
                return Err(bad(name, format!("{word:?} isn't a template's name")));
            }
            Some((*word).to_owned())
        }
        _ => return Err(bad(name, usage)),
    };
    Ok(SlashCommand::Templates {
        name: template,
        global,
    })
}

fn parse_run(name: &str, word: &str) -> Result<RunId, InputError> {
    RunId::new(word).map_err(|err| bad(name, err.to_string()))
}

/// At most one word, which must not look like a flag.
fn optional_word<'a>(
    name: &str,
    args: &'a str,
    usage: &str,
) -> Result<Option<&'a str>, InputError> {
    match args.split_whitespace().collect::<Vec<_>>().as_slice() {
        [] => Ok(None),
        [word] if !word.starts_with('-') => Ok(Some(*word)),
        _ => Err(bad(name, format!("use {usage}"))),
    }
}

/// `conversation-<n>` is a conversation, `<adapter>:<id>` a CLI's own session, `<run>/<session>` an
/// agent's session in a run, and any other word a run. `branch` after a conversation or a CLI's
/// session starts a new one from it.
fn parse_resume(name: &str, args: &str) -> Result<SlashCommand, InputError> {
    let (args, branch) = match args.trim_end().strip_suffix("branch") {
        Some(rest) if rest.ends_with(char::is_whitespace) => (rest, true),
        _ => (args, false),
    };
    let target = parse_resume_target(name, args)?;
    let target = match (target, branch) {
        (target, false) => target,
        (Some(ResumeTarget::Conversation(id)), true) => Some(ResumeTarget::BranchConversation(id)),
        (Some(ResumeTarget::CliSession(session)), true) => {
            Some(ResumeTarget::BranchCliSession(session))
        }
        (_, true) => {
            return Err(bad(
                name,
                "only a conversation or a CLI's session branches".to_owned(),
            ));
        }
    };
    Ok(SlashCommand::Resume { target })
}

fn parse_resume_target(name: &str, args: &str) -> Result<Option<ResumeTarget>, InputError> {
    let usage = "/resume [conversation, session, or run] [branch]";
    let target = match optional_word(name, args, usage)? {
        None => None,
        Some(word) if word.contains('/') => {
            let agent = AgentSessionRef::parse(word)
                .ok_or_else(|| bad(name, format!("{word:?} isn't an agent's session")))?;
            Some(ResumeTarget::Agent(agent))
        }
        Some(word) if word.starts_with("conversation-") => {
            let conversation = ConversationId::parse(word)
                .ok_or_else(|| bad(name, format!("{word:?} isn't a conversation")))?;
            Some(ResumeTarget::Conversation(conversation))
        }
        Some(word) if word.contains(':') => {
            let session = CliSessionRef::parse(word)
                .ok_or_else(|| bad(name, format!("{word:?} isn't a session")))?;
            Some(ResumeTarget::CliSession(session))
        }
        Some(word) => Some(ResumeTarget::Run(parse_run(name, word)?)),
    };
    Ok(target)
}

/// The first word is a run only when it looks like one (`run-...`), so a request that starts
/// with an ordinary word stays whole.
fn parse_follow_up(name: &str, args: &str) -> Result<SlashCommand, InputError> {
    let (word, rest) = split_word(args);
    let run = word
        .starts_with("run-")
        .then(|| RunId::new(word).ok())
        .flatten();
    let request = if run.is_some() { rest } else { args };
    if request.is_empty() {
        return Err(bad(name, "needs a request: /follow-up [run] <request>"));
    }
    Ok(SlashCommand::FollowUp {
        run,
        request: request.to_owned(),
    })
}

fn parse_rewind(name: &str, args: &str) -> Result<SlashCommand, InputError> {
    let task = match optional_word(name, args, "/rewind [task]")? {
        Some(word) => Some(parse_task(name, word)?),
        None => None,
    };
    Ok(SlashCommand::Rewind { task })
}

fn parse_runs(name: &str, args: &str) -> Result<SlashCommand, InputError> {
    let mut words = args.split_whitespace();
    if words.next() != Some("archive") {
        return Err(bad(name, "takes archive, then a run or nothing"));
    }
    let run = words.next().map(|word| parse_run(name, word)).transpose()?;
    if words.next().is_some() {
        return Err(bad(
            name,
            "archives one run, or with no run every one that can't open",
        ));
    }
    Ok(SlashCommand::Runs { archive: run })
}

fn parse_stop(name: &str, args: &str) -> Result<SlashCommand, InputError> {
    let run = match optional_word(name, args, "/stop [run]")? {
        Some(word) => Some(parse_run(name, word)?),
        None => None,
    };
    Ok(SlashCommand::Stop { run })
}

/// Anything that starts with `-` is a mistyped flag, not a model.
fn parse_worker(name: &str, text: &str) -> Result<ModelRef, InputError> {
    if text.starts_with('-') {
        return Err(bad(name, format!("{text:?} isn't an agent")));
    }
    text.parse()
        .map_err(|err: crate::names::ModelRefError| bad(name, err.to_string()))
}

fn parse_reassign(name: &str, args: &str) -> Result<SlashCommand, InputError> {
    let words: Vec<&str> = args.split_whitespace().collect();
    match words.as_slice() {
        ["--from", agent_id, to] => {
            let agent_id = parse_worker(name, agent_id)?;
            if agent_id.model.is_some() {
                return Err(bad(name, "--from takes an agent, such as codex"));
            }
            Ok(SlashCommand::Reassign {
                task: None,
                from: Some(agent_id.agent_id()),
                to: parse_worker(name, to)?,
            })
        }
        [task, to] if !task.starts_with('-') => Ok(SlashCommand::Reassign {
            task: Some(TaskId::new(*task).map_err(|err| bad(name, err.to_string()))?),
            from: None,
            to: parse_worker(name, to)?,
        }),
        _ => Err(bad(
            name,
            "use /reassign <task> <agent> or /reassign --from <agent> <agent>",
        )),
    }
}

fn parse_narrator(name: &str, args: &str) -> Result<SlashCommand, InputError> {
    let words: Vec<&str> = args.split_whitespace().collect();
    let change = match words.as_slice() {
        [] => None,
        ["off"] => Some(NarratorChange::Mode {
            mode: NarratorMode::Off,
        }),
        ["on_demand" | "on-demand"] => Some(NarratorChange::Mode {
            mode: NarratorMode::OnDemand,
        }),
        ["live"] => Some(NarratorChange::Mode {
            mode: NarratorMode::Live,
        }),
        ["model", worker] => Some(NarratorChange::Model {
            model: parse_worker(name, worker)?,
        }),
        _ => {
            return Err(bad(
                name,
                "use off, on_demand, live, or model <agent>/<model>",
            ));
        }
    };
    Ok(SlashCommand::Narrator { change })
}

fn parse_config(name: &str, args: &str) -> Result<SlashCommand, InputError> {
    // `/config key=value` sets one directly, as Claude Code's does.
    let (key, value) = match args.split_once('=') {
        Some((key, value)) if !key.trim().is_empty() && !key.contains(char::is_whitespace) => (
            Some(key.trim().to_owned()),
            Some(value.trim().to_owned()).filter(|value| !value.is_empty()),
        ),
        _ => key_and_value(args),
    };
    if key.as_deref().is_some_and(|key| key.starts_with('-')) {
        return Err(bad(
            name,
            "use /config, /config key=value, /config all, /config <key>, or /config <key> --reset",
        ));
    }
    let reset = value.as_deref() == Some("--reset");
    Ok(SlashCommand::Config {
        key,
        value: value.filter(|_| !reset),
        reset,
    })
}

fn parse_approvals(name: &str, args: &str) -> Result<SlashCommand, InputError> {
    let (key, value) = key_and_value(args);
    if key.is_some() && value.is_none() {
        return Err(bad(name, "use /approvals or /approvals <key> <value>"));
    }
    Ok(SlashCommand::Approvals { key, value })
}

fn parse_permissions(name: &str, args: &str) -> Result<SlashCommand, InputError> {
    let (first, rest) = split_word(args);
    if let Some(role) = RoleName::parse(first) {
        let (key, value) = key_and_value(rest);
        // `sandbox.mode` reads as well as `mode` after a role.
        let key = key.map(|key| match key.strip_prefix("sandbox.") {
            Some(leaf) => leaf.to_owned(),
            None => key,
        });
        let known = key
            .as_deref()
            .is_none_or(|key| ROLE_SANDBOX_KEYS.contains(&key));
        if !known || (key.is_some() && value.is_none()) {
            return Err(bad(
                name,
                format!(
                    "use /permissions <role> or /permissions <role> <key> <value>, the key one \
                     of {}",
                    ROLE_SANDBOX_KEYS.join(", ")
                ),
            ));
        }
        return Ok(SlashCommand::Permissions {
            role: Some(role),
            key,
            value,
        });
    }
    let (key, value) = key_and_value(args);
    let known = key.as_deref().is_none_or(is_permission_key);
    if !known || (key.is_some() && value.is_none()) {
        return Err(bad(
            name,
            format!(
                "use /permissions [role] or /permissions [role] <key> <value>, the key one of {}",
                PERMISSION_KEYS.join(", ")
            ),
        ));
    }
    Ok(SlashCommand::Permissions {
        role: None,
        key,
        value,
    })
}

fn parse_role(name: &str, word: &str) -> Result<RoleName, InputError> {
    RoleName::parse(word).ok_or_else(|| bad(name, format!("no role named {word:?}")))
}

/// One word is a role when it names one, and otherwise the main agent's model. An effort
/// level may follow the model.
fn parse_model(name: &str, args: &str) -> Result<SlashCommand, InputError> {
    let words: Vec<&str> = args.split_whitespace().collect();
    let (words, effort) = match words.split_last() {
        Some((last, rest)) if !rest.is_empty() && EFFORT_LEVELS.contains(last) => {
            (rest, Some((*last).to_owned()))
        }
        _ => (words.as_slice(), None),
    };
    let (role, worker) = match words {
        [] => (None, None),
        [word] => match RoleName::parse(word) {
            Some(role) => (Some(role), None),
            None => (None, Some(parse_worker(name, word)?)),
        },
        [role, worker] => (
            Some(parse_role(name, role)?),
            Some(parse_worker(name, worker)?),
        ),
        _ => return Err(bad(name, "use /model [role] [model] [effort]")),
    };
    if effort.is_some() && worker.is_none() {
        return Err(bad(name, "name the model before its effort"));
    }
    Ok(SlashCommand::Model {
        role,
        worker,
        effort,
    })
}

fn parse_effort(name: &str, args: &str) -> Result<SlashCommand, InputError> {
    let level = |word: &str| {
        if EFFORT_LEVELS.contains(&word) {
            Ok(word.to_owned())
        } else {
            Err(bad(
                name,
                format!("{word:?} isn't a level; use {}", EFFORT_LEVELS.join(", ")),
            ))
        }
    };
    let words: Vec<&str> = args.split_whitespace().collect();
    let (role, level) = match words.as_slice() {
        [] => (None, None),
        [word] => match RoleName::parse(word) {
            Some(role) => (Some(role), None),
            None => (None, Some(level(word)?)),
        },
        [role, word] => (Some(parse_role(name, role)?), Some(level(word)?)),
        _ => return Err(bad(name, "use /effort [role] [level]")),
    };
    Ok(SlashCommand::Effort {
        role,
        level,
        ultracode: None,
    })
}

fn parse_agents(name: &str, args: &str) -> Result<SlashCommand, InputError> {
    let action = match args.split_whitespace().collect::<Vec<_>>().as_slice() {
        [] => AgentsAction::List,
        ["add", agent] if !agent.starts_with('-') => AgentsAction::Add {
            name: (*agent).to_owned(),
        },
        _ => return Err(bad(name, "use /agents or /agents add <name>")),
    };
    Ok(SlashCommand::Agents { action })
}

fn parse_verbosity(name: &str, args: &str) -> Result<SlashCommand, InputError> {
    let level = match args {
        "" => None,
        "full" => Some(Verbosity::Full),
        "normal" => Some(Verbosity::Normal),
        "quiet" => Some(Verbosity::Quiet),
        _ => return Err(bad(name, "use /verbosity [full|normal|quiet]")),
    };
    Ok(SlashCommand::Verbosity { level })
}

fn parse_connect(name: &str, args: &str) -> Result<SlashCommand, InputError> {
    let outside_agent = match args {
        "hermes" => OutsideAgent::Hermes,
        "openclaw" => OutsideAgent::OpenClaw,
        _ => return Err(bad(name, "use /connect hermes or /connect openclaw")),
    };
    Ok(SlashCommand::Connect { outside_agent })
}

fn parse_one_word(name: &str, args: &str) -> Result<Option<String>, InputError> {
    let words: Vec<&str> = args.split_whitespace().collect();
    match words.as_slice() {
        [] => Ok(None),
        [word] if !word.starts_with('-') => Ok(Some((*word).to_owned())),
        _ => Err(bad(name, format!("use /{name} [name]"))),
    }
}

/// The words after `/workflow` that say what to do, so no workflow can be named one.
const WORKFLOW_ACTIONS: [&str; 6] = ["show", "diff", "copy", "new", "try", "edit"];

fn parse_workflow(name: &str, args: &str) -> Result<SlashCommand, InputError> {
    let words: Vec<&str> = args.split_whitespace().collect();
    let named = |word: &str| !word.starts_with('-') && !WORKFLOW_ACTIONS.contains(&word);
    match words.as_slice() {
        ["new", workflow] if named(workflow) => {
            return Ok(SlashCommand::WorkflowNew {
                name: (*workflow).to_owned(),
                from: None,
            });
        }
        ["new", workflow, "from", from] if named(workflow) => {
            return Ok(SlashCommand::WorkflowNew {
                name: (*workflow).to_owned(),
                from: Some((*from).to_owned()),
            });
        }
        ["new", ..] => return Err(bad(name, "use /workflow new <name> [from <built-in>]")),
        ["try", workflow, _, ..] if named(workflow) => {
            // The request as typed, spaces and all, after the workflow's name.
            let after_try = args.trim_start()["try".len()..].trim_start();
            let request = after_try[workflow.len()..].trim();
            return Ok(SlashCommand::WorkflowTry {
                name: (*workflow).to_owned(),
                request: request.to_owned(),
            });
        }
        ["try", ..] => return Err(bad(name, "use /workflow try <name> <request>")),
        ["edit", workflow] if named(workflow) => {
            return Ok(SlashCommand::WorkflowEdit {
                name: (*workflow).to_owned(),
            });
        }
        ["edit", ..] => return Err(bad(name, "use /workflow edit <name>")),
        _ => {}
    }
    let (chosen, action) = match words.as_slice() {
        [] => {
            return Ok(SlashCommand::Workflow {
                name: None,
                action: WorkflowVerb::Use,
            });
        }
        ["show", chosen] => (*chosen, WorkflowVerb::Show),
        ["diff", chosen] => (*chosen, WorkflowVerb::Diff),
        ["copy", chosen] => (*chosen, WorkflowVerb::Copy),
        ["copy", chosen, "--project"] => (*chosen, WorkflowVerb::CopyToProject),
        [chosen] if !WORKFLOW_ACTIONS.contains(chosen) => (*chosen, WorkflowVerb::Use),
        _ => ("", WorkflowVerb::Use),
    };
    if chosen.is_empty() || chosen.starts_with('-') {
        return Err(bad(
            name,
            "use /workflow [name], /workflow new <name>, /workflow try <name> <request>, \
             /workflow edit|show|diff <name>, or /workflow copy <name> [--project]",
        ));
    }
    Ok(SlashCommand::Workflow {
        name: Some(chosen.to_owned()),
        action,
    })
}

fn parse_task(name: &str, word: &str) -> Result<TaskId, InputError> {
    if !looks_like_task(word) {
        return Err(bad(
            name,
            format!("{word:?} isn't a task, such as task-002"),
        ));
    }
    TaskId::new(word).map_err(|err| bad(name, err.to_string()))
}

/// A key and the rest of the line as its TOML value, or neither.
fn key_and_value(args: &str) -> (Option<String>, Option<String>) {
    let (key, value) = split_word(args);
    (
        (!key.is_empty()).then(|| key.to_owned()),
        (!value.is_empty()).then(|| value.to_owned()),
    )
}

fn parse_fallback(name: &str, args: &str) -> Result<SlashCommand, InputError> {
    let (role, rest) = split_word(args);
    let role = match role {
        "" => None,
        role => Some(parse_role(name, role)?),
    };
    let (key, value) = key_and_value(rest);
    if key.is_some() && value.is_none() {
        return Err(bad(
            name,
            "use /fallback [role] or /fallback <role> <key> <value>",
        ));
    }
    Ok(SlashCommand::Fallback { role, key, value })
}

fn looks_like_task(word: &str) -> bool {
    ["task-", "test-", "experiment-"].iter().any(|prefix| {
        word.strip_prefix(prefix)
            .is_some_and(|n| !n.is_empty() && n.chars().all(|c| c.is_ascii_digit()))
    })
}

fn parse_mention_target(word: &str) -> Option<MentionTarget> {
    if word == "all" {
        return Some(MentionTarget::All);
    }
    if word.chars().all(|c| c.is_ascii_digit()) {
        return word.parse().ok().map(MentionTarget::Number);
    }
    if looks_like_task(word) {
        return TaskId::new(word).ok().map(MentionTarget::Task);
    }
    if let Some(role) = RoleName::parse(word) {
        return Some(MentionTarget::Role(role));
    }
    word.parse().ok().map(MentionTarget::Worker)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn slash(command: SlashCommand) -> Result<ParsedInput, InputError> {
        Ok(ParsedInput::Slash { command })
    }

    fn plain(text: &str) -> Result<ParsedInput, InputError> {
        Ok(ParsedInput::Plain {
            text: text.to_owned(),
        })
    }

    fn mention(target: MentionTarget, text: &str) -> Result<ParsedInput, InputError> {
        Ok(ParsedInput::Mention {
            target,
            text: text.to_owned(),
        })
    }

    fn worker(text: &str) -> ModelRef {
        text.parse().unwrap()
    }

    fn run(text: &str) -> RunId {
        RunId::new(text).unwrap()
    }

    fn task(text: &str) -> TaskId {
        TaskId::new(text).unwrap()
    }

    fn unknown(name: &str) -> Result<ParsedInput, InputError> {
        Err(InputError::UnknownCommand(name.to_owned()))
    }

    fn bad_args(command: &str) -> Result<ParsedInput, InputError> {
        Err(InputError::BadArguments {
            command: command.to_owned(),
            problem: String::new(),
        })
    }

    #[test]
    fn a_prompt_may_start_with_a_path() {
        let path = "/Users/me/shots/menu.png why is the second row cut off?";
        assert_eq!(parse_input(path), plain(path));
        assert_eq!(parse_input("/tmp/build.log"), plain("/tmp/build.log"));
        assert_eq!(parse_input("/3 is the answer"), plain("/3 is the answer"));
        // A word that could name a command still has to be one.
        assert_eq!(parse_input("/tmp"), unknown("tmp"));
        assert_eq!(parse_input("/ tmp"), unknown(""));
    }

    /// Compares errors by kind and command, since problem texts are for people.
    fn same(got: &Result<ParsedInput, InputError>, want: &Result<ParsedInput, InputError>) -> bool {
        match (got, want) {
            (
                Err(InputError::BadArguments { command: a, .. }),
                Err(InputError::BadArguments { command: b, .. }),
            ) => a == b,
            _ => got == want,
        }
    }

    #[test]
    fn grammar_table() {
        #[rustfmt::skip]
        let cases: Vec<(&str, Result<ParsedInput, InputError>)> = vec![
            ("", Ok(ParsedInput::Empty)),
            ("   \n\t ", Ok(ParsedInput::Empty)),
            ("hi", plain("hi")),
            ("  fix the README install command  ", plain("fix the README install command")),
            ("what does a/b mean?", plain("what does a/b mean?")),
            ("mail me@example.com", plain("mail me@example.com")),
            ("@!!! hello", plain("@!!! hello")),
            ("@", Err(InputError::EmptyMention)),
            ("@   hello", Err(InputError::EmptyMention)),
            ("/clear", slash(SlashCommand::Clear)),
            ("/clear all", bad_args("clear")),
            ("/resume", slash(SlashCommand::Resume { target: None })),
            ("/resume run-01", slash(SlashCommand::Resume { target: Some(ResumeTarget::Run(run("run-01"))) })),
            ("/resume conversation-003", slash(SlashCommand::Resume { target: Some(ResumeTarget::Conversation(ConversationId::numbered(3))) })),
            ("/resume run-01 run-02", bad_args("resume")),
            ("/resume conversation-003 branch", slash(SlashCommand::Resume { target: Some(ResumeTarget::BranchConversation(ConversationId::numbered(3))) })),
            ("/resume run-01 branch", bad_args("resume")),
            ("/resume ../x", bad_args("resume")),
            ("/resume conversation-../x", bad_args("resume")),
            ("/resume --last", bad_args("resume")),
            ("/resume codex:-p", bad_args("resume")),
            ("/exit", slash(SlashCommand::Exit)),
            ("/ask how does auth work?", slash(SlashCommand::Ask { question: "how does auth work?".into() })),
            ("/ask", bad_args("ask")),
            ("/captain fix the typo", slash(SlashCommand::Captain { task: "fix the typo".into() })),
            ("  /crew add per-key rate limiting", slash(SlashCommand::Crew { task: "add per-key rate limiting".into() })),
            ("/crew", bad_args("crew")),
            ("/follow-up run-01 add tests", slash(SlashCommand::FollowUp { run: Some(run("run-01")), request: "add tests".into() })),
            ("/follow-up add tests", slash(SlashCommand::FollowUp { run: None, request: "add tests".into() })),
            ("/follow-up running the linter too", slash(SlashCommand::FollowUp { run: None, request: "running the linter too".into() })),
            ("/follow-up", bad_args("follow-up")),
            ("/follow-up run-01", bad_args("follow-up")),
            ("/status", slash(SlashCommand::Status { action: ViewAction::Open })),
            ("/status now", bad_args("status")),
            ("/workflows", slash(SlashCommand::Workflows)),
            ("/workflows review-panel", bad_args("workflows")),
            ("/context", slash(SlashCommand::Context { all: false, action: ViewAction::Open })),
            ("/context now", bad_args("context")),
            ("/rewind", slash(SlashCommand::Rewind { task: None })),
            ("/rewind task-002", slash(SlashCommand::Rewind { task: Some(task("task-002")) })),
            ("/rewind test-004", slash(SlashCommand::Rewind { task: Some(task("test-004")) })),
            ("/rewind run-01", bad_args("rewind")),
            ("/rewind task-002 fallback-1", bad_args("rewind")),
            ("/reassign task-003 codex/gpt-x", slash(SlashCommand::Reassign { task: Some(task("task-003")), from: None, to: worker("codex/gpt-x") })),
            ("/reassign --from claude-code codex/gpt-x", slash(SlashCommand::Reassign { task: None, from: Some(AgentId("claude-code".into())), to: worker("codex/gpt-x") })),
            ("/reassign --from claude-code/opus codex", bad_args("reassign")),
            ("/reassign task-003", bad_args("reassign")),
            ("/reassign task-003 -x", bad_args("reassign")),
            ("/stop", slash(SlashCommand::Stop { run: None })),
            ("/stop run-01", slash(SlashCommand::Stop { run: Some(run("run-01")) })),
            ("/stop run-01 run-02", bad_args("stop")),
            ("/stop ../x", bad_args("stop")),
            ("/model", slash(SlashCommand::Model { role: None, worker: None, effort: None })),
            ("/model reviewer", slash(SlashCommand::Model { role: Some(RoleName::Reviewer), worker: None, effort: None })),
            ("/model codex", slash(SlashCommand::Model { role: None, worker: Some(worker("codex")), effort: None })),
            ("/model claude-code/opus", slash(SlashCommand::Model { role: None, worker: Some(worker("claude-code/opus")), effort: None })),
            ("/model tie-breaker codex/gpt-x", slash(SlashCommand::Model { role: Some(RoleName::TieBreaker), worker: Some(worker("codex/gpt-x")), effort: None })),
            ("/model claude-code/opus xhigh", slash(SlashCommand::Model { role: None, worker: Some(worker("claude-code/opus")), effort: Some("xhigh".into()) })),
            ("/model reviewer codex low", slash(SlashCommand::Model { role: Some(RoleName::Reviewer), worker: Some(worker("codex")), effort: Some("low".into()) })),
            ("/resume codex:019a2f-4c", slash(SlashCommand::Resume { target: Some(ResumeTarget::CliSession(CliSessionRef { agent_id: AgentId("codex".into()), id: "019a2f-4c".into() })) })),
            ("/resume run-03/s-002", slash(SlashCommand::Resume { target: Some(ResumeTarget::Agent(AgentSessionRef::parse("run-03/s-002").unwrap())) })),
            ("/model boss codex", bad_args("model")),
            ("/model -x", bad_args("model")),
            ("/model worker codex extra", bad_args("model")),
            ("/model reviewer high", bad_args("model")),
            ("/effort", slash(SlashCommand::Effort { role: None, level: None, ultracode: None })),
            ("/effort high", slash(SlashCommand::Effort { role: None, level: Some("high".into()), ultracode: None })),
            ("/effort reviewer", slash(SlashCommand::Effort { role: Some(RoleName::Reviewer), level: None, ultracode: None })),
            ("/effort worker max", slash(SlashCommand::Effort { role: Some(RoleName::Worker), level: Some("max".into()), ultracode: None })),
            ("/effort loud", bad_args("effort")),
            ("/effort boss high", bad_args("effort")),
            ("/effort worker high now", bad_args("effort")),
            ("/agents", slash(SlashCommand::Agents { action: AgentsAction::List })),
            ("/agents add hermes", slash(SlashCommand::Agents { action: AgentsAction::Add { name: "hermes".into() } })),
            ("/agents add my-cli", slash(SlashCommand::Agents { action: AgentsAction::Add { name: "my-cli".into() } })),
            ("/agents add", bad_args("agents")),
            ("/agents add --force", bad_args("agents")),
            ("/agents add a b", bad_args("agents")),
            ("/agents codex", bad_args("agents")),
            ("/agents codex model \"gpt-x\"", bad_args("agents")),
            ("/workflow", slash(SlashCommand::Workflow { name: None, action: WorkflowVerb::Use })),
            ("/workflow orchestrator", slash(SlashCommand::Workflow { name: Some("orchestrator".into()), action: WorkflowVerb::Use })),
            ("/workflow show council", slash(SlashCommand::Workflow { name: Some("council".into()), action: WorkflowVerb::Show })),
            ("/workflow diff council", slash(SlashCommand::Workflow { name: Some("council".into()), action: WorkflowVerb::Diff })),
            ("/workflow copy orchestrator", slash(SlashCommand::Workflow { name: Some("orchestrator".into()), action: WorkflowVerb::Copy })),
            ("/workflow copy orchestrator --project", slash(SlashCommand::Workflow { name: Some("orchestrator".into()), action: WorkflowVerb::CopyToProject })),
            ("/workflow show", bad_args("workflow")),
            ("/workflow new spec-first", slash(SlashCommand::WorkflowNew { name: "spec-first".into(), from: None })),
            ("/workflow new mine from council", slash(SlashCommand::WorkflowNew { name: "mine".into(), from: Some("council".into()) })),
            ("/workflow new", bad_args("workflow")),
            ("/workflow new try", bad_args("workflow")),
            ("/workflow try spec-first add  due dates", slash(SlashCommand::WorkflowTry { name: "spec-first".into(), request: "add  due dates".into() })),
            ("/workflow try spec-first", bad_args("workflow")),
            ("/workflow edit spec-first", slash(SlashCommand::WorkflowEdit { name: "spec-first".into() })),
            ("/workflow edit", bad_args("workflow")),
            ("/workflow copy council --global", bad_args("workflow")),
            ("/workflow --project", bad_args("workflow")),
            ("/workflow a b", bad_args("workflow")),
            ("/fallback", slash(SlashCommand::Fallback { role: None, key: None, value: None })),
            ("/fallback worker", slash(SlashCommand::Fallback { role: Some(RoleName::Worker), key: None, value: None })),
            ("/fallback workers", bad_args("fallback")),
            ("/fallback reviewer mode auto", slash(SlashCommand::Fallback { role: Some(RoleName::Reviewer), key: Some("mode".into()), value: Some("auto".into()) })),
            ("/fallback worker mode auto", slash(SlashCommand::Fallback { role: Some(RoleName::Worker), key: Some("mode".into()), value: Some("auto".into()) })),
            ("/fallback worker mode", bad_args("fallback")),
            ("/fallback boss", bad_args("fallback")),
            ("/approvals", slash(SlashCommand::Approvals { key: None, value: None })),
            ("/approvals merge.mode auto", slash(SlashCommand::Approvals { key: Some("merge.mode".into()), value: Some("auto".into()) })),
            ("/approvals rules.merge.max_changed_lines 400", slash(SlashCommand::Approvals { key: Some("rules.merge.max_changed_lines".into()), value: Some("400".into()) })),
            ("/approvals merge.mode", bad_args("approvals")),
            ("/limits", slash(SlashCommand::Limits { key: None, value: None, action: ViewAction::Open })),
            ("/limits spend_per_run 30", slash(SlashCommand::Limits { key: Some("spend_per_run".into()), value: Some("30".into()), action: ViewAction::Open })),
            ("/limits spend_per_run", bad_args("limits")),
            ("/permissions", slash(SlashCommand::Permissions { role: None, key: None, value: None })),
            ("/permissions sandbox.network true", slash(SlashCommand::Permissions { role: None, key: Some("sandbox.network".into()), value: Some("true".into()) })),
            ("/permissions limits.spend_per_day 25.0", slash(SlashCommand::Permissions { role: None, key: Some("limits.spend_per_day".into()), value: Some("25.0".into()) })),
            ("/permissions reviewer", slash(SlashCommand::Permissions { role: Some(RoleName::Reviewer), key: None, value: None })),
            ("/permissions worker mode tool", slash(SlashCommand::Permissions { role: Some(RoleName::Worker), key: Some("mode".into()), value: Some("tool".into()) })),
            ("/permissions worker sandbox.agents_commit false", slash(SlashCommand::Permissions { role: Some(RoleName::Worker), key: Some("agents_commit".into()), value: Some("false".into()) })),
            ("/permissions worker mode", bad_args("permissions")),
            ("/permissions worker protected_paths []", bad_args("permissions")),
            ("/permissions sandbox.fail_closed false", slash(SlashCommand::Permissions { role: None, key: Some("sandbox.fail_closed".into()), value: Some("false".into()) })),
            ("/permissions sandbox. true", bad_args("permissions")),
            ("/permissions all", bad_args("permissions")),
            ("/permissions sandbox.network", bad_args("permissions")),
            ("/permissions ui.keys vim", bad_args("permissions")),
            ("/verbosity", slash(SlashCommand::Verbosity { level: None })),
            ("/verbosity full", slash(SlashCommand::Verbosity { level: Some(Verbosity::Full) })),
            ("/verbosity normal", slash(SlashCommand::Verbosity { level: Some(Verbosity::Normal) })),
            ("/verbosity quiet", slash(SlashCommand::Verbosity { level: Some(Verbosity::Quiet) })),
            ("/verbosity loud", bad_args("verbosity")),
            ("/verbosity quiet now", bad_args("verbosity")),
            ("/narrator", slash(SlashCommand::Narrator { change: None })),
            ("/narrator off", slash(SlashCommand::Narrator { change: Some(NarratorChange::Mode { mode: NarratorMode::Off }) })),
            ("/narrator on_demand", slash(SlashCommand::Narrator { change: Some(NarratorChange::Mode { mode: NarratorMode::OnDemand }) })),
            ("/narrator live", slash(SlashCommand::Narrator { change: Some(NarratorChange::Mode { mode: NarratorMode::Live }) })),
            ("/narrator model ollama/qwen3", slash(SlashCommand::Narrator { change: Some(NarratorChange::Model { model: worker("ollama/qwen3") }) })),
            ("/narrator loud", bad_args("narrator")),
            ("/theme", slash(SlashCommand::Theme { name: None })),
            ("/theme light", slash(SlashCommand::Theme { name: Some("light".into()) })),
            ("/vim", slash(SlashCommand::Vim)),
            ("/config", slash(SlashCommand::Config { key: None, value: None, reset: false })),
            ("/config ui.keys", slash(SlashCommand::Config { key: Some("ui.keys".into()), value: None, reset: false })),
            ("/config ui.keys \"vim\"", slash(SlashCommand::Config { key: Some("ui.keys".into()), value: Some("\"vim\"".into()), reset: false })),
            ("/config checks.run [\"cargo\", \"test\"]", slash(SlashCommand::Config { key: Some("checks.run".into()), value: Some("[\"cargo\", \"test\"]".into()), reset: false })),
            ("/config narrator.model --reset", slash(SlashCommand::Config { key: Some("narrator.model".into()), value: None, reset: true })),
            ("/config ui.tips=false", slash(SlashCommand::Config { key: Some("ui.tips".into()), value: Some("false".into()), reset: false })),
            ("/config checks.run=[\"cargo\", \"test\"]", slash(SlashCommand::Config { key: Some("checks.run".into()), value: Some("[\"cargo\", \"test\"]".into()), reset: false })),
            ("/config all", slash(SlashCommand::Config { key: Some("all".into()), value: None, reset: false })),
            ("/settings", slash(SlashCommand::Config { key: None, value: None, reset: false })),
            ("/settings ui.keys=vim", slash(SlashCommand::Config { key: Some("ui.keys".into()), value: Some("vim".into()), reset: false })),
            ("/config --reset", bad_args("config")),
            ("/config -x 1", bad_args("config")),
            ("/setup", slash(SlashCommand::Setup)),
            ("/setup now", bad_args("setup")),
            ("/connect hermes", slash(SlashCommand::Connect { outside_agent: OutsideAgent::Hermes })),
            ("/connect openclaw", slash(SlashCommand::Connect { outside_agent: OutsideAgent::OpenClaw })),
            ("/connect", bad_args("connect")),
            ("/connect slack", bad_args("connect")),
            ("/connect hermes openclaw", bad_args("connect")),
            ("/git", slash(SlashCommand::Git)),
            ("/git clean", slash(SlashCommand::GitClean)),
            ("/git init", bad_args("git")),
            ("/runs archive", slash(SlashCommand::Runs { archive: None })),
            ("/runs archive run-20261004-072329-63c3", slash(SlashCommand::Runs { archive: Some(RunId::new("run-20261004-072329-63c3").unwrap()) })),
            ("/runs", bad_args("runs")),
            ("/runs list", bad_args("runs")),
            ("/runs archive a b", bad_args("runs")),
            ("/doctor", slash(SlashCommand::Doctor)),
            ("/update", slash(SlashCommand::Update)),
            ("/doctor codex", bad_args("doctor")),
            ("/templates", slash(SlashCommand::Templates { name: None, global: false })),
            (
                "/templates frames/reviewer --global",
                slash(SlashCommand::Templates { name: Some("frames/reviewer".into()), global: true }),
            ),
            ("/templates ../config", bad_args("templates")),
            ("/templates --global", bad_args("templates")),
            ("/templates council now", bad_args("templates")),
            ("/awake", slash(SlashCommand::Awake { time: None })),
            ("/awake 2h", slash(SlashCommand::Awake { time: Some("2h".into()) })),
            ("/awake off", slash(SlashCommand::Awake { time: Some("off".into()) })),
            ("/awake forever", bad_args("awake")),
            ("/awake 0m", bad_args("awake")),
            ("/awake 2h now", bad_args("awake")),
            ("/help", slash(SlashCommand::Help)),
            ("/artifacts", slash(SlashCommand::Artifacts)),
            ("/frobnicate", unknown("frobnicate")),
            ("/", unknown("")),
            ("/ ask why", unknown("")),
            ("@codex please add a test", mention(MentionTarget::Worker(worker("codex")), "please add a test")),
            ("@claude-code/opus look at the store", mention(MentionTarget::Worker(worker("claude-code/opus")), "look at the store")),
            ("@task-002 keep a fallback", mention(MentionTarget::Task(task("task-002")), "keep a fallback")),
            ("@test-004 fail first", mention(MentionTarget::Task(task("test-004")), "fail first")),
            ("@reviewer be strict", mention(MentionTarget::Role(RoleName::Reviewer), "be strict")),
            ("@reviewer be strict", mention(MentionTarget::Role(RoleName::Reviewer), "be strict")),
            ("@worker keep it small", mention(MentionTarget::Role(RoleName::Worker), "keep it small")),
            ("@tie-breaker hurry", mention(MentionTarget::Role(RoleName::TieBreaker), "hurry")),
            ("@2 stop there", mention(MentionTarget::Number(2), "stop there")),
            ("@all wrap up", mention(MentionTarget::All, "wrap up")),
            ("@codex", mention(MentionTarget::Worker(worker("codex")), "")),
            ("@999 too big", plain("@999 too big")),
        ];
        for (input, want) in cases {
            let got = parse_input(input);
            assert!(same(&got, &want), "{input:?}: got {got:?}, want {want:?}");
        }
    }

    #[test]
    fn every_menu_command_parses_and_every_parsed_command_is_in_the_menu() {
        for spec in SLASH_COMMANDS {
            let args = match spec.name {
                "ask" | "captain" | "crew" | "follow-up" => " something",
                "reassign" => " task-001 codex",
                "connect" => " hermes",
                "pin" | "unpin" => " context",
                "runs" => " archive",
                _ => "",
            };
            let parsed = parse_input(&format!("/{}{args}", spec.name));
            assert!(
                matches!(parsed, Ok(ParsedInput::Slash { .. })),
                "/{}: {parsed:?}",
                spec.name
            );
        }
        assert_eq!(SLASH_COMMANDS.len(), 46);
        let mut names: Vec<&str> = SLASH_COMMANDS.iter().map(|spec| spec.name).collect();
        names.sort_unstable();
        names.dedup();
        assert_eq!(
            names.len(),
            SLASH_COMMANDS.len(),
            "no command is listed twice"
        );
    }

    #[test]
    fn the_menu_groups_commands_by_category() {
        let mut groups: Vec<(&str, Vec<&str>)> = Vec::new();
        for spec in SLASH_COMMANDS {
            let title = spec.category.title();
            match groups.last_mut() {
                Some((last, names)) if *last == title => names.push(spec.name),
                _ => groups.push((title, vec![spec.name])),
            }
        }
        assert_eq!(
            groups,
            [
                ("Session", vec!["clear", "resume", "artifacts", "exit"]),
                ("Work", vec!["ask", "captain", "crew", "follow-up"]),
                (
                    "Runs",
                    vec![
                        "status",
                        "workflows",
                        "context",
                        "rewind",
                        "reassign",
                        "stop",
                        "runs"
                    ]
                ),
                (
                    "Crew",
                    vec![
                        "model",
                        "effort",
                        "agents",
                        "workflow",
                        "prompts",
                        "templates",
                        "fallback"
                    ]
                ),
                (
                    "Approvals and limits",
                    vec!["approvals", "limits", "stats", "permissions"]
                ),
                ("Learning", vec!["records", "learned", "skills", "memory"]),
                (
                    "Display",
                    vec![
                        "verbosity",
                        "narrator",
                        "theme",
                        "vim",
                        "mods",
                        "pin",
                        "unpin"
                    ]
                ),
                (
                    "Setup and help",
                    vec![
                        "config", "settings", "setup", "git", "connect", "doctor", "awake",
                        "update", "help"
                    ]
                ),
            ]
        );
        assert!(
            SLASH_COMMANDS
                .windows(2)
                .all(|pair| pair[0].category <= pair[1].category),
            "the categories follow the enum's order"
        );
    }

    #[test]
    fn handlers_match_the_contract() {
        let handler = |name: &str| {
            SLASH_COMMANDS
                .iter()
                .find(|spec| spec.name == name)
                .map(|spec| spec.handler)
        };
        for name in ["workflows", "help", "exit"] {
            assert_eq!(handler(name), Some(Handler::Ui), "{name}");
        }
        for name in ["verbosity", "vim", "theme"] {
            assert_eq!(handler(name), Some(Handler::Both), "{name}");
        }
        for name in [
            "clear",
            "resume",
            "captain",
            "follow-up",
            "rewind",
            "stop",
            "model",
            "effort",
            "workflow",
            "agents",
            "limits",
            "fallback",
            "permissions",
            "records",
            "learned",
            "skills",
            "memory",
            "setup",
            "git",
            "connect",
            "doctor",
            "update",
        ] {
            assert_eq!(handler(name), Some(Handler::App), "{name}");
        }
    }

    #[test]
    fn learning_commands_parse_with_their_optional_names() {
        let slash = |text: &str| match parse_input(text) {
            Ok(ParsedInput::Slash { command }) => Ok(command),
            Ok(other) => panic!("{text}: {other:?}"),
            Err(err) => Err(err),
        };
        assert_eq!(slash("/records"), Ok(SlashCommand::Records { agent: None }));
        assert_eq!(
            slash("/records codex"),
            Ok(SlashCommand::Records {
                agent: Some("codex".to_owned())
            })
        );
        assert_eq!(
            slash("/learned api-feature"),
            Ok(SlashCommand::LearnedWorkflows {
                name: Some("api-feature".to_owned())
            })
        );
        assert_eq!(slash("/skills"), Ok(SlashCommand::Skills));
        assert_eq!(slash("/memory"), Ok(SlashCommand::Memory));
        assert!(slash("/memory now").is_err());
        assert!(slash("/learned a b").is_err());
    }

    #[test]
    fn only_a_required_argument_makes_the_menu_wait() {
        let waits: Vec<&str> = SLASH_COMMANDS
            .iter()
            .filter(|spec| spec.needs_argument())
            .map(|spec| spec.name)
            .collect();
        assert_eq!(
            waits,
            [
                "ask",
                "captain",
                "crew",
                "follow-up",
                "reassign",
                "runs",
                "pin",
                "unpin",
                "connect"
            ]
        );
    }

    #[test]
    fn parsed_input_round_trips() {
        for input in [
            "/model reviewer codex/x",
            "/model codex",
            "@all hi",
            "/narrator live",
            "/config narrator.model --reset",
            "/approvals merge.mode auto",
            "/permissions sandbox.network true",
            "/resume conversation-003",
            "/resume run-01",
            "/follow-up run-01 add tests",
            "/rewind task-002",
            "/stop run-01",
            "/agents",
            "/agents add hermes",
            "/verbosity quiet",
            "/connect openclaw",
            "hi",
            "",
        ] {
            let parsed = parse_input(input).unwrap();
            let json = serde_json::to_string(&parsed).unwrap();
            assert_eq!(serde_json::from_str::<ParsedInput>(&json).unwrap(), parsed);
        }
    }
}
