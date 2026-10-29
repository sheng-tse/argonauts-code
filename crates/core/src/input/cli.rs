//! Parsing with the commands of the main agent's CLI added to Argonauts Code's own.

use super::scope_flag::scope_of_flag;
use super::{
    InputError, ParsedInput, SlashCommand, WorkflowVerb, bad, parse_input, parse_slash, split_word,
};
use crate::cli_commands::{CliCommandSet, CommandKind};

/// [`parse_input`] with the main agent's commands added, Argonauts Code's own names first. A CLI's
/// name for an Argonauts Code command parses as that command, one that runs inside the CLI becomes
/// [`SlashCommand::Agent`], and one only the CLI's own screen runs is
/// [`InputError::AgentScreenOnly`]. An Argonauts Code command given arguments it doesn't take goes
/// to the CLI's command of that name, when the CLI has one that does, such as `/doctor
/// prompt-audit` to Claude Code's `/checkup`.
pub fn parse_input_with(
    text: &str,
    commands: Option<&CliCommandSet>,
) -> Result<ParsedInput, InputError> {
    let parsed = parse_input(text);
    let Some(set) = commands else {
        return parsed;
    };
    let rest = text.trim().strip_prefix('/').unwrap_or_default();
    let (_, args) = split_word(rest);
    let typed = match &parsed {
        Err(InputError::UnknownCommand(typed)) => typed,
        Err(InputError::BadArguments { command, .. }) => {
            return match set.runs_inside_as(command) {
                Some(found) => Ok(ParsedInput::Slash {
                    command: SlashCommand::Cli {
                        name: found.name.clone(),
                        args: args.to_owned(),
                    },
                }),
                None => parsed,
            };
        }
        _ => return parsed,
    };
    let Some(found) = set.find(typed) else {
        return parsed;
    };
    let command = match &found.kind {
        CommandKind::Argonauts { command } => {
            parse_slash(command, args).map_err(|err| match err {
                InputError::BadArguments { problem, .. } => InputError::BadArguments {
                    command: typed.clone(),
                    problem,
                },
                other => other,
            })?
        }
        CommandKind::Cli => SlashCommand::Cli {
            name: found.name.clone(),
            args: args.to_owned(),
        },
        // Bare, it switches to the workflow, as `/workflow <name>` does.
        CommandKind::Workflow if args.trim().is_empty() => SlashCommand::Workflow {
            name: Some(found.name.clone()),
            action: WorkflowVerb::Use,
        },
        // A place flag alone switches to it there, as `/workflow <name> --session` does.
        CommandKind::Workflow if only_a_flag(args) => match scope_of_flag(args.trim()) {
            Some(scope) => SlashCommand::SavedIn {
                scope,
                change: Box::new(SlashCommand::Workflow {
                    name: Some(found.name.clone()),
                    action: WorkflowVerb::Use,
                }),
            },
            None => return Err(not_a_request(&found.name, args)),
        },
        CommandKind::Workflow => SlashCommand::WorkflowRequest {
            workflow: found.name.clone(),
            request: args.trim().to_owned(),
        },
        CommandKind::ScreenOnly => {
            return Err(InputError::CliScreenOnly {
                command: typed.clone(),
                agent_id: set.title.clone(),
            });
        }
    };
    Ok(ParsedInput::Slash { command })
}

/// Whether a workflow command's words are one flag, such as `--session`, and no request. A run
/// with a flag for its request would spend every seat's turn on nothing.
pub(super) fn only_a_flag(args: &str) -> bool {
    let mut words = args.split_whitespace();
    matches!((words.next(), words.next()), (Some(word), None) if word.starts_with("--"))
}

/// Why `/<workflow> <flag>` runs nothing.
pub(super) fn not_a_request(workflow: &str, args: &str) -> InputError {
    bad(
        workflow,
        format!(
            "{} isn't a request; type one after /{workflow}, or /{workflow} alone to switch to it",
            args.trim()
        ),
    )
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use crate::cli_commands::{
        BuiltinCommand, BuiltinKind, CliCommand, CommandScope, CommandSource,
    };
    use crate::input::ViewAction;
    use crate::names::AgentId;

    const CLAUDE: &[BuiltinCommand] = &[
        BuiltinCommand {
            name: "clear",
            aliases: &["reset", "new"],
            args: "",
            help: "New conversation",
            kind: BuiltinKind::Argonauts("clear"),
        },
        BuiltinCommand {
            name: "usage",
            aliases: &["cost", "stats"],
            args: "",
            help: "Plan usage",
            kind: BuiltinKind::Argonauts("usage"),
        },
        BuiltinCommand {
            name: "simplify",
            aliases: &[],
            args: "[focus]",
            help: "Simplify the changed code",
            kind: BuiltinKind::Cli,
        },
        BuiltinCommand {
            name: "voice",
            aliases: &[],
            args: "[hold|tap|off]",
            help: "Talk instead of typing",
            kind: BuiltinKind::ScreenOnly,
        },
    ];

    fn set() -> CliCommandSet {
        let skill = CliCommand {
            name: "anthropic-skills:example-a".into(),
            aliases: Vec::new(),
            args: "[file]".into(),
            help: "Work with PDFs".into(),
            kind: CommandKind::Cli,
            source: CommandSource::Skill {
                scope: CommandScope::User,
                path: PathBuf::from("/home/.claude/skills/pdf/SKILL.md"),
            },
        };
        CliCommandSet::new(
            AgentId("claude-code".into()),
            "Claude Code",
            CLAUDE,
            vec![skill],
        )
    }

    fn slash(command: SlashCommand) -> Result<ParsedInput, InputError> {
        Ok(ParsedInput::Slash { command })
    }

    #[test]
    fn an_agent_name_for_an_argonauts_command_runs_argonauts_version() {
        let set = set();
        assert_eq!(
            parse_input_with("/reset", Some(&set)),
            slash(SlashCommand::Clear)
        );
        assert_eq!(
            parse_input_with("/cost weekly", Some(&set)),
            slash(SlashCommand::Usage {
                period: Some("weekly".into()),
                action: ViewAction::Open
            })
        );
        assert_eq!(
            parse_input_with("/reset now", Some(&set)),
            Err(InputError::BadArguments {
                command: "reset".into(),
                problem: "takes no arguments".into()
            }),
            "the error names what you typed"
        );
    }

    #[test]
    fn cli_commands_go_to_the_session_or_back_to_its_own_screen() {
        let set = set();
        assert_eq!(
            parse_input_with("/simplify  the parser ", Some(&set)),
            slash(SlashCommand::Cli {
                name: "simplify".into(),
                args: "the parser".into()
            })
        );
        assert_eq!(
            parse_input_with("/anthropic-skills:example-a", Some(&set)),
            slash(SlashCommand::Cli {
                name: "anthropic-skills:example-a".into(),
                args: String::new()
            })
        );
        let screen = parse_input_with("/voice tap", Some(&set)).unwrap_err();
        assert_eq!(
            screen.to_string(),
            "/voice works only in Claude Code itself"
        );
    }

    #[test]
    fn a_workflow_of_yours_is_a_command_that_switches_bare_and_runs_one_request_with_words() {
        let mut set = set();
        set.commands.insert(
            0,
            CliCommand {
                name: "spec-first".into(),
                aliases: Vec::new(),
                args: "<request>".into(),
                help: "Write the spec first".into(),
                kind: CommandKind::Workflow,
                source: CommandSource::Custom {
                    scope: CommandScope::Project,
                    path: PathBuf::from("/repo/.argonauts/workflows/spec-first.toml"),
                },
            },
        );
        assert_eq!(
            parse_input_with("/spec-first add  due dates ", Some(&set)),
            slash(SlashCommand::WorkflowRequest {
                workflow: "spec-first".into(),
                request: "add  due dates".into()
            })
        );
        assert_eq!(
            parse_input_with("/spec-first", Some(&set)),
            slash(SlashCommand::Workflow {
                name: Some("spec-first".into()),
                action: crate::input::WorkflowVerb::Use
            })
        );
        // A place flag alone switches to it there; any other flag alone is no request, so
        // nothing runs on it.
        assert_eq!(
            parse_input_with("/spec-first --session", Some(&set)),
            slash(SlashCommand::SavedIn {
                scope: crate::settings::SettingScope::Session,
                change: Box::new(SlashCommand::Workflow {
                    name: Some("spec-first".into()),
                    action: crate::input::WorkflowVerb::Use
                }),
            })
        );
        let Err(err) = parse_input_with("/spec-first --verbose", Some(&set)) else {
            panic!("a flag ran as a request");
        };
        assert_eq!(
            err.to_string(),
            "/spec-first: --verbose isn't a request; type one after /spec-first, or /spec-first \
             alone to switch to it"
        );
        // `/workflow` itself is Argonauts Code's own, whatever your workflows are called.
        assert_eq!(
            parse_input_with("/workflow spec-first", Some(&set)),
            slash(SlashCommand::Workflow {
                name: Some("spec-first".into()),
                action: crate::input::WorkflowVerb::Use
            })
        );
    }

    #[test]
    fn argonauts_names_come_first_and_unknown_stays_unknown() {
        let set = set();
        assert_eq!(
            parse_input_with("/status", Some(&set)),
            slash(SlashCommand::Status {
                action: ViewAction::Open
            })
        );
        assert_eq!(
            parse_input_with("/nothing", Some(&set)),
            Err(InputError::UnknownCommand("nothing".into()))
        );
        assert_eq!(
            parse_input_with("/reset", None),
            Err(InputError::UnknownCommand("reset".into()))
        );
        assert!(matches!(
            parse_input_with("hello", Some(&set)),
            Ok(ParsedInput::Plain { .. })
        ));
    }
}
