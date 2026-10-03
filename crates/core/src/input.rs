//! Parses what you type: slash commands, `@mentions`, and plain text. The UI's menus and the
//! controller's routing share this parser, so they never disagree.

mod shared;
mod vendor;

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::CardId;
use crate::RunId;
use crate::Verbosity;
use crate::names::{ChatId, RoleName, VendorId, WorkerRef};
use crate::settings::ui::NarratorMode;
use crate::vendor_sessions::VendorSessionRef;

pub use shared::SHARED_COMMANDS;
pub use vendor::parse_input_with;

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
    /// A fresh chat with the lead. Runs keep going.
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
    Solo {
        task: String,
    },
    Crew {
        task: String,
    },
    /// A new run that builds on a finished one.
    FollowUp {
        /// The latest merged run when omitted.
        run: Option<RunId>,
        request: String,
    },
    /// Where each run stands. Zero tokens; `s` in its view asks the narrator for a summary.
    Status,
    /// The workflow view, as Ctrl+T opens it. The screen handles it; a front end without one
    /// gets the run's workflow, or the list of workflows when no run is on screen.
    Workflows,
    /// Every live session's context, cap, tool calls, and spend. Zero tokens.
    Context,
    /// Go back to one of `card`'s checkpoints, or any card's. The work after it is kept on a side
    /// branch.
    Rewind {
        card: Option<CardId>,
    },
    Reassign {
        /// The card, or every card of `from`.
        card: Option<CardId>,
        from: Option<VendorId>,
        to: WorkerRef,
    },
    /// Stop `run`, or the run on screen. Its work is committed.
    Stop {
        run: Option<RunId>,
    },
    /// Neither opens the picker. A worker without a role is the default agent's.
    Model {
        role: Option<RoleName>,
        worker: Option<WorkerRef>,
        /// Saved with the model, as the models level's effort line sets it.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        effort: Option<String>,
    },
    /// Reasoning effort for the default agent or a role. No level opens the picker.
    Effort {
        role: Option<RoleName>,
        /// One of [`EFFORT_LEVELS`].
        level: Option<String>,
        /// `triage.all_hands`, as the slider's toggle set it, saved with the level. Alone, it's
        /// saved without one.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        all_hands: Option<bool>,
    },
    Agents {
        #[serde(default)]
        action: AgentsAction,
    },
    /// No name opens the picker. `show`, `copy`, and `diff` are actions, so they can't be names.
    Preset {
        name: Option<String>,
        #[serde(default)]
        action: PresetAction,
    },
    /// `/handoff <role> <key> <value>` sets one key under `roles.<role>.handoff.`.
    Handoff {
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
    /// `/budget <key> <value>` sets one key under `budget.`.
    Budget {
        key: Option<String>,
        value: Option<String>,
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
    /// Recipes and how they evolve. A name opens the picker on that recipe.
    Recipes {
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
    /// Shows the MCP config `front_end` needs, and writes it only after you confirm.
    Connect {
        front_end: FrontEnd,
    },
    /// The agent CLIs, their versions, and logins.
    Doctor,
    Help,
    /// Free the main agent's context by summarizing the conversation, whatever the CLI.
    Compact {
        instructions: Option<String>,
    },
    /// No name lets the lead suggest one.
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
    /// Spend and quotas for each agent.
    Usage {
        /// Such as `daily` or `weekly`; the current windows when `None`.
        period: Option<String>,
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
    /// One of the main agent CLI's own commands, sent to its session as the command itself.
    Vendor {
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
    /// A chat with the lead, `chat-<n>`.
    Conversation(ChatId),
    Run(RunId),
    /// One of the main agent CLI's own sessions, `<adapter>:<session id>`, carried on through
    /// the CLI's own resume.
    VendorSession(VendorSessionRef),
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AgentsAction {
    /// Every agent Argonauts can use: CLIs, endpoints, and ACP agents.
    #[default]
    List,
    /// Hermes, OpenClaw, OpenCode, Ollama, or the name of your own CLI.
    Add { name: String },
}

/// A front end that drives Argonauts over MCP.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FrontEnd {
    Hermes,
    OpenClaw,
}

impl FrontEnd {
    /// The word `/connect` takes.
    pub fn as_str(self) -> &'static str {
        match self {
            FrontEnd::Hermes => "hermes",
            FrontEnd::OpenClaw => "openclaw",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "change", rename_all = "snake_case")]
pub enum NarratorChange {
    Mode { mode: NarratorMode },
    Model { model: WorkerRef },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "target", content = "value", rename_all = "snake_case")]
pub enum MentionTarget {
    /// `@codex`, `@claude-code`: every session of that worker in the run.
    Worker(WorkerRef),
    /// `@card-002`: the card's owner.
    Card(CardId),
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
    /// Whether an argument outside `[...]` is required, as in `/follow-up [run] <request>`, so
    /// picking the command from the menu waits for it.
    pub fn needs_argument(&self) -> bool {
        let mut optional = 0_usize;
        for c in self.args.chars() {
            match c {
                '[' => optional += 1,
                ']' => optional = optional.saturating_sub(1),
                '<' if optional == 0 => return true,
                _ => {}
            }
        }
        false
    }
}

/// The `/` menu, in display order: by category, then in the order written here.
#[rustfmt::skip]
pub const SLASH_COMMANDS: &[SlashSpec] = &[
    SlashSpec { name: "clear", args: "", help: "Start a fresh chat with the lead. Runs keep going", handler: Handler::App, category: CommandCategory::Session },
    SlashSpec { name: "resume", args: "", help: "Pick an earlier conversation or a run to carry on", handler: Handler::App, category: CommandCategory::Session },
    SlashSpec { name: "exit", args: "", help: "Close the screen. Runs keep going", handler: Handler::Ui, category: CommandCategory::Session },
    SlashSpec { name: "ask", args: "<question>", help: "Answer only, never a run", handler: Handler::App, category: CommandCategory::Work },
    SlashSpec { name: "solo", args: "<task>", help: "One agent does it", handler: Handler::App, category: CommandCategory::Work },
    SlashSpec { name: "crew", args: "<task>", help: "The crew does it", handler: Handler::App, category: CommandCategory::Work },
    SlashSpec { name: "follow-up", args: "[run] <request>", help: "Follow up on a finished run", handler: Handler::App, category: CommandCategory::Work },
    SlashSpec { name: "status", args: "", help: "Where each run stands: goal, plan, cards, budget, quotas. Costs nothing. s in it asks the narrator for a summary", handler: Handler::App, category: CommandCategory::Runs },
    SlashSpec { name: "workflows", args: "", help: "The workflow view (Ctrl+T). Its Library tab shows, copies, and diffs workflow files", handler: Handler::Ui, category: CommandCategory::Runs },
    SlashSpec { name: "context", args: "", help: "Each session's context, cap, calls, and spend", handler: Handler::App, category: CommandCategory::Runs },
    SlashSpec { name: "rewind", args: "[card]", help: "Go back to a checkpoint", handler: Handler::App, category: CommandCategory::Runs },
    SlashSpec { name: "reassign", args: "<card> <agent>", help: "Move a card to another agent", handler: Handler::App, category: CommandCategory::Runs },
    SlashSpec { name: "stop", args: "[run]", help: "Stop a run. Its work is committed", handler: Handler::App, category: CommandCategory::Runs },
    SlashSpec { name: "model", args: "[role] [model]", help: "The default agent or a role, then its model, from each CLI's live list", handler: Handler::App, category: CommandCategory::Crew },
    SlashSpec { name: "effort", args: "[role] [level]", help: "Reasoning effort, from the levels that model supports", handler: Handler::App, category: CommandCategory::Crew },
    SlashSpec { name: "agents", args: "[add <name>]", help: "The agents Argonauts can use (CLIs, endpoints, ACP agents): add one (Hermes, OpenClaw, OpenCode, Ollama, your own CLI), turn one on or off, check its login", handler: Handler::App, category: CommandCategory::Crew },
    SlashSpec { name: "preset", args: "[name]", help: "How the crew works", handler: Handler::App, category: CommandCategory::Crew },
    SlashSpec { name: "handoff", args: "", help: "Who takes over when an agent stops, and what it's told", handler: Handler::App, category: CommandCategory::Crew },
    SlashSpec { name: "approvals", args: "", help: "Each checkpoint's mode, and what waits for you", handler: Handler::App, category: CommandCategory::ApprovalsAndLimits },
    SlashSpec { name: "budget", args: "", help: "Run and card budgets", handler: Handler::App, category: CommandCategory::ApprovalsAndLimits },
    SlashSpec { name: "permissions", args: "", help: "Whose rules guard the agents, internet, commits, protected paths", handler: Handler::App, category: CommandCategory::ApprovalsAndLimits },
    SlashSpec { name: "records", args: "[agent]", help: "Who is good at what: each agent's track record and prediction score by kind of work. Pin, reset, or rebuild one", handler: Handler::App, category: CommandCategory::Learning },
    SlashSpec { name: "recipes", args: "[name]", help: "Recipes and how they evolve: versions, candidates, replays, canaries. Promote, roll back, or export one as a skill", handler: Handler::App, category: CommandCategory::Learning },
    SlashSpec { name: "skills", args: "", help: "Exported and imported skills, and any drift. Import one as a branch for you to review", handler: Handler::App, category: CommandCategory::Learning },
    SlashSpec { name: "memory", args: "", help: "Notes the crew keeps, flagged ones waiting for you, and the conventions. Sync AGENTS.md and CLAUDE.md", handler: Handler::App, category: CommandCategory::Learning },
    SlashSpec { name: "verbosity", args: "[full|normal|quiet]", help: "How much of each agent's work you see", handler: Handler::Both, category: CommandCategory::Display },
    SlashSpec { name: "narrator", args: "", help: "The narrator line", handler: Handler::App, category: CommandCategory::Display },
    SlashSpec { name: "theme", args: "", help: "Light, dark, or auto", handler: Handler::Both, category: CommandCategory::Display },
    SlashSpec { name: "vim", args: "", help: "Vim keys on or off", handler: Handler::Both, category: CommandCategory::Display },
    SlashSpec { name: "config", args: "", help: "Every setting and where it came from", handler: Handler::App, category: CommandCategory::SetupAndHelp },
    SlashSpec { name: "setup", args: "", help: "The guided setup again", handler: Handler::App, category: CommandCategory::SetupAndHelp },
    SlashSpec { name: "connect", args: "<hermes|openclaw>", help: "Let a front end drive Argonauts over MCP: shows the config it needs, and writes it only after you confirm", handler: Handler::App, category: CommandCategory::SetupAndHelp },
    SlashSpec { name: "doctor", args: "", help: "The agent CLIs, their versions, and logins", handler: Handler::App, category: CommandCategory::SetupAndHelp },
    SlashSpec { name: "help", args: "", help: "Keys and commands, by category", handler: Handler::Ui, category: CommandCategory::SetupAndHelp },
];

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PresetAction {
    #[default]
    Use,
    /// `/preset show <name>`: print it, from its file or the built-in.
    Show,
    /// `/preset copy <name>`: write the built-in out to `~/.argonauts/presets/` to edit.
    Copy,
    /// `/preset copy <name> --project`: write it to `.argonauts/presets/`, to commit and share.
    CopyToProject,
    /// `/preset diff <name>`: compare your file with the current built-in.
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
    "sandbox.agents_see_other_copies",
    "sandbox.agents_commit",
    "limits.protected_paths",
    "limits.dependency_allowlist",
    "limits.usd_per_day",
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
    "agents_see_other_copies",
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
    #[error("@ needs a target, such as @codex or @card-002")]
    EmptyMention,
    /// One of the main agent's commands that only its own screen runs.
    #[error("/{command} works only in {vendor} itself")]
    VendorScreenOnly { command: String, vendor: String },
}

/// `/` and `@` count only at the start. An `@` followed by something that isn't a valid target
/// stays plain text, like an email address.
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
        return parse_slash(name, args).map(|command| ParsedInput::Slash { command });
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
        "solo" => Ok(SlashCommand::Solo {
            task: text_arg("a task")?,
        }),
        "crew" => Ok(SlashCommand::Crew {
            task: text_arg("a task")?,
        }),
        "follow-up" => parse_follow_up(name, args),
        "status" => no_args(SlashCommand::Status),
        "workflows" => no_args(SlashCommand::Workflows),
        "context" => no_args(SlashCommand::Context),
        "rewind" => parse_rewind(name, args),
        "reassign" => parse_reassign(name, args),
        "stop" => parse_stop(name, args),
        "model" => parse_model(name, args),
        "effort" => parse_effort(name, args),
        "agents" => parse_agents(name, args),
        "preset" => parse_preset(name, args),
        "handoff" => parse_handoff(name, args),
        "approvals" => parse_approvals(name, args),
        "budget" => parse_budget(name, args),
        "permissions" => parse_permissions(name, args),
        "records" => parse_one_word(name, args).map(|agent| SlashCommand::Records { agent }),
        "recipes" => parse_one_word(name, args).map(|name| SlashCommand::Recipes { name }),
        "skills" => no_args(SlashCommand::Skills),
        "memory" => no_args(SlashCommand::Memory),
        "verbosity" => parse_verbosity(name, args),
        "narrator" => parse_narrator(name, args),
        "theme" => parse_one_word(name, args).map(|name| SlashCommand::Theme { name }),
        "vim" => no_args(SlashCommand::Vim),
        "config" => parse_config(name, args),
        "setup" => no_args(SlashCommand::Setup),
        "connect" => parse_connect(name, args),
        "doctor" => no_args(SlashCommand::Doctor),
        "help" => no_args(SlashCommand::Help),
        other => shared::parse(other, args)
            .unwrap_or_else(|| Err(InputError::UnknownCommand(other.to_owned()))),
    }
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

/// `chat-<n>` is a conversation, `<adapter>:<id>` a CLI's own session, and any other word a
/// run.
fn parse_resume(name: &str, args: &str) -> Result<SlashCommand, InputError> {
    let target = match optional_word(name, args, "/resume [conversation, session, or run]")? {
        None => None,
        Some(word) if word.starts_with("chat-") => {
            let chat = ChatId::parse(word)
                .ok_or_else(|| bad(name, format!("{word:?} isn't a conversation")))?;
            Some(ResumeTarget::Conversation(chat))
        }
        Some(word) if word.contains(':') => {
            let session = VendorSessionRef::parse(word)
                .ok_or_else(|| bad(name, format!("{word:?} isn't a session")))?;
            Some(ResumeTarget::VendorSession(session))
        }
        Some(word) => Some(ResumeTarget::Run(parse_run(name, word)?)),
    };
    Ok(SlashCommand::Resume { target })
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
    let card = match optional_word(name, args, "/rewind [card]")? {
        Some(word) => Some(parse_card(name, word)?),
        None => None,
    };
    Ok(SlashCommand::Rewind { card })
}

fn parse_stop(name: &str, args: &str) -> Result<SlashCommand, InputError> {
    let run = match optional_word(name, args, "/stop [run]")? {
        Some(word) => Some(parse_run(name, word)?),
        None => None,
    };
    Ok(SlashCommand::Stop { run })
}

/// Anything that starts with `-` is a mistyped flag, not a worker.
fn parse_worker(name: &str, text: &str) -> Result<WorkerRef, InputError> {
    if text.starts_with('-') {
        return Err(bad(name, format!("{text:?} isn't an agent")));
    }
    text.parse()
        .map_err(|err: crate::names::WorkerRefError| bad(name, err.to_string()))
}

fn parse_reassign(name: &str, args: &str) -> Result<SlashCommand, InputError> {
    let words: Vec<&str> = args.split_whitespace().collect();
    match words.as_slice() {
        ["--from", vendor, to] => {
            let vendor = parse_worker(name, vendor)?;
            if vendor.model.is_some() {
                return Err(bad(name, "--from takes a vendor, such as codex"));
            }
            Ok(SlashCommand::Reassign {
                card: None,
                from: Some(vendor.vendor()),
                to: parse_worker(name, to)?,
            })
        }
        [card, to] if !card.starts_with('-') => Ok(SlashCommand::Reassign {
            card: Some(CardId::new(*card).map_err(|err| bad(name, err.to_string()))?),
            from: None,
            to: parse_worker(name, to)?,
        }),
        _ => Err(bad(
            name,
            "use /reassign <card> <agent> or /reassign --from <vendor> <agent>",
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
                "use off, on_demand, live, or model <vendor>/<model>",
            ));
        }
    };
    Ok(SlashCommand::Narrator { change })
}

fn parse_config(name: &str, args: &str) -> Result<SlashCommand, InputError> {
    let (key, value) = key_and_value(args);
    if key.as_deref().is_some_and(|key| key.starts_with('-')) {
        return Err(bad(
            name,
            "use /config [key], /config <key> <value>, or /config <key> --reset",
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
    if let Some(role) = role_named(first) {
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
    role_named(word).ok_or_else(|| bad(name, format!("no role named {word:?}")))
}

/// One word is a role when it names one, and otherwise the default agent's model. An effort
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
        [word] => match role_named(word) {
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
        [word] => match role_named(word) {
            Some(role) => (Some(role), None),
            None => (None, Some(level(word)?)),
        },
        [role, word] => (Some(parse_role(name, role)?), Some(level(word)?)),
        _ => return Err(bad(name, "use /effort [role] [level]")),
    };
    Ok(SlashCommand::Effort {
        role,
        level,
        all_hands: None,
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
    let front_end = match args {
        "hermes" => FrontEnd::Hermes,
        "openclaw" => FrontEnd::OpenClaw,
        _ => return Err(bad(name, "use /connect hermes or /connect openclaw")),
    };
    Ok(SlashCommand::Connect { front_end })
}

fn parse_one_word(name: &str, args: &str) -> Result<Option<String>, InputError> {
    let words: Vec<&str> = args.split_whitespace().collect();
    match words.as_slice() {
        [] => Ok(None),
        [word] if !word.starts_with('-') => Ok(Some((*word).to_owned())),
        _ => Err(bad(name, format!("use /{name} [name]"))),
    }
}

fn parse_preset(name: &str, args: &str) -> Result<SlashCommand, InputError> {
    let words: Vec<&str> = args.split_whitespace().collect();
    let (preset, action) = match words.as_slice() {
        [] => {
            return Ok(SlashCommand::Preset {
                name: None,
                action: PresetAction::Use,
            });
        }
        ["show", preset] => (*preset, PresetAction::Show),
        ["diff", preset] => (*preset, PresetAction::Diff),
        ["copy", preset] => (*preset, PresetAction::Copy),
        ["copy", preset, "--project"] => (*preset, PresetAction::CopyToProject),
        [preset] if !["show", "diff", "copy"].contains(preset) => (*preset, PresetAction::Use),
        _ => ("", PresetAction::Use),
    };
    if preset.is_empty() || preset.starts_with('-') {
        return Err(bad(
            name,
            "use /preset [name], /preset show|diff <name>, or /preset copy <name> [--project]",
        ));
    }
    Ok(SlashCommand::Preset {
        name: Some(preset.to_owned()),
        action,
    })
}

fn parse_card(name: &str, word: &str) -> Result<CardId, InputError> {
    if !looks_like_card(word) {
        return Err(bad(
            name,
            format!("{word:?} isn't a card, such as card-002"),
        ));
    }
    CardId::new(word).map_err(|err| bad(name, err.to_string()))
}

/// A key and the rest of the line as its TOML value, or neither.
fn key_and_value(args: &str) -> (Option<String>, Option<String>) {
    let (key, value) = split_word(args);
    (
        (!key.is_empty()).then(|| key.to_owned()),
        (!value.is_empty()).then(|| value.to_owned()),
    )
}

fn parse_budget(name: &str, args: &str) -> Result<SlashCommand, InputError> {
    let (key, value) = key_and_value(args);
    if key.is_some() && value.is_none() {
        return Err(bad(name, "use /budget or /budget <key> <value>"));
    }
    Ok(SlashCommand::Budget { key, value })
}

fn parse_handoff(name: &str, args: &str) -> Result<SlashCommand, InputError> {
    let (role, rest) = split_word(args);
    let role = if role.is_empty() {
        None
    } else {
        Some(parse_role(name, role)?)
    };
    let (key, value) = key_and_value(rest);
    if key.is_some() && value.is_none() {
        return Err(bad(
            name,
            "use /handoff [role] or /handoff <role> <key> <value>",
        ));
    }
    Ok(SlashCommand::Handoff { role, key, value })
}

/// A role by its config name, such as `phase-lead`.
fn role_named(word: &str) -> Option<RoleName> {
    serde_json::from_value(serde_json::Value::String(word.to_owned())).ok()
}

fn looks_like_card(word: &str) -> bool {
    ["card-", "test-", "spike-"].iter().any(|prefix| {
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
    if looks_like_card(word) {
        return CardId::new(word).ok().map(MentionTarget::Card);
    }
    if let Some(role) = role_named(word) {
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

    fn worker(text: &str) -> WorkerRef {
        text.parse().unwrap()
    }

    fn run(text: &str) -> RunId {
        RunId::new(text).unwrap()
    }

    fn card(text: &str) -> CardId {
        CardId::new(text).unwrap()
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
            ("/resume chat-003", slash(SlashCommand::Resume { target: Some(ResumeTarget::Conversation(ChatId::numbered(3))) })),
            ("/resume run-01 run-02", bad_args("resume")),
            ("/resume ../x", bad_args("resume")),
            ("/resume chat-../x", bad_args("resume")),
            ("/resume --last", bad_args("resume")),
            ("/resume codex:-p", bad_args("resume")),
            ("/exit", slash(SlashCommand::Exit)),
            ("/ask how does auth work?", slash(SlashCommand::Ask { question: "how does auth work?".into() })),
            ("/ask", bad_args("ask")),
            ("/solo fix the typo", slash(SlashCommand::Solo { task: "fix the typo".into() })),
            ("  /crew add per-key rate limiting", slash(SlashCommand::Crew { task: "add per-key rate limiting".into() })),
            ("/crew", bad_args("crew")),
            ("/follow-up run-01 add tests", slash(SlashCommand::FollowUp { run: Some(run("run-01")), request: "add tests".into() })),
            ("/follow-up add tests", slash(SlashCommand::FollowUp { run: None, request: "add tests".into() })),
            ("/follow-up running the linter too", slash(SlashCommand::FollowUp { run: None, request: "running the linter too".into() })),
            ("/follow-up", bad_args("follow-up")),
            ("/follow-up run-01", bad_args("follow-up")),
            ("/status", slash(SlashCommand::Status)),
            ("/status now", bad_args("status")),
            ("/workflows", slash(SlashCommand::Workflows)),
            ("/workflows review-panel", bad_args("workflows")),
            ("/context", slash(SlashCommand::Context)),
            ("/context now", bad_args("context")),
            ("/rewind", slash(SlashCommand::Rewind { card: None })),
            ("/rewind card-002", slash(SlashCommand::Rewind { card: Some(card("card-002")) })),
            ("/rewind test-004", slash(SlashCommand::Rewind { card: Some(card("test-004")) })),
            ("/rewind run-01", bad_args("rewind")),
            ("/rewind card-002 handoff-1", bad_args("rewind")),
            ("/reassign card-003 codex/gpt-x", slash(SlashCommand::Reassign { card: Some(card("card-003")), from: None, to: worker("codex/gpt-x") })),
            ("/reassign --from claude-code codex/gpt-x", slash(SlashCommand::Reassign { card: None, from: Some(VendorId("claude-code".into())), to: worker("codex/gpt-x") })),
            ("/reassign --from claude-code/opus codex", bad_args("reassign")),
            ("/reassign card-003", bad_args("reassign")),
            ("/reassign card-003 -x", bad_args("reassign")),
            ("/stop", slash(SlashCommand::Stop { run: None })),
            ("/stop run-01", slash(SlashCommand::Stop { run: Some(run("run-01")) })),
            ("/stop run-01 run-02", bad_args("stop")),
            ("/stop ../x", bad_args("stop")),
            ("/model", slash(SlashCommand::Model { role: None, worker: None, effort: None })),
            ("/model reviewer", slash(SlashCommand::Model { role: Some(RoleName::Reviewer), worker: None, effort: None })),
            ("/model codex", slash(SlashCommand::Model { role: None, worker: Some(worker("codex")), effort: None })),
            ("/model claude-code/opus", slash(SlashCommand::Model { role: None, worker: Some(worker("claude-code/opus")), effort: None })),
            ("/model phase-lead codex/gpt-x", slash(SlashCommand::Model { role: Some(RoleName::PhaseLead), worker: Some(worker("codex/gpt-x")), effort: None })),
            ("/model claude-code/opus xhigh", slash(SlashCommand::Model { role: None, worker: Some(worker("claude-code/opus")), effort: Some("xhigh".into()) })),
            ("/model reviewer codex low", slash(SlashCommand::Model { role: Some(RoleName::Reviewer), worker: Some(worker("codex")), effort: Some("low".into()) })),
            ("/resume codex:019a2f-4c", slash(SlashCommand::Resume { target: Some(ResumeTarget::VendorSession(VendorSessionRef { vendor: VendorId("codex".into()), id: "019a2f-4c".into() })) })),
            ("/model boss codex", bad_args("model")),
            ("/model -x", bad_args("model")),
            ("/model owner codex extra", bad_args("model")),
            ("/model reviewer high", bad_args("model")),
            ("/effort", slash(SlashCommand::Effort { role: None, level: None, all_hands: None })),
            ("/effort high", slash(SlashCommand::Effort { role: None, level: Some("high".into()), all_hands: None })),
            ("/effort reviewer", slash(SlashCommand::Effort { role: Some(RoleName::Reviewer), level: None, all_hands: None })),
            ("/effort owner max", slash(SlashCommand::Effort { role: Some(RoleName::Owner), level: Some("max".into()), all_hands: None })),
            ("/effort loud", bad_args("effort")),
            ("/effort boss high", bad_args("effort")),
            ("/effort owner high now", bad_args("effort")),
            ("/agents", slash(SlashCommand::Agents { action: AgentsAction::List })),
            ("/agents add hermes", slash(SlashCommand::Agents { action: AgentsAction::Add { name: "hermes".into() } })),
            ("/agents add my-cli", slash(SlashCommand::Agents { action: AgentsAction::Add { name: "my-cli".into() } })),
            ("/agents add", bad_args("agents")),
            ("/agents add --force", bad_args("agents")),
            ("/agents add a b", bad_args("agents")),
            ("/agents codex", bad_args("agents")),
            ("/agents codex model \"gpt-x\"", bad_args("agents")),
            ("/preset", slash(SlashCommand::Preset { name: None, action: PresetAction::Use })),
            ("/preset coordinated-crew", slash(SlashCommand::Preset { name: Some("coordinated-crew".into()), action: PresetAction::Use })),
            ("/preset show peer-crew", slash(SlashCommand::Preset { name: Some("peer-crew".into()), action: PresetAction::Show })),
            ("/preset diff peer-crew", slash(SlashCommand::Preset { name: Some("peer-crew".into()), action: PresetAction::Diff })),
            ("/preset copy coordinated-crew", slash(SlashCommand::Preset { name: Some("coordinated-crew".into()), action: PresetAction::Copy })),
            ("/preset copy coordinated-crew --project", slash(SlashCommand::Preset { name: Some("coordinated-crew".into()), action: PresetAction::CopyToProject })),
            ("/preset show", bad_args("preset")),
            ("/preset copy peer-crew --global", bad_args("preset")),
            ("/preset --project", bad_args("preset")),
            ("/preset a b", bad_args("preset")),
            ("/handoff", slash(SlashCommand::Handoff { role: None, key: None, value: None })),
            ("/handoff owner", slash(SlashCommand::Handoff { role: Some(RoleName::Owner), key: None, value: None })),
            ("/handoff owner mode auto", slash(SlashCommand::Handoff { role: Some(RoleName::Owner), key: Some("mode".into()), value: Some("auto".into()) })),
            ("/handoff owner mode", bad_args("handoff")),
            ("/handoff boss", bad_args("handoff")),
            ("/approvals", slash(SlashCommand::Approvals { key: None, value: None })),
            ("/approvals merge.mode auto", slash(SlashCommand::Approvals { key: Some("merge.mode".into()), value: Some("auto".into()) })),
            ("/approvals merge.rules.max_combined_diff_lines 400", slash(SlashCommand::Approvals { key: Some("merge.rules.max_combined_diff_lines".into()), value: Some("400".into()) })),
            ("/approvals merge.mode", bad_args("approvals")),
            ("/budget", slash(SlashCommand::Budget { key: None, value: None })),
            ("/budget run_usd 30", slash(SlashCommand::Budget { key: Some("run_usd".into()), value: Some("30".into()) })),
            ("/budget run_usd", bad_args("budget")),
            ("/permissions", slash(SlashCommand::Permissions { role: None, key: None, value: None })),
            ("/permissions sandbox.network true", slash(SlashCommand::Permissions { role: None, key: Some("sandbox.network".into()), value: Some("true".into()) })),
            ("/permissions limits.usd_per_day 25.0", slash(SlashCommand::Permissions { role: None, key: Some("limits.usd_per_day".into()), value: Some("25.0".into()) })),
            ("/permissions reviewer", slash(SlashCommand::Permissions { role: Some(RoleName::Reviewer), key: None, value: None })),
            ("/permissions owner mode tool", slash(SlashCommand::Permissions { role: Some(RoleName::Owner), key: Some("mode".into()), value: Some("tool".into()) })),
            ("/permissions owner sandbox.agents_commit false", slash(SlashCommand::Permissions { role: Some(RoleName::Owner), key: Some("agents_commit".into()), value: Some("false".into()) })),
            ("/permissions owner mode", bad_args("permissions")),
            ("/permissions owner protected_paths []", bad_args("permissions")),
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
            ("/config gates.run [\"cargo\", \"test\"]", slash(SlashCommand::Config { key: Some("gates.run".into()), value: Some("[\"cargo\", \"test\"]".into()), reset: false })),
            ("/config narrator.model --reset", slash(SlashCommand::Config { key: Some("narrator.model".into()), value: None, reset: true })),
            ("/config --reset", bad_args("config")),
            ("/config -x 1", bad_args("config")),
            ("/setup", slash(SlashCommand::Setup)),
            ("/setup now", bad_args("setup")),
            ("/connect hermes", slash(SlashCommand::Connect { front_end: FrontEnd::Hermes })),
            ("/connect openclaw", slash(SlashCommand::Connect { front_end: FrontEnd::OpenClaw })),
            ("/connect", bad_args("connect")),
            ("/connect slack", bad_args("connect")),
            ("/connect hermes openclaw", bad_args("connect")),
            ("/doctor", slash(SlashCommand::Doctor)),
            ("/doctor codex", bad_args("doctor")),
            ("/help", slash(SlashCommand::Help)),
            ("/frobnicate", unknown("frobnicate")),
            ("/", unknown("")),
            ("/ ask why", unknown("")),
            ("@codex please add a test", mention(MentionTarget::Worker(worker("codex")), "please add a test")),
            ("@claude-code/opus look at the store", mention(MentionTarget::Worker(worker("claude-code/opus")), "look at the store")),
            ("@card-002 keep a fallback", mention(MentionTarget::Card(card("card-002")), "keep a fallback")),
            ("@test-004 fail first", mention(MentionTarget::Card(card("test-004")), "fail first")),
            ("@reviewer be strict", mention(MentionTarget::Role(RoleName::Reviewer), "be strict")),
            ("@phase-lead hurry", mention(MentionTarget::Role(RoleName::PhaseLead), "hurry")),
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
    fn dropped_and_renamed_commands_are_unknown() {
        for name in [
            "continue",
            "summary",
            "tree",
            "workflow",
            "checkpoints",
            "revert",
            "agent",
            "verbose",
            "normal",
            "quiet",
            "init",
        ] {
            assert_eq!(parse_input(&format!("/{name}")), unknown(name), "/{name}");
        }
    }

    #[test]
    fn every_menu_command_parses_and_every_parsed_command_is_in_the_menu() {
        for spec in SLASH_COMMANDS {
            let args = match spec.name {
                "ask" | "solo" | "crew" | "follow-up" => " something",
                "reassign" => " card-001 codex",
                "connect" => " hermes",
                _ => "",
            };
            let parsed = parse_input(&format!("/{}{args}", spec.name));
            assert!(
                matches!(parsed, Ok(ParsedInput::Slash { .. })),
                "/{}: {parsed:?}",
                spec.name
            );
        }
        assert_eq!(SLASH_COMMANDS.len(), 34);
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
                ("Session", vec!["clear", "resume", "exit"]),
                ("Work", vec!["ask", "solo", "crew", "follow-up"]),
                (
                    "Runs",
                    vec![
                        "status",
                        "workflows",
                        "context",
                        "rewind",
                        "reassign",
                        "stop"
                    ]
                ),
                (
                    "Crew",
                    vec!["model", "effort", "agents", "preset", "handoff"]
                ),
                (
                    "Approvals and limits",
                    vec!["approvals", "budget", "permissions"]
                ),
                ("Learning", vec!["records", "recipes", "skills", "memory"]),
                ("Display", vec!["verbosity", "narrator", "theme", "vim"]),
                (
                    "Setup and help",
                    vec!["config", "setup", "connect", "doctor", "help"]
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
            "solo",
            "follow-up",
            "rewind",
            "stop",
            "model",
            "effort",
            "preset",
            "agents",
            "budget",
            "handoff",
            "permissions",
            "records",
            "recipes",
            "skills",
            "memory",
            "setup",
            "connect",
            "doctor",
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
            slash("/recipes api-feature"),
            Ok(SlashCommand::Recipes {
                name: Some("api-feature".to_owned())
            })
        );
        assert_eq!(slash("/skills"), Ok(SlashCommand::Skills));
        assert_eq!(slash("/memory"), Ok(SlashCommand::Memory));
        assert!(slash("/memory now").is_err());
        assert!(slash("/recipes a b").is_err());
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
            ["ask", "solo", "crew", "follow-up", "reassign", "connect"]
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
            "/resume chat-003",
            "/resume run-01",
            "/follow-up run-01 add tests",
            "/rewind card-002",
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
