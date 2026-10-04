//! Every command the `/` menu, `/help`, and the screen reader list: Argonauts' own by category,
//! then the main agent CLI's that run inside it under its name, and the ones only its own
//! screen runs under "Open <CLI> for these". A CLI command an Argonauts one of the same name
//! hides is left out, since typing it runs Argonauts' version.

use argonauts_core::input::{
    CommandCategory, Handler, SHARED_COMMANDS, SLASH_COMMANDS, SlashSpec, slash_spec,
};
use argonauts_core::vendor_commands::{
    CommandKind, CommandScope, CommandSource, VendorCommand, VendorCommandSet,
};

use crate::bottom::MenuItem;

/// Argonauts' commands in menu order: by category, each category's shared commands after the
/// rest of it.
pub fn argonauts_commands() -> Vec<&'static SlashSpec> {
    let mut specs: Vec<&'static SlashSpec> = SLASH_COMMANDS.iter().chain(SHARED_COMMANDS).collect();
    specs.sort_by_key(|spec| spec.category);
    specs
}

/// The main agent's commands that run inside it.
pub fn vendor_menu(set: &VendorCommandSet) -> impl Iterator<Item = &VendorCommand> {
    set.menu()
        .filter(|command| slash_spec(&command.name).is_none())
}

/// The main agent's commands that only its own screen runs.
pub fn vendor_screen_only(set: &VendorCommandSet) -> impl Iterator<Item = &VendorCommand> {
    set.screen_only()
        .filter(|command| slash_spec(&command.name).is_none())
}

/// Who handles `/typed`: the Argonauts command's own handler, also when the main agent's name
/// for it was typed, such as `/quit` for `/exit`.
pub fn handler(typed: &str, vendor: Option<&VendorCommandSet>) -> Handler {
    let spec = slash_spec(typed).or_else(|| match &vendor?.find(typed)?.kind {
        CommandKind::Argonauts { command } => slash_spec(command),
        CommandKind::Vendor | CommandKind::ScreenOnly => None,
    });
    spec.map_or(Handler::App, |spec| spec.handler)
}

/// The `/` menu's rows, each under its category, the main agent's last.
pub fn command_items(vendor: Option<&VendorCommandSet>) -> Vec<MenuItem> {
    let mut items: Vec<MenuItem> = argonauts_commands()
        .into_iter()
        .map(|spec| item(spec.name, spec.help, spec.args).in_group(spec.category.title()))
        .collect();
    if let Some(set) = vendor {
        items.extend(vendor_menu(set).map(|command| {
            let item = item(&command.name, &command.help, &command.args).in_group(&set.title);
            match origin(command) {
                Some(origin) => item.with_kind(origin),
                None => item,
            }
        }));
    }
    items
}

/// A heading and its `(usage, description)` rows, as `/help` lists them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HelpSection {
    pub heading: String,
    /// Argonauts' categories and the main agent's menu, not the ones its own screen runs.
    pub runs_here: bool,
    pub rows: Vec<(String, String)>,
}

pub fn help_sections(vendor: Option<&VendorCommandSet>) -> Vec<HelpSection> {
    let mut sections: Vec<HelpSection> = Vec::new();
    let mut category: Option<CommandCategory> = None;
    for spec in argonauts_commands() {
        let row = (usage(spec.name, spec.args), spec.help.to_owned());
        match sections.last_mut() {
            Some(section) if category == Some(spec.category) => section.rows.push(row),
            _ => {
                category = Some(spec.category);
                sections.push(HelpSection {
                    heading: spec.category.title().to_owned(),
                    runs_here: true,
                    rows: vec![row],
                });
            }
        }
    }
    let Some(set) = vendor else {
        return sections;
    };
    let rows = |commands: Vec<&VendorCommand>| -> Vec<(String, String)> {
        commands
            .into_iter()
            .map(|command| {
                let help = match origin(command) {
                    Some(origin) => format!("{} ({origin})", command.help),
                    None => command.help.clone(),
                };
                (usage(&command.name, &command.args), help)
            })
            .collect()
    };
    let menu = rows(vendor_menu(set).collect());
    if !menu.is_empty() {
        sections.push(HelpSection {
            heading: set.title.clone(),
            runs_here: true,
            rows: menu,
        });
    }
    let screen = rows(vendor_screen_only(set).collect());
    if !screen.is_empty() {
        sections.push(HelpSection {
            heading: set.screen_only_heading(),
            runs_here: false,
            rows: screen,
        });
    }
    sections
}

/// `/name args`.
pub fn usage(name: &str, args: &str) -> String {
    if args.is_empty() {
        format!("/{name}")
    } else {
        format!("/{name} {args}")
    }
}

fn item(name: &str, help: &str, args: &str) -> MenuItem {
    let item = MenuItem::new(name, help);
    if needs_argument(args) {
        item.needing_argument()
    } else {
        item
    }
}

/// As [`SlashSpec::needs_argument`]: an argument outside `[...]`.
fn needs_argument(args: &str) -> bool {
    let mut optional = 0_usize;
    for c in args.chars() {
        match c {
            '[' => optional += 1,
            ']' => optional = optional.saturating_sub(1),
            '<' if optional == 0 => return true,
            _ => {}
        }
    }
    false
}

/// Where a discovered command comes from, as Claude Code marks it: `project`, `user`, or the
/// plugin's name.
fn origin(command: &VendorCommand) -> Option<String> {
    match &command.source {
        CommandSource::BuiltIn => None,
        CommandSource::Custom { scope, .. } | CommandSource::Skill { scope, .. } => {
            Some(match scope {
                CommandScope::Project => "project".to_owned(),
                CommandScope::User => "user".to_owned(),
            })
        }
        // A mod's command whose plugin the CLI didn't name has no mark.
        CommandSource::Plugin { plugin } => Some(plugin.clone()).filter(|p| !p.is_empty()),
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use std::path::PathBuf;

    use argonauts_core::names::VendorId;
    use argonauts_core::vendor_commands::{BuiltinCommand, BuiltinKind};

    use super::*;

    const TABLE: &[BuiltinCommand] = &[
        BuiltinCommand {
            name: "clear",
            aliases: &["reset"],
            args: "",
            help: "New conversation",
            kind: BuiltinKind::Argonauts("clear"),
        },
        BuiltinCommand {
            name: "exit",
            aliases: &["quit"],
            args: "",
            help: "Leave",
            kind: BuiltinKind::Argonauts("exit"),
        },
        BuiltinCommand {
            name: "batch",
            aliases: &[],
            args: "<instruction>",
            help: "One change across many files",
            kind: BuiltinKind::Vendor,
        },
        BuiltinCommand {
            name: "simplify",
            aliases: &[],
            args: "",
            help: "Clean up the changed code",
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

    /// Claude Code's commands as a small table, a project command, a plugin's, and a custom
    /// `status` that Argonauts' own hides.
    pub(crate) fn claude_commands() -> VendorCommandSet {
        let custom = |name: &str, help: &str, source: CommandSource| VendorCommand {
            name: name.into(),
            aliases: Vec::new(),
            args: String::new(),
            help: help.into(),
            kind: CommandKind::Vendor,
            source,
        };
        let project = |name: &str| CommandSource::Custom {
            scope: CommandScope::Project,
            path: PathBuf::from(format!("/repo/.claude/commands/{name}.md")),
        };
        VendorCommandSet::new(
            VendorId("claude-code".into()),
            "Claude Code",
            TABLE,
            vec![
                custom("ship", "Ship the branch", project("ship")),
                custom("status", "A custom status", project("status")),
                custom(
                    "commit-commands:commit",
                    "Create a git commit",
                    CommandSource::Plugin {
                        plugin: "commit-commands".into(),
                    },
                ),
            ],
        )
    }

    #[test]
    fn the_menu_adds_the_shared_commands_then_the_main_agents_category() {
        let plain = command_items(None);
        assert_eq!(plain.len(), SLASH_COMMANDS.len() + SHARED_COMMANDS.len());
        let groups: Vec<&str> = plain.iter().filter_map(|item| item.group.as_deref()).fold(
            Vec::new(),
            |mut groups, group| {
                if groups.last() != Some(&group) {
                    groups.push(group);
                }
                groups
            },
        );
        assert_eq!(
            groups,
            [
                "Session",
                "Work",
                "Runs",
                "Crew",
                "Approvals and limits",
                "Learning",
                "Display",
                "Setup and help",
            ],
            "each category once"
        );
        let session: Vec<&str> = plain
            .iter()
            .filter(|item| item.group.as_deref() == Some("Session"))
            .map(|item| item.name.as_str())
            .collect();
        assert_eq!(&session[..3], ["clear", "resume", "exit"]);
        assert!(session.contains(&"compact") && session.contains(&"add-dir"));

        let set = claude_commands();
        let items = command_items(Some(&set));
        let claude: Vec<(&str, Option<&str>, bool)> = items
            .iter()
            .filter(|item| item.group.as_deref() == Some("Claude Code"))
            .map(|item| {
                (
                    item.name.as_str(),
                    item.kind.as_deref(),
                    item.needs_argument,
                )
            })
            .collect();
        assert_eq!(
            claude,
            [
                ("batch", None, true),
                ("simplify", None, false),
                ("ship", Some("project"), false),
                ("commit-commands:commit", Some("commit-commands"), false),
            ],
            "what runs inside Claude Code, less what Argonauts' own names hide"
        );
    }

    #[test]
    fn help_lists_what_runs_here_then_what_only_the_clis_screen_runs() {
        let set = claude_commands();
        let sections = help_sections(Some(&set));
        let headings: Vec<&str> = sections.iter().map(|s| s.heading.as_str()).collect();
        assert_eq!(
            &headings[8..],
            ["Claude Code", "Open Claude Code for these"]
        );
        let claude = &sections[8];
        assert_eq!(
            claude.rows[0],
            (
                "/batch <instruction>".to_owned(),
                "One change across many files".to_owned()
            )
        );
        assert_eq!(claude.rows[2].1, "Ship the branch (project)");
        let screen = &sections[9];
        assert!(!screen.runs_here);
        assert_eq!(
            screen.rows,
            [(
                "/voice [hold|tap|off]".to_owned(),
                "Talk instead of typing".to_owned()
            )]
        );
        assert_eq!(help_sections(None).len(), 8);
    }

    #[test]
    fn a_clis_name_for_an_argonauts_command_takes_its_handler() {
        let set = claude_commands();
        assert_eq!(handler("exit", None), Handler::Ui);
        assert_eq!(handler("quit", Some(&set)), Handler::Ui);
        assert_eq!(handler("quit", None), Handler::App);
        assert_eq!(handler("copy", Some(&set)), Handler::Ui);
        assert_eq!(handler("simplify", Some(&set)), Handler::App);
        assert_eq!(handler("reset", Some(&set)), Handler::App);
    }
}
