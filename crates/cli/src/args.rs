//! Command-line arguments, and how run flags become config overrides.

use anyhow::{Context as _, bail};
use argonauts_core::KNOWN_AGENT_IDS;
use clap::builder::PossibleValuesParser;
use clap::{Args, Parser, Subcommand, ValueEnum};

/// Runs coding-agent CLIs headlessly in git worktrees, with budgets, gates, and a merge card.
#[derive(Debug, Parser)]
#[command(name = "argonauts", version)]
pub struct Cli {
    /// What to do. With none, the interactive app opens.
    #[command(subcommand)]
    pub command: Option<Command>,
}

/// The subcommands.
#[derive(Debug, Subcommand)]
pub enum Command {
    /// Run a task with one agent in a new git worktree, then show the merge card.
    Run(RunArgs),
    /// Check which agent CLIs are installed and whether their versions are tested.
    Doctor(DoctorArgs),
    /// Show the resolved config or its JSON Schema.
    #[command(subcommand)]
    Config(ConfigCommand),
    /// Walk through the guided setup again: your agents, who you chat with, how the crew
    /// works, approvals, and this folder. It opens by itself the first time you start.
    Init,
}

/// Arguments for `argonauts run`.
#[derive(Debug, Clone, Default, Args)]
pub struct RunArgs {
    /// What the agent should do. Several words are joined with spaces.
    #[arg(required = true, value_name = "TASK")]
    pub task: Vec<String>,

    /// Run the task as a single card. Solo is the only mode for now.
    #[arg(long)]
    pub solo: bool,

    /// Agent to run. Defaults to `default_agent` from the config.
    #[arg(long, value_name = "ID", value_parser = PossibleValuesParser::new(KNOWN_AGENT_IDS))]
    pub agent: Option<String>,

    /// Model id or alias passed to the agent's CLI.
    #[arg(long, value_name = "MODEL")]
    pub model: Option<String>,

    /// Reasoning effort passed to the agent's CLI, such as `low`.
    #[arg(long, value_name = "EFFORT")]
    pub effort: Option<String>,

    /// Use the agent's cheapest model and effort, for trying things out. `--model` and
    /// `--effort` still win.
    #[arg(long)]
    pub cheapest: bool,

    /// Print plain, append-only lines instead of the full screen.
    #[arg(long)]
    pub plain: bool,

    /// Budget for the card in US dollars.
    #[arg(long, value_name = "USD", value_parser = parse_usd)]
    pub budget_usd: Option<f64>,

    /// Command that must pass before merging, run in the worktree. Repeat for more.
    /// Replaces the configured gates.
    #[arg(long = "gate", value_name = "CMD")]
    pub gates: Vec<String>,

    /// Whether to ask at the merge card or leave the branch without asking.
    #[arg(long, value_enum, value_name = "MODE")]
    pub merge: Option<MergeArg>,

    /// Override a config setting, such as `budget.per_card=5`. Repeatable.
    #[arg(long = "set", value_name = "KEY=VALUE")]
    pub set: Vec<String>,
}

/// `--merge` values.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum MergeArg {
    /// Show the merge card and wait for an answer.
    Ask,
    /// Keep the card branch and finish.
    Leave,
}

/// Arguments for `argonauts doctor`.
#[derive(Debug, Clone, Default, Args)]
pub struct DoctorArgs {
    /// Print the report as JSON.
    #[arg(long)]
    pub json: bool,
}

/// `argonauts config` subcommands.
#[derive(Debug, Clone, Subcommand)]
pub enum ConfigCommand {
    /// Print the resolved config as TOML, with the layers it came from.
    Show {
        /// Override a config setting, such as `budget.per_card=5`. Repeatable.
        #[arg(long = "set", value_name = "KEY=VALUE")]
        set: Vec<String>,
    },
    /// Print the JSON Schema for config files.
    Schema,
}

impl RunArgs {
    pub fn task_text(&self) -> String {
        self.task.join(" ")
    }

    /// Overrides that pick the agent, applied after `--set` so the flag wins.
    pub fn agent_overrides(&self) -> Vec<String> {
        let mut overrides = self.set.clone();
        if let Some(agent) = &self.agent {
            overrides.push(format!("default_agent={}", toml_string(agent)));
        }
        overrides
    }

    /// `--set` comes first so the flags win. Flags go through the config so the run's recorded
    /// config and hash include them.
    pub fn overrides(
        &self,
        agent: &str,
        cheapest: (&str, Option<&str>),
    ) -> anyhow::Result<Vec<String>> {
        let agent_setting =
            |key: &str, value: &str| format!("agents.{agent}.{key}={}", toml_string(value));
        let mut overrides = self.agent_overrides();
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
            overrides.push("budget.unit=\"usd\"".to_owned());
            overrides.push(format!("budget.per_card={}", toml::Value::Float(usd)));
        }
        if !self.gates.is_empty() {
            overrides.push(format!("gates.commands={}", gate_commands(&self.gates)?));
        }
        if let Some(merge) = self.merge {
            overrides.push(format!("merge.mode=\"{}\"", merge.config_value()));
        }
        if self.plain {
            overrides.push("ui.plain=true".to_owned());
        }
        Ok(overrides)
    }
}

impl MergeArg {
    fn config_value(self) -> &'static str {
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

/// The `--gate` commands as a TOML array of argument vectors.
fn gate_commands(gates: &[String]) -> anyhow::Result<String> {
    let mut commands = Vec::with_capacity(gates.len());
    for gate in gates {
        let argv = shell_words::split(gate).with_context(|| format!("--gate {gate:?}"))?;
        if argv.is_empty() {
            bail!("--gate {gate:?} has no command");
        }
        commands.push(toml::Value::Array(
            argv.into_iter().map(toml::Value::String).collect(),
        ));
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
    use argonauts_core::{BudgetLimit, Config, ConfigSources, MergeMode};
    use clap::CommandFactory;

    use super::*;

    fn parse(args: &[&str]) -> Cli {
        Cli::try_parse_from(std::iter::once("argonauts").chain(args.iter().copied())).unwrap()
    }

    fn run_args(args: &[&str]) -> RunArgs {
        match parse(&[&["run"], args].concat()).command {
            Some(Command::Run(run)) => run,
            other => panic!("expected run, got {other:?}"),
        }
    }

    const CHEAPEST: (&str, Option<&str>) = ("tiny", Some("low"));

    fn resolve(args: &RunArgs, agent: &str) -> Config {
        let sources = ConfigSources {
            overrides: args.overrides(agent, CHEAPEST).unwrap(),
            ..ConfigSources::default()
        };
        Config::load(&sources).unwrap().config
    }

    #[test]
    fn cheapest_sets_the_model_and_effort_but_flags_win() {
        let args = run_args(&["--cheapest", "task"]);
        let agent = resolve(&args, "codex").agent("codex");
        assert_eq!(agent.model.as_deref(), Some("tiny"));
        assert_eq!(agent.effort.as_deref(), Some("low"));
        let args = run_args(&["--cheapest", "--effort", "high", "task"]);
        let agent = resolve(&args, "codex").agent("codex");
        assert_eq!(agent.model.as_deref(), Some("tiny"));
        assert_eq!(agent.effort.as_deref(), Some("high"));
    }

    #[test]
    fn plain_is_recorded_in_the_config() {
        let args = run_args(&["--plain", "task"]);
        assert!(resolve(&args, "codex").ui.plain);
        assert!(!resolve(&run_args(&["task"]), "codex").ui.plain);
    }

    #[test]
    fn the_cli_definition_is_consistent() {
        Cli::command().debug_assert();
    }

    #[test]
    fn run_accepts_every_documented_flag() {
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
        assert!(args.solo && args.plain && args.cheapest);
        assert_eq!(args.agent.as_deref(), Some("codex"));
        assert_eq!(args.task_text(), "Create hello.txt");
        assert_eq!(args.gates.len(), 2);
        assert_eq!(args.merge, Some(MergeArg::Leave));
    }

    #[test]
    fn flags_become_config_overrides() {
        let args = run_args(&[
            "--model",
            "haiku",
            "--effort",
            "low",
            "--budget-usd",
            "0.5",
            "--gate",
            "test -f 'hello world.txt'",
            "--merge",
            "leave",
            "task",
        ]);
        let config = resolve(&args, "claude-code");
        let agent = config.agent("claude-code");
        assert_eq!(agent.model.as_deref(), Some("haiku"));
        assert_eq!(agent.effort.as_deref(), Some("low"));
        assert_eq!(config.budget.per_card_limit(), BudgetLimit::Usd(0.5));
        assert_eq!(
            config.gates.commands,
            vec![vec!["test", "-f", "hello world.txt"]]
        );
        assert_eq!(config.merge.mode, MergeMode::Leave);
    }

    #[test]
    fn flags_win_over_set() {
        let args = run_args(&[
            "--set",
            "agents.codex.model=other",
            "--set",
            "budget.unit=tokens",
            "--model",
            "gpt-5.6-luna",
            "--budget-usd",
            "2",
            "task",
        ]);
        let config = resolve(&args, "codex");
        assert_eq!(config.agent("codex").model.as_deref(), Some("gpt-5.6-luna"));
        assert_eq!(config.budget.per_card_limit(), BudgetLimit::Usd(2.0));
    }

    #[test]
    fn agent_flag_sets_the_default_agent() {
        let args = run_args(&["--agent", "antigravity-cli", "task"]);
        let sources = ConfigSources {
            overrides: args.agent_overrides(),
            ..ConfigSources::default()
        };
        let config = Config::load(&sources).unwrap().config;
        assert_eq!(config.default_agent, "antigravity-cli");
    }

    #[test]
    fn values_that_look_like_numbers_stay_strings() {
        let args = run_args(&["--model", "1.5", "--effort", "true", "task"]);
        let agent = resolve(&args, "codex").agent("codex");
        assert_eq!(agent.model.as_deref(), Some("1.5"));
        assert_eq!(agent.effort.as_deref(), Some("true"));
    }

    #[test]
    fn no_flags_means_only_set() {
        let args = run_args(&["--set", "ui.verbosity=quiet", "task"]);
        assert_eq!(
            args.overrides("codex", CHEAPEST).unwrap(),
            vec!["ui.verbosity=quiet"]
        );
    }

    #[test]
    fn bad_gates_are_rejected() {
        let unbalanced = run_args(&["--gate", "test -f 'hello", "task"]);
        assert!(unbalanced.overrides("codex", CHEAPEST).is_err());
        let empty = run_args(&["--gate", "  ", "task"]);
        let err = empty.overrides("codex", CHEAPEST).unwrap_err();
        assert!(err.to_string().contains("no command"), "{err}");
    }

    #[test]
    fn bad_values_fail_to_parse() {
        let parse = |args: &[&str]| {
            Cli::try_parse_from(std::iter::once("argonauts").chain(args.iter().copied()))
        };
        assert!(parse(&["run", "--budget-usd", "0", "task"]).is_err());
        assert!(parse(&["run", "--budget-usd", "lots", "task"]).is_err());
        assert!(parse(&["run", "--agent", "gemini", "task"]).is_err());
        assert!(parse(&["run", "--merge", "maybe", "task"]).is_err());
        assert!(parse(&["run"]).is_err());
    }

    #[test]
    fn no_subcommand_opens_the_interactive_app() {
        assert!(parse(&[]).command.is_none());
        assert!(matches!(parse(&["init"]).command, Some(Command::Init)));
    }

    #[test]
    fn doctor_and_config_parse() {
        assert!(matches!(
            parse(&["doctor", "--json"]).command,
            Some(Command::Doctor(DoctorArgs { json: true }))
        ));
        assert!(matches!(
            parse(&["config", "schema"]).command,
            Some(Command::Config(ConfigCommand::Schema))
        ));
        match parse(&["config", "show", "--set", "budget.per_card=5"]).command {
            Some(Command::Config(ConfigCommand::Show { set })) => {
                assert_eq!(set, vec!["budget.per_card=5"]);
            }
            other => panic!("expected config show, got {other:?}"),
        }
    }
}
