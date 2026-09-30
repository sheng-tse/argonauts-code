use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use argonauts_conductor::{Git, budget_problem};
use argonauts_core::{Config, ConfigSources, LoadedConfig};
use argonauts_tui::clean_line;

use crate::agents;
use crate::args::ConfigCommand;
use crate::usage::usage;

pub async fn repo_root(dir: &Path) -> Option<PathBuf> {
    Git::new().toplevel(dir).await.ok()
}

/// Resolves the defaults, the global file, both project files of `repo_root`, then `overrides`.
pub fn load(repo_root: Option<&Path>, overrides: Vec<String>) -> anyhow::Result<LoadedConfig> {
    Ok(Config::load(&ConfigSources::standard(
        repo_root, overrides,
    ))?)
}

/// Warnings can quote key names from a repository's config file, so they are cleaned first.
pub fn print_warnings(loaded: &LoadedConfig) {
    for warning in &loaded.warnings {
        eprintln!("warning: {}", clean_line(warning));
    }
}

pub async fn run(command: ConfigCommand) -> anyhow::Result<ExitCode> {
    match command {
        ConfigCommand::Schema => {
            println!("{}", serde_json::to_string_pretty(&Config::json_schema())?);
        }
        ConfigCommand::Show { set } => {
            let cwd = std::env::current_dir()?;
            let root = repo_root(&cwd).await;
            let loaded = load(root.as_deref(), set).map_err(usage)?;
            print_warnings(&loaded);
            print!("{}", show(&loaded)?);
        }
    }
    Ok(ExitCode::SUCCESS)
}

pub fn show(loaded: &LoadedConfig) -> anyhow::Result<String> {
    let mut out = String::new();
    writeln!(out, "# Resolved config, sha256 {}", loaded.hash())?;
    for layer in &loaded.layers {
        match &layer.path {
            None => writeln!(out, "# {}", layer.name)?,
            Some(path) => {
                let state = if layer.applied {
                    "applied"
                } else {
                    "not found"
                };
                writeln!(out, "# {} {} ({state})", layer.name, path.display())?;
            }
        }
    }
    for set in &loaded.overrides {
        writeln!(out, "# --set {}={}", set.key, set.value)?;
    }
    if let Ok(adapter) = agents::by_id(&loaded.config.default_agent)
        && let Some(problem) = budget_problem(&loaded.config, adapter.as_ref())
    {
        writeln!(out, "# warning: {problem}")?;
    }
    writeln!(out)?;
    out.push_str(&loaded.config.to_toml_string()?);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn load_with(project: &str, overrides: &[&str]) -> (tempfile::TempDir, LoadedConfig) {
        let dir = tempfile::tempdir().unwrap();
        let project_file = dir.path().join("project.toml");
        std::fs::write(&project_file, project).unwrap();
        let sources = ConfigSources {
            global: Some(dir.path().join("missing.toml")),
            project: Some(project_file),
            project_local: None,
            overrides: overrides.iter().map(|s| (*s).to_owned()).collect(),
        };
        let loaded = Config::load(&sources).unwrap();
        (dir, loaded)
    }

    #[test]
    fn show_is_valid_toml_that_names_its_layers() {
        let (_dir, loaded) = load_with("[budget]\nper_card = 1.5\n", &["merge.mode=leave"]);
        let text = show(&loaded).unwrap();
        assert!(text.contains("# defaults\n"), "{text}");
        assert!(text.contains("missing.toml (not found)"), "{text}");
        assert!(text.contains("project.toml (applied)"), "{text}");
        assert!(text.contains("# --set merge.mode=leave"), "{text}");
        assert!(text.contains(&loaded.hash()), "{text}");

        let reparsed = Config::from_toml_str(&text).unwrap();
        assert_eq!(reparsed, loaded.config);
        assert_eq!(reparsed.budget.per_card, 1.5);
    }

    #[test]
    fn show_warns_when_the_budget_cannot_be_enforced() {
        let (_dir, loaded) = load_with("default_agent = \"codex\"\n", &[]);
        let text = show(&loaded).unwrap();
        assert!(
            text.contains("# warning: Codex reports only tokens"),
            "{text}"
        );
        let (_dir, loaded) = load_with(
            "default_agent = \"codex\"\n",
            &["agents.codex.model=gpt-6-luna"],
        );
        assert!(!show(&loaded).unwrap().contains("# warning"));
        Config::from_toml_str(&show(&loaded).unwrap()).unwrap();
    }

    #[tokio::test]
    async fn repo_root_is_none_outside_git() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(repo_root(dir.path()).await, None);
    }
}
