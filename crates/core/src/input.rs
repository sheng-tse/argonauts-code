//! Parses what you type: slash commands, `@mentions`, and plain text. The UI's menus and the
//! controller's routing share this parser, so they never disagree.

use serde::{Deserialize, Serialize};

use crate::CardId;
use crate::RunId;
use crate::handoff::CheckpointTag;
use crate::names::{RoleName, VendorId, WorkerRef};
use crate::settings::ui::NarratorMode;

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

/// Every slash command. `value` fields hold TOML text.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "command", rename_all = "snake_case")]
pub enum SlashCommand {
    /// Resume the paused run on screen, or open the picker, or resume `run`.
    Resume {
        run: Option<RunId>,
    },
    /// Start a follow-up run on a merged run.
    Continue {
        /// The merged run; the latest when omitted.
        run: Option<RunId>,
        request: Option<String>,
    },
    Tree,
    /// A one-off narrator summary.
    Summary,
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
    /// Verbosity `full`, or `normal` when it's already `full`. Saved as `ui.verbosity` in your
    /// global config.
    Verbose,
    /// Verbosity `normal`, which folds quiet calls into one line per group. Saved as
    /// `ui.verbosity` in your global config.
    Normal,
    /// Verbosity `quiet`, milestones only, or `normal` when it's already `quiet`. Saved as
    /// `ui.verbosity` in your global config.
    Quiet,
    /// Toggle vim keys; saved as `ui.keys` in your global config.
    Vim,
    /// The status summary. Zero tokens.
    Status,
    Stop,
    /// Show settings, set one, or reset one (`/config <key> --reset`).
    Config {
        /// A dotted key.
        key: Option<String>,
        value: Option<String>,
        /// Drop `key` from the layer that sets it now, so the layer below shows through.
        #[serde(default)]
        reset: bool,
    },
    Model {
        role: Option<RoleName>,
        worker: Option<WorkerRef>,
    },
    /// Show or change the default agent for this app session.
    Agent {
        worker: Option<WorkerRef>,
    },
    /// Reasoning effort for the default agent or a role. No level opens the picker.
    Effort {
        role: Option<RoleName>,
        /// One of [`EFFORT_LEVELS`].
        level: Option<String>,
    },
    /// No name opens the picker. `show`, `copy`, and `diff` are actions, so they can't be names.
    Preset {
        name: Option<String>,
        #[serde(default)]
        action: PresetAction,
    },
    /// `/agents <id> <key> <value>` sets one key of `agents.<id>` or `endpoints.<id>`.
    Agents {
        id: Option<String>,
        key: Option<String>,
        value: Option<String>,
    },
    /// `/budget <key> <value>` sets one key under `budget.`.
    Budget {
        key: Option<String>,
        value: Option<String>,
    },
    /// `/handoff <role> <key> <value>` sets one key under `roles.<role>.handoff.`.
    Handoff {
        role: Option<RoleName>,
        key: Option<String>,
        value: Option<String>,
    },
    /// No argument opens the picker; `/permissions <key> <value>` sets one of [`PERMISSION_KEYS`].
    Permissions {
        #[serde(default)]
        key: Option<String>,
        #[serde(default)]
        value: Option<String>,
    },
    /// No name opens the picker.
    Theme {
        /// `auto`, `dark`, or `light`.
        name: Option<String>,
    },
    Init,
    /// No argument opens the picker; `/approvals <key> <value>` sets one key under `approvals.`.
    Approvals {
        /// Without the `approvals.` prefix, such as `merge.mode`.
        #[serde(default)]
        key: Option<String>,
        #[serde(default)]
        value: Option<String>,
    },
    Narrator {
        change: Option<NarratorChange>,
    },
    Reassign {
        /// The card, or every card of `from`.
        card: Option<CardId>,
        from: Option<VendorId>,
        to: WorkerRef,
    },
    /// Every live session's context, cap, tool calls, and spend. Zero tokens.
    Context,
    /// No name opens the workflow view of the run on screen, or the picker when there's none.
    /// A name sets the `workflow` key. `show`, `copy`, and `diff` work like `/preset`'s.
    Workflow {
        name: Option<String>,
        #[serde(default)]
        action: WorkflowAction,
    },
    /// A card's handoff checkpoints, or every card's.
    Checkpoints {
        card: Option<CardId>,
    },
    /// Reset a card to checkpoint `handoff-<checkpoint>` after a confirm card. The work after it
    /// is kept on a side branch.
    Revert {
        card: CardId,
        checkpoint: u32,
    },
    Help,
    /// Close the screen. Runs keep going.
    Exit,
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

/// One row of the `/` menu.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SlashSpec {
    /// Without the slash.
    pub name: &'static str,
    /// Argument hint, such as `<task>`.
    pub args: &'static str,
    pub help: &'static str,
    pub handler: Handler,
}

/// The `/` menu, in display order.
#[rustfmt::skip]
pub const SLASH_COMMANDS: &[SlashSpec] = &[
    SlashSpec { name: "ask", args: "<question>", help: "Answer only, never a run", handler: Handler::App },
    SlashSpec { name: "solo", args: "<task>", help: "One agent does it; skips triage", handler: Handler::App },
    SlashSpec { name: "crew", args: "<task>", help: "The crew does it; skips triage", handler: Handler::App },
    SlashSpec { name: "status", args: "", help: "Goal, plan, cards, budget, quotas. No tokens", handler: Handler::App },
    SlashSpec { name: "context", args: "", help: "Each session's context, cap, calls, and spend", handler: Handler::App },
    SlashSpec { name: "tree", args: "", help: "Open the run tree", handler: Handler::Ui },
    SlashSpec { name: "workflow", args: "[name] | show|copy|diff <name>", help: "Open the run's workflow, or change or copy one", handler: Handler::App },
    SlashSpec { name: "workflows", args: "", help: "Open the workflow view", handler: Handler::App },
    SlashSpec { name: "summary", args: "", help: "A narrator summary of the run", handler: Handler::App },
    SlashSpec { name: "resume", args: "[run]", help: "Resume a paused, parked, or crashed run", handler: Handler::App },
    SlashSpec { name: "continue", args: "[run] [request]", help: "Follow up on a merged run", handler: Handler::App },
    SlashSpec { name: "stop", args: "", help: "Stop the run; work is committed", handler: Handler::App },
    SlashSpec { name: "reassign", args: "<card> <agent>", help: "Move a card to another agent", handler: Handler::App },
    SlashSpec { name: "checkpoints", args: "[card]", help: "A card's handoff checkpoints", handler: Handler::App },
    SlashSpec { name: "revert", args: "<card> handoff-<n>", help: "Reset a card to a checkpoint; later work is kept", handler: Handler::App },
    SlashSpec { name: "model", args: "[role] [agent]", help: "Show or change a role's model", handler: Handler::App },
    SlashSpec { name: "effort", args: "[role] [level]", help: "Show or change reasoning effort", handler: Handler::App },
    SlashSpec { name: "agent", args: "[agent]", help: "Show or change the default agent", handler: Handler::App },
    SlashSpec { name: "agents", args: "[id] [key value]", help: "The crew: harnesses, endpoints, models, auth", handler: Handler::App },
    SlashSpec { name: "preset", args: "[name] | show|copy|diff <name>", help: "Show, change, or copy out a preset", handler: Handler::App },
    SlashSpec { name: "approvals", args: "[key value]", help: "What waits for you, and each checkpoint's mode", handler: Handler::App },
    SlashSpec { name: "budget", args: "[key value]", help: "Run and card budgets, and what happens at the limit", handler: Handler::App },
    SlashSpec { name: "handoff", args: "[role] [key value]", help: "Who takes over when an agent stops", handler: Handler::App },
    SlashSpec { name: "permissions", args: "[key value]", help: "Sandbox, network, protected paths, daily cap", handler: Handler::App },
    SlashSpec { name: "narrator", args: "[off|on_demand|live|model <m>]", help: "Set the narrator", handler: Handler::App },
    SlashSpec { name: "config", args: "[key] [value|--reset]", help: "Every setting and where each came from", handler: Handler::App },
    SlashSpec { name: "theme", args: "[auto|dark|light]", help: "Show or change the theme", handler: Handler::Both },
    SlashSpec { name: "verbose", args: "", help: "Every call and message in full, saved globally; again for normal", handler: Handler::Both },
    SlashSpec { name: "normal", args: "", help: "Fold quiet calls into one line per group, saved globally", handler: Handler::Both },
    SlashSpec { name: "quiet", args: "", help: "Milestones only, saved globally; again for normal", handler: Handler::Both },
    SlashSpec { name: "vim", args: "", help: "Vim keys on or off, saved globally", handler: Handler::Both },
    SlashSpec { name: "init", args: "", help: "Run the guided setup again", handler: Handler::App },
    SlashSpec { name: "help", args: "", help: "Keys and commands", handler: Handler::Ui },
    SlashSpec { name: "exit", args: "", help: "Close the screen; runs keep going", handler: Handler::Ui },
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

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkflowAction {
    /// `/workflow <name>` sets the `workflow` key. With no name, the view opens.
    #[default]
    Use,
    /// `/workflow show <name>`: print it, from its file or the built-in.
    Show,
    /// `/workflow copy <name>`: write it out to `~/.argonauts/workflows/` to edit.
    Copy,
    /// `/workflow copy <name> --project`: write it to `.argonauts/workflows/`, to commit and
    /// share.
    CopyToProject,
    /// `/workflow diff <name>`: compare your file with the current built-in.
    Diff,
}

/// Reasoning effort levels `/effort` accepts. Pickers offer only the ones the model supports.
pub const EFFORT_LEVELS: &[&str] = &["minimal", "low", "medium", "high", "xhigh", "max"];

/// The keys `/permissions` covers, in picker order.
pub const PERMISSION_KEYS: &[&str] = &[
    "sandbox.mode",
    "sandbox.network",
    "limits.protected_paths",
    "limits.dependency_allowlist",
    "limits.usd_per_day",
];

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum InputError {
    #[error("unknown command /{0}")]
    UnknownCommand(String),
    #[error("/{command}: {problem}")]
    BadArguments { command: String, problem: String },
    #[error("@ needs a target, such as @codex or @card-002")]
    EmptyMention,
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
        "ask" => Ok(SlashCommand::Ask {
            question: text_arg("a question")?,
        }),
        "solo" => Ok(SlashCommand::Solo {
            task: text_arg("a task")?,
        }),
        "crew" => Ok(SlashCommand::Crew {
            task: text_arg("a task")?,
        }),
        "status" => no_args(SlashCommand::Status),
        "context" => no_args(SlashCommand::Context),
        "workflow" => parse_workflow(name, args),
        "workflows" => no_args(SlashCommand::Workflow {
            name: None,
            action: WorkflowAction::Use,
        }),
        "checkpoints" => parse_checkpoints(name, args),
        "revert" => parse_revert(name, args),
        "tree" => no_args(SlashCommand::Tree),
        "summary" => no_args(SlashCommand::Summary),
        "stop" => no_args(SlashCommand::Stop),
        "verbose" => no_args(SlashCommand::Verbose),
        "normal" => no_args(SlashCommand::Normal),
        "quiet" => no_args(SlashCommand::Quiet),
        "vim" => no_args(SlashCommand::Vim),
        "help" => no_args(SlashCommand::Help),
        "exit" => no_args(SlashCommand::Exit),
        "init" => no_args(SlashCommand::Init),
        "approvals" => parse_approvals(name, args),
        "permissions" => parse_permissions(name, args),
        "resume" => parse_resume(name, args),
        "continue" => Ok(parse_continue(args)),
        "reassign" => parse_reassign(name, args),
        "narrator" => parse_narrator(name, args),
        "config" => parse_config(name, args),
        "model" => parse_model(name, args),
        "agent" => parse_agent(name, args),
        "effort" => parse_effort(name, args),
        "preset" => parse_preset(name, args),
        "theme" => parse_one_word(name, args).map(|name| SlashCommand::Theme { name }),
        "agents" => parse_agents(name, args),
        "budget" => parse_budget(name, args),
        "handoff" => parse_handoff(name, args),
        other => Err(InputError::UnknownCommand(other.to_owned())),
    }
}

fn parse_resume(name: &str, args: &str) -> Result<SlashCommand, InputError> {
    let (word, rest) = split_word(args);
    if !rest.is_empty() {
        return Err(bad(name, "takes at most one run id"));
    }
    let run = if word.is_empty() {
        None
    } else {
        Some(RunId::new(word).map_err(|err| bad(name, err.to_string()))?)
    };
    Ok(SlashCommand::Resume { run })
}

/// The first word is a run only when it looks like one (`run-...`), so a request that starts
/// with an ordinary word stays whole.
fn parse_continue(args: &str) -> SlashCommand {
    let (word, rest) = split_word(args);
    let run = word
        .starts_with("run-")
        .then(|| RunId::new(word).ok())
        .flatten();
    let request = if run.is_some() { rest } else { args };
    SlashCommand::Continue {
        run,
        request: (!request.is_empty()).then(|| request.to_owned()),
    }
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
    let (key, value) = key_and_value(args);
    let known = key
        .as_deref()
        .is_none_or(|key| PERMISSION_KEYS.contains(&key));
    if !known || (key.is_some() && value.is_none()) {
        return Err(bad(
            name,
            format!(
                "use /permissions or /permissions <key> <value>, the key one of {}",
                PERMISSION_KEYS.join(", ")
            ),
        ));
    }
    Ok(SlashCommand::Permissions { key, value })
}

fn parse_role(name: &str, word: &str) -> Result<RoleName, InputError> {
    role_named(word).ok_or_else(|| bad(name, format!("no role named {word:?}")))
}

fn parse_model(name: &str, args: &str) -> Result<SlashCommand, InputError> {
    let words: Vec<&str> = args.split_whitespace().collect();
    let (role, worker) = match words.as_slice() {
        [] => (None, None),
        [role] => (Some(parse_role(name, role)?), None),
        [role, worker] => (
            Some(parse_role(name, role)?),
            Some(parse_worker(name, worker)?),
        ),
        _ => return Err(bad(name, "use /model [role] [agent]")),
    };
    Ok(SlashCommand::Model { role, worker })
}

fn parse_agent(name: &str, args: &str) -> Result<SlashCommand, InputError> {
    let words: Vec<&str> = args.split_whitespace().collect();
    let worker = match words.as_slice() {
        [] => None,
        [worker] => Some(parse_worker(name, worker)?),
        _ => return Err(bad(name, "use /agent [agent]")),
    };
    Ok(SlashCommand::Agent { worker })
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
    Ok(SlashCommand::Effort { role, level })
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

fn parse_workflow(name: &str, args: &str) -> Result<SlashCommand, InputError> {
    let words: Vec<&str> = args.split_whitespace().collect();
    let (workflow, action) = match words.as_slice() {
        [] => {
            return Ok(SlashCommand::Workflow {
                name: None,
                action: WorkflowAction::Use,
            });
        }
        ["show", workflow] => (*workflow, WorkflowAction::Show),
        ["diff", workflow] => (*workflow, WorkflowAction::Diff),
        ["copy", workflow] => (*workflow, WorkflowAction::Copy),
        ["copy", workflow, "--project"] => (*workflow, WorkflowAction::CopyToProject),
        [workflow] if !["show", "diff", "copy"].contains(workflow) => {
            (*workflow, WorkflowAction::Use)
        }
        _ => ("", WorkflowAction::Use),
    };
    if workflow.is_empty() || workflow.starts_with('-') {
        return Err(bad(
            name,
            "use /workflow [name], /workflow show|diff <name>, or /workflow copy <name> [--project]",
        ));
    }
    Ok(SlashCommand::Workflow {
        name: Some(workflow.to_owned()),
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

fn parse_checkpoints(name: &str, args: &str) -> Result<SlashCommand, InputError> {
    match args.split_whitespace().collect::<Vec<_>>().as_slice() {
        [] => Ok(SlashCommand::Checkpoints { card: None }),
        [card] => Ok(SlashCommand::Checkpoints {
            card: Some(parse_card(name, card)?),
        }),
        _ => Err(bad(name, "use /checkpoints [card]")),
    }
}

/// Takes `handoff-<n>`, or `<n>` alone.
fn parse_revert(name: &str, args: &str) -> Result<SlashCommand, InputError> {
    let usage = || bad(name, "use /revert <card> handoff-<n>");
    let [card, checkpoint] = args.split_whitespace().collect::<Vec<_>>()[..] else {
        return Err(usage());
    };
    let card = parse_card(name, card)?;
    let checkpoint = CheckpointTag::parse_label(checkpoint)
        .or_else(|| checkpoint.parse().ok().filter(|n| *n > 0))
        .ok_or_else(usage)?;
    Ok(SlashCommand::Revert { card, checkpoint })
}

/// A key and the rest of the line as its TOML value, or neither.
fn key_and_value(args: &str) -> (Option<String>, Option<String>) {
    let (key, value) = split_word(args);
    (
        (!key.is_empty()).then(|| key.to_owned()),
        (!value.is_empty()).then(|| value.to_owned()),
    )
}

fn parse_agents(name: &str, args: &str) -> Result<SlashCommand, InputError> {
    let (id, rest) = split_word(args);
    let (key, value) = key_and_value(rest);
    if id.starts_with('-') || (key.is_some() && value.is_none()) {
        return Err(bad(name, "use /agents [id] or /agents <id> <key> <value>"));
    }
    Ok(SlashCommand::Agents {
        id: (!id.is_empty()).then(|| id.to_owned()),
        key,
        value,
    })
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
            ("/ask how does auth work?", slash(SlashCommand::Ask { question: "how does auth work?".into() })),
            ("/ask", bad_args("ask")),
            ("/solo fix the typo", slash(SlashCommand::Solo { task: "fix the typo".into() })),
            ("  /crew add per-key rate limiting", slash(SlashCommand::Crew { task: "add per-key rate limiting".into() })),
            ("/crew", bad_args("crew")),
            ("/status", slash(SlashCommand::Status)),
            ("/status now", bad_args("status")),
            ("/tree", slash(SlashCommand::Tree)),
            ("/summary", slash(SlashCommand::Summary)),
            ("/stop", slash(SlashCommand::Stop)),
            ("/approvals", slash(SlashCommand::Approvals { key: None, value: None })),
            ("/approvals merge.mode auto", slash(SlashCommand::Approvals { key: Some("merge.mode".into()), value: Some("auto".into()) })),
            ("/approvals merge.rules.max_combined_diff_lines 400", slash(SlashCommand::Approvals { key: Some("merge.rules.max_combined_diff_lines".into()), value: Some("400".into()) })),
            ("/approvals merge.mode", bad_args("approvals")),
            ("/verbose", slash(SlashCommand::Verbose)),
            ("/normal", slash(SlashCommand::Normal)),
            ("/normal now", bad_args("normal")),
            ("/quiet", slash(SlashCommand::Quiet)),
            ("/workflows", slash(SlashCommand::Workflow { name: None, action: WorkflowAction::Use })),
            ("/workflows review-panel", bad_args("workflows")),
            ("/vim", slash(SlashCommand::Vim)),
            ("/help", slash(SlashCommand::Help)),
            ("/exit", slash(SlashCommand::Exit)),
            ("/resume", slash(SlashCommand::Resume { run: None })),
            ("/resume run-01", slash(SlashCommand::Resume { run: Some(run("run-01")) })),
            ("/resume run-01 run-02", bad_args("resume")),
            ("/resume ../x", bad_args("resume")),
            ("/continue", slash(SlashCommand::Continue { run: None, request: None })),
            ("/continue run-01", slash(SlashCommand::Continue { run: Some(run("run-01")), request: None })),
            ("/continue run-01 add tests", slash(SlashCommand::Continue { run: Some(run("run-01")), request: Some("add tests".into()) })),
            ("/continue add tests", slash(SlashCommand::Continue { run: None, request: Some("add tests".into()) })),
            ("/reassign card-003 codex/gpt-x", slash(SlashCommand::Reassign { card: Some(CardId::new("card-003").unwrap()), from: None, to: worker("codex/gpt-x") })),
            ("/reassign --from claude-code codex/gpt-x", slash(SlashCommand::Reassign { card: None, from: Some(VendorId("claude-code".into())), to: worker("codex/gpt-x") })),
            ("/reassign --from claude-code/opus codex", bad_args("reassign")),
            ("/reassign card-003", bad_args("reassign")),
            ("/reassign card-003 -x", bad_args("reassign")),
            ("/narrator", slash(SlashCommand::Narrator { change: None })),
            ("/narrator off", slash(SlashCommand::Narrator { change: Some(NarratorChange::Mode { mode: NarratorMode::Off }) })),
            ("/narrator on_demand", slash(SlashCommand::Narrator { change: Some(NarratorChange::Mode { mode: NarratorMode::OnDemand }) })),
            ("/narrator live", slash(SlashCommand::Narrator { change: Some(NarratorChange::Mode { mode: NarratorMode::Live }) })),
            ("/narrator model ollama/qwen3", slash(SlashCommand::Narrator { change: Some(NarratorChange::Model { model: worker("ollama/qwen3") }) })),
            ("/narrator loud", bad_args("narrator")),
            ("/config", slash(SlashCommand::Config { key: None, value: None, reset: false })),
            ("/config ui.keys", slash(SlashCommand::Config { key: Some("ui.keys".into()), value: None, reset: false })),
            ("/config ui.keys \"vim\"", slash(SlashCommand::Config { key: Some("ui.keys".into()), value: Some("\"vim\"".into()), reset: false })),
            ("/config gates.run [\"cargo\", \"test\"]", slash(SlashCommand::Config { key: Some("gates.run".into()), value: Some("[\"cargo\", \"test\"]".into()), reset: false })),
            ("/config narrator.model --reset", slash(SlashCommand::Config { key: Some("narrator.model".into()), value: None, reset: true })),
            ("/config --reset", bad_args("config")),
            ("/config -x 1", bad_args("config")),
            ("/model", slash(SlashCommand::Model { role: None, worker: None })),
            ("/model reviewer", slash(SlashCommand::Model { role: Some(RoleName::Reviewer), worker: None })),
            ("/model phase-lead codex/gpt-x", slash(SlashCommand::Model { role: Some(RoleName::PhaseLead), worker: Some(worker("codex/gpt-x")) })),
            ("/model boss codex", bad_args("model")),
            ("/agent", slash(SlashCommand::Agent { worker: None })),
            ("/agent codex", slash(SlashCommand::Agent { worker: Some(worker("codex")) })),
            ("/agent codex claude-code", bad_args("agent")),
            ("/effort", slash(SlashCommand::Effort { role: None, level: None })),
            ("/effort high", slash(SlashCommand::Effort { role: None, level: Some("high".into()) })),
            ("/effort reviewer", slash(SlashCommand::Effort { role: Some(RoleName::Reviewer), level: None })),
            ("/effort owner max", slash(SlashCommand::Effort { role: Some(RoleName::Owner), level: Some("max".into()) })),
            ("/effort loud", bad_args("effort")),
            ("/effort boss high", bad_args("effort")),
            ("/effort owner high now", bad_args("effort")),
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
            ("/agents", slash(SlashCommand::Agents { id: None, key: None, value: None })),
            ("/agents codex", slash(SlashCommand::Agents { id: Some("codex".into()), key: None, value: None })),
            ("/agents codex model \"gpt-x\"", slash(SlashCommand::Agents { id: Some("codex".into()), key: Some("model".into()), value: Some("\"gpt-x\"".into()) })),
            ("/agents codex enabled", bad_args("agents")),
            ("/budget", slash(SlashCommand::Budget { key: None, value: None })),
            ("/budget run_usd 30", slash(SlashCommand::Budget { key: Some("run_usd".into()), value: Some("30".into()) })),
            ("/budget run_usd", bad_args("budget")),
            ("/handoff", slash(SlashCommand::Handoff { role: None, key: None, value: None })),
            ("/handoff owner", slash(SlashCommand::Handoff { role: Some(RoleName::Owner), key: None, value: None })),
            ("/handoff owner mode auto", slash(SlashCommand::Handoff { role: Some(RoleName::Owner), key: Some("mode".into()), value: Some("auto".into()) })),
            ("/handoff owner mode", bad_args("handoff")),
            ("/handoff boss", bad_args("handoff")),
            ("/permissions", slash(SlashCommand::Permissions { key: None, value: None })),
            ("/permissions sandbox.network true", slash(SlashCommand::Permissions { key: Some("sandbox.network".into()), value: Some("true".into()) })),
            ("/permissions limits.usd_per_day 25.0", slash(SlashCommand::Permissions { key: Some("limits.usd_per_day".into()), value: Some("25.0".into()) })),
            ("/permissions all", bad_args("permissions")),
            ("/permissions sandbox.network", bad_args("permissions")),
            ("/permissions ui.keys vim", bad_args("permissions")),
            ("/theme", slash(SlashCommand::Theme { name: None })),
            ("/theme light", slash(SlashCommand::Theme { name: Some("light".into()) })),
            ("/init", slash(SlashCommand::Init)),
            ("/init now", bad_args("init")),
            ("/context", slash(SlashCommand::Context)),
            ("/context now", bad_args("context")),
            ("/workflow", slash(SlashCommand::Workflow { name: None, action: WorkflowAction::Use })),
            ("/workflow review-panel", slash(SlashCommand::Workflow { name: Some("review-panel".into()), action: WorkflowAction::Use })),
            ("/workflow show review-panel", slash(SlashCommand::Workflow { name: Some("review-panel".into()), action: WorkflowAction::Show })),
            ("/workflow diff peer-crew", slash(SlashCommand::Workflow { name: Some("peer-crew".into()), action: WorkflowAction::Diff })),
            ("/workflow copy review-panel", slash(SlashCommand::Workflow { name: Some("review-panel".into()), action: WorkflowAction::Copy })),
            ("/workflow copy review-panel --project", slash(SlashCommand::Workflow { name: Some("review-panel".into()), action: WorkflowAction::CopyToProject })),
            ("/workflow show", bad_args("workflow")),
            ("/workflow --project", bad_args("workflow")),
            ("/workflow a b", bad_args("workflow")),
            ("/checkpoints", slash(SlashCommand::Checkpoints { card: None })),
            ("/checkpoints card-002", slash(SlashCommand::Checkpoints { card: Some(CardId::new("card-002").unwrap()) })),
            ("/checkpoints run-01", bad_args("checkpoints")),
            ("/checkpoints card-002 card-003", bad_args("checkpoints")),
            ("/revert card-002 handoff-1", slash(SlashCommand::Revert { card: CardId::new("card-002").unwrap(), checkpoint: 1 })),
            ("/revert test-004 2", slash(SlashCommand::Revert { card: CardId::new("test-004").unwrap(), checkpoint: 2 })),
            ("/revert card-002", bad_args("revert")),
            ("/revert card-002 handoff-0", bad_args("revert")),
            ("/revert card-002 handoff-x", bad_args("revert")),
            ("/revert handoff-1 card-002", bad_args("revert")),
            ("/revert card-002 handoff-1 now", bad_args("revert")),
            ("/frobnicate", Err(InputError::UnknownCommand("frobnicate".into()))),
            ("/", Err(InputError::UnknownCommand(String::new()))),
            ("/ ask why", Err(InputError::UnknownCommand(String::new()))),
            ("@codex please add a test", mention(MentionTarget::Worker(worker("codex")), "please add a test")),
            ("@claude-code/opus look at the store", mention(MentionTarget::Worker(worker("claude-code/opus")), "look at the store")),
            ("@card-002 keep a fallback", mention(MentionTarget::Card(CardId::new("card-002").unwrap()), "keep a fallback")),
            ("@test-004 fail first", mention(MentionTarget::Card(CardId::new("test-004").unwrap()), "fail first")),
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
    fn every_menu_command_parses_and_every_parsed_command_is_in_the_menu() {
        for spec in SLASH_COMMANDS {
            let args = match spec.name {
                "ask" | "solo" | "crew" => " something",
                "reassign" => " card-001 codex",
                "revert" => " card-001 handoff-1",
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
    fn handlers_match_the_contract() {
        let handler = |name: &str| {
            SLASH_COMMANDS
                .iter()
                .find(|spec| spec.name == name)
                .map(|spec| spec.handler)
        };
        for name in ["tree", "help", "exit"] {
            assert_eq!(handler(name), Some(Handler::Ui), "{name}");
        }
        for name in ["verbose", "normal", "quiet", "vim", "theme"] {
            assert_eq!(handler(name), Some(Handler::Both), "{name}");
        }
        for name in [
            "solo",
            "effort",
            "preset",
            "agents",
            "budget",
            "handoff",
            "permissions",
            "init",
        ] {
            assert_eq!(handler(name), Some(Handler::App), "{name}");
        }
    }

    #[test]
    fn parsed_input_round_trips() {
        for input in [
            "/model reviewer codex/x",
            "@all hi",
            "/narrator live",
            "/config narrator.model --reset",
            "/approvals merge.mode auto",
            "/permissions sandbox.network true",
            "hi",
            "",
        ] {
            let parsed = parse_input(input).unwrap();
            let json = serde_json::to_string(&parsed).unwrap();
            assert_eq!(serde_json::from_str::<ParsedInput>(&json).unwrap(), parsed);
        }
    }
}
