//! Codex's own slash commands: the built-in ones, and the custom prompts, skills, and plugin
//! skills it loads for a folder. `codex exec` runs no slash command, so a skill goes as its
//! `$name` mention and a custom prompt as its text, the way the TUI expands it.

use std::fs;
use std::path::{Path, PathBuf};

use argonauts_core::cli_commands::files::{CommandFile, command_files, folders, skill_files};
use argonauts_core::cli_commands::{
    BuiltinCommand, BuiltinKind, CliCommand, CommandKind, CommandScope, CommandSource,
};
use argonauts_core::cli_sessions::CliScan;

use BuiltinKind::{Argonauts, ScreenOnly};

/// The TUI names a custom prompt `/prompts:<name>`.
const PROMPTS: &str = "prompts";

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

/// Each runs Argonauts Code's version or only in Codex's own screens, since `codex exec` takes none
/// of them. The app's own commands are here too; the internal debugging ones aren't.
#[rustfmt::skip]
pub const BUILTIN_COMMANDS: &[BuiltinCommand] = &[
    command("agent", &[], "", "Switch to another agent thread", ScreenOnly),
    command("agents", &[], "", "Agents", Argonauts("agents")),
    command("subagents", &[], "", "Subagent threads", ScreenOnly),
    command("app", &[], "", "Open the Codex app", ScreenOnly),
    command("apps", &[], "", "Connectors", ScreenOnly),
    command("approve", &[], "", "Approve what waits", ScreenOnly),
    command("archive", &[], "", "Archive this thread", ScreenOnly),
    command("cd", &["pwd"], "[path]", "Work in another folder", Argonauts("cd")),
    command("clear", &["new"], "", "A new conversation", Argonauts("clear")),
    command("compact", &[], "", "Summarize the conversation to free context", Argonauts("compact")),
    command("copy", &[], "", "Copy the last answer", Argonauts("copy")),
    command("daemon", &[], "", "The background daemon", ScreenOnly),
    command("debug-config", &[], "", "Where each setting came from", ScreenOnly),
    command("delete", &[], "", "Delete this thread", ScreenOnly),
    command("diff", &[], "", "The changes, untracked files included", Argonauts("diff")),
    command("exit", &["quit"], "", "Leave", Argonauts("exit")),
    command("experimental", &[], "", "Experimental features", ScreenOnly),
    command("export", &[], "", "Save the conversation", Argonauts("export")),
    command("fast", &[], "", "Fast mode", Argonauts("fast")),
    command("feedback", &[], "", "Send feedback", Argonauts("feedback")),
    command("fork", &[], "", "Fork the conversation", Argonauts("fork")),
    command("goal", &[], "[objective|edit|pause|resume|clear]", "Work toward a goal", Argonauts("goal")),
    command("hooks", &[], "", "Hooks", ScreenOnly),
    command("ide", &[], "", "Connect an IDE", ScreenOnly),
    command("import", &[], "", "Import another tool's settings", ScreenOnly),
    command("init", &[], "", "Write an AGENTS.md for this repo", ScreenOnly),
    command("keymap", &[], "[debug]", "Key bindings", ScreenOnly),
    command("logout", &[], "", "Log out", Argonauts("logout")),
    command("mcp", &[], "[verbose]", "MCP tools", ScreenOnly),
    command("memories", &[], "", "Memories", ScreenOnly),
    command("mention", &[], "<path>", "Attach a file", ScreenOnly),
    command("model", &[], "", "The model and its reasoning", Argonauts("model")),
    command("permissions", &[], "", "What Codex may do without asking", Argonauts("permissions")),
    command("personality", &[], "", "How Codex talks", ScreenOnly),
    command("pet", &[], "", "Your pet", ScreenOnly),
    command("pets", &[], "[off]", "Pets", ScreenOnly),
    command("plan", &[], "[prompt]", "Plan before changing anything", Argonauts("plan")),
    command("plugins", &[], "", "Plugins", ScreenOnly),
    command("ps", &[], "", "Background terminals", ScreenOnly),
    command("raw", &[], "[on|off]", "Raw output", ScreenOnly),
    command("recap", &[], "", "Where the conversation stands", Argonauts("recap")),
    command("rename", &[], "[name]", "Rename the thread", Argonauts("rename")),
    command("resume", &[], "", "Resume a thread", Argonauts("resume")),
    command("review", &[], "", "Review the changes", Argonauts("review")),
    command("rollout", &[], "", "The rollout file's path", ScreenOnly),
    command("side", &["btw"], "[question]", "A side question", Argonauts("btw")),
    command("skills", &[], "", "Skills", Argonauts("skills")),
    command("status", &[], "", "Session settings and usage", Argonauts("status")),
    command("statusline", &[], "", "The status line", ScreenOnly),
    command("stop", &["clean"], "", "Stop background terminals", Argonauts("stop")),
    command("theme", &[], "", "Colors", Argonauts("theme")),
    command("title", &[], "", "The terminal title", ScreenOnly),
    command("tui", &[], "", "The screen layout", ScreenOnly),
    command("usage", &[], "[daily|weekly|cumulative]", "Usage", Argonauts("usage")),
    command("vim", &[], "", "Vim keys", Argonauts("vim")),
    command("voice", &[], "[settings|mute|stop]", "Talk instead of typing", ScreenOnly),
    command("warnings", &[], "", "Warnings", ScreenOnly),
    command("worktree", &[], "", "Work in a git worktree", ScreenOnly),
    // The Codex app's own.
    command("cloud", &[], "", "Run the task in the cloud, in the Codex app", ScreenOnly),
    command("cloud-environment", &[], "", "The cloud environment, in the Codex app", ScreenOnly),
    command("ide-context", &[], "", "Share the IDE's context, in the Codex app", ScreenOnly),
    command("local", &[], "", "Run the task here, in the Codex app", ScreenOnly),
    command("project", &[], "", "Pick the project, in the Codex app", ScreenOnly),
    command("reasoning", &[], "", "Reasoning effort", Argonauts("effort")),
    command("task", &[], "", "The task, in the Codex app", ScreenOnly),
];

/// The project's skills, then yours, Codex's own, your custom prompts, and your enabled
/// plugins' skills. The first command of a name wins, so the project's come first.
pub fn discover(scan: &CliScan) -> Vec<CliCommand> {
    let codex = scan.home.join(".codex");
    let mut found = Vec::new();
    for dir in project_skill_dirs(&scan.folder) {
        found.extend(skills(&dir, CommandScope::Project));
    }
    found.extend(skills(
        &scan.home.join(".agents").join("skills"),
        CommandScope::User,
    ));
    found.extend(skills(&codex.join("skills"), CommandScope::User));
    found.extend(skills(
        &codex.join("skills").join(".system"),
        CommandScope::User,
    ));
    found.extend(
        command_files(&codex.join(PROMPTS), false)
            .into_iter()
            .map(|file| {
                let source = CommandSource::Custom {
                    scope: CommandScope::User,
                    path: file.path.clone(),
                };
                agent_id(format!("{PROMPTS}:{}", file.name), &file, source)
            }),
    );
    found.extend(plugins(scan));
    found
}

/// What `codex exec` takes for `command`: a skill's `$name`, or a custom prompt's text with the
/// arguments put in.
pub fn prompt(command: &CliCommand, args: &str) -> Option<String> {
    if command.kind != CommandKind::Cli {
        return None;
    }
    let args = args.trim();
    match &command.source {
        CommandSource::Custom { path, .. } => {
            let file = CommandFile::read(path, command.name.as_str())?;
            Some(expand(&file.body, args))
        }
        CommandSource::Skill { .. } | CommandSource::Plugin { .. } if args.is_empty() => {
            Some(format!("${}", command.name))
        }
        CommandSource::Skill { .. } | CommandSource::Plugin { .. } => {
            Some(format!("${} {args}", command.name))
        }
        CommandSource::BuiltIn => None,
    }
}

/// `.agents/skills` in the folder and each parent up to the repo's root.
fn project_skill_dirs(folder: &Path) -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    for dir in folder.ancestors() {
        dirs.push(dir.join(".agents").join("skills"));
        if dir.join(".git").exists() {
            break;
        }
    }
    if !folder.ancestors().any(|dir| dir.join(".git").exists()) {
        dirs.truncate(1);
    }
    dirs
}

fn skills(dir: &Path, scope: CommandScope) -> Vec<CliCommand> {
    skill_files(dir)
        .into_iter()
        .map(|file| {
            let source = CommandSource::Skill {
                scope,
                path: file.path.clone(),
            };
            agent_id(file.name.clone(), &file, source)
        })
        .collect()
}

/// Each plugin `config.toml` turns on, from its newest copy in the plugin cache.
fn plugins(scan: &CliScan) -> Vec<CliCommand> {
    let codex = scan.home.join(".codex");
    let Ok(text) = fs::read_to_string(codex.join("config.toml")) else {
        return Vec::new();
    };
    let Ok(config) = text.parse::<toml::Table>() else {
        return Vec::new();
    };
    let Some(plugins) = config.get("plugins").and_then(toml::Value::as_table) else {
        return Vec::new();
    };
    let mut found = Vec::new();
    for (key, entry) in plugins {
        let enabled = entry.get("enabled").and_then(toml::Value::as_bool);
        let Some((plugin, marketplace)) = key.split_once('@') else {
            continue;
        };
        if enabled != Some(true) {
            continue;
        }
        let cached = codex
            .join("plugins")
            .join("cache")
            .join(marketplace)
            .join(plugin);
        let Some(version) = newest(folders(&cached)) else {
            continue;
        };
        for file in skill_files(&version.join("skills")) {
            let source = CommandSource::Plugin {
                plugin: plugin.to_owned(),
            };
            found.push(agent_id(format!("{plugin}:{}", file.name), &file, source));
        }
    }
    found
}

/// Versions compare by their numbers, so `10.0` is newer than `9.1`.
fn newest(versions: Vec<PathBuf>) -> Option<PathBuf> {
    let key = |path: &PathBuf| -> Vec<(u64, String)> {
        let name = path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or_default();
        name.split(['.', '-'])
            .map(|part| (part.parse().unwrap_or(0), part.to_owned()))
            .collect()
    };
    versions.into_iter().max_by_key(key)
}

/// The TUI's placeholders: `$ARGUMENTS`, `$1` to `$9`, `$NAME` for a `NAME=value` argument,
/// and `$$` for a dollar sign. Arguments a prompt has no place for go after it.
fn expand(body: &str, args: &str) -> String {
    let words: Vec<&str> = args.split_whitespace().collect();
    let named: Vec<(&str, &str)> = words
        .iter()
        .filter_map(|word| word.split_once('='))
        .filter(|(name, _)| is_placeholder_name(name))
        .collect();
    let mut out = String::with_capacity(body.len() + args.len());
    let mut used = false;
    let mut rest = body;
    while let Some(at) = rest.find('$') {
        out.push_str(&rest[..at]);
        let after = &rest[at + 1..];
        let name_len = after
            .find(|c: char| !(c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_'))
            .unwrap_or(after.len());
        let name = &after[..name_len];
        if let Some(escaped) = after.strip_prefix('$') {
            out.push('$');
            rest = escaped;
        } else if name == "ARGUMENTS" {
            out.push_str(args);
            used = true;
            rest = &after[name_len..];
        } else if let Some(digit) = after.chars().next().and_then(|c| c.to_digit(10)) {
            let nth = usize::try_from(digit).unwrap_or(0);
            if nth > 0 {
                out.push_str(words.get(nth - 1).copied().unwrap_or_default());
                used = true;
                rest = &after[1..];
            } else {
                out.push('$');
                rest = after;
            }
        } else if let Some((_, value)) = named.iter().find(|(key, _)| *key == name) {
            out.push_str(value);
            used = true;
            rest = &after[name_len..];
        } else {
            out.push('$');
            rest = after;
        }
    }
    out.push_str(rest);
    let mut out = out.trim_end().to_owned();
    if !used && !args.is_empty() {
        out.push_str("\n\n");
        out.push_str(args);
    }
    out
}

fn is_placeholder_name(name: &str) -> bool {
    name.starts_with(|c: char| c.is_ascii_uppercase())
        && name
            .chars()
            .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
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

    use argonauts_core::input::slash_spec;

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
                assert!(seen.insert(name), "/{name} twice");
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
            // `codex exec` runs none of them as typed.
            assert_ne!(builtin.kind, BuiltinKind::Cli, "/{}", builtin.name);
        }
    }

    #[test]
    fn discovery_finds_skills_prompts_and_enabled_plugin_skills() {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path().join("repo");
        let folder = repo.join("app");
        let home = dir.path().join("home");
        fs::create_dir_all(repo.join(".git")).unwrap();
        write(
            &repo.join(".agents/skills/deploy/SKILL.md"),
            "---\nname: deploy\ndescription: Deploy the app\n---\n",
        );
        write(
            &folder.join(".agents/skills/lint/SKILL.md"),
            "---\nname: lint\ndescription: Lint this package\n---\n",
        );
        write(
            &dir.path().join(".agents/skills/outside/SKILL.md"),
            "---\nname: outside\ndescription: Above the repo\n---\n",
        );
        write(
            &home.join(".codex/skills/.system/imagegen/SKILL.md"),
            "---\nname: \"imagegen\"\ndescription: \"Make images\"\n---\n",
        );
        write(
            &home.join(".agents/skills/notes/SKILL.md"),
            "---\nname: notes\ndescription: Take notes\n---\n",
        );
        write(
            &home.join(".codex/prompts/draftpr.md"),
            "---\ndescription: Draft a pull request\nargument-hint: [FILES=<paths>]\n---\nDraft a PR for $FILES.\n",
        );
        let cache = home.join(".codex/plugins/cache/openai-primary-runtime");
        for version in ["9.2.0", "26.921.10847"] {
            write(
                &cache.join(format!("pdf/{version}/skills/pdf/SKILL.md")),
                &format!("---\nname: pdf\ndescription: PDFs {version}\n---\n"),
            );
        }
        write(
            &cache.join("documents/1.0/skills/docx/SKILL.md"),
            "---\nname: docx\ndescription: Word files\n---\n",
        );
        write(
            &home.join(".codex/config.toml"),
            "model = \"gpt-6\"\n\n[plugins.\"pdf@openai-primary-runtime\"]\nenabled = true\n\n[plugins.\"documents@openai-primary-runtime\"]\nenabled = false\n",
        );

        let found = discover(&CliScan::new(&folder, &home));
        let names: Vec<&str> = found.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(
            names,
            [
                "lint",
                "deploy",
                "notes",
                "imagegen",
                "prompts:draftpr",
                "pdf:pdf"
            ]
        );
        assert_eq!(found[5].help, "PDFs 26.921.10847", "the newest version");
        assert_eq!(found[4].args, "[FILES=<paths>]");
        assert!(matches!(
            found[0].source,
            CommandSource::Skill {
                scope: CommandScope::Project,
                ..
            }
        ));

        assert_eq!(prompt(&found[3], "").as_deref(), Some("$imagegen"));
        assert_eq!(
            prompt(&found[5], " a.pdf ").as_deref(),
            Some("$pdf:pdf a.pdf")
        );
        assert_eq!(
            prompt(&found[4], "FILES=src/main.rs").as_deref(),
            Some("Draft a PR for src/main.rs.")
        );
        let builtin = CliCommand::from(&BUILTIN_COMMANDS[0]);
        assert_eq!(prompt(&builtin, ""), None);
    }

    #[test]
    fn a_custom_prompt_takes_its_arguments_as_the_tui_puts_them_in() {
        assert_eq!(expand("Fix $ARGUMENTS now", "the bug"), "Fix the bug now");
        assert_eq!(expand("From $1 to $2, $3.", "a b"), "From a to b, .");
        assert_eq!(
            expand("Costs $$5 and $lower", "x"),
            "Costs $5 and $lower\n\nx"
        );
        assert_eq!(expand("No places\n", "keep me"), "No places\n\nkeep me");
        assert_eq!(expand("No places\n", ""), "No places");
        assert_eq!(
            expand("$TICKET: $NOTE", "TICKET=AB-1 NOTE=late"),
            "AB-1: late"
        );
    }
}
