//! The project instruction files Claude Code loads by itself, so a prompt names only the rest.
//! Since 2.1.277 its built-in AGENTS.md mod decides, by the "Project instructions" choice in
//! `/config`.

use std::fs;
use std::path::Path;

use serde_json::Value;

pub const CLAUDE_MD: &str = "CLAUDE.md";
pub const AGENTS_MD: &str = "AGENTS.md";

/// The mod whose options hold the choice.
const AGENTS_MOD: &str = "cc-plugin-agents-md@builtin";

/// "Project instructions" in `/config`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Choice {
    /// CLAUDE.md only.
    ClaudeMd,
    /// AGENTS.md when the project has no CLAUDE.md. The default.
    AgentsFallback,
    /// Both, a file CLAUDE.md already imports read once.
    Both,
    /// Only your organization's managed instructions.
    ManagedOnly,
}

impl Choice {
    /// The names 2.1.289 takes, with the older short ones it maps to them.
    fn parse(value: &str) -> Option<Self> {
        Some(match value {
            "claude-md" | "claude" => Self::ClaudeMd,
            "claude-md-or-agents-md" | "agents-fallback" => Self::AgentsFallback,
            "claude-md-and-agents-md" | "both" => Self::Both,
            "managed-only" | "none" => Self::ManagedOnly,
            _ => return None,
        })
    }
}

/// What Claude Code loads from `workdir`. An isolated session leaves the project's settings out
/// with `--setting-sources user`, and then loads neither file (2.1.289). `config_dir` holds
/// your settings, such as `~/.claude`.
pub fn loaded(workdir: &Path, config_dir: Option<&Path>, isolated: bool) -> Vec<String> {
    if isolated {
        return Vec::new();
    }
    let choice = choice(workdir, config_dir);
    let claude = [
        workdir.join(CLAUDE_MD),
        workdir.join(".claude").join(CLAUDE_MD),
    ]
    .into_iter()
    .find(|path| path.is_file());
    let has_agents = workdir.join(AGENTS_MD).is_file();
    let imports_agents = claude
        .as_deref()
        .is_some_and(|path| imports(path, AGENTS_MD));
    let (claude_md, agents_md) = match choice {
        Choice::ManagedOnly => (false, false),
        Choice::ClaudeMd => (claude.is_some(), imports_agents),
        Choice::AgentsFallback => (
            claude.is_some(),
            if claude.is_some() {
                imports_agents
            } else {
                has_agents
            },
        ),
        Choice::Both => (claude.is_some(), has_agents),
    };
    let mut files = Vec::new();
    if claude_md {
        files.push(CLAUDE_MD.to_owned());
    }
    if agents_md {
        files.push(AGENTS_MD.to_owned());
    }
    files
}

/// Your settings, then the project's, then its local file, a later one winning, as Claude Code
/// layers them. So the last file that says wins.
fn choice(workdir: &Path, config_dir: Option<&Path>) -> Choice {
    let mut files = Vec::new();
    if let Some(dir) = config_dir {
        files.push(dir.join("settings.json"));
    }
    files.push(workdir.join(".claude").join("settings.json"));
    files.push(workdir.join(".claude").join("settings.local.json"));
    files
        .iter()
        .rev()
        .find_map(|file| {
            let settings: Value = serde_json::from_str(&fs::read_to_string(file).ok()?).ok()?;
            let options = settings
                .get("pluginConfigs")?
                .get(AGENTS_MOD)?
                .get("options")?;
            Choice::parse(options.get("instructionFiles")?.as_str()?)
        })
        .unwrap_or(Choice::AgentsFallback)
}

/// A line of its own such as `@AGENTS.md`, which Claude Code reads as an import.
fn imports(file: &Path, name: &str) -> bool {
    let Ok(text) = fs::read_to_string(file) else {
        return false;
    };
    text.lines().any(|line| {
        let line = line.trim();
        line.strip_prefix('@')
            .map(|target| target.trim_start_matches("./"))
            .is_some_and(|target| target == name)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(path: &Path, text: &str) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, text).unwrap();
    }

    fn choose(config: &Path, choice: &str) {
        let settings = serde_json::json!({
            "pluginConfigs": {AGENTS_MOD: {"options": {"instructionFiles": choice}}}
        });
        write(&config.join("settings.json"), &settings.to_string());
    }

    #[test]
    fn claude_md_and_its_import_load_by_default() {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path().join("repo");
        let config = dir.path().join("claude");
        write(&repo.join("AGENTS.md"), "Use tabs.\n");
        assert_eq!(loaded(&repo, Some(&config), false), ["AGENTS.md"]);

        write(&repo.join("CLAUDE.md"), "Be brief.\n");
        assert_eq!(loaded(&repo, Some(&config), false), ["CLAUDE.md"]);
        write(&repo.join("CLAUDE.md"), "@AGENTS.md\n\nBe brief.\n");
        assert_eq!(
            loaded(&repo, Some(&config), false),
            ["CLAUDE.md", "AGENTS.md"]
        );
        // `--setting-sources user` loads neither.
        assert!(loaded(&repo, Some(&config), true).is_empty());
    }

    #[test]
    fn your_project_instructions_choice_decides() {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path().join("repo");
        let config = dir.path().join("claude");
        write(&repo.join("CLAUDE.md"), "Be brief.\n");
        write(&repo.join("AGENTS.md"), "Use tabs.\n");
        choose(&config, "claude-md-and-agents-md");
        assert_eq!(
            loaded(&repo, Some(&config), false),
            ["CLAUDE.md", "AGENTS.md"]
        );
        choose(&config, "claude-md");
        assert_eq!(loaded(&repo, Some(&config), false), ["CLAUDE.md"]);
        choose(&config, "managed-only");
        assert!(loaded(&repo, Some(&config), false).is_empty());
        // The project's local file outranks yours.
        write(
            &repo.join(".claude/settings.local.json"),
            r#"{"pluginConfigs": {"cc-plugin-agents-md@builtin": {"options": {"instructionFiles": "both"}}}}"#,
        );
        assert_eq!(
            loaded(&repo, Some(&config), false),
            ["CLAUDE.md", "AGENTS.md"]
        );
    }
}
