//! Claude Code's own slash commands: the built-in ones, as 2.1.289 lists them, and the custom
//! commands, skills, and plugin commands it loads for a folder. `claude -p` runs any of the
//! ones sent to it as the slash command itself.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use argonauts_core::cli_commands::files::{CommandFile, command_files, folders, skill_files};
use argonauts_core::cli_commands::{
    BuiltinCommand, BuiltinKind, CliCommand, CommandKind, CommandScope, CommandSource,
};
use argonauts_core::cli_sessions::CliScan;
use serde_json::Value;

use crate::isolation::SYNCED_SKILLS;
use BuiltinKind::{Argonauts, Cli, ScreenOnly};

/// Where Claude Code keeps what your claude.ai account syncs, under `skills` and `plugins`.
const SYNCED: &str = "synced";

const fn command(
    name: &'static str,
    aliases: &'static [&'static str],
    args: &'static str,
    help: &'static str,
    kind: BuiltinKind,
) -> BuiltinCommand {
    BuiltinCommand {
        name,
        aliases,
        args,
        help,
        kind,
    }
}

/// Grouped as Claude Code's own list groups them.
#[rustfmt::skip]
pub const BUILTIN_COMMANDS: &[BuiltinCommand] = &[
    // Session and conversation.
    command("clear", &["reset", "new"], "", "New conversation with empty context", Argonauts("clear")),
    command("compact", &[], "[instructions]", "Summarize the conversation to free context", Argonauts("compact")),
    command("autocompact", &[], "[auto|tokens]", "When auto-compact starts", ScreenOnly),
    command("context", &[], "", "Context usage as a colored grid", Argonauts("context")),
    command("resume", &["continue"], "", "Reopen a past session", Argonauts("resume")),
    command("rename", &[], "[name]", "Rename the conversation", Argonauts("rename")),
    command("branch", &[], "[name]", "Branch the conversation here", Argonauts("branch")),
    command("fork", &[], "[prompt]", "Copy the conversation into a new background session", Argonauts("fork")),
    command("rewind", &["checkpoint", "undo"], "", "Roll back the conversation, the code, or both", Argonauts("rewind")),
    command("btw", &[], "[question]", "Side question kept out of the history", Argonauts("btw")),
    command("recap", &[], "", "One-line summary", Argonauts("recap")),
    command("copy", &[], "[N]", "Copy the last response", Argonauts("copy")),
    command("export", &[], "[file]", "Save as plain text", Argonauts("export")),
    command("cd", &[], "<path>", "Work in another folder", Argonauts("cd")),
    command("add-dir", &[], "<path>", "Add a working folder", Argonauts("add-dir")),
    command("exit", &["quit"], "", "Leave", Argonauts("exit")),
    // Model and behavior.
    command("model", &[], "[model]", "Pick the model", Argonauts("model")),
    command("effort", &[], "[level|auto|ultracode]", "Reasoning effort", Argonauts("effort")),
    command("fast", &[], "[on|off]", "Fast mode", Argonauts("fast")),
    command("advisor", &[], "[model|off]", "A second model that advises this one", ScreenOnly),
    command("plan", &[], "[description]", "Plan before changing anything", Argonauts("plan")),
    command("goal", &[], "[condition|clear]", "Keep working until a condition holds", Argonauts("goal")),
    command("output-style", &[], "[style]", "How answers are written", Argonauts("output-style")),
    // Background work, agents, automation.
    command("agents", &[], "", "Manage subagents", Argonauts("agents")),
    command("list-agents", &["peers"], "", "Other Claude Code sessions on this machine", ScreenOnly),
    command("subtask", &[], "<task>", "Hand a task to a subagent", Cli),
    command("tasks", &["bashes"], "", "Background tasks and shells", ScreenOnly),
    command("background", &["bg"], "", "Send this session to the background", ScreenOnly),
    command("stop", &[], "", "Stop background work", Argonauts("stop")),
    command("workflows", &[], "", "Workflows", Argonauts("workflows")),
    command("loop", &["proactive"], "[interval] [prompt]", "Run a prompt on an interval", ScreenOnly),
    command("loops", &[], "", "List, create, and delete loops", ScreenOnly),
    command("daemon", &[], "", "Background services and routines", ScreenOnly),
    command("schedule", &["routines"], "", "Agents that run on a schedule", Cli),
    command("autofix-pr", &[], "", "Fix a pull request's failing checks in the cloud", ScreenOnly),
    command("teleport", &["tp"], "", "Bring a web session here", ScreenOnly),
    command("ultraplan", &[], "<prompt>", "A cloud session drafts a plan you can edit and approve", ScreenOnly),
    command("cloud-plugins", &[], "", "Whether cloud sessions use this machine's plugins", ScreenOnly),
    command("remote-control", &["rc"], "", "Drive this session from another device", ScreenOnly),
    command("remote-env", &[], "", "The remote environment", ScreenOnly),
    command("desktop", &["app"], "", "Carry on in the desktop app", ScreenOnly),
    // Code review and git.
    command("code-review", &["review"], "[level] [--fix] [--comment] [target]", "Review the changes", Argonauts("review")),
    command("ultrareview", &[], "", "A deep review in the cloud", ScreenOnly),
    command("security-review", &[], "", "Review the changes for security problems", Argonauts("security-review")),
    command("simplify", &[], "", "Clean up the changed code", Cli),
    command("diff", &[], "", "The uncommitted changes", Argonauts("diff")),
    command("install-github-app", &[], "", "Set up the GitHub app", ScreenOnly),
    // Config and setup.
    command("config", &["settings"], "", "Settings", Argonauts("config")),
    command("status", &[], "", "Version, model, and account", Argonauts("status")),
    command("permissions", &["allowed-tools"], "", "Tool permission rules", Argonauts("permissions")),
    command("hooks", &[], "", "Hooks", ScreenOnly),
    command("memory", &[], "", "Edit the memory files", Argonauts("memory")),
    command("pause-memory", &["memory-pause", "toggle-memory"], "", "Pause memory for this session", ScreenOnly),
    command("init", &[], "", "Write a CLAUDE.md for this repo", Cli),
    command("mcp", &[], "", "MCP servers", ScreenOnly),
    command("plugin", &[], "", "Plugins", ScreenOnly),
    command("reload-plugins", &[], "", "Load changed plugins", ScreenOnly),
    command("skills", &[], "", "Skills", Argonauts("skills")),
    command("reload-skills", &[], "", "Load changed skills", ScreenOnly),
    command("skill-doctor", &[], "", "Check the skills for problems", ScreenOnly),
    command("keybindings", &[], "", "Key bindings", ScreenOnly),
    command("statusline", &[], "", "The status line", ScreenOnly),
    command("theme", &[], "", "Colors", Argonauts("theme")),
    command("color", &[], "", "The prompt bar's color", ScreenOnly),
    command("tui", &[], "[default|fullscreen]", "The screen layout", ScreenOnly),
    command("focus", &[], "", "Focus view", ScreenOnly),
    command("scroll-speed", &[], "", "Scroll speed", ScreenOnly),
    command("terminal-setup", &[], "", "Set up the terminal's keys", ScreenOnly),
    command("sandbox", &[], "", "The sandbox", ScreenOnly),
    command("ide", &[], "", "Connect an IDE", ScreenOnly),
    command("chrome", &[], "", "Connect Chrome", ScreenOnly),
    command("voice", &[], "[hold|tap|off]", "Talk instead of typing", ScreenOnly),
    command("import", &[], "[codex|gemini|cursor]", "Import another tool's settings", ScreenOnly),
    command("auto-mode-setup", &[], "", "Set up auto mode", ScreenOnly),
    command("privacy-settings", &[], "", "Privacy settings", ScreenOnly),
    command("web-setup", &[], "", "Set up Claude Code on the web", ScreenOnly),
    command("setup-bedrock", &[], "", "Set up Amazon Bedrock", ScreenOnly),
    command("setup-vertex", &[], "", "Set up Google Vertex AI", ScreenOnly),
    command("install", &[], "", "Install the native build", ScreenOnly),
    // Its `/update` alias is Argonauts Code's own `/update` here, which runs `claude update`.
    command("restart", &[], "", "Restart Claude Code and keep this session", ScreenOnly),
    command("brief", &[], "", "Brief-only mode", ScreenOnly),
    command("wellbeing", &[], "", "Break reminders and quiet hours", ScreenOnly),
    // Account and usage.
    command("login", &[], "", "Log in", Argonauts("login")),
    command("logout", &[], "", "Log out", Argonauts("logout")),
    // Its `/stats` alias is Argonauts Code's own `/stats` here, the usage panel's Stats tab.
    command("usage", &["cost"], "", "Plan usage and spend", Argonauts("usage")),
    command("usage-credits", &["extra-usage"], "", "Usage credits", ScreenOnly),
    command("claim-credit", &[], "", "Claim the one-time credit for cloud sessions, by October 7", ScreenOnly),
    command("rate-limit-options", &[], "", "What to do at a rate limit", ScreenOnly),
    command("upgrade", &[], "", "Upgrade the plan", ScreenOnly),
    command("passes", &[], "", "Guest passes", ScreenOnly),
    // Help and extras.
    command("help", &[], "", "Help", Argonauts("help")),
    command("powerup", &[], "", "Feature tour", ScreenOnly),
    command("release-notes", &[], "", "What's new", Argonauts("release-notes")),
    command("doctor", &[], "", "Check the install", Argonauts("doctor")),
    // Claude Code's own doctor, under its other name, so Argonauts Code's /doctor stays. `/doctor`
    // with an argument comes here too, since Argonauts Code's takes none.
    command("checkup", &["doctor"], "[prompt-audit [<path>]]", "Claude Code's own checkup, or an audit of its instructions", Cli),
    command("bug", &["share"], "", "Report a bug in Claude Code", ScreenOnly),
    command("feedback", &[], "", "Send feedback", Argonauts("feedback")),
    command("insights", &[], "", "A report on how you use Claude Code", Cli),
    command("team-onboarding", &[], "", "A guide for teammates new to this repo", Cli),
    command("artifacts", &[], "", "The pages this conversation published", Argonauts("artifacts")),
    command("install-slack-app", &[], "", "Set up the Slack app", ScreenOnly),
    command("mobile", &["ios", "android"], "", "Get the mobile app", ScreenOnly),
    command("stickers", &[], "", "Order stickers", ScreenOnly),
    command("radio", &[], "", "Music while you work", ScreenOnly),
    // Built-in skills.
    command("run", &[], "", "Start the app and see a change working", Cli),
    command("verify", &[], "", "Check that a change works", Cli),
    command("run-skill-generator", &[], "", "Make a skill that runs this project", Cli),
    command("debug", &[], "", "Find out why something fails", Cli),
    command("batch", &[], "<instruction>", "One change across many files, in parallel", Cli),
    command("deep-research", &[], "<question>", "Research a question in depth", Cli),
    command("fewer-permission-prompts", &[], "", "Allow the read-only commands you keep approving", Cli),
    command("update-config", &[], "", "Change Claude Code's settings", Cli),
    command("keybindings-help", &[], "", "Change the key bindings", Cli),
    command("plugin-authoring", &[], "", "Write a plugin", Cli),
    command("workflow-authoring", &[], "", "Write a workflow", Cli),
    command("claude-api", &[], "[build-eval|hillclimb|prompt-audit|cost-optimize|migrate|upgrade|managed-agents-onboard|preserved-thinking-migration]", "Build with the Claude API", Cli),
    command("claude-in-chrome", &[], "[task]", "Work in your Chrome browser", Cli),
    command("dataviz", &[], "", "Charts and dashboards", Cli),
    command("design", &[], "", "A visual design", Cli),
    command("design-sync", &[], "", "Sync a design system", Cli),
    command("design-login", &[], "", "Log in to the design tools", Cli),
    command("slides", &[], "", "A slide deck", Cli),
    command("artifact-design", &[], "", "Design an artifact", Cli),
    command("artifact-diagramming", &[], "", "Diagrams in an artifact", Cli),
    command("artifact-capabilities", &[], "", "What an artifact page can do", Cli),
];

/// The project's commands and skills, then yours, your synced skills, your installed plugins',
/// and your synced plugins'. The first command of a name wins, so the project's come first.
pub fn discover(scan: &CliScan) -> Vec<CliCommand> {
    let project = scan.folder.join(".claude");
    let user = scan.home.join(".claude");
    let sync = SyncSwitches::read(scan);
    let mut found = Vec::new();
    found.extend(custom(&project.join("commands"), CommandScope::Project));
    found.extend(skills(&project.join("skills"), CommandScope::Project, None));
    found.extend(custom(&user.join("commands"), CommandScope::User));
    found.extend(skills(&user.join("skills"), CommandScope::User, None));
    if sync.skills {
        for bucket in folders(&user.join("skills").join(SYNCED)) {
            found.extend(skills(&bucket, CommandScope::User, Some(SYNCED_SKILLS)));
        }
    }
    found.extend(plugins(scan));
    if sync.plugins {
        for root in synced_plugins(&user.join("plugins").join(SYNCED)) {
            found.extend(plugin_commands(&root, &plugin_name(&root)));
        }
    }
    found
}

/// Whether what your claude.ai account syncs shows. Claude Code honors only `false`, and only
/// in your settings or the project's local file, never the project's shared settings.
struct SyncSwitches {
    skills: bool,
    plugins: bool,
}

impl SyncSwitches {
    fn read(scan: &CliScan) -> Self {
        let files = [
            scan.home.join(".claude").join("settings.json"),
            scan.folder.join(".claude").join("settings.local.json"),
        ];
        let settings: Vec<Value> = files.iter().filter_map(|file| read_json(file)).collect();
        let off = |key: &str| {
            settings
                .iter()
                .any(|settings| settings.get(key) == Some(&Value::Bool(false)))
        };
        Self {
            skills: !off("syncClaudeAiSkills"),
            plugins: !off("syncClaudeAiPlugins"),
        }
    }
}

/// Each synced plugin's folder: one holding a plugin, or a bucket of them, as synced skills come
/// in a bucket per account.
fn synced_plugins(dir: &Path) -> Vec<PathBuf> {
    let is_plugin = |dir: &Path| {
        [".claude-plugin", "commands", "skills"]
            .iter()
            .any(|part| dir.join(part).is_dir())
    };
    folders(dir)
        .into_iter()
        .flat_map(|folder| {
            if is_plugin(&folder) {
                vec![folder]
            } else {
                folders(&folder)
                    .into_iter()
                    .filter(|f| is_plugin(f))
                    .collect()
            }
        })
        .collect()
}

/// The name in the plugin's manifest, else its folder's.
fn plugin_name(root: &Path) -> String {
    read_json(&root.join(".claude-plugin").join("plugin.json"))
        .and_then(|manifest| manifest.get("name")?.as_str().map(str::to_owned))
        .filter(|name| !name.trim().is_empty())
        .unwrap_or_else(|| {
            root.file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_default()
        })
}

fn custom(dir: &Path, scope: CommandScope) -> Vec<CliCommand> {
    command_files(dir, true)
        .into_iter()
        .map(|file| {
            let source = CommandSource::Custom {
                scope,
                path: file.path.clone(),
            };
            agent_id(file.name.clone(), &file, source)
        })
        .collect()
}

fn skills(dir: &Path, scope: CommandScope, namespace: Option<&str>) -> Vec<CliCommand> {
    skill_files(dir)
        .into_iter()
        .filter(CommandFile::user_invocable)
        .map(|file| {
            let source = CommandSource::Skill {
                scope,
                path: file.path.clone(),
            };
            agent_id(namespaced(namespace, &file.name), &file, source)
        })
        .collect()
}

/// Every plugin in `installed_plugins.json` that applies to this folder, unless a settings
/// file turns it off. A plugin's commands and skills go by `<plugin>:<name>`.
fn plugins(scan: &CliScan) -> Vec<CliCommand> {
    let installed = scan
        .home
        .join(".claude")
        .join("plugins")
        .join("installed_plugins.json");
    let Some(installed) = read_json(&installed) else {
        return Vec::new();
    };
    let Some(plugins) = installed.get("plugins").and_then(Value::as_object) else {
        return Vec::new();
    };
    let switches = plugin_switches(scan);
    let mut found = Vec::new();
    for (key, installs) in plugins {
        if switches.get(key.as_str()) == Some(&false) {
            continue;
        }
        let plugin = key.split('@').next().unwrap_or(key);
        let Some(root) = install_path(installs, &scan.folder) else {
            continue;
        };
        found.extend(plugin_commands(&root, plugin));
    }
    found
}

/// A plugin's commands and skills, as `<plugin>:<name>`.
fn plugin_commands(root: &Path, plugin: &str) -> Vec<CliCommand> {
    let source = || CommandSource::Plugin {
        plugin: plugin.to_owned(),
    };
    let commands = command_files(&root.join("commands"), true);
    let skills = skill_files(&root.join("skills"))
        .into_iter()
        .filter(CommandFile::user_invocable);
    commands
        .into_iter()
        .chain(skills)
        .map(|file| agent_id(namespaced(Some(plugin), &file.name), &file, source()))
        .collect()
}

/// A plugin installed for you, or for this project.
fn install_path(installs: &Value, folder: &Path) -> Option<PathBuf> {
    installs
        .as_array()?
        .iter()
        .filter(
            |install| match install.get("scope").and_then(Value::as_str) {
                Some("project" | "local") => install
                    .get("projectPath")
                    .and_then(Value::as_str)
                    .is_some_and(|path| Path::new(path) == folder),
                _ => true,
            },
        )
        .find_map(|install| install.get("installPath").and_then(Value::as_str))
        .map(PathBuf::from)
}

/// `enabledPlugins` from your settings, then the project's, then its local file, a later one
/// winning. Only that key is read.
fn plugin_switches(scan: &CliScan) -> HashMap<String, bool> {
    let files = [
        scan.home.join(".claude").join("settings.json"),
        scan.folder.join(".claude").join("settings.json"),
        scan.folder.join(".claude").join("settings.local.json"),
    ];
    let mut switches = HashMap::new();
    for file in files {
        let Some(settings) = read_json(&file) else {
            continue;
        };
        let Some(enabled) = settings.get("enabledPlugins").and_then(Value::as_object) else {
            continue;
        };
        for (key, on) in enabled {
            if let Some(on) = on.as_bool() {
                switches.insert(key.clone(), on);
            }
        }
    }
    switches
}

fn read_json(path: &Path) -> Option<Value> {
    serde_json::from_str(&fs::read_to_string(path).ok()?).ok()
}

fn namespaced(namespace: Option<&str>, name: &str) -> String {
    match namespace {
        Some(namespace) => format!("{namespace}:{name}"),
        None => name.to_owned(),
    }
}

fn agent_id(name: String, file: &CommandFile, source: CommandSource) -> CliCommand {
    CliCommand {
        name,
        aliases: Vec::new(),
        args: file.args(),
        help: file.help(),
        kind: CommandKind::Cli,
        source,
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use argonauts_core::cli_commands::CliCommandSet;
    use argonauts_core::input::{SLASH_COMMANDS, parse_input_with, slash_spec};
    use argonauts_core::names::AgentId;

    use super::*;

    fn write(path: &Path, text: &str) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, text).unwrap();
    }

    #[test]
    fn the_table_maps_each_name_once_and_only_to_argonauts_commands_that_exist() {
        let mut seen = HashSet::new();
        for builtin in BUILTIN_COMMANDS {
            for name in std::iter::once(builtin.name).chain(builtin.aliases.iter().copied()) {
                // A command running inside the CLI may answer to an Argonauts Code command's name
                // for the arguments Argonauts Code's own doesn't take, as /checkup does for
                // /doctor.
                if builtin.kind == Cli && name != builtin.name && slash_spec(name).is_some() {
                    continue;
                }
                assert!(seen.insert(name), "/{name} twice");
                // Argonauts Code's own name comes first, so a CLI command of that name must be the
                // same job.
                if slash_spec(name).is_some() {
                    assert!(
                        matches!(builtin.kind, Argonauts(ours) if ours == name),
                        "/{name} is an Argonauts Code command"
                    );
                }
            }
            if let Argonauts(ours) = builtin.kind {
                assert!(
                    slash_spec(ours).is_some(),
                    "/{} maps to /{ours}",
                    builtin.name
                );
            }
            assert!(!builtin.help.is_empty(), "/{}", builtin.name);
        }
        assert!(BUILTIN_COMMANDS.len() > 100);
        // Every Argonauts Code command the CLI also has is listed under its own name.
        for spec in SLASH_COMMANDS {
            if let Some(builtin) = BUILTIN_COMMANDS.iter().find(|b| b.name == spec.name) {
                assert_eq!(builtin.kind, Argonauts(spec.name));
            }
        }
    }

    #[test]
    fn the_three_kinds_parse_as_the_spec_says() {
        let set = CliCommandSet::new(
            AgentId("claude-code".into()),
            "Claude Code",
            BUILTIN_COMMANDS,
            Vec::new(),
        );
        let parse = |text: &str| parse_input_with(text, Some(&set));
        let slash = |text: &str| match parse(text) {
            Ok(argonauts_core::input::ParsedInput::Slash { command }) => {
                serde_json::to_value(command).unwrap()["command"].clone()
            }
            other => panic!("{text}: {other:?}"),
        };
        assert_eq!(slash("/reset"), "clear");
        assert_eq!(slash("/new"), "clear");
        assert_eq!(slash("/cost"), "usage");
        assert_eq!(slash("/quit"), "exit");
        assert_eq!(slash("/continue"), "resume");
        assert_eq!(slash("/settings"), "config");
        assert_eq!(slash("/code-review src"), "review");
        assert_eq!(slash("/simplify"), "cli");
        assert_eq!(slash("/init"), "cli");
        assert_eq!(slash("/batch rename foo"), "cli");
        // Claude Code's own doctor goes by its other name, and takes what Argonauts Code's doesn't.
        assert_eq!(slash("/doctor"), "doctor");
        assert_eq!(slash("/checkup"), "cli");
        let audit = |text: &str| match parse(text) {
            Ok(argonauts_core::input::ParsedInput::Slash {
                command: argonauts_core::input::SlashCommand::Cli { name, args },
            }) => format!("/{name} {args}"),
            other => panic!("{text}: {other:?}"),
        };
        assert_eq!(audit("/checkup prompt-audit"), "/checkup prompt-audit");
        assert_eq!(
            audit("/doctor prompt-audit src/"),
            "/checkup prompt-audit src/"
        );
        for screen in [
            "/claim-credit",
            "/cloud-plugins",
            "/daemon",
            "/loops",
            "/wellbeing",
            "/brief",
            "/install",
            "/setup-bedrock",
            "/setup-vertex",
            "/ultraplan",
            "/restart",
            "/pause-memory",
            "/extra-usage",
        ] {
            let err = parse(screen).unwrap_err().to_string();
            assert!(
                err.ends_with("works only in Claude Code itself"),
                "{screen}: {err}"
            );
        }
        for screen in ["/voice", "/mcp", "/ide", "/stickers", "/ios", "/tp"] {
            let err = parse(screen).unwrap_err().to_string();
            assert!(
                err.ends_with("works only in Claude Code itself"),
                "{screen}: {err}"
            );
        }
        let menu: Vec<&str> = set.menu().map(|c| c.name.as_str()).collect();
        assert!(menu.contains(&"init") && menu.contains(&"simplify") && menu.contains(&"dataviz"));
        assert!(!menu.contains(&"voice") && !menu.contains(&"clear"));
        assert!(menu.contains(&"checkup"));
        // /help lists these under "Open Claude Code for these".
        let screen: Vec<&str> = set.screen_only().map(|c| c.name.as_str()).collect();
        assert!(screen.contains(&"claim-credit") && screen.contains(&"wellbeing"));
        assert_eq!(set.screen_only_heading(), "Open Claude Code for these");
        // What init's terminal_slash_commands named in 2.1.289 is handled or listed.
        for name in ["doctor", "color", "focus", "reload-plugins"] {
            assert!(set.find(name).is_some(), "/{name}");
        }
    }

    #[test]
    fn discovery_finds_project_user_synced_and_plugin_commands() {
        let dir = tempfile::tempdir().unwrap();
        let folder = dir.path().join("repo");
        let home = dir.path().join("home");
        write(
            &folder.join(".claude/commands/ship.md"),
            "---\ndescription: Ship the branch\nargument-hint: [branch]\n---\nShip $ARGUMENTS\n",
        );
        write(
            &folder.join(".claude/commands/team/review-pr.md"),
            "Review a pull request\n",
        );
        write(
            &folder.join(".claude/skills/release/SKILL.md"),
            "---\nname: release\ndescription: Cut a release\n---\n",
        );
        write(
            &folder.join(".claude/skills/internal/SKILL.md"),
            "---\ndescription: Model only\nuser-invocable: false\n---\n",
        );
        write(&home.join(".claude/commands/ship.md"), "Your own ship\n");
        write(
            &home.join(".claude/commands/standup.md"),
            "Write my standup\n",
        );
        write(
            &home.join(".claude/skills/synced/bucket-1/example-a/SKILL.md"),
            "---\nname: example-a\ndescription: An example skill\n---\n",
        );
        write(&home.join(".claude/skills/synced/.bucket-1"), "");

        let plugins = home.join(".claude/plugins");
        let commit = plugins.join("cache/official/commit-commands/1.0.0");
        write(
            &commit.join("commands/commit.md"),
            "---\ndescription: Create a git commit\n---\n",
        );
        write(
            &commit.join("skills/changelog/SKILL.md"),
            "---\nname: changelog\ndescription: Update the changelog\n---\n",
        );
        let off = plugins.join("cache/official/off-plugin/1.0.0");
        write(&off.join("commands/nope.md"), "Turned off\n");
        let elsewhere = plugins.join("cache/official/other-repo/1.0.0");
        write(&elsewhere.join("commands/there.md"), "Another project's\n");
        let installed = serde_json::json!({
            "version": 2,
            "plugins": {
                "commit-commands@official": [{"scope": "user", "installPath": commit}],
                "off-plugin@official": [{"scope": "user", "installPath": off}],
                "other-repo@official": [{"scope": "project", "projectPath": "/elsewhere", "installPath": elsewhere}],
            }
        });
        write(
            &plugins.join("installed_plugins.json"),
            &installed.to_string(),
        );
        write(
            &home.join(".claude/settings.json"),
            r#"{"enabledPlugins": {"off-plugin@official": true, "commit-commands@official": true}}"#,
        );
        write(
            &folder.join(".claude/settings.local.json"),
            r#"{"enabledPlugins": {"off-plugin@official": false}}"#,
        );

        let scan = CliScan::new(&folder, &home);
        let found = discover(&scan);
        let names: Vec<&str> = found.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(
            names,
            [
                "ship",
                "review-pr",
                "release",
                "ship",
                "standup",
                "anthropic-skills:example-a",
                "commit-commands:commit",
                "commit-commands:changelog",
            ]
        );
        let ship = &found[0];
        assert_eq!(
            (ship.help.as_str(), ship.args.as_str()),
            ("Ship the branch", "[branch]")
        );
        assert_eq!(
            ship.source,
            CommandSource::Custom {
                scope: CommandScope::Project,
                path: folder.join(".claude/commands/ship.md"),
            }
        );
        assert!(matches!(
            found[5].source,
            CommandSource::Skill {
                scope: CommandScope::User,
                ..
            }
        ));
        assert_eq!(
            found[6].source,
            CommandSource::Plugin {
                plugin: "commit-commands".into()
            }
        );
        assert!(found.iter().all(|c| c.kind == CommandKind::Cli));

        // The project's ship wins over yours once the set is built.
        let set = CliCommandSet::new(
            AgentId("claude-code".into()),
            "Claude Code",
            BUILTIN_COMMANDS,
            found,
        );
        let ships: Vec<_> = set.commands.iter().filter(|c| c.name == "ship").collect();
        assert_eq!(ships.len(), 1);
        assert_eq!(ships[0].help, "Ship the branch");
    }

    #[test]
    fn nothing_on_disk_finds_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let scan = CliScan::new(dir.path().join("repo"), dir.path().join("home"));
        assert!(discover(&scan).is_empty());
        write(
            &dir.path()
                .join("home/.claude/plugins/installed_plugins.json"),
            "not json",
        );
        assert!(discover(&scan).is_empty());
    }

    /// What your claude.ai account syncs is listed like your own, plugins too, unless you
    /// turned syncing off in your settings or the project's local file.
    #[test]
    fn synced_plugins_are_listed_and_the_opt_outs_are_honored() {
        let dir = tempfile::tempdir().unwrap();
        let folder = dir.path().join("repo");
        let home = dir.path().join("home");
        write(
            &home.join(".claude/skills/synced/bucket-1/example-a/SKILL.md"),
            "---\nname: example-a\ndescription: An example skill\n---\n",
        );
        let synced = home.join(".claude/plugins/synced/bucket-1/design-kit");
        write(
            &synced.join(".claude-plugin/plugin.json"),
            r#"{"name": "design-kit"}"#,
        );
        write(
            &synced.join("commands/mock.md"),
            "---\ndescription: Draw a mockup\n---\n",
        );
        let scan = CliScan::new(&folder, &home);
        let names = |scan: &CliScan| -> Vec<String> {
            discover(scan).into_iter().map(|c| c.name).collect()
        };
        assert_eq!(
            names(&scan),
            ["anthropic-skills:example-a", "design-kit:mock"]
        );
        let mock = discover(&scan).pop().unwrap();
        assert_eq!(
            mock.source,
            CommandSource::Plugin {
                plugin: "design-kit".into()
            }
        );

        // The project's shared settings can't turn it off.
        write(
            &folder.join(".claude/settings.json"),
            r#"{"syncClaudeAiSkills": false, "syncClaudeAiPlugins": false}"#,
        );
        assert_eq!(names(&scan).len(), 2);
        write(
            &folder.join(".claude/settings.local.json"),
            r#"{"syncClaudeAiPlugins": false}"#,
        );
        assert_eq!(names(&scan), ["anthropic-skills:example-a"]);
        write(
            &home.join(".claude/settings.json"),
            r#"{"syncClaudeAiSkills": false, "syncClaudeAiPlugins": true}"#,
        );
        assert!(names(&scan).is_empty());
    }
}
