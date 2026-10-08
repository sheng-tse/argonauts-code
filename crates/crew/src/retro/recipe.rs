//! Which recipe version a run used, recorded with the run when it starts: the app matches the
//! recipe at triage, applies its settings, and writes this file beside the run's settings. The
//! retro copies it into every outcome and the run line, so recipe statistics and the canary
//! read what actually ran.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use argonauts_core::layout::{project_files, repo_state};
use argonauts_learning::RecipeRef;
use argonauts_learning::evolution::state::CANDIDATE_DIRS;

/// `runs/<run>/recipe.json`.
pub const RECIPE_FILE: &str = "recipe.json";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunRecipe {
    pub recipe: RecipeRef,
    /// SHA-256 of the recipe file as it was read from the main checkout.
    pub sha256: String,
    /// One of the version's canary runs, which merge as pull requests.
    #[serde(default)]
    pub canary: bool,
    /// The folder the version file sits in: your checkout's `.argonauts/learned/<name>/`, or a
    /// candidate's folder in the conductor's state. The run reads the recipe's prompt and
    /// workflow files from here.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dir: Option<PathBuf>,
}

pub fn write(run_dir: &Path, recipe: &RunRecipe) -> std::io::Result<()> {
    std::fs::create_dir_all(run_dir)?;
    let text = serde_json::to_string_pretty(recipe).map_err(std::io::Error::other)?;
    std::fs::write(run_dir.join(RECIPE_FILE), text)
}

/// The run's recipe, if it used one. A file that doesn't parse counts as none, with a warning,
/// since the run's own result never depends on it.
pub fn read(run_dir: &Path) -> Option<RunRecipe> {
    let path = run_dir.join(RECIPE_FILE);
    let text = std::fs::read_to_string(&path).ok()?;
    match serde_json::from_str(&text) {
        Ok(recipe) => Some(recipe),
        Err(err) => {
            tracing::warn!("{}: {err}", path.display());
            None
        }
    }
}

/// `file` when it's a plain file right inside the run's recipe folder, the one place a prompt or
/// workflow named by its full path is read from, so no setting can hand an agent any other file.
pub fn recipe_file(run_dir: &Path, file: &Path) -> Option<PathBuf> {
    let dir = read(run_dir)?.dir?;
    if !is_recipe_folder(&dir) || file.parent() != Some(dir.as_path()) {
        return None;
    }
    // Not a link, which could point anywhere.
    let kind = std::fs::symlink_metadata(file).ok()?.file_type();
    kind.is_file().then(|| file.to_path_buf())
}

/// `.argonauts/learned/<name>` in a checkout, or `evolution/candidates/<id>` in a repo's state.
fn is_recipe_folder(dir: &Path) -> bool {
    let up = dir.parent();
    let parent = up.and_then(Path::file_name);
    let grandparent = up.and_then(Path::parent).and_then(Path::file_name);
    match (parent, grandparent) {
        (Some(parent), Some(grandparent)) => {
            (parent == project_files::RECIPES && grandparent == ".argonauts")
                || (parent == CANDIDATE_DIRS && grandparent == repo_state::EVOLUTION)
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_files_in_the_runs_recipe_folder_are_read() {
        let dir = tempfile::tempdir().unwrap();
        let run_dir = dir.path().join("run");
        let candidate = dir.path().join("state/evolution/candidates/api-feature-v5");
        let elsewhere = dir.path().join("elsewhere");
        for folder in [&candidate, &elsewhere] {
            std::fs::create_dir_all(folder).unwrap();
            std::fs::write(folder.join("build-v5.md"), "Build it the house way.").unwrap();
        }
        let file = candidate.join("build-v5.md");
        // No recipe, or a recipe from before folders were recorded: nothing by full path.
        assert_eq!(recipe_file(&run_dir, &file), None);
        let mut recorded = RunRecipe {
            recipe: RecipeRef {
                name: "api-feature".to_owned(),
                version: 5,
            },
            sha256: "ab".repeat(32),
            canary: false,
            dir: Some(candidate.clone()),
        };
        write(&run_dir, &recorded).unwrap();
        assert_eq!(recipe_file(&run_dir, &file), Some(file.clone()));
        for refused in [
            elsewhere.join("build-v5.md"),
            candidate.join("missing.md"),
            candidate.join("../api-feature-v5/build-v5.md"),
            candidate.join("nested/build-v5.md"),
        ] {
            assert_eq!(
                recipe_file(&run_dir, &refused),
                None,
                "{}",
                refused.display()
            );
        }
        std::os::unix::fs::symlink(elsewhere.join("build-v5.md"), candidate.join("link.md"))
            .unwrap();
        assert_eq!(recipe_file(&run_dir, &candidate.join("link.md")), None);
        // A folder that isn't a recipe's is never trusted, whatever the record says.
        recorded.dir = Some(elsewhere.clone());
        write(&run_dir, &recorded).unwrap();
        assert_eq!(recipe_file(&run_dir, &elsewhere.join("build-v5.md")), None);
        let checkout = dir.path().join("repo/.argonauts/learned/api-feature");
        std::fs::create_dir_all(&checkout).unwrap();
        std::fs::write(checkout.join("drafting.md"), "Draft it.").unwrap();
        recorded.dir = Some(checkout.clone());
        write(&run_dir, &recorded).unwrap();
        assert!(recipe_file(&run_dir, &checkout.join("drafting.md")).is_some());
    }

    #[test]
    fn the_run_recipe_reads_back() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(read(dir.path()), None);
        let recipe = RunRecipe {
            recipe: RecipeRef {
                name: "api-feature".to_owned(),
                version: 4,
            },
            sha256: "ab".repeat(32),
            canary: true,
            dir: None,
        };
        write(dir.path(), &recipe).unwrap();
        assert_eq!(read(dir.path()), Some(recipe));
    }
}
