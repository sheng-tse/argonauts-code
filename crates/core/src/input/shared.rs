//! Argonauts Code's versions of the commands the agent CLIs share, such as `/compact` and
//! `/rename`. They work whatever CLI the main agent is, so a CLI's own name for one runs it.

use std::path::PathBuf;

use super::{CommandCategory, Handler, InputError, SlashCommand, SlashSpec, bad, optional_word};

/// In [`CommandCategory`] order, like [`super::SLASH_COMMANDS`].
#[rustfmt::skip]
pub const SHARED_COMMANDS: &[SlashSpec] = &[
    SlashSpec { name: "compact", args: "[instructions]", help: "Summarize the conversation to free context", handler: Handler::App, category: CommandCategory::Session },
    SlashSpec { name: "rename", args: "[name]", help: "Rename the conversation", handler: Handler::App, category: CommandCategory::Session },
    SlashSpec { name: "branch", args: "[name]", help: "Start a new conversation from here and keep this one", handler: Handler::App, category: CommandCategory::Session },
    SlashSpec { name: "fork", args: "[prompt]", help: "Copy the conversation into a new background session", handler: Handler::App, category: CommandCategory::Session },
    SlashSpec { name: "btw", args: "[question]", help: "Ask a side question that stays out of the conversation", handler: Handler::App, category: CommandCategory::Session },
    SlashSpec { name: "recap", args: "", help: "One line on where the conversation stands", handler: Handler::App, category: CommandCategory::Session },
    SlashSpec { name: "copy", args: "[n]", help: "Copy the last answer, or the nth from last", handler: Handler::Ui, category: CommandCategory::Session },
    SlashSpec { name: "export", args: "[file]", help: "Save the conversation as plain text", handler: Handler::App, category: CommandCategory::Session },
    SlashSpec { name: "cd", args: "[path]", help: "Work in another folder. No path shows the current one", handler: Handler::App, category: CommandCategory::Session },
    SlashSpec { name: "add-dir", args: "<path>", help: "Let the agents work in one more folder", handler: Handler::App, category: CommandCategory::Session },
    SlashSpec { name: "plan", args: "[task]", help: "Plan first, and change nothing until you agree", handler: Handler::App, category: CommandCategory::Work },
    SlashSpec { name: "goal", args: "[condition|show|pause|resume|clear]", help: "Keep going until a condition holds, on any CLI and in any workflow. No condition shows the goal", handler: Handler::App, category: CommandCategory::Work },
    SlashSpec { name: "review", args: "[target]", help: "Review the changes: the working tree, a branch, or a pull request", handler: Handler::App, category: CommandCategory::Work },
    SlashSpec { name: "security-review", args: "[target]", help: "Review the changes for security problems", handler: Handler::App, category: CommandCategory::Work },
    SlashSpec { name: "diff", args: "", help: "The changes in the working tree", handler: Handler::App, category: CommandCategory::Work },
    SlashSpec { name: "fast", args: "[on|off]", help: "Faster answers, where the agent offers it", handler: Handler::App, category: CommandCategory::Crew },
    SlashSpec { name: "usage", args: "[day|week|month|all] [print|pin]", help: "Spend and plan usage for each agent", handler: Handler::App, category: CommandCategory::ApprovalsAndLimits },
    SlashSpec { name: "output-style", args: "[style]", help: "How answers are written", handler: Handler::App, category: CommandCategory::Display },
    SlashSpec { name: "login", args: "[agent]", help: "Log in to an agent CLI", handler: Handler::App, category: CommandCategory::SetupAndHelp },
    SlashSpec { name: "logout", args: "[agent]", help: "Log out of an agent CLI", handler: Handler::App, category: CommandCategory::SetupAndHelp },
    SlashSpec { name: "release-notes", args: "", help: "What's new in Argonauts Code", handler: Handler::App, category: CommandCategory::SetupAndHelp },
    SlashSpec { name: "feedback", args: "[text]", help: "Send feedback about Argonauts Code", handler: Handler::App, category: CommandCategory::SetupAndHelp },
];

/// `None` when `name` isn't one of them.
pub(super) fn parse(name: &str, args: &str) -> Option<Result<SlashCommand, InputError>> {
    let text = || (!args.is_empty()).then(|| args.to_owned());
    let path = || (!args.is_empty()).then(|| PathBuf::from(args));
    let no_args = |command: SlashCommand| {
        if args.is_empty() {
            Ok(command)
        } else {
            Err(bad(name, "takes no arguments"))
        }
    };
    Some(match name {
        "compact" => Ok(SlashCommand::Compact {
            instructions: text(),
        }),
        "rename" => Ok(SlashCommand::Rename { name: text() }),
        "branch" => Ok(SlashCommand::Branch { name: text() }),
        "fork" => Ok(SlashCommand::Fork { prompt: text() }),
        "btw" => Ok(SlashCommand::Btw { question: text() }),
        "recap" => no_args(SlashCommand::Recap),
        "copy" => parse_copy(name, args),
        "export" => Ok(SlashCommand::Export { file: path() }),
        "cd" => Ok(SlashCommand::Cd { path: path() }),
        "add-dir" => match path() {
            Some(path) => Ok(SlashCommand::AddDir { path }),
            None => Err(bad(name, "needs a folder")),
        },
        "plan" => Ok(SlashCommand::Plan { task: text() }),
        "goal" => Ok(SlashCommand::Goal { text: text() }),
        "review" => Ok(SlashCommand::Review { target: text() }),
        "security-review" => Ok(SlashCommand::SecurityReview { target: text() }),
        "diff" => no_args(SlashCommand::Diff),
        "fast" => parse_fast(name, args),
        "usage" => super::mods::parse_usage(name, args),
        "output-style" => Ok(SlashCommand::OutputStyle { style: text() }),
        "login" => word(name, args, "/login [agent]").map(|agent| SlashCommand::Login { agent }),
        "logout" => word(name, args, "/logout [agent]").map(|agent| SlashCommand::Logout { agent }),
        "release-notes" => no_args(SlashCommand::ReleaseNotes),
        "feedback" => Ok(SlashCommand::Feedback { text: text() }),
        _ => return None,
    })
}

fn word(name: &str, args: &str, usage: &str) -> Result<Option<String>, InputError> {
    optional_word(name, args, usage).map(|word| word.map(str::to_owned))
}

fn parse_copy(name: &str, args: &str) -> Result<SlashCommand, InputError> {
    let nth = match optional_word(name, args, "/copy [n]")? {
        None => None,
        Some(word) => match word.parse::<u32>() {
            Ok(n) if n > 0 => Some(n),
            _ => return Err(bad(name, "use /copy [n], where 1 is the last answer")),
        },
    };
    Ok(SlashCommand::Copy { nth })
}

fn parse_fast(name: &str, args: &str) -> Result<SlashCommand, InputError> {
    let on = match optional_word(name, args, "/fast [on|off]")? {
        None => None,
        Some("on") => Some(true),
        Some("off") => Some(false),
        Some(_) => return Err(bad(name, "use /fast [on|off]")),
    };
    Ok(SlashCommand::Fast { on })
}

#[cfg(test)]
mod tests {
    use super::super::{ParsedInput, SLASH_COMMANDS, parse_input, slash_spec};
    use super::*;

    fn parsed(text: &str) -> Result<SlashCommand, InputError> {
        match parse_input(text)? {
            ParsedInput::Slash { command } => Ok(command),
            other => panic!("{text} parsed as {other:?}"),
        }
    }

    #[test]
    fn every_shared_command_parses_bare_or_with_its_argument() {
        for spec in SHARED_COMMANDS {
            let args = if spec.needs_argument() { " ./docs" } else { "" };
            let command = parsed(&format!("/{}{args}", spec.name));
            assert!(command.is_ok(), "/{}: {command:?}", spec.name);
            assert_eq!(slash_spec(spec.name), Some(spec));
        }
        let mut names: Vec<&str> = SLASH_COMMANDS
            .iter()
            .chain(SHARED_COMMANDS)
            .map(|spec| spec.name)
            .collect();
        let all = names.len();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), all, "no name is in both tables");
        assert!(
            SHARED_COMMANDS
                .windows(2)
                .all(|pair| pair[0].category <= pair[1].category)
        );
    }

    #[test]
    fn arguments_are_read_and_checked() {
        assert_eq!(
            parsed("/compact keep the test plan"),
            Ok(SlashCommand::Compact {
                instructions: Some("keep the test plan".into())
            })
        );
        assert_eq!(parsed("/copy"), Ok(SlashCommand::Copy { nth: None }));
        assert_eq!(parsed("/copy 2"), Ok(SlashCommand::Copy { nth: Some(2) }));
        assert!(parsed("/copy 0").is_err());
        assert!(parsed("/copy two").is_err());
        assert_eq!(
            parsed("/fast off"),
            Ok(SlashCommand::Fast { on: Some(false) })
        );
        assert!(parsed("/fast maybe").is_err());
        assert_eq!(
            parsed("/cd ~/code/my app"),
            Ok(SlashCommand::Cd {
                path: Some(PathBuf::from("~/code/my app"))
            })
        );
        assert!(parsed("/add-dir").is_err());
        assert!(parsed("/diff HEAD").is_err());
        assert_eq!(
            parsed("/login codex"),
            Ok(SlashCommand::Login {
                agent: Some("codex".into())
            })
        );
        assert!(parsed("/usage daily extra").is_err());
    }
}
