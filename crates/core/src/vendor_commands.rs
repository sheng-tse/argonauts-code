//! The main agent CLI's own slash commands. Each one runs Argonauts' version of it, goes to the
//! main agent's session as the command itself, or belongs to the CLI's own screen.

pub mod files;

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::adapter::Adapter;
use crate::keywords::Keyword;
use crate::names::VendorId;
use crate::vendor_sessions::VendorScan;

/// One row of an adapter's built-in command table.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BuiltinCommand {
    /// Without the slash.
    pub name: &'static str,
    /// Other names the CLI takes for it, such as `reset` and `new` for `clear`.
    pub aliases: &'static [&'static str],
    /// Usage after the name, such as `[instructions]`.
    pub args: &'static str,
    pub help: &'static str,
    pub kind: BuiltinKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BuiltinKind {
    /// The Argonauts command of this name does the same job, for every vendor.
    Argonauts(&'static str),
    Vendor,
    ScreenOnly,
}

/// A vendor command, built in or discovered.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VendorCommand {
    /// Without the slash, such as `compact` or `anthropic-skills:example-a`.
    pub name: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub aliases: Vec<String>,
    /// Usage after the name, such as `[instructions]`.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub args: String,
    /// One line.
    pub help: String,
    #[serde(flatten)]
    pub kind: CommandKind,
    #[serde(flatten)]
    pub source: CommandSource,
}

impl VendorCommand {
    pub fn answers_to(&self, name: &str) -> bool {
        self.name == name || self.aliases.iter().any(|alias| alias == name)
    }

    /// The command as typed into the CLI, such as `/review src`.
    pub fn invocation(&self, args: &str) -> String {
        let args = args.trim();
        if args.is_empty() {
            format!("/{}", self.name)
        } else {
            format!("/{} {args}", self.name)
        }
    }
}

impl From<&BuiltinCommand> for VendorCommand {
    fn from(builtin: &BuiltinCommand) -> Self {
        Self {
            name: builtin.name.to_owned(),
            aliases: builtin
                .aliases
                .iter()
                .map(|&alias| alias.to_owned())
                .collect(),
            args: builtin.args.to_owned(),
            help: builtin.help.to_owned(),
            kind: match builtin.kind {
                BuiltinKind::Argonauts(command) => CommandKind::Argonauts {
                    command: command.to_owned(),
                },
                BuiltinKind::Vendor => CommandKind::Vendor,
                BuiltinKind::ScreenOnly => CommandKind::ScreenOnly,
            },
            source: CommandSource::BuiltIn,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum CommandKind {
    /// Argonauts' `/<command>` does the same job for every vendor, so it runs instead, with the
    /// arguments given.
    Argonauts { command: String },
    /// Sent to the main agent's session as the command itself. The reply shows in the chat.
    Vendor,
    /// Only the CLI's own screen can run it, such as a theme or voice setting.
    ScreenOnly,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "source", rename_all = "snake_case")]
pub enum CommandSource {
    BuiltIn,
    /// A command file in the project or the home folder.
    Custom {
        scope: CommandScope,
        path: PathBuf,
    },
    Skill {
        scope: CommandScope,
        path: PathBuf,
    },
    /// A command or skill from an installed plugin.
    Plugin {
        plugin: String,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CommandScope {
    Project,
    User,
}

/// The main agent's commands, which the `/` menu, `/help`, and the parser add to Argonauts' own,
/// and the words its CLI treats specially, which the input draws as the CLI does.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VendorCommandSet {
    pub vendor: VendorId,
    /// The CLI's name, which titles its menu category, such as "Claude Code".
    pub title: String,
    pub commands: Vec<VendorCommand>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub keywords: Vec<Keyword>,
}

impl VendorCommandSet {
    /// The built-in table, then what discovery found. The first command of a name wins, so a
    /// custom command can't shadow a built-in one.
    pub fn new(
        vendor: VendorId,
        title: impl Into<String>,
        builtins: &[BuiltinCommand],
        discovered: Vec<VendorCommand>,
    ) -> Self {
        let mut commands: Vec<VendorCommand> = Vec::new();
        for command in builtins.iter().map(VendorCommand::from).chain(discovered) {
            if !commands.iter().any(|known| known.name == command.name) {
                commands.push(command);
            }
        }
        Self {
            vendor,
            title: title.into(),
            commands,
            keywords: Vec::new(),
        }
    }

    /// The adapter's tables and discovery for `scan`.
    pub fn of(adapter: &dyn Adapter, scan: &VendorScan) -> anyhow::Result<Self> {
        Ok(Self::new(
            VendorId(adapter.id().to_owned()),
            adapter.display_name(),
            adapter.builtin_commands(),
            adapter.discover_commands(scan)?,
        )
        .with_keywords(adapter.keywords()))
    }

    pub fn with_keywords(mut self, keywords: &[Keyword]) -> Self {
        self.keywords = keywords.to_vec();
        self
    }

    /// By name, then by alias.
    pub fn find(&self, name: &str) -> Option<&VendorCommand> {
        self.commands
            .iter()
            .find(|command| command.name == name)
            .or_else(|| {
                self.commands
                    .iter()
                    .find(|command| command.answers_to(name))
            })
    }

    /// The command running inside the CLI that also answers to `name`, an Argonauts command's
    /// name, for when Argonauts' own can't take what was typed.
    pub fn runs_inside_as(&self, name: &str) -> Option<&VendorCommand> {
        self.commands.iter().find(|command| {
            command.kind == CommandKind::Vendor && command.aliases.iter().any(|alias| alias == name)
        })
    }

    /// The menu's category for this CLI: what runs inside it.
    pub fn menu(&self) -> impl Iterator<Item = &VendorCommand> {
        self.of_kind(|kind| matches!(kind, CommandKind::Vendor))
    }

    /// What `/help` lists under [`Self::screen_only_heading`].
    pub fn screen_only(&self) -> impl Iterator<Item = &VendorCommand> {
        self.of_kind(|kind| matches!(kind, CommandKind::ScreenOnly))
    }

    pub fn screen_only_heading(&self) -> String {
        format!("Open {} for these", self.title)
    }

    fn of_kind(&self, wanted: fn(&CommandKind) -> bool) -> impl Iterator<Item = &VendorCommand> {
        self.commands
            .iter()
            .filter(move |command| wanted(&command.kind))
    }
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};

    use super::*;

    const TABLE: &[BuiltinCommand] = &[
        BuiltinCommand {
            name: "clear",
            aliases: &["reset", "new"],
            args: "",
            help: "New conversation with empty context",
            kind: BuiltinKind::Argonauts("clear"),
        },
        BuiltinCommand {
            name: "init",
            aliases: &[],
            args: "",
            help: "Write a CLAUDE.md for this repo",
            kind: BuiltinKind::Vendor,
        },
        BuiltinCommand {
            name: "theme",
            aliases: &[],
            args: "",
            help: "Change the colors",
            kind: BuiltinKind::ScreenOnly,
        },
    ];

    fn round_trip(value: Value) -> VendorCommand {
        let parsed: VendorCommand = serde_json::from_value(value.clone()).unwrap();
        assert_eq!(serde_json::to_value(&parsed).unwrap(), value);
        parsed
    }

    #[test]
    fn commands_round_trip_flat() {
        let clear = round_trip(json!({
            "name": "clear", "aliases": ["reset", "new"], "help": "New conversation",
            "kind": "argonauts", "command": "clear", "source": "built_in"
        }));
        assert_eq!(
            clear.kind,
            CommandKind::Argonauts {
                command: "clear".into()
            }
        );
        let custom = round_trip(json!({
            "name": "ship", "args": "[branch]", "help": "Ship it", "kind": "vendor",
            "source": "custom", "scope": "project", "path": "/repo/.claude/commands/ship.md"
        }));
        assert_eq!(
            custom.source,
            CommandSource::Custom {
                scope: CommandScope::Project,
                path: PathBuf::from("/repo/.claude/commands/ship.md")
            }
        );
        round_trip(json!({
            "name": "pdf", "help": "Work with PDFs", "kind": "vendor",
            "source": "skill", "scope": "user", "path": "/home/.claude/skills/pdf/SKILL.md"
        }));
        round_trip(json!({
            "name": "anthropic-skills:example-a", "help": "Work with PDFs", "kind": "vendor",
            "source": "plugin", "plugin": "anthropic-skills"
        }));
        round_trip(json!({
            "name": "voice", "help": "Talk to it", "kind": "screen_only", "source": "built_in"
        }));

        let set =
            VendorCommandSet::new(VendorId("claude-code".into()), "Claude Code", TABLE, vec![]);
        let value = serde_json::to_value(&set).unwrap();
        assert_eq!(value["vendor"], "claude-code");
        assert_eq!(
            serde_json::from_value::<VendorCommandSet>(value).unwrap(),
            set
        );
    }

    #[test]
    fn a_set_finds_by_name_then_alias_and_sorts_by_kind() {
        let custom = VendorCommand {
            name: "theme".into(),
            aliases: vec!["reset".into()],
            args: String::new(),
            help: "A custom theme command".into(),
            kind: CommandKind::Vendor,
            source: CommandSource::Custom {
                scope: CommandScope::User,
                path: PathBuf::from("/home/.claude/commands/theme.md"),
            },
        };
        let skill = VendorCommand {
            name: "pdf".into(),
            aliases: Vec::new(),
            args: "[file]".into(),
            help: "Work with PDFs".into(),
            kind: CommandKind::Vendor,
            source: CommandSource::Plugin {
                plugin: "anthropic-skills".into(),
            },
        };
        let set = VendorCommandSet::new(
            VendorId("claude-code".into()),
            "Claude Code",
            TABLE,
            vec![custom, skill],
        );
        let names: Vec<&str> = set.commands.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(
            names,
            ["clear", "init", "theme", "pdf"],
            "a built-in name wins"
        );
        assert_eq!(set.find("new").map(|c| c.name.as_str()), Some("clear"));
        assert_eq!(set.find("pdf").map(|c| c.args.as_str()), Some("[file]"));
        assert!(set.find("nothing").is_none());
        let menu: Vec<&str> = set.menu().map(|c| c.name.as_str()).collect();
        assert_eq!(menu, ["init", "pdf"]);
        let screen: Vec<&str> = set.screen_only().map(|c| c.name.as_str()).collect();
        assert_eq!(screen, ["theme"]);
        assert_eq!(set.screen_only_heading(), "Open Claude Code for these");
        assert_eq!(set.commands[3].invocation("  a.pdf "), "/pdf a.pdf");
        assert_eq!(set.commands[1].invocation(""), "/init");
    }
}
