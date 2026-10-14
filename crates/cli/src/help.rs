//! `argonauts --help` lists the commands under the categories the app's `/` menu uses, so the
//! shell and the app read the same way where they overlap.

use argonauts_core::input::CommandCategory;
use clap::{Command, CommandFactory as _};

use crate::args::Cli;

/// Each shell command's category. A command missing here lands under the last one.
const GROUPS: &[(&str, CommandCategory)] = &[
    ("resume", CommandCategory::Session),
    ("run", CommandCategory::Work),
    ("status", CommandCategory::Runs),
    ("models", CommandCategory::Crew),
    ("approve", CommandCategory::ApprovalsAndLimits),
    ("approvals", CommandCategory::ApprovalsAndLimits),
    ("auto", CommandCategory::ApprovalsAndLimits),
    ("run-review", CommandCategory::Runs),
    ("records", CommandCategory::Learning),
    ("learned", CommandCategory::Learning),
    ("skills", CommandCategory::Learning),
    ("sync", CommandCategory::Learning),
    ("setup", CommandCategory::SetupAndHelp),
    ("doctor", CommandCategory::SetupAndHelp),
    ("update", CommandCategory::SetupAndHelp),
    ("config", CommandCategory::SetupAndHelp),
    ("telemetry", CommandCategory::SetupAndHelp),
    ("mcp", CommandCategory::SetupAndHelp),
    ("help", CommandCategory::SetupAndHelp),
];

/// The command line, with its help grouped.
pub fn command() -> Command {
    let command = Cli::command();
    let groups = grouped(&command);
    command.help_template(format!(
        "{{before-help}}{{about-with-newline}}\n{{usage-heading}} {{usage}}\n\n{groups}{{options}}\
         {{after-help}}"
    ))
}

/// The visible commands under their category headings, in the menu's order, named and
/// described as clap would.
fn grouped(command: &Command) -> String {
    let header = command.get_styles().get_header();
    let literal = command.get_styles().get_literal();
    let mut rows: Vec<(CommandCategory, String, String)> = command
        .get_subcommands()
        .filter(|sub| !sub.is_hide_set())
        .map(|sub| {
            let name = sub.get_name().to_owned();
            let about = sub.get_about().map(ToString::to_string).unwrap_or_default();
            (category_of(&name), name, about)
        })
        .collect();
    // clap adds `help` itself once the command is built.
    if !rows.iter().any(|(_, name, _)| name == "help") {
        rows.push((
            category_of("help"),
            "help".to_owned(),
            "Print this message or the help of the given subcommand(s)".to_owned(),
        ));
    }
    rows.sort_by_key(|(category, ..)| *category);
    let width = rows
        .iter()
        .map(|(_, name, _)| name.len())
        .max()
        .unwrap_or(0);
    let mut text = String::new();
    let mut shown = None;
    for (category, name, about) in rows {
        if shown != Some(category) {
            if shown.is_some() {
                text.push('\n');
            }
            shown = Some(category);
            text.push_str(&format!("{header}{}:{header:#}\n", category.title()));
        }
        let pad = " ".repeat(width - name.len() + 2);
        text.push_str(&format!("  {literal}{name}{literal:#}{pad}{about}\n"));
    }
    text.push_str(&format!("\n{header}Options:{header:#}\n"));
    text
}

fn category_of(name: &str) -> CommandCategory {
    GROUPS
        .iter()
        .find(|(command, _)| *command == name)
        .map_or(CommandCategory::SetupAndHelp, |(_, category)| *category)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn help() -> String {
        let mut command = command();
        command.render_help().to_string()
    }

    #[test]
    fn every_visible_command_has_a_category() {
        let command = Cli::command();
        for sub in command.get_subcommands().filter(|sub| !sub.is_hide_set()) {
            assert!(
                GROUPS.iter().any(|(name, _)| *name == sub.get_name()),
                "{} has no category",
                sub.get_name()
            );
        }
    }

    #[test]
    fn help_lists_the_commands_under_the_menus_categories_in_order() {
        let help = help();
        let headings: Vec<&str> = help
            .lines()
            .filter(|line| !line.starts_with(' ') && line.ends_with(':'))
            .collect();
        assert_eq!(
            headings,
            [
                "Session:",
                "Work:",
                "Runs:",
                "Crew:",
                "Approvals and limits:",
                "Learning:",
                "Setup and help:",
                "Options:",
            ],
            "{help}"
        );
        let at = |text: &str| {
            help.find(text)
                .unwrap_or_else(|| panic!("{text} in {help}"))
        };
        assert!(at("Session:") < at("  resume "));
        assert!(at("  resume ") < at("Work:"));
        assert!(at("Approvals and limits:") < at("  approvals "));
        assert!(at("Setup and help:") < at("  help "));
        assert!(at("Options:") < at("--screen-reader"));
        assert!(!help.contains("host"), "{help}");
        assert!(!help.contains("Commands:"), "{help}");
    }

    /// The `/` menu and `argonauts --help` group the learning commands under Learning, between
    /// Approvals and limits and what follows.
    #[test]
    fn learning_commands_grouped_in_menu_and_help() {
        use argonauts_core::input::SLASH_COMMANDS;

        let menu: Vec<&str> = SLASH_COMMANDS
            .iter()
            .filter(|spec| spec.category == CommandCategory::Learning)
            .map(|spec| spec.name)
            .collect();
        assert_eq!(menu, ["records", "learned", "skills", "memory"]);
        let order: Vec<CommandCategory> = SLASH_COMMANDS.iter().map(|spec| spec.category).collect();
        let learning = order
            .iter()
            .position(|category| *category == CommandCategory::Learning)
            .unwrap();
        assert_eq!(order[learning - 1], CommandCategory::ApprovalsAndLimits);
        assert_eq!(order[learning + 4], CommandCategory::Display);

        let help = help();
        let section = help
            .split("Learning:")
            .nth(1)
            .and_then(|rest| rest.split("Setup and help:").next())
            .unwrap_or_else(|| panic!("no Learning section in {help}"));
        let names: Vec<&str> = section
            .lines()
            .filter_map(|line| line.split_whitespace().next())
            .collect();
        assert_eq!(names, ["records", "learned", "skills", "sync"], "{help}");
        let runs = help
            .split("Runs:")
            .nth(1)
            .and_then(|rest| rest.split("Crew:").next())
            .unwrap_or_default();
        assert!(runs.contains("  run-review "), "{help}");
    }
}
