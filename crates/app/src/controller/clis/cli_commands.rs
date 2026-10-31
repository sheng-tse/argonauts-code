//! The conversation's CLI's own commands and keywords: which ones the screen offers, and
//! sending one to the conversation. They're the main agent's, or those of the agent whose session
//! the conversation carries on, and they're looked up again whenever that CLI or its folder
//! changes. A command a CLI lists as its own that its adapter's table doesn't have, as a newer
//! version adds, joins them too.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use argonauts_conductor::boards::cli_versions::{CliVersions, new_command};
use argonauts_core::Adapter;
use argonauts_core::NoteLevel;
use argonauts_core::ReportedCommand;
use argonauts_core::agents::cli_commands::{CliCommand, CliCommandSet, CommandKind, CommandSource};
use argonauts_core::agents::cli_sessions::CliScan;
use argonauts_core::app::{AppEvent, UiCommand};
use argonauts_core::base::names::AgentId;
use argonauts_core::base::paths::{create_private_dir_all, write_private_file};
use argonauts_core::input::{InputError, ParsedInput, parse_input};

use crate::controller::conversation::{self, ConversationState};
use crate::controller::{Ctx, Effect};

/// What each CLI reported last, by folder, so the `/` menu has a mod's commands before the
/// next app session's first turn: `~/.argonauts/state/<cli>-commands.json`.
const REPORTED_FILE: &str = "commands.json";

#[derive(Default)]
pub(crate) struct CliCommands {
    adapter: Option<Arc<dyn Adapter>>,
    set: Option<CliCommandSet>,
    /// The CLI they belong to and the folder they were found for.
    found_for: Option<(String, PathBuf)>,
}

impl CliCommands {
    pub(crate) fn set(&self) -> Option<&CliCommandSet> {
        self.set.as_ref()
    }
}

/// Looks the conversation's CLI's commands up again, and tells the screen when they changed. An
/// agent with no CLI of its own, such as an endpoint, has none.
pub(crate) fn refresh(ctx: &Ctx, state: &mut CliCommands, conversation: &ConversationState) {
    let cli = conversation.cli(ctx).to_owned();
    let folder = conversation.folder(ctx).to_path_buf();
    let adapter = match crate::commands::adapters_here(Some(&ctx.env), ctx.settings.resolved()) {
        Ok(adapters) => adapters.into_iter().find(|adapter| adapter.id() == cli),
        Err(err) => {
            ctx.notice(
                NoteLevel::Warn,
                format!(
                    "Couldn't list {}'s own commands: {err:#}",
                    ctx.names.cli(&cli)
                ),
            );
            None
        }
    };
    let mut set = match &adapter {
        Some(adapter) => command_set(ctx, adapter.as_ref(), &folder),
        None => CliCommandSet::new(AgentId(cli.clone()), cli.clone(), &[], Vec::new()),
    };
    // A workflow never takes the name of one of the CLI's own commands, so a
    // cloned `simplify.toml` never runs in place of the CLI's /simplify. Your other workflows
    // come first, so a name of yours is never taken for a command the CLI only lists.
    let builtin: Vec<String> = set
        .commands
        .iter()
        .filter(|command| command.kind != CommandKind::Workflow)
        .flat_map(|command| std::iter::once(&command.name).chain(&command.aliases))
        .cloned()
        .collect();
    let mut commands: Vec<CliCommand> = crate::controller::workflows::commands(ctx)
        .into_iter()
        .filter(|workflow| !builtin.contains(&workflow.name))
        .collect();
    commands.append(&mut set.commands);
    set.commands = commands;
    if state.set.as_ref() != Some(&set) {
        ctx.send(AppEvent::CliCommands { set: set.clone() });
    }
    state.adapter = adapter;
    state.set = Some(set);
    state.found_for = Some((cli, folder));
}

/// Looks them up again once the conversation goes to another CLI or works in another folder, as
/// when it carries on an agent's session from a run.
pub(crate) fn follow(ctx: &Ctx, state: &mut CliCommands, conversation: &ConversationState) {
    let current = state.found_for.as_ref().is_some_and(|(cli, folder)| {
        cli == conversation.cli(ctx) && folder == conversation.folder(ctx)
    });
    if !current {
        refresh(ctx, state, conversation);
    }
}

/// The list the conversation's own session just reported, such as Claude Code's `initialize`
/// reply or `commands_changed`. A plugin's or a mod's commands join the menu, kept for this
/// folder. The CLI's own are in its table already, and one a newer version added that the
/// table doesn't have joins the menu in every folder.
pub(crate) fn reported(
    ctx: &Ctx,
    state: &mut CliCommands,
    conversation: &ConversationState,
    commands: &[ReportedCommand],
) {
    let cli = conversation.cli(ctx).to_owned();
    let folder = conversation.folder(ctx).to_path_buf();
    let (own, theirs): (Vec<ReportedCommand>, Vec<ReportedCommand>) = commands
        .iter()
        .cloned()
        .partition(|command| command.builtin);
    let mut changed = keep_for_folder(ctx, &cli, &folder, theirs);
    if let Some(adapter) = state.adapter.clone().filter(|adapter| adapter.id() == cli) {
        let new = own
            .into_iter()
            .filter(|command| new_command(adapter.as_ref(), command))
            .collect();
        changed |= learn_new_commands(ctx, &cli, new);
    }
    if changed {
        refresh(ctx, state, conversation);
    }
}

/// Keeps a plugin's or a mod's commands for `folder`. True when they changed.
fn keep_for_folder(ctx: &Ctx, cli: &str, folder: &Path, theirs: Vec<ReportedCommand>) -> bool {
    let mut kept = load_reported(ctx, cli);
    let key = folder.to_string_lossy().into_owned();
    if kept.get(&key) == Some(&theirs) {
        return false;
    }
    kept.insert(key, theirs);
    if let Err(err) = save_reported(ctx, cli, &kept) {
        warn_unkept(ctx, cli, &err);
    }
    true
}

/// Keeps the CLI's own commands its table lacks where Doctor counts them. True when they
/// changed.
fn learn_new_commands(ctx: &Ctx, cli: &str, new: Vec<ReportedCommand>) -> bool {
    let path = ctx.env.paths.cli_versions();
    let mut file = CliVersions::load(&path);
    if !file.learn_commands(cli, new) {
        return false;
    }
    if let Err(err) = file.save(&path) {
        warn_unkept(ctx, cli, &err.into());
    }
    true
}

fn warn_unkept(ctx: &Ctx, cli: &str, err: &anyhow::Error) {
    ctx.notice(
        NoteLevel::Warn,
        format!(
            "Couldn't keep {}'s command list: {err:#}",
            ctx.names.cli(cli)
        ),
    );
}

fn reported_path(ctx: &Ctx, cli: &str) -> PathBuf {
    let state = &ctx.env.state_dir;
    state
        .parent()
        .unwrap_or(state)
        .join(format!("{cli}-{REPORTED_FILE}"))
}

fn load_reported(ctx: &Ctx, cli: &str) -> BTreeMap<String, Vec<ReportedCommand>> {
    std::fs::read(reported_path(ctx, cli))
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default()
}

fn save_reported(
    ctx: &Ctx,
    cli: &str,
    kept: &BTreeMap<String, Vec<ReportedCommand>>,
) -> anyhow::Result<()> {
    let path = reported_path(ctx, cli);
    if let Some(dir) = path.parent() {
        create_private_dir_all(dir)?;
    }
    write_private_file(&path, &serde_json::to_vec_pretty(kept)?)?;
    Ok(())
}

/// A reported command as the menu lists it: sent to the session as itself, marked as the CLI's
/// own or with its plugin where its name says.
fn as_cli_command(command: &ReportedCommand) -> CliCommand {
    CliCommand {
        name: command.name.clone(),
        aliases: command.aliases.clone(),
        args: command.argument_hint.clone(),
        help: command
            .description
            .lines()
            .next()
            .unwrap_or_default()
            .to_owned(),
        kind: CommandKind::Cli,
        source: if command.builtin {
            CommandSource::BuiltIn
        } else {
            CommandSource::Plugin {
                plugin: command.plugin().unwrap_or_default().to_owned(),
            }
        },
    }
}

/// The CLI's tables, then what it loads from the project and your home, then what it reported
/// that neither knows: its own commands a newer version added, then what it reported last for
/// `folder`, such as a mod's commands, a name listed once. A discovery that fails still leaves
/// the rest.
fn command_set(ctx: &Ctx, adapter: &dyn Adapter, folder: &Path) -> CliCommandSet {
    let own = CliVersions::load(&ctx.env.paths.cli_versions()).commands(adapter.id());
    let for_folder = load_reported(ctx, adapter.id())
        .remove(folder.to_string_lossy().as_ref())
        .unwrap_or_default();
    let reported = own.iter().chain(&for_folder).map(as_cli_command).collect();
    with_reported(base_set(ctx, adapter, folder), reported)
}

/// `set` with the reported commands it doesn't know after it. Discovery knows where a command
/// of yours or the project's comes from, which the report doesn't say.
fn with_reported(set: CliCommandSet, reported: Vec<CliCommand>) -> CliCommandSet {
    let mut commands = set.commands;
    for command in reported {
        if !commands.iter().any(|known| known.name == command.name) {
            commands.push(command);
        }
    }
    CliCommandSet { commands, ..set }
}

/// The CLI's tables and what it loads for `folder` from the project and your home. A discovery
/// that fails still leaves the tables.
fn base_set(ctx: &Ctx, adapter: &dyn Adapter, folder: &Path) -> CliCommandSet {
    let builtins = || {
        CliCommandSet::new(
            AgentId(adapter.id().to_owned()),
            adapter.display_name(),
            adapter.builtin_commands(),
            Vec::new(),
        )
        .with_keywords(adapter.keywords())
    };
    let Some(home) = &ctx.env.user_home else {
        return builtins();
    };
    let scan = CliScan::new(folder, home);
    CliCommandSet::of(adapter, &scan).unwrap_or_else(|err| {
        ctx.notice(
            NoteLevel::Warn,
            format!(
                "Only {}'s built-in commands are offered: {err:#}",
                adapter.display_name()
            ),
        );
        builtins()
    })
}

/// `/<name>` as the conversation's CLI has it: sent to the conversation's session as the command
/// itself, or Argonauts Code's own version when that's what it maps to.
pub(crate) fn run(
    ctx: &mut Ctx,
    conversation: &mut ConversationState,
    state: &CliCommands,
    name: &str,
    args: &str,
) -> anyhow::Result<Vec<Effect>> {
    let found = state
        .set
        .as_ref()
        .and_then(|set| set.find(name).map(|command| (set, command)));
    let (Some(adapter), Some((set, command))) = (&state.adapter, found) else {
        let cli = ctx.names.cli(conversation.cli(ctx));
        ctx.notice(NoteLevel::Info, format!("{cli} has no command /{name}."));
        return Ok(Vec::new());
    };
    if let CommandKind::Argonauts { command: ours } = &command.kind {
        let text = format!("/{ours} {args}");
        return match parse_input(text.trim()) {
            Ok(ParsedInput::Slash { command }) => {
                Ok(vec![Effect::Input(Box::new(UiCommand::Slash { command }))])
            }
            Ok(_) => Ok(Vec::new()),
            Err(err) => {
                ctx.notice(NoteLevel::Info, err.to_string());
                Ok(Vec::new())
            }
        };
    }
    let Some(prompt) = adapter.command_prompt(command, args) else {
        let err = InputError::CliScreenOnly {
            command: name.to_owned(),
            agent_id: set.title.clone(),
        };
        ctx.notice(NoteLevel::Info, err.to_string());
        return Ok(Vec::new());
    };
    let command = command.clone();
    conversation::cli_command(ctx, conversation, &command, args, prompt)
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::Path;

    use argonauts_core::AgentEvent;
    use argonauts_core::agents::cli_commands::CommandScope;
    use argonauts_core::agents::session_runner::{Access, RunnerEvent};
    use argonauts_core::app::{ConversationEvent, TurnReply};
    use argonauts_core::input::SlashCommand;
    use argonauts_testkit::{FakeRunner, FakeSession, TempRepo};

    use super::*;
    use crate::controller::base::test_support::{CliOptions, TestDriver};

    fn write(path: &Path, text: &str) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, text).unwrap();
    }

    fn with_commands(repo: &TempRepo) {
        write(
            &repo.root().join(".claude/commands/ship.md"),
            "---\ndescription: Ship the branch\nargument-hint: [branch]\n---\nShip $ARGUMENTS\n",
        );
        write(
            &repo.user_home().join(".claude/skills/notes/SKILL.md"),
            "---\nname: notes\ndescription: Take notes\n---\n",
        );
    }

    async fn start() -> (TestDriver, CliCommandSet) {
        let mut driver = TestDriver::start_with(CliOptions {
            prepare: Some(Box::new(with_commands)),
            ..CliOptions::default()
        })
        .await;
        let event = driver
            .until(|event| matches!(event, AppEvent::CliCommands { .. }))
            .await;
        let AppEvent::CliCommands { set } = event else {
            unreachable!()
        };
        (driver, set)
    }

    fn conversation(event: &AppEvent) -> Option<&ConversationEvent> {
        match event {
            AppEvent::Conversation { event } => Some(event),
            _ => None,
        }
    }

    #[tokio::test]
    async fn the_screen_gets_the_main_agents_built_in_and_discovered_commands() {
        let (driver, set) = start().await;
        assert_eq!(set.title, "Claude Code");
        assert!(set.find("init").is_some() && set.find("voice").is_some());
        let ship = set.find("ship").expect("the project's command");
        assert_eq!(ship.args, "[branch]");
        assert!(matches!(
            ship.source,
            CommandSource::Custom {
                scope: CommandScope::Project,
                ..
            }
        ));
        assert!(set.find("notes").is_some(), "your own skill");
        driver.exit().await;
    }

    /// A workflow file named after one of the CLI's own commands never takes its place: the
    /// CLI's /init stays the CLI's. Another name of yours is a command.
    #[tokio::test]
    async fn a_workflow_never_takes_a_name_the_cli_uses() {
        let mut driver = TestDriver::start_with(CliOptions {
            prepare: Some(Box::new(|repo: &TempRepo| {
                for name in ["init", "sweep"] {
                    write(
                        &repo.root().join(format!(".argonauts/workflows/{name}.toml")),
                        &format!(
                            "name = \"{name}\"\nabout = \"Mine\"\n[[step]]\nname = \"Do\"\nrole = \"worker\"\n"
                        ),
                    );
                }
            })),
            ..CliOptions::default()
        })
        .await;
        let event = driver
            .until(|event| matches!(event, AppEvent::CliCommands { .. }))
            .await;
        let AppEvent::CliCommands { set } = event else {
            unreachable!()
        };
        let init = set.find("init").expect("the CLI's own");
        assert_ne!(init.kind, CommandKind::Workflow);
        let sweep = set.find("sweep").expect("your workflow");
        assert_eq!(sweep.kind, CommandKind::Workflow);
        driver.exit().await;
    }

    #[tokio::test]
    async fn a_command_that_runs_inside_the_cli_goes_to_the_conversation_session_as_typed() {
        let (mut driver, _) = start().await;
        // A style you set goes with answers, never with a command.
        driver
            .send(UiCommand::Slash {
                command: SlashCommand::OutputStyle {
                    style: Some("brief".into()),
                },
            })
            .await;
        driver
            .until(|event| matches!(event, AppEvent::Notice { text, .. } if text.contains("brief")))
            .await;
        driver.agent.push(FakeSession::completed("Shipped main."));
        driver.submit("/ship  main").await;
        let started = driver
            .until(|event| {
                matches!(
                    conversation(event),
                    Some(ConversationEvent::TurnStarted { .. })
                )
            })
            .await;
        let Some(ConversationEvent::TurnStarted { text, .. }) = conversation(&started) else {
            unreachable!()
        };
        assert_eq!(text, "/ship main", "the conversation shows it as typed");
        let finished = driver
            .until(|event| {
                matches!(
                    conversation(event),
                    Some(
                        ConversationEvent::TurnFinished { .. }
                            | ConversationEvent::TurnFailed { .. }
                    )
                )
            })
            .await;
        let Some(ConversationEvent::TurnFinished { reply, .. }) = conversation(&finished) else {
            panic!("{finished:?}")
        };
        assert_eq!(
            reply,
            &TurnReply::Answer {
                markdown: "Shipped main.".into()
            }
        );

        let spec = &driver.agent.specs()[0];
        let prompt: Vec<&str> = spec.prompt.iter().map(|part| part.text.as_str()).collect();
        assert_eq!(prompt, ["/ship main"], "the command is the whole prompt");
        assert_eq!(spec.json_schema, None);
        assert_eq!(spec.access, Access::ReadOnly);
        assert!(!spec.isolated, "the CLI loads its own commands and skills");
        driver.exit().await;
    }

    fn reported(name: &str, description: &str, builtin: bool) -> ReportedCommand {
        ReportedCommand {
            name: name.to_owned(),
            description: description.to_owned(),
            argument_hint: String::new(),
            aliases: Vec::new(),
            builtin,
        }
    }

    #[tokio::test]
    async fn a_mods_commands_join_the_menu_from_the_clis_own_list_and_stay() {
        let (mut driver, _) = start().await;
        let list = vec![
            reported("compact", "Compact", true),
            reported("probe-cmd", "Argonauts Code probe", false),
            reported(
                "pr-tools:review-checklist",
                "The team's review checklist",
                false,
            ),
            reported("ship", "Ship it, as the plugin does", false),
        ];
        let init = RunnerEvent::Agent(AgentEvent::CommandsChanged { commands: list });
        driver
            .agent
            .push(FakeSession::completed("Hello.").with_event(init));
        driver.submit("hi").await;
        let event = driver
            .until(|event| {
                matches!(event, AppEvent::CliCommands { set } if set.find("probe-cmd").is_some())
            })
            .await;
        let AppEvent::CliCommands { set } = event else {
            unreachable!()
        };
        let checklist = set.find("pr-tools:review-checklist").unwrap();
        assert_eq!(checklist.kind, CommandKind::Cli);
        assert_eq!(
            checklist.source,
            CommandSource::Plugin {
                plugin: "pr-tools".into()
            }
        );
        // A file of yours keeps where it comes from, which the report doesn't say, and the
        // CLI's own stay its table's.
        let ship = set.find("ship").unwrap();
        assert_eq!(ship.help, "Ship the branch");
        assert!(matches!(ship.source, CommandSource::Custom { .. }));
        assert_eq!(set.find("compact").unwrap().source, CommandSource::BuiltIn);
        let names: Vec<&str> = set.commands.iter().map(|c| c.name.as_str()).collect();
        let first_reported = names.iter().position(|n| *n == "probe-cmd").unwrap();
        let notes = names.iter().position(|n| *n == "notes").unwrap();
        assert!(
            notes < first_reported,
            "after what discovery found: {names:?}"
        );
        // Kept per folder, so the next app session's menu has them before its first turn.
        let kept = driver
            .env
            .state_dir
            .parent()
            .unwrap()
            .join("claude-code-commands.json");
        let text = fs::read_to_string(kept).unwrap();
        assert!(
            text.contains("probe-cmd") && !text.contains("\"compact\""),
            "{text}"
        );
        driver.exit().await;
    }

    /// A newer Claude Code lists `/heapdump` as its own, which the table doesn't have. It joins
    /// the menu as the CLI's own, kept for every folder where Doctor counts it.
    #[tokio::test]
    async fn a_command_a_newer_cli_lists_as_its_own_joins_the_menu() {
        let (mut driver, _) = start().await;
        let heapdump = reported("heapdump", "Dump the JS heap", true);
        let list = vec![
            reported("compact", "Compact", true),
            heapdump.clone(),
            reported("__remote-workflow", "Run the delivered workflow", true),
        ];
        let init = RunnerEvent::Agent(AgentEvent::CommandsChanged { commands: list });
        driver
            .agent
            .push(FakeSession::completed("Hello.").with_event(init));
        driver.submit("hi").await;
        let event = driver
            .until(|event| {
                matches!(event, AppEvent::CliCommands { set } if set.find("heapdump").is_some())
            })
            .await;
        let AppEvent::CliCommands { set } = event else {
            unreachable!()
        };
        let command = set.find("heapdump").unwrap();
        assert_eq!(command.kind, CommandKind::Cli);
        assert_eq!(command.source, CommandSource::BuiltIn);
        assert_eq!(command.help, "Dump the JS heap");
        assert!(
            set.find("__remote-workflow").is_none(),
            "the CLI's plumbing"
        );
        let kept = CliVersions::load(&driver.env.paths.cli_versions());
        assert_eq!(kept.commands("claude-code"), [heapdump]);
        driver.exit().await;
    }

    /// A review runs in a session of its own, maybe on another provider's CLI, so the list it
    /// reports isn't the conversation's. Filed under the conversation's CLI, Claude Code's
    /// skills would be offered to Codex and sent to it.
    #[tokio::test]
    async fn a_review_sessions_command_list_stays_out_of_the_menu() {
        let list = vec![reported("anthropic-skills:example-a", "Make a PDF", false)];
        let reviewed = |text: &str| {
            FakeSession::structured(serde_json::json!({ "answer_markdown": text })).with_event(
                RunnerEvent::Agent(AgentEvent::CommandsChanged {
                    commands: list.clone(),
                }),
            )
        };
        // Another provider's agent reviews, and with none in the crew, the main agent does,
        // in a session of its own all the same.
        for other_provider in [true, false] {
            let (mut driver, _) = start().await;
            driver.repo.write("a.txt", "a\n").unwrap();
            if other_provider {
                let codex = driver.directory.add(FakeRunner::cli("codex", "openai"));
                codex.push(reviewed("One bug."));
            } else {
                driver.agent.push(reviewed("Looks fine."));
            }
            driver
                .send(UiCommand::Slash {
                    command: SlashCommand::Review { target: None },
                })
                .await;
            driver
                .until(|event| {
                    matches!(
                        conversation(event),
                        Some(ConversationEvent::TurnFinished { .. })
                    )
                })
                .await;
            let state = driver.env.state_dir.parent().unwrap().to_path_buf();
            for cli in ["claude-code", "codex"] {
                let kept = state.join(format!("{cli}-commands.json"));
                assert!(!kept.exists(), "{other_provider}: {}", kept.display());
            }
            driver.exit().await;
        }
    }

    #[tokio::test]
    async fn a_screen_only_command_says_where_it_works_and_runs_nothing() {
        let (mut driver, _) = start().await;
        driver
            .send(UiCommand::Slash {
                command: SlashCommand::Cli {
                    name: "voice".into(),
                    args: "tap".into(),
                },
            })
            .await;
        let notice = driver
            .until(|event| matches!(event, AppEvent::Notice { .. }))
            .await;
        let AppEvent::Notice { text, .. } = notice else {
            unreachable!()
        };
        assert_eq!(text, "/voice works only in Claude Code itself");
        driver
            .send(UiCommand::Slash {
                command: SlashCommand::Cli {
                    name: "nothing-here".into(),
                    args: String::new(),
                },
            })
            .await;
        let notice = driver
            .until(|event| matches!(event, AppEvent::Notice { .. }))
            .await;
        let AppEvent::Notice { text, .. } = notice else {
            unreachable!()
        };
        assert_eq!(text, "Claude Code has no command /nothing-here.");
        assert!(driver.agent.specs().is_empty());
        driver.exit().await;
    }
}
