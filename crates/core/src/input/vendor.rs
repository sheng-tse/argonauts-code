//! Parsing with the main agent CLI's own commands added to Argonauts' own.

use super::{InputError, ParsedInput, SlashCommand, parse_input, parse_slash, split_word};
use crate::vendor_commands::{CommandKind, VendorCommandSet};

/// [`parse_input`] with the main agent's commands added, Argonauts' own names first. A CLI's
/// name for an Argonauts command parses as that command, one that runs inside the CLI becomes
/// [`SlashCommand::Vendor`], and one only the CLI's own screen runs is
/// [`InputError::VendorScreenOnly`]. An Argonauts command given arguments it doesn't take goes
/// to the CLI's command of that name, when the CLI has one that does, such as `/doctor
/// prompt-audit` to Claude Code's `/checkup`.
pub fn parse_input_with(
    text: &str,
    commands: Option<&VendorCommandSet>,
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
                    command: SlashCommand::Vendor {
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
        CommandKind::Vendor => SlashCommand::Vendor {
            name: found.name.clone(),
            args: args.to_owned(),
        },
        CommandKind::Workflow if args.trim().is_empty() => {
            return Err(InputError::BadArguments {
                command: typed.clone(),
                problem: format!("needs a request: /{typed} <request>"),
            });
        }
        CommandKind::Workflow => SlashCommand::WorkflowRequest {
            workflow: found.name.clone(),
            request: args.trim().to_owned(),
        },
        CommandKind::ScreenOnly => {
            return Err(InputError::VendorScreenOnly {
                command: typed.clone(),
                vendor: set.title.clone(),
            });
        }
    };
    Ok(ParsedInput::Slash { command })
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use crate::input::ViewAction;
    use crate::names::VendorId;
    use crate::vendor_commands::{
        BuiltinCommand, BuiltinKind, CommandScope, CommandSource, VendorCommand,
    };

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
            kind: BuiltinKind::Vendor,
        },
        BuiltinCommand {
            name: "voice",
            aliases: &[],
            args: "[hold|tap|off]",
            help: "Talk instead of typing",
            kind: BuiltinKind::ScreenOnly,
        },
    ];

    fn set() -> VendorCommandSet {
        let skill = VendorCommand {
            name: "anthropic-skills:example-a".into(),
            aliases: Vec::new(),
            args: "[file]".into(),
            help: "Work with PDFs".into(),
            kind: CommandKind::Vendor,
            source: CommandSource::Skill {
                scope: CommandScope::User,
                path: PathBuf::from("/home/.claude/skills/pdf/SKILL.md"),
            },
        };
        VendorCommandSet::new(
            VendorId("claude-code".into()),
            "Claude Code",
            CLAUDE,
            vec![skill],
        )
    }

    fn slash(command: SlashCommand) -> Result<ParsedInput, InputError> {
        Ok(ParsedInput::Slash { command })
    }

    #[test]
    fn a_vendor_name_for_an_argonauts_command_runs_argonauts_version() {
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
    fn vendor_commands_go_to_the_session_or_back_to_its_own_screen() {
        let set = set();
        assert_eq!(
            parse_input_with("/simplify  the parser ", Some(&set)),
            slash(SlashCommand::Vendor {
                name: "simplify".into(),
                args: "the parser".into()
            })
        );
        assert_eq!(
            parse_input_with("/anthropic-skills:example-a", Some(&set)),
            slash(SlashCommand::Vendor {
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
    fn a_workflow_of_yours_is_a_command_that_runs_one_request_with_it() {
        let mut set = set();
        set.commands.insert(
            0,
            VendorCommand {
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
            Err(InputError::BadArguments {
                command: "spec-first".into(),
                problem: "needs a request: /spec-first <request>".into()
            })
        );
        // `/workflow` itself is Argonauts' own, whatever your workflows are called.
        assert_eq!(
            parse_input_with("/workflow spec-first", Some(&set)),
            slash(SlashCommand::Workflow {
                name: Some("spec-first".into()),
                action: crate::input::PresetAction::Use
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
