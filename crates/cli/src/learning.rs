//! `argonauts records`, `learned`, `skills`, `sync`, `retro`, and `mcp package`: the learning
//! stream's commands for scripts and the shell. Read commands print text, or JSON with
//! `--json`.

use std::path::PathBuf;
use std::process::ExitCode;

use argonauts_app::{LearningShell, learning};
use argonauts_core::sanitize::sanitize_for_terminal;

use crate::args::{RecipesAction, RecordsAction, SkillsAction};

/// Runs `command` and prints what it said.
pub async fn run(command: LearningShell, json: bool, set: &[String]) -> anyhow::Result<ExitCode> {
    let output = learning(command, None, set.to_vec()).await?;
    if json {
        println!("{}", serde_json::to_string_pretty(&output.json)?);
    } else {
        print!("{}", sanitize_for_terminal(&output.text));
        if !output.text.ends_with('\n') {
            println!();
        }
    }
    Ok(ExitCode::SUCCESS)
}

pub fn records(action: Option<RecordsAction>, as_of: Option<String>) -> LearningShell {
    LearningShell::Records {
        as_of,
        rebuild: action == Some(RecordsAction::Rebuild),
    }
}

pub fn recipes(action: Option<RecipesAction>) -> LearningShell {
    match action.unwrap_or(RecipesAction::List) {
        RecipesAction::List => LearningShell::RecipesList,
        RecipesAction::Show { name } => LearningShell::RecipesShow { name },
        RecipesAction::Diff { name, from, to } => LearningShell::RecipesDiff { name, from, to },
        RecipesAction::Replay => LearningShell::RecipesReplay,
        RecipesAction::Promote { candidate } => LearningShell::RecipesPromote { candidate },
        RecipesAction::Rollback { name } => LearningShell::RecipesRollback { name },
    }
}

pub fn skills(action: Option<SkillsAction>) -> LearningShell {
    match action.unwrap_or(SkillsAction::List) {
        SkillsAction::List => LearningShell::SkillsList,
        SkillsAction::Export { recipe } => LearningShell::SkillsExport { recipe },
        SkillsAction::Import { source } => LearningShell::SkillsImport { source },
    }
}

pub fn package(dir: PathBuf, repo: PathBuf) -> LearningShell {
    LearningShell::McpPackage { dir, repo }
}

#[cfg(test)]
mod tests {
    use clap::Parser as _;

    use super::*;
    use crate::args::{Cli, Command, McpCommand};

    fn parse(line: &[&str]) -> Command {
        let mut argv = vec!["argonauts"];
        argv.extend_from_slice(line);
        Cli::try_parse_from(argv)
            .unwrap_or_else(|err| panic!("{line:?}: {err}"))
            .command
            .unwrap_or_else(|| panic!("{line:?} has a command"))
    }

    #[test]
    fn the_learning_commands_parse_as_the_brief_lists_them() {
        let Command::Records {
            action,
            as_of,
            json,
        } = parse(&["records", "rebuild", "--as-of", "2026-09-30", "--json"])
        else {
            panic!("records");
        };
        assert_eq!(
            records(action, as_of),
            LearningShell::Records {
                as_of: Some("2026-09-30".to_owned()),
                rebuild: true,
            }
        );
        assert!(json);
        let Command::Recipes { action, json } = parse(&["learned", "diff", "api", "--from", "3"])
        else {
            panic!("learned");
        };
        assert!(!json);
        assert_eq!(
            recipes(action),
            LearningShell::RecipesDiff {
                name: "api".to_owned(),
                from: Some(3),
                to: None,
            }
        );
        let Command::Recipes { action, json } = parse(&["learned", "--json"]) else {
            panic!("learned");
        };
        assert!(json);
        assert_eq!(recipes(action), LearningShell::RecipesList);
        let Command::Skills { action, .. } = parse(&["skills", "import", "../their-skill"]) else {
            panic!("skills");
        };
        assert_eq!(
            skills(action),
            LearningShell::SkillsImport {
                source: "../their-skill".to_owned()
            }
        );
        assert!(matches!(
            parse(&["sync", "--check"]),
            Command::Sync {
                check: true,
                force: false,
                ..
            }
        ));
        assert!(Cli::try_parse_from(["argonauts", "sync", "--check", "--force"]).is_err());
        assert!(matches!(
            parse(&["run-review", "run-03"]),
            Command::Retro { .. }
        ));
        assert!(matches!(
            parse(&["mcp", "package", "out", "--repo", "."]),
            Command::Mcp(McpCommand::Package { .. })
        ));
    }
}
