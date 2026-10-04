//! Command-line arguments, and how `run`'s flags become settings overrides.

use std::path::PathBuf;

use anyhow::{Context as _, bail};
use clap::{Args, Parser, Subcommand, ValueEnum};

/// Runs your coding-agent CLIs as one crew, in git worktrees, and asks you before anything lands.
#[derive(Debug, Parser)]
#[command(name = "argonauts", version)]
pub struct Cli {
    /// Plain, append-only lines for a screen reader instead of the full screen.
    #[arg(long, global = true)]
    pub screen_reader: bool,

    /// Override a setting for this session, such as `ui.bell=false`. Repeatable.
    #[arg(long = "set", value_name = "KEY=VALUE", global = true)]
    pub set: Vec<String>,

    /// Carry on the newest conversation in this repo.
    #[arg(short = 'c', long = "continue")]
    pub continue_chat: bool,

    /// What to do. With none, the chat opens.
    #[command(subcommand)]
    pub command: Option<Command>,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Start a run for scripts and CI, then watch it in the app, or as plain lines without a
    /// terminal.
    Run(RunArgs),
    /// Open the app on an earlier conversation, a run, an agent's session in a run, or the main
    /// agent CLI's own session to carry it on, or on the list of them to pick one.
    Resume {
        /// A conversation such as `chat-002`, a run such as `run-03`, an agent's session in a
        /// run such as `run-03/s-002`, or the main agent CLI's own session such as
        /// `codex:019a2f`. With none, you pick from the list, which lists each run's agents.
        id: Option<String>,
    },
    /// Runs going across your repos, and this repo's runs that haven't ended.
    Status {
        /// Print it as JSON.
        #[arg(long)]
        json: bool,
    },
    /// Walk through the guided setup again: your agents, who you chat with, how the crew
    /// works, approvals, and this folder. It opens by itself the first time you start.
    Init,
    /// Check which agent CLIs are installed, and remind you of runs parked a long time.
    Doctor(DoctorArgs),
    /// Show, explain, or change your settings.
    #[command(subcommand)]
    Config(ConfigCommand),
    /// The models Argonauts knows about.
    Models {
        /// Ask each CLI and local server which models it has first. Never calls a model.
        #[arg(long)]
        discover: bool,
    },
    /// Open a run's waiting checkpoint in the app, to answer it there. It needs you at a
    /// terminal, and refuses inside an agent's session.
    Approve {
        /// Such as `run-03`.
        run: String,
        /// Such as `cp-002`. `argonauts inbox` lists what waits.
        checkpoint: String,
    },
    /// How each checkpoint is approved, and how often your rules agreed with you.
    Approvals,
    /// Turn auto approval off.
    #[command(subcommand)]
    Auto(AutoCommand),
    /// What waits for you, across this repo's runs, and the proposals learning keeps for you.
    Inbox,
    /// Who is good at what: track records and prediction scores, or a rebuild from the outcome
    /// log as of a date.
    Records {
        #[arg(value_enum)]
        action: Option<RecordsAction>,
        /// Count only what happened before the end of this date, such as `2026-09-30`.
        #[arg(long, value_name = "DATE")]
        as_of: Option<String>,
        /// Print it as JSON.
        #[arg(long)]
        json: bool,
    },
    /// Recipes, candidates, and their replays.
    Recipes {
        #[command(subcommand)]
        action: Option<RecipesAction>,
        /// Print it as JSON.
        #[arg(long, global = true)]
        json: bool,
    },
    /// Exported and imported skills: list, export a recipe, or import one for review.
    Skills {
        #[command(subcommand)]
        action: Option<SkillsAction>,
        /// Print it as JSON.
        #[arg(long, global = true)]
        json: bool,
    },
    /// Render AGENTS.md and CLAUDE.md from .argonauts/conventions.md, and report drift.
    Sync {
        /// Only report what would change.
        #[arg(long, conflicts_with = "force")]
        check: bool,
        /// Overwrite files edited by hand. Needs you at a terminal.
        #[arg(long)]
        force: bool,
        /// Print it as JSON.
        #[arg(long)]
        json: bool,
    },
    /// A run's retro: its outcome, predictions against results, memory, routing, and the change
    /// it proposed.
    Retro {
        /// Such as `run-03`.
        run: String,
        /// Print it as JSON.
        #[arg(long)]
        json: bool,
    },
    /// Host one run. The app starts these; you don't need to.
    #[command(hide = true)]
    Supervise {
        #[arg(long)]
        repo: PathBuf,
        #[arg(long)]
        run: String,
    },
    /// Talk to Argonauts over MCP.
    #[command(subcommand)]
    Mcp(McpCommand),
}

#[derive(Debug, Clone, Default, Args)]
pub struct RunArgs {
    /// What to do. Several words are joined with spaces.
    #[arg(required = true, value_name = "TASK")]
    pub task: Vec<String>,

    /// One agent on one card.
    #[arg(long, conflicts_with = "crew")]
    pub solo: bool,

    /// The whole crew: goal, plan, cards, reviews.
    #[arg(long)]
    pub crew: bool,

    /// Print plain, append-only lines instead of the app.
    #[arg(long)]
    pub plain: bool,

    /// Start the run and print its id without waiting.
    #[arg(long)]
    pub detach: bool,

    /// The agent to use. Defaults to `default_agent` from your settings.
    #[arg(long, value_name = "ID")]
    pub agent: Option<String>,

    /// Model id or alias passed to the agent's CLI.
    #[arg(long, value_name = "MODEL")]
    pub model: Option<String>,

    /// Reasoning effort passed to the agent's CLI, such as `low`.
    #[arg(long, value_name = "EFFORT")]
    pub effort: Option<String>,

    /// The agent's cheapest model and effort, for trying things out. `--model` and `--effort`
    /// still win.
    #[arg(long)]
    pub cheapest: bool,

    /// Budget for each card in US dollars.
    #[arg(long, value_name = "USD", value_parser = parse_usd)]
    pub budget_usd: Option<f64>,

    /// A command that must pass before merging, run in the worktree. Repeat for more. Replaces
    /// your configured gates, so it needs you at a terminal. In scripts and CI, set `gates.run`
    /// in a settings file.
    #[arg(long = "gate", value_name = "CMD")]
    pub gates: Vec<String>,

    /// Whether to ask at the merge checkpoint or leave the branch without asking. It needs you
    /// at a terminal. In scripts and CI, set `merge.mode` in a settings file.
    #[arg(long, value_enum, value_name = "MODE")]
    pub merge: Option<MergeArg>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum MergeArg {
    /// Wait for your answer at the merge checkpoint.
    Ask,
    /// Keep the branch and finish.
    Leave,
}

#[derive(Debug, Clone, Default, Args)]
pub struct DoctorArgs {
    /// Print the report as JSON.
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Clone, Subcommand)]
pub enum ConfigCommand {
    /// Print the settings in effect as TOML, with the files they came from.
    Show,
    /// Print the JSON Schema of the settings files.
    Schema,
    /// Save one setting. Each key has a usual file; a flag picks another.
    Set {
        /// Such as `ui.bell`.
        key: String,
        /// A TOML value. A bare word is read as a string.
        value: String,
        /// This repo, just for you: `.argonauts/config.local.toml`.
        #[arg(long, group = "file")]
        local: bool,
        /// This repo, shared with your team: `.argonauts/config.toml`.
        #[arg(long, group = "file")]
        project: bool,
        /// Everywhere: `~/.argonauts/config.toml`.
        #[arg(long, group = "file")]
        global: bool,
    },
}

#[derive(Debug, Clone, Subcommand)]
pub enum AutoCommand {
    /// Every checkpoint waits for you again, in every run, until you turn auto back on.
    Off,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum RecordsAction {
    /// Recompute the records from the outcome log.
    Rebuild,
}

#[derive(Debug, Clone, Subcommand)]
pub enum RecipesAction {
    /// Every recipe and its current version.
    List,
    /// A recipe's current version.
    Show { name: String },
    /// What changed between two versions: the current and its parent unless you name them.
    Diff {
        name: String,
        #[arg(long, value_name = "VERSION")]
        from: Option<u32>,
        #[arg(long, value_name = "VERSION")]
        to: Option<u32>,
    },
    /// Replay the oldest queued candidate now, within the month's evolution budget.
    Replay,
    /// Promote a candidate that waits for you. Needs you at a terminal.
    Promote { candidate: String },
    /// Roll a recipe back to the version before; the pointer change waits on a branch.
    Rollback { name: String },
}

#[derive(Debug, Clone, Subcommand)]
pub enum SkillsAction {
    /// Every exported and imported skill, and any drift.
    List,
    /// Propose a recipe as a skill, on a branch for you.
    Export { recipe: String },
    /// Propose a skill from a folder or a git URL, on a branch for you. Nothing in it runs.
    Import {
        #[arg(value_name = "FOLDER_OR_URL")]
        source: String,
    },
}

#[derive(Debug, Clone, Subcommand)]
pub enum McpCommand {
    /// Serve your runs to an MCP client on stdin and stdout. The repo must be set up with
    /// `argonauts init` and its settings trusted.
    Serve {
        /// The repo to serve. Defaults to the one holding the current directory.
        #[arg(long, value_name = "PATH")]
        repo: Option<PathBuf>,
    },
    /// Serve one session's status file, for the agent in that session.
    Status {
        #[arg(long, value_name = "PATH")]
        file: PathBuf,
    },
    /// Serve one agent session's tools: its status, and the report tool when its step has an
    /// output schema. The conductor starts it for the agent.
    #[command(hide = true)]
    Session {
        #[arg(long, value_name = "PATH")]
        status_file: Option<PathBuf>,
        /// The step's output schema, which the report tool checks each call against.
        #[arg(long, value_name = "PATH", requires = "report_file")]
        report_schema: Option<PathBuf>,
        /// Where the last valid report goes.
        #[arg(long, value_name = "PATH", requires = "report_schema")]
        report_file: Option<PathBuf>,
        /// A direct turn's questions go to the app listening here, with the token in
        /// `ARGONAUTS_APPROVE_TOKEN`.
        #[arg(long, value_name = "HOST:PORT")]
        ask: Option<String>,
        /// Offer `approve`, Claude Code's permission prompt tool.
        #[arg(long, requires = "ask")]
        approve_tool: bool,
        /// Offer `argonauts_start_run`.
        #[arg(long, requires = "ask")]
        start_run: bool,
    },
    /// Write the delegation package other agents install: an MCP entry that runs this binary,
    /// the delegate-to-argonauts skill, and the plugin layouts.
    Package {
        dir: PathBuf,
        #[arg(long, value_name = "PATH")]
        repo: PathBuf,
    },
}

impl RunArgs {
    pub fn task_text(&self) -> String {
        self.task.join(" ")
    }

    /// Whether a flag names the agent's model or effort, which needs the agent's id.
    pub fn needs_agent(&self) -> bool {
        self.model.is_some() || self.effort.is_some() || self.cheapest
    }

    /// `--set` first, so the flags win. `agent` is `--agent` or your default agent, and
    /// `cheapest` is its cheapest model and effort.
    pub fn overrides(
        &self,
        global_set: &[String],
        agent: &str,
        cheapest: (&str, Option<&str>),
    ) -> anyhow::Result<Vec<String>> {
        let agent_setting =
            |key: &str, value: &str| format!("agents.{agent}.{key}={}", toml_string(value));
        let mut overrides = global_set.to_vec();
        if self.agent.is_some() {
            overrides.push(format!("default_agent={}", toml_string(agent)));
        }
        if self.cheapest {
            let (model, effort) = cheapest;
            overrides.push(agent_setting("model", model));
            if let Some(effort) = effort {
                overrides.push(agent_setting("effort", effort));
            }
        }
        if let Some(model) = &self.model {
            overrides.push(agent_setting("model", model));
        }
        if let Some(effort) = &self.effort {
            overrides.push(agent_setting("effort", effort));
        }
        if let Some(usd) = self.budget_usd {
            overrides.push(format!("budget.per_card_usd={}", toml::Value::Float(usd)));
        }
        if !self.gates.is_empty() {
            overrides.push(format!("gates.run={}", gate_commands(&self.gates)?));
        }
        if let Some(merge) = self.merge {
            overrides.push(format!("merge.mode=\"{}\"", merge.setting()));
        }
        Ok(overrides)
    }
}

impl MergeArg {
    fn setting(self) -> &'static str {
        match self {
            MergeArg::Ask => "ask",
            MergeArg::Leave => "leave",
        }
    }
}

/// `text` as a quoted TOML string, so values such as `1.5` or `true` stay strings.
fn toml_string(text: &str) -> String {
    toml::Value::String(text.to_owned()).to_string()
}

/// The `--gate` commands as TOML gates, each named by what you typed and run as an argument
/// vector.
fn gate_commands(gates: &[String]) -> anyhow::Result<String> {
    let mut commands = Vec::with_capacity(gates.len());
    for gate in gates {
        let argv = shell_words::split(gate).with_context(|| format!("--gate {gate:?}"))?;
        if argv.is_empty() {
            bail!("--gate {gate:?} has no command");
        }
        let mut table = toml::Table::new();
        table.insert("name".to_owned(), toml::Value::String(gate.clone()));
        table.insert(
            "command".to_owned(),
            toml::Value::Array(argv.into_iter().map(toml::Value::String).collect()),
        );
        commands.push(toml::Value::Table(table));
    }
    Ok(toml::Value::Array(commands).to_string())
}

fn parse_usd(text: &str) -> Result<f64, String> {
    let usd: f64 = text
        .parse()
        .map_err(|_| format!("{text:?} is not a number"))?;
    if usd.is_finite() && usd > 0.0 {
        Ok(usd)
    } else {
        Err("the budget must be more than 0".to_owned())
    }
}

#[cfg(test)]
mod tests {
    use clap::CommandFactory;

    use super::*;

    fn parse(args: &[&str]) -> Cli {
        Cli::try_parse_from(std::iter::once("argonauts").chain(args.iter().copied())).unwrap()
    }

    fn fails(args: &[&str]) -> bool {
        Cli::try_parse_from(std::iter::once("argonauts").chain(args.iter().copied())).is_err()
    }

    fn run_args(args: &[&str]) -> RunArgs {
        match parse(&[&["run"], args].concat()).command {
            Some(Command::Run(run)) => run,
            other => panic!("expected run, got {other:?}"),
        }
    }

    const CHEAPEST: (&str, Option<&str>) = ("tiny", Some("low"));

    /// Each override's value parses as TOML.
    fn valid(overrides: &[String]) {
        for line in overrides {
            let (key, value) = line.split_once('=').unwrap();
            let text = format!("x = {value}");
            assert!(text.parse::<toml::Table>().is_ok(), "{key}: {value}");
        }
    }

    #[test]
    fn the_cli_definition_is_consistent() {
        Cli::command().debug_assert();
    }

    #[test]
    fn no_subcommand_opens_the_app() {
        let cli = parse(&["--screen-reader", "--set", "ui.bell=false"]);
        assert!(cli.command.is_none());
        assert!(cli.screen_reader);
        assert_eq!(cli.set, vec!["ui.bell=false"]);
        assert!(matches!(parse(&["init"]).command, Some(Command::Init)));
    }

    #[test]
    fn run_keeps_the_solo_flags() {
        let args = run_args(&[
            "--solo",
            "--agent",
            "codex",
            "--model",
            "gpt-5.6-luna",
            "--effort",
            "low",
            "--cheapest",
            "--plain",
            "--budget-usd",
            "0.5",
            "--gate",
            "test -f hello.txt",
            "--gate",
            "cargo test",
            "--merge",
            "leave",
            "Create",
            "hello.txt",
        ]);
        assert!(args.solo && args.plain && args.cheapest && !args.crew);
        assert_eq!(args.agent.as_deref(), Some("codex"));
        assert_eq!(args.task_text(), "Create hello.txt");
        assert_eq!(args.gates.len(), 2);
        assert_eq!(args.merge, Some(MergeArg::Leave));
    }

    #[test]
    fn flags_become_overrides_after_set() {
        let args = run_args(&[
            "--agent",
            "codex",
            "--model",
            "1.5",
            "--budget-usd",
            "2",
            "--gate",
            "test -f 'hello world.txt'",
            "--merge",
            "ask",
            "task",
        ]);
        let set = vec!["ui.bell=false".to_owned()];
        let overrides = args.overrides(&set, "codex", CHEAPEST).unwrap();
        assert_eq!(
            overrides,
            vec![
                "ui.bell=false".to_owned(),
                "default_agent=\"codex\"".to_owned(),
                "agents.codex.model=\"1.5\"".to_owned(),
                "budget.per_card_usd=2.0".to_owned(),
                "gates.run=[{ command = [\"test\", \"-f\", \"hello world.txt\"], name = \"test -f 'hello world.txt'\" }]".to_owned(),
                "merge.mode=\"ask\"".to_owned(),
            ]
        );
        valid(&overrides);
    }

    #[test]
    fn cheapest_sets_the_model_and_effort_but_flags_win() {
        let args = run_args(&["--cheapest", "--effort", "high", "task"]);
        assert!(args.needs_agent());
        let overrides = args.overrides(&[], "codex", CHEAPEST).unwrap();
        assert_eq!(
            overrides,
            vec![
                "agents.codex.model=\"tiny\"",
                "agents.codex.effort=\"low\"",
                "agents.codex.effort=\"high\"",
            ]
        );
        assert!(!run_args(&["task"]).needs_agent());
    }

    #[test]
    fn bad_gates_are_rejected() {
        let unbalanced = run_args(&["--gate", "test -f 'hello", "task"]);
        assert!(unbalanced.overrides(&[], "codex", CHEAPEST).is_err());
        let empty = run_args(&["--gate", "  ", "task"]);
        let err = empty.overrides(&[], "codex", CHEAPEST).unwrap_err();
        assert!(err.to_string().contains("no command"), "{err}");
    }

    #[test]
    fn bad_values_fail_to_parse() {
        assert!(fails(&["run", "--budget-usd", "0", "task"]));
        assert!(fails(&["run", "--budget-usd", "lots", "task"]));
        assert!(fails(&["run", "--merge", "maybe", "task"]));
        assert!(fails(&["run", "--solo", "--crew", "task"]));
        assert!(fails(&["run"]));
        assert!(fails(&[
            "config", "set", "ui.bell", "false", "--local", "--global"
        ]));
    }

    #[test]
    fn every_subcommand_parses() {
        assert!(matches!(
            parse(&["resume", "run-03"]).command,
            Some(Command::Resume { id: Some(id) }) if id == "run-03"
        ));
        assert!(matches!(
            parse(&["resume", "chat-002"]).command,
            Some(Command::Resume { id: Some(id) }) if id == "chat-002"
        ));
        assert!(matches!(
            parse(&["resume"]).command,
            Some(Command::Resume { id: None })
        ));
        let continued = parse(&["-c"]);
        assert!(continued.continue_chat && continued.command.is_none());
        assert!(parse(&["--continue"]).continue_chat);
        assert!(!parse(&[]).continue_chat);
        assert!(matches!(
            parse(&["status", "--json"]).command,
            Some(Command::Status { json: true })
        ));
        assert!(matches!(
            parse(&["doctor", "--json"]).command,
            Some(Command::Doctor(DoctorArgs { json: true }))
        ));
        assert!(matches!(
            parse(&["config", "schema"]).command,
            Some(Command::Config(ConfigCommand::Schema))
        ));
        assert!(matches!(
            parse(&["config", "show", "--set", "ui.bell=false"]).command,
            Some(Command::Config(ConfigCommand::Show))
        ));
        assert!(matches!(
            parse(&["config", "set", "ui.bell", "false", "--project"]).command,
            Some(Command::Config(ConfigCommand::Set {
                project: true,
                local: false,
                global: false,
                ..
            }))
        ));
        assert!(matches!(
            parse(&["models", "--discover"]).command,
            Some(Command::Models { discover: true })
        ));
        assert!(matches!(
            parse(&["approvals"]).command,
            Some(Command::Approvals)
        ));
        assert!(matches!(
            parse(&["auto", "off"]).command,
            Some(Command::Auto(AutoCommand::Off))
        ));
        assert!(matches!(parse(&["inbox"]).command, Some(Command::Inbox)));
        assert!(matches!(
            parse(&["supervise", "--repo", "/r", "--run", "run-01"]).command,
            Some(Command::Supervise { .. })
        ));
        assert!(matches!(
            parse(&["mcp", "serve"]).command,
            Some(Command::Mcp(McpCommand::Serve { repo: None }))
        ));
        assert!(matches!(
            parse(&["mcp", "serve", "--repo", "/r"]).command,
            Some(Command::Mcp(McpCommand::Serve { repo: Some(_) }))
        ));
        assert!(matches!(
            parse(&["approve", "run-03", "cp-002"]).command,
            Some(Command::Approve { .. })
        ));
        assert!(matches!(
            parse(&["mcp", "status", "--file", "/s.json"]).command,
            Some(Command::Mcp(McpCommand::Status { .. }))
        ));
        // As the conductor writes it into each session's MCP config: the status file, the
        // report tool's two files, or both.
        assert!(matches!(
            parse(&[
                "mcp",
                "session",
                "--status-file",
                "/s/status.json",
                "--report-schema",
                "/s/schema.json",
                "--report-file",
                "/s/report.json",
            ])
            .command,
            Some(Command::Mcp(McpCommand::Session {
                status_file: Some(_),
                report_schema: Some(_),
                report_file: Some(_),
                ..
            }))
        ));
        // A direct turn's server, which asks the app and may start a run.
        assert!(matches!(
            parse(&[
                "mcp",
                "session",
                "--ask",
                "127.0.0.1:9",
                "--approve-tool",
                "--start-run"
            ])
            .command,
            Some(Command::Mcp(McpCommand::Session {
                ask: Some(_),
                approve_tool: true,
                start_run: true,
                ..
            }))
        ));
        assert!(matches!(
            parse(&["mcp", "session", "--status-file", "/s/status.json"]).command,
            Some(Command::Mcp(McpCommand::Session {
                status_file: Some(_),
                report_schema: None,
                report_file: None,
                ..
            }))
        ));
        assert!(matches!(
            parse(&[
                "mcp",
                "session",
                "--report-schema",
                "/s/schema.json",
                "--report-file",
                "/s/report.json",
            ])
            .command,
            Some(Command::Mcp(McpCommand::Session {
                status_file: None,
                report_schema: Some(_),
                report_file: Some(_),
                ..
            }))
        ));
    }
}
