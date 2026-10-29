//! `--session`, `--local`, `--project` and `--global` after a command that changes a setting:
//! where that one change saves, in place of where `save_changes_to` sends your changes. The
//! flag is taken off in one place, here, so every such command takes it the same way.

use super::{AgentsAction, InputError, SlashCommand, WorkflowVerb, bad};
use crate::settings::SettingScope;

/// Each flag and the place it names, in the order the `/` menu's help lists them.
pub const SCOPE_FLAGS: [(&str, SettingScope); 4] = [
    ("--session", SettingScope::Session),
    ("--local", SettingScope::ProjectLocal),
    ("--project", SettingScope::Project),
    ("--global", SettingScope::Global),
];

/// The commands whose change a flag may place. `/prompts` and the mods commands read their own
/// flags.
const TAKE_A_FLAG: &[&str] = &[
    "model",
    "effort",
    "agents",
    "workflow",
    "captain",
    "orchestrator",
    "council",
    "fallback",
    "approvals",
    "limits",
    "permissions",
    "verbosity",
    "narrator",
    "theme",
    "vim",
    "config",
    "settings",
    "add-dir",
    "templates",
];

/// The place `word` names, when it's one of [`SCOPE_FLAGS`].
pub fn scope_of_flag(word: &str) -> Option<SettingScope> {
    SCOPE_FLAGS
        .iter()
        .find(|(flag, _)| *flag == word)
        .map(|(_, scope)| *scope)
}

/// Parses `/name args` with `parse`, its change saved where a last word such as `--session`
/// says. The command then arrives as [`SlashCommand::SavedIn`].
pub(super) fn parse(
    name: &str,
    args: &str,
    parse: impl Fn(&str, &str) -> Result<SlashCommand, InputError>,
) -> Result<SlashCommand, InputError> {
    // `/workflow copy <name> --project` writes a file there, which isn't a setting.
    let copies = name == "workflow" && args.split_whitespace().next() == Some("copy");
    if !TAKE_A_FLAG.contains(&name) || copies {
        return parse(name, args);
    }
    let Some((rest, scope)) = split(args).map_err(|problem| bad(name, problem))? else {
        return parse(name, args);
    };
    let command = parse(name, rest);
    if let Ok(command) = &command
        && saves_a_change(command)
    {
        return Ok(SlashCommand::SavedIn {
            scope,
            change: Box::new(command.clone()),
        });
    }
    // A command may read the flag itself, as `/limits pin --local` pins its mod there.
    match parse(name, args) {
        Ok(own) if !saves_a_change(&own) => Ok(own),
        _ => command.and_then(|_| {
            Err(bad(
                name,
                format!(
                    "{} goes with a change, such as /effort high --session; in a picker, \
                     ctrl+s saves for this session",
                    flag_of(scope)
                ),
            ))
        }),
    }
}

/// `args` without its last word and the place that word names, or `None` when it names none.
/// A second flag before it is refused, since the change goes to one place.
fn split(args: &str) -> Result<Option<(&str, SettingScope)>, String> {
    let args = args.trim_end();
    let (rest, last) = match args.rsplit_once(char::is_whitespace) {
        Some((rest, last)) => (rest.trim_end(), last),
        None => ("", args),
    };
    let Some(scope) = scope_of_flag(last) else {
        return Ok(None);
    };
    let before = rest.rsplit(char::is_whitespace).next().unwrap_or_default();
    if scope_of_flag(before).is_some() {
        return Err(format!("pick one of {}", flag_list()));
    }
    Ok(Some((rest, scope)))
}

/// Whether `command` changes a setting or writes a file, rather than opening a picker or a
/// view.
fn saves_a_change(command: &SlashCommand) -> bool {
    match command {
        SlashCommand::Model { worker, .. } => worker.is_some(),
        SlashCommand::Effort {
            level, ultracode, ..
        } => level.is_some() || ultracode.is_some(),
        SlashCommand::Agents { action } => matches!(action, AgentsAction::Add { .. }),
        SlashCommand::Workflow { name, action } => name.is_some() && *action == WorkflowVerb::Use,
        SlashCommand::Fallback { key, value, .. }
        | SlashCommand::Approvals { key, value }
        | SlashCommand::Limits { key, value, .. }
        | SlashCommand::Permissions { key, value, .. } => key.is_some() && value.is_some(),
        SlashCommand::Config { key, value, reset } => key.is_some() && (value.is_some() || *reset),
        SlashCommand::Verbosity { level } => level.is_some(),
        SlashCommand::Narrator { change } => change.is_some(),
        SlashCommand::Theme { name } => name.is_some(),
        // Alone, `/templates` asks which one, and the flag places the copy.
        SlashCommand::Templates { .. } | SlashCommand::Vim | SlashCommand::AddDir { .. } => true,
        _ => false,
    }
}

fn flag_of(scope: SettingScope) -> &'static str {
    SCOPE_FLAGS
        .iter()
        .find(|(_, place)| *place == scope)
        .map_or("--global", |(flag, _)| *flag)
}

fn flag_list() -> String {
    let flags: Vec<&str> = SCOPE_FLAGS.iter().map(|(flag, _)| *flag).collect();
    flags.join(", ")
}

#[cfg(test)]
mod tests {
    use super::super::parse_input;
    use super::*;
    use crate::input::ParsedInput;

    fn command(text: &str) -> Result<SlashCommand, InputError> {
        match parse_input(text)? {
            ParsedInput::Slash { command } => Ok(command),
            other => panic!("{text} isn't a command: {other:?}"),
        }
    }

    fn saved_in(text: &str) -> (SettingScope, SlashCommand) {
        match command(text) {
            Ok(SlashCommand::SavedIn { scope, change }) => (scope, *change),
            other => panic!("{text} kept no place: {other:?}"),
        }
    }

    #[test]
    fn each_flag_places_the_change_it_ends() {
        for (flag, scope) in SCOPE_FLAGS {
            let (placed, effort) = saved_in(&format!("/effort max {flag}"));
            assert_eq!(placed, scope, "{flag}");
            assert_eq!(effort, command("/effort max").unwrap(), "{flag}");
        }
        let (scope, theme) = saved_in("/theme light --session");
        assert_eq!(scope, SettingScope::Session);
        assert_eq!(theme, command("/theme light").unwrap());
        let (scope, config) = saved_in("/config ui.tips=false --local");
        assert_eq!(scope, SettingScope::ProjectLocal);
        assert_eq!(config, command("/config ui.tips=false").unwrap());
        let (scope, reset) = saved_in("/config ui.theme --reset --global");
        assert_eq!(scope, SettingScope::Global);
        assert_eq!(reset, command("/config ui.theme --reset").unwrap());
        assert_eq!(saved_in("/vim --session").1, SlashCommand::Vim);
        assert!(matches!(
            saved_in("/add-dir ../shared --project").1,
            SlashCommand::AddDir { .. }
        ));
        assert_eq!(
            saved_in("/templates --project"),
            (
                SettingScope::Project,
                SlashCommand::Templates { name: None }
            )
        );
        assert_eq!(
            saved_in("/templates captain --global"),
            (
                SettingScope::Global,
                SlashCommand::Templates {
                    name: Some("captain".to_owned())
                }
            )
        );
    }

    #[test]
    fn a_command_without_a_flag_is_as_it_was() {
        assert!(matches!(
            command("/effort max").unwrap(),
            SlashCommand::Effort { .. }
        ));
        // A word that only looks like a flag is the command's own.
        assert!(matches!(
            command("/workflow copy captain --project").unwrap(),
            SlashCommand::Workflow {
                action: WorkflowVerb::CopyToProject,
                ..
            }
        ));

        // Pinning a mod reads the flag as where the mod shows.
        assert!(matches!(
            command("/limits pin --local").unwrap(),
            SlashCommand::Limits {
                key: None,
                action: crate::input::ViewAction::Pin { .. },
                ..
            }
        ));
    }

    #[test]
    fn a_flag_needs_a_change_and_only_one_place() {
        let problem = |text: &str| match command(text) {
            Err(InputError::BadArguments { problem, .. }) => problem,
            other => panic!("{text} wasn't refused: {other:?}"),
        };
        assert_eq!(
            problem("/model --session"),
            "--session goes with a change, such as /effort high --session; in a picker, ctrl+s \
             saves for this session"
        );
        assert_eq!(
            problem("/effort max --local --global"),
            "pick one of --session, --local, --project, --global"
        );
        assert!(command("/clear --session").is_err());
    }
}
