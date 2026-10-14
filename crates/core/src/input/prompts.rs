//! `/prompts`: who writes what each agent reads, and the templates you may change.

use serde::{Deserialize, Serialize};

use super::{InputError, SlashCommand, bad};

/// What `/prompts` does. A template goes by its file's name under `prompts/`, such as
/// `frames/reviewer` or `review/review`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "do", rename_all = "snake_case")]
pub enum PromptsAction {
    /// Lists every role's parts, who writes each, and which you changed.
    #[default]
    List,
    /// Prints a template as it is in use: yours, or the built-in.
    Show { name: String },
    /// Writes a template out to edit: into the project's `.argonauts/prompts/`, or yours for
    /// every project with `--global`.
    Copy { name: String, global: bool },
    /// Writes every template and the README, as `/setup` does.
    CopyAll { global: bool },
    /// Writes a starter `roles/<role>.md` for your own instructions to a role's agents.
    Role { role: String, global: bool },
    /// Compares your template with the built-in.
    Diff { name: String },
    /// Deletes your template, so the built-in is in use again.
    Reset { name: String },
    /// Deletes your `roles/<role>.md`, so nothing of yours is added for the role.
    ResetRole { role: String },
    /// Trusts the roles and prompts files that came with this repository, so they are used.
    Trust,
    /// Who writes instructions: for the crew, or for one role.
    Writer { role: Option<String>, who: String },
}

const USAGE: &str = "use /prompts, /prompts show|diff|reset <name>, /prompts copy <name>|all \
                     [--global], /prompts role <role> [--global], /prompts reset role <role>, \
                     /prompts trust, or /prompts writer [role] orchestrator|built-in|<model>";

pub(super) fn parse_prompts(name: &str, args: &str) -> Result<SlashCommand, InputError> {
    let words: Vec<&str> = args.split_whitespace().collect();
    let named = |word: &str| -> Result<String, InputError> {
        // A name becomes a path under `prompts/`, so it must not reach outside it.
        let word = word.strip_suffix(".md").unwrap_or(word);
        let safe = !word.is_empty()
            && word.split('/').all(|piece| {
                !piece.is_empty()
                    && piece
                        .chars()
                        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
            });
        if safe {
            Ok(word.to_owned())
        } else {
            Err(bad(name, format!("{word:?} isn't a template's name")))
        }
    };
    let action = match words.as_slice() {
        [] => PromptsAction::List,
        ["show", template] => PromptsAction::Show {
            name: named(template)?,
        },
        ["diff", template] => PromptsAction::Diff {
            name: named(template)?,
        },
        ["reset", template] => PromptsAction::Reset {
            name: named(template)?,
        },
        ["reset", "role", role] => PromptsAction::ResetRole {
            role: (*role).to_owned(),
        },
        ["trust"] => PromptsAction::Trust,
        ["copy", "all"] => PromptsAction::CopyAll { global: false },
        ["copy", "all", "--global"] => PromptsAction::CopyAll { global: true },
        ["copy", template] => PromptsAction::Copy {
            name: named(template)?,
            global: false,
        },
        ["copy", template, "--global"] => PromptsAction::Copy {
            name: named(template)?,
            global: true,
        },
        ["role", role] => PromptsAction::Role {
            role: (*role).to_owned(),
            global: false,
        },
        ["role", role, "--global"] => PromptsAction::Role {
            role: (*role).to_owned(),
            global: true,
        },
        ["writer", who] => PromptsAction::Writer {
            role: None,
            who: (*who).to_owned(),
        },
        ["writer", role, who] => PromptsAction::Writer {
            role: Some((*role).to_owned()),
            who: (*who).to_owned(),
        },
        _ => return Err(bad(name, USAGE)),
    };
    Ok(SlashCommand::Prompts { action })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn action(args: &str) -> PromptsAction {
        match parse_prompts("prompts", args) {
            Ok(SlashCommand::Prompts { action }) => action,
            other => panic!("{args:?}: {other:?}"),
        }
    }

    #[test]
    fn each_form_parses() {
        assert_eq!(action(""), PromptsAction::List);
        assert_eq!(
            action("show review/review"),
            PromptsAction::Show {
                name: "review/review".into()
            }
        );
        assert_eq!(
            action("copy frames/reviewer --global"),
            PromptsAction::Copy {
                name: "frames/reviewer".into(),
                global: true
            }
        );
        assert_eq!(action("copy all"), PromptsAction::CopyAll { global: false });
        assert_eq!(action("trust"), PromptsAction::Trust);
        assert_eq!(
            action("role reviewer"),
            PromptsAction::Role {
                role: "reviewer".into(),
                global: false
            }
        );
        assert_eq!(
            action("reset plan/draft"),
            PromptsAction::Reset {
                name: "plan/draft".into()
            }
        );
        assert_eq!(
            action("reset role reviewer"),
            PromptsAction::ResetRole {
                role: "reviewer".into()
            }
        );
        assert_eq!(
            action("writer built-in"),
            PromptsAction::Writer {
                role: None,
                who: "built-in".into()
            }
        );
        assert_eq!(
            action("writer judge codex/gpt-5.6"),
            PromptsAction::Writer {
                role: Some("judge".into()),
                who: "codex/gpt-5.6".into()
            }
        );
    }

    #[test]
    fn a_name_cant_reach_outside_the_prompts_folder() {
        for args in ["show ../config", "copy /etc/passwd", "reset a//b", "show"] {
            assert!(parse_prompts("prompts", args).is_err(), "{args}");
        }
    }
}
