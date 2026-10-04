//! The conversation's CLI's own commands and keywords: which ones the screen offers, and
//! sending one to the chat. They're the main agent's, or those of the agent whose session the
//! conversation carries on, and they're looked up again whenever that CLI or its folder changes.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use argonauts_core::Adapter;
use argonauts_core::NoteLevel;
use argonauts_core::app::{AppEvent, UiCommand};
use argonauts_core::input::{InputError, ParsedInput, parse_input};
use argonauts_core::names::VendorId;
use argonauts_core::vendor_commands::{CommandKind, VendorCommandSet};
use argonauts_core::vendor_sessions::VendorScan;

use super::chat::{self, ChatState};
use super::{Ctx, Effect};

#[derive(Default)]
pub(crate) struct VendorCommands {
    adapter: Option<Arc<dyn Adapter>>,
    set: Option<VendorCommandSet>,
    /// The CLI they belong to and the folder they were found for.
    found_for: Option<(String, PathBuf)>,
}

impl VendorCommands {
    pub(crate) fn set(&self) -> Option<&VendorCommandSet> {
        self.set.as_ref()
    }
}

/// Looks the conversation's CLI's commands up again, and tells the screen when they changed. An
/// agent with no CLI of its own, such as an endpoint, has none.
pub(crate) fn refresh(ctx: &Ctx, state: &mut VendorCommands, chat: &ChatState) {
    let cli = chat.cli(ctx).to_owned();
    let folder = chat.folder(ctx).to_path_buf();
    let adapter = match crate::commands::adapters_here(Some(&ctx.env), ctx.settings.resolved()) {
        Ok(adapters) => adapters.into_iter().find(|adapter| adapter.id() == cli),
        Err(err) => {
            ctx.notice(
                NoteLevel::Warn,
                format!(
                    "Couldn't list {}'s own commands: {err:#}",
                    ctx.names.harness(&cli)
                ),
            );
            None
        }
    };
    let set = match &adapter {
        Some(adapter) => command_set(ctx, adapter.as_ref(), &folder),
        None => VendorCommandSet::new(VendorId(cli.clone()), cli.clone(), &[], Vec::new()),
    };
    if state.set.as_ref() != Some(&set) {
        ctx.send(AppEvent::VendorCommands { set: set.clone() });
    }
    state.adapter = adapter;
    state.set = Some(set);
    state.found_for = Some((cli, folder));
}

/// Looks them up again once the conversation goes to another CLI or works in another folder, as
/// when it carries on an agent's session from a run.
pub(crate) fn follow(ctx: &Ctx, state: &mut VendorCommands, chat: &ChatState) {
    let current = state
        .found_for
        .as_ref()
        .is_some_and(|(cli, folder)| cli == chat.cli(ctx) && folder == chat.folder(ctx));
    if !current {
        refresh(ctx, state, chat);
    }
}

/// The CLI's tables and what it loads for `folder` from the project and your home. A discovery
/// that fails still leaves the tables.
fn command_set(ctx: &Ctx, adapter: &dyn Adapter, folder: &Path) -> VendorCommandSet {
    let builtins = || {
        VendorCommandSet::new(
            VendorId(adapter.id().to_owned()),
            adapter.display_name(),
            adapter.builtin_commands(),
            Vec::new(),
        )
        .with_keywords(adapter.keywords())
    };
    let Some(home) = &ctx.env.user_home else {
        return builtins();
    };
    let scan = VendorScan::new(folder, home);
    VendorCommandSet::of(adapter, &scan).unwrap_or_else(|err| {
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

/// `/<name>` as the conversation's CLI has it: sent to the chat's session as the command
/// itself, or Argonauts' own version when that's what it maps to.
pub(crate) fn run(
    ctx: &mut Ctx,
    chat: &mut ChatState,
    state: &VendorCommands,
    name: &str,
    args: &str,
) -> anyhow::Result<Vec<Effect>> {
    let found = state
        .set
        .as_ref()
        .and_then(|set| set.find(name).map(|command| (set, command)));
    let (Some(adapter), Some((set, command))) = (&state.adapter, found) else {
        let cli = ctx.names.harness(chat.cli(ctx));
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
        let err = InputError::VendorScreenOnly {
            command: name.to_owned(),
            vendor: set.title.clone(),
        };
        ctx.notice(NoteLevel::Info, err.to_string());
        return Ok(Vec::new());
    };
    let command = command.clone();
    chat::vendor_command(ctx, chat, &command, args, prompt)
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::Path;

    use argonauts_core::app::{ChatEvent, TurnReply};
    use argonauts_core::input::SlashCommand;
    use argonauts_core::vendor_commands::{CommandScope, CommandSource};
    use argonauts_core::worker::Access;
    use argonauts_testkit::{FakeSession, TempRepo};

    use super::*;
    use crate::controller::test_support::{Harness, HarnessOptions};

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

    async fn start() -> (Harness, VendorCommandSet) {
        let mut harness = Harness::start_with(HarnessOptions {
            prepare: Some(Box::new(with_commands)),
            ..HarnessOptions::default()
        })
        .await;
        let event = harness
            .until(|event| matches!(event, AppEvent::VendorCommands { .. }))
            .await;
        let AppEvent::VendorCommands { set } = event else {
            unreachable!()
        };
        (harness, set)
    }

    fn chat(event: &AppEvent) -> Option<&ChatEvent> {
        match event {
            AppEvent::Chat { event } => Some(event),
            _ => None,
        }
    }

    #[tokio::test]
    async fn the_screen_gets_the_main_agents_built_in_and_discovered_commands() {
        let (harness, set) = start().await;
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
        harness.exit().await;
    }

    #[tokio::test]
    async fn a_command_that_runs_inside_the_cli_goes_to_the_chat_session_as_typed() {
        let (mut harness, _) = start().await;
        // A style you set goes with answers, never with a command.
        harness
            .send(UiCommand::Slash {
                command: SlashCommand::OutputStyle {
                    style: Some("brief".into()),
                },
            })
            .await;
        harness
            .until(|event| matches!(event, AppEvent::Notice { text, .. } if text.contains("brief")))
            .await;
        harness.agent.push(FakeSession::completed("Shipped main."));
        harness.submit("/ship  main").await;
        let started = harness
            .until(|event| matches!(chat(event), Some(ChatEvent::TurnStarted { .. })))
            .await;
        let Some(ChatEvent::TurnStarted { text, .. }) = chat(&started) else {
            unreachable!()
        };
        assert_eq!(text, "/ship main", "the chat shows it as typed");
        let finished = harness
            .until(|event| {
                matches!(
                    chat(event),
                    Some(ChatEvent::TurnFinished { .. } | ChatEvent::TurnFailed { .. })
                )
            })
            .await;
        let Some(ChatEvent::TurnFinished { reply, .. }) = chat(&finished) else {
            panic!("{finished:?}")
        };
        assert_eq!(
            reply,
            &TurnReply::Answer {
                markdown: "Shipped main.".into()
            }
        );

        let spec = &harness.agent.specs()[0];
        let prompt: Vec<&str> = spec.prompt.iter().map(|part| part.text.as_str()).collect();
        assert_eq!(prompt, ["/ship main"], "the command is the whole prompt");
        assert_eq!(spec.json_schema, None);
        assert_eq!(spec.access, Access::ReadOnly);
        assert!(!spec.isolated, "the CLI loads its own commands and skills");
        harness.exit().await;
    }

    #[tokio::test]
    async fn a_screen_only_command_says_where_it_works_and_runs_nothing() {
        let (mut harness, _) = start().await;
        harness
            .send(UiCommand::Slash {
                command: SlashCommand::Vendor {
                    name: "voice".into(),
                    args: "tap".into(),
                },
            })
            .await;
        let notice = harness
            .until(|event| matches!(event, AppEvent::Notice { .. }))
            .await;
        let AppEvent::Notice { text, .. } = notice else {
            unreachable!()
        };
        assert_eq!(text, "/voice works only in Claude Code itself");
        harness
            .send(UiCommand::Slash {
                command: SlashCommand::Vendor {
                    name: "nothing-here".into(),
                    args: String::new(),
                },
            })
            .await;
        let notice = harness
            .until(|event| matches!(event, AppEvent::Notice { .. }))
            .await;
        let AppEvent::Notice { text, .. } = notice else {
            unreachable!()
        };
        assert_eq!(text, "Claude Code has no command /nothing-here.");
        assert!(harness.agent.specs().is_empty());
        harness.exit().await;
    }
}
