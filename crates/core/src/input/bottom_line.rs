//! `/bottom-line`: which CLIs show their plan meters under the input, and in what order. Bare,
//! it opens a picker with the line as a live preview; `add`, `remove` and `reset` change it at
//! once. A flag saves the change elsewhere than your changes go.

use serde::{Deserialize, Serialize};

use super::{InputError, SlashCommand, bad};
use crate::settings::SettingScope;
use crate::settings::bottom_line::LineCli;

/// What `/bottom-line` does to the line's CLIs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "do", rename_all = "snake_case")]
pub enum LineChange {
    /// The picker, which the screen opens.
    Open,
    /// Shows a CLI, by its id or its name, such as `agy` or `Claude Code`, after the others.
    Add {
        cli: String,
    },
    Remove {
        cli: String,
    },
    /// Back to the CLI in use and the one used most.
    Reset,
    /// What the picker saves: the CLIs in order, each with its own windows, and whether a CLI
    /// that holds up a running task joins them.
    Set {
        clis: Vec<LineCli>,
        holding_up: bool,
    },
}

const FLAGS: &str = "--global, --project or --session";

const USAGE: &str = "use /bottom-line, or add, remove or reset, such as /bottom-line add agy";

pub(super) fn parse_bottom_line(name: &str, args: &str) -> Result<SlashCommand, InputError> {
    let mut words: Vec<&str> = args.split_whitespace().collect();
    let scope = take_scope(name, &mut words)?;
    let cli = |rest: &[&str]| -> Result<String, InputError> {
        match rest {
            [] => Err(bad(name, "name the CLI, such as /bottom-line add agy")),
            words => Ok(words.join(" ")),
        }
    };
    let change = match words.as_slice() {
        [] => LineChange::Open,
        ["add", rest @ ..] => LineChange::Add { cli: cli(rest)? },
        ["remove", rest @ ..] => LineChange::Remove { cli: cli(rest)? },
        ["reset"] => LineChange::Reset,
        _ => return Err(bad(name, USAGE)),
    };
    Ok(SlashCommand::BottomLine { change, scope })
}

/// Takes `--global`, `--project` or `--session` out of `words`. The line's files sit under
/// `~/.argonauts/` and a repository's `.argonauts/`, with no copy for you alone in a project.
fn take_scope(name: &str, words: &mut Vec<&str>) -> Result<Option<SettingScope>, InputError> {
    let mut scope = None;
    let mut rest = Vec::with_capacity(words.len());
    for word in words.drain(..) {
        if !word.starts_with("--") {
            rest.push(word);
            continue;
        }
        let picked = match word {
            "--session" => SettingScope::Session,
            "--local" => {
                return Err(bad(
                    name,
                    format!("the line has no file for you alone in a project; use {FLAGS}"),
                ));
            }
            flag => super::scope_flag::scope_of_flag(flag)
                .ok_or_else(|| bad(name, format!("{flag} isn't a flag; use {FLAGS}")))?,
        };
        if scope.replace(picked).is_some() {
            return Err(bad(name, format!("pick one of {FLAGS}")));
        }
    }
    *words = rest;
    Ok(scope)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::input::{ParsedInput, parse_input};

    fn parsed(text: &str) -> SlashCommand {
        match parse_input(text) {
            Ok(ParsedInput::Slash { command }) => command,
            other => panic!("{text}: {other:?}"),
        }
    }

    fn problem(text: &str) -> String {
        parse_input(text).unwrap_err().to_string()
    }

    fn line(change: LineChange, scope: Option<SettingScope>) -> SlashCommand {
        SlashCommand::BottomLine { change, scope }
    }

    #[test]
    fn bare_it_opens_the_picker_and_its_words_change_the_line() {
        assert_eq!(parsed("/bottom-line"), line(LineChange::Open, None));
        assert_eq!(
            parsed("/bottom-line add agy"),
            line(LineChange::Add { cli: "agy".into() }, None)
        );
        assert_eq!(
            parsed("/bottom-line add Claude Code --project"),
            line(
                LineChange::Add {
                    cli: "Claude Code".into()
                },
                Some(SettingScope::Project)
            )
        );
        assert_eq!(
            parsed("/bottom-line remove codex --session"),
            line(
                LineChange::Remove {
                    cli: "codex".into()
                },
                Some(SettingScope::Session)
            )
        );
        assert_eq!(
            parsed("/bottom-line reset --global"),
            line(LineChange::Reset, Some(SettingScope::Global))
        );
        assert_eq!(
            parsed("/bottom-line --session"),
            line(LineChange::Open, Some(SettingScope::Session))
        );
    }

    #[test]
    fn a_word_it_doesnt_take_says_what_it_does_take() {
        assert_eq!(
            problem("/bottom-line add"),
            "/bottom-line: name the CLI, such as /bottom-line add agy"
        );
        assert_eq!(
            problem("/bottom-line show agy"),
            format!("/bottom-line: {USAGE}")
        );
        assert_eq!(
            problem("/bottom-line reset agy"),
            format!("/bottom-line: {USAGE}")
        );
        assert!(problem("/bottom-line add agy --local").contains("no file for you alone"));
        assert_eq!(
            problem("/bottom-line reset --global --session"),
            format!("/bottom-line: pick one of {FLAGS}")
        );
        assert_eq!(
            problem("/bottom-line reset --everywhere"),
            format!("/bottom-line: --everywhere isn't a flag; use {FLAGS}")
        );
    }

    /// The picker's save travels as the command, so an outside agent's copy reads the same.
    #[test]
    fn the_pickers_save_reads_back_as_it_was_sent() {
        let saved = line(
            LineChange::Set {
                clis: vec![LineCli::new("claude-code"), LineCli::new("codex")],
                holding_up: false,
            },
            None,
        );
        let wire = serde_json::to_string(&saved).unwrap();
        assert_eq!(serde_json::from_str::<SlashCommand>(&wire).unwrap(), saved);
    }
}
