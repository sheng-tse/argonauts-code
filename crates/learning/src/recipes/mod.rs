//! Recipes: a versioned workflow for one task type, with the protocol settings tuned for it.
//! Each version is a YAML file, `.argonauts/recipes/<name>/v<N>.yaml`, that names its parent
//! and never changes after it's written. Statistics never live in the file: they come from the
//! outcome log, so a run never edits a recipe.
//!
//! A recipe sits in the preset layer. It can't set a safety key or a fixed-core key, so neither
//! a recipe you copied nor an evolved one can loosen what guards the crew.

mod diff;
mod graph;
mod keys;
pub mod loops;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use argonauts_core::layout::project_files::RECIPES;

use crate::error::{LearningError, Result};
use crate::store::Store;
use crate::types::RecipeRef;

pub use diff::{diff, load_workflow};
pub use graph::{PROTECTED_KINDS, protected_counts};
pub use keys::{
    CHECKLIST_KEY, WORKFLOW_KEY, class_for_key, class_name, flatten, guarded, is_fixed_core_key,
    is_safety_key, key_matches, patterns_overlap,
};

/// One version of a recipe, as written.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Recipe {
    pub recipe: String,
    pub version: u32,
    /// The version this one evolved from.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<u32>,
    pub description: String,
    /// The preset whose settings it starts from.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preset: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<RecipeMode>,
    /// A workflow name, or a `.star` file beside the recipe that builds one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workflow: Option<String>,
    #[serde(default, rename = "match")]
    pub matches: Match,
    /// Prompt templates beside the recipe, by the `prompts.*` key they replace.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub prompts: BTreeMap<String, String>,
    /// Every other key: protocol settings, applied in the preset layer.
    #[serde(flatten)]
    pub settings: BTreeMap<String, serde_json::Value>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RecipeMode {
    Solo,
    Crew,
}

/// Which tasks a recipe is for.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Match {
    #[serde(default)]
    pub task_types: Vec<String>,
    #[serde(default)]
    pub labels_any: Vec<String>,
}

/// A recipe version as read from disk, with the hash a run records.
#[derive(Debug, Clone, PartialEq)]
pub struct LoadedRecipe {
    pub recipe: Recipe,
    pub path: PathBuf,
    pub text: String,
    pub sha256: String,
}

impl LoadedRecipe {
    pub fn reference(&self) -> RecipeRef {
        RecipeRef {
            name: self.recipe.recipe.clone(),
            version: self.recipe.version,
        }
    }
}

/// A recipe's versions, oldest first, and the one that runs.
#[derive(Debug, Clone, PartialEq)]
pub struct RecipeSet {
    pub name: String,
    pub versions: Vec<LoadedRecipe>,
    /// From `<name>/current`, else the newest version.
    pub current: u32,
}

impl RecipeSet {
    pub fn current(&self) -> Option<&LoadedRecipe> {
        self.version(self.current)
    }

    pub fn version(&self, version: u32) -> Option<&LoadedRecipe> {
        self.versions
            .iter()
            .find(|loaded| loaded.recipe.version == version)
    }
}

/// Top-level keys that would hold a statistic. Pass rates and costs per version come from the
/// outcome log, so two runs never race to edit a recipe and a file can't vouch for itself.
const STATISTICS: &[&str] = &[
    "stats",
    "statistics",
    "pass_rate",
    "passes",
    "runs",
    "wins",
    "losses",
    "win_rate",
    "success_rate",
    "cost_usd",
    "mean_cost_usd",
    "score",
    "scores",
    "last_run",
    "last_promoted",
];

/// The prompt templates a recipe may replace. Judge and reviewer prompts never have a key.
const PROMPT_KEYS: &[&str] = &["drafting", "build"];

/// The file that names the version that runs.
pub const CURRENT_FILE: &str = "current";

/// Every recipe in the main checkout. A file that doesn't parse or breaks a rule is left out
/// with a warning, never half-applied.
pub fn load_all(store: &Store) -> Result<Vec<RecipeSet>> {
    let root = store.recipes_dir();
    if !root.is_dir() {
        return Ok(Vec::new());
    }
    let mut folders = Vec::new();
    for entry in std::fs::read_dir(&root)? {
        let path = entry?.path();
        if path.is_dir() {
            folders.push(path);
        }
    }
    folders.sort();
    let mut sets = Vec::new();
    for folder in folders {
        if let Some(set) = load_set(&folder)? {
            sets.push(set);
        }
    }
    Ok(sets)
}

/// One recipe's folder, or `None` when nothing in it can be used.
pub fn load_set(folder: &Path) -> Result<Option<RecipeSet>> {
    let Some(name) = folder.file_name().and_then(|name| name.to_str()) else {
        return Ok(None);
    };
    let mut versions = Vec::new();
    for entry in std::fs::read_dir(folder)? {
        let path = entry?.path();
        let Some(number) = version_of_file(&path) else {
            continue;
        };
        match load_file(&path) {
            Ok(loaded) => {
                let mut problems = validate(&loaded.recipe);
                if loaded.recipe.recipe != name {
                    problems.push(format!(
                        "it names the recipe `{}`, but its folder is `{name}`",
                        loaded.recipe.recipe
                    ));
                }
                if loaded.recipe.version != number {
                    problems.push(format!(
                        "it says version {}, but its file is v{number}.yaml",
                        loaded.recipe.version
                    ));
                }
                if problems.is_empty() {
                    versions.push(loaded);
                } else {
                    tracing::warn!("leaving out {}: {}", path.display(), problems.join("; "));
                }
            }
            Err(error) => tracing::warn!("leaving out {}: {error}", path.display()),
        }
    }
    versions.sort_by_key(|loaded| loaded.recipe.version);
    let known: Vec<u32> = versions
        .iter()
        .map(|loaded| loaded.recipe.version)
        .collect();
    versions.retain(|loaded| match loaded.recipe.parent {
        Some(parent) if !known.contains(&parent) => {
            tracing::warn!(
                "leaving out {}: its parent v{parent} isn't there",
                loaded.path.display()
            );
            false
        }
        _ => true,
    });
    let Some(newest) = versions.last().map(|loaded| loaded.recipe.version) else {
        return Ok(None);
    };
    let current = match read_current(folder)? {
        Some(version) if versions.iter().any(|l| l.recipe.version == version) => version,
        Some(version) => {
            tracing::warn!(
                "{}/{CURRENT_FILE} names v{version}, which isn't usable; running v{newest}",
                folder.display()
            );
            newest
        }
        None => newest,
    };
    Ok(Some(RecipeSet {
        name: name.to_owned(),
        versions,
        current,
    }))
}

/// Reads and parses one version file, without the folder's checks.
pub fn load_file(path: &Path) -> Result<LoadedRecipe> {
    let text = std::fs::read_to_string(path)?;
    let recipe = parse(path, &text)?;
    Ok(LoadedRecipe {
        recipe,
        path: path.to_owned(),
        sha256: argonauts_core::sha256_hex(text.as_bytes()),
        text,
    })
}

pub fn parse(path: &Path, text: &str) -> Result<Recipe> {
    serde_saphyr::from_str(text).map_err(|error| LearningError::Parse {
        path: path.to_owned(),
        message: error.to_string(),
    })
}

/// The YAML a version file holds.
pub fn render(recipe: &Recipe) -> Result<String> {
    serde_saphyr::to_string(recipe).map_err(|error| LearningError::Parse {
        path: PathBuf::from(format!("{}/v{}.yaml", recipe.recipe, recipe.version)),
        message: error.to_string(),
    })
}

/// `v3.yaml` is version 3.
pub fn version_of_file(path: &Path) -> Option<u32> {
    let file = path.file_name()?.to_str()?;
    let number = file.strip_prefix('v')?.strip_suffix(".yaml")?;
    number.parse().ok().filter(|version| *version > 0)
}

pub fn version_file(version: u32) -> String {
    format!("v{version}.yaml")
}

/// The `current` pointer, which holds `3` or `v3`.
pub fn read_current(folder: &Path) -> Result<Option<u32>> {
    let path = folder.join(CURRENT_FILE);
    if !path.is_file() {
        return Ok(None);
    }
    let text = std::fs::read_to_string(&path)?;
    let text = text.trim();
    text.trim_start_matches('v')
        .parse()
        .map(Some)
        .map_err(|_| LearningError::Parse {
            path,
            message: format!("`{text}` isn't a version number"),
        })
}

/// Why a recipe can't be used: a safety or fixed-core key, a statistic in the file, a version
/// or parent that doesn't fit, or a match with nothing in it. Empty when it's fine.
pub fn validate(recipe: &Recipe) -> Vec<String> {
    let mut problems = Vec::new();
    if !valid_name(&recipe.recipe) {
        problems.push(format!(
            "`{}` isn't a recipe name: use lowercase letters, digits, and dashes",
            recipe.recipe
        ));
    }
    if recipe.version == 0 {
        problems.push("versions start at 1".to_owned());
    }
    match recipe.parent {
        Some(parent) if parent == 0 || parent >= recipe.version => problems.push(format!(
            "its parent v{parent} must be an earlier version than v{}",
            recipe.version
        )),
        _ => {}
    }
    if recipe.matches.task_types.is_empty() && recipe.matches.labels_any.is_empty() {
        problems.push("its `match` names no task type or label, so it fits nothing".to_owned());
    }
    for key in recipe.settings.keys() {
        if STATISTICS.contains(&key.as_str()) {
            problems.push(format!(
                "`{key}` is a statistic; a recipe's statistics come from the outcome log, never \
                 its file"
            ));
        }
    }
    for key in flatten(&recipe.settings).keys() {
        if let Some(reason) = guarded(key) {
            problems.push(reason);
        }
    }
    for (key, file) in &recipe.prompts {
        if !PROMPT_KEYS.contains(&key.as_str()) {
            problems.push(format!(
                "`prompts.{key}` isn't a template a recipe can replace; only `drafting` and \
                 `build` are"
            ));
        }
        if !plain_file(file) {
            problems.push(format!(
                "`prompts.{key}` names `{file}`, which must be a file beside the recipe"
            ));
        }
    }
    if let Some(workflow) = &recipe.workflow {
        if workflow.trim().is_empty() {
            problems.push("its `workflow` is empty".to_owned());
        } else if is_workflow_file(workflow) && !plain_file(workflow) {
            problems.push(format!(
                "its workflow `{workflow}` must be a file beside the recipe"
            ));
        }
    }
    problems
}

fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && !name.starts_with('-')
        && name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_')
}

/// A file name with no folder in it, so a recipe can only point beside itself.
fn plain_file(name: &str) -> bool {
    !name.is_empty()
        && !name.starts_with('.')
        && !name.contains('/')
        && !name.contains('\\')
        && !name.contains("..")
}

pub fn is_workflow_file(workflow: &str) -> bool {
    [".star", ".yaml", ".yml"]
        .iter()
        .any(|extension| workflow.ends_with(extension))
}

/// The recipe a task gets: the current version of the one whose `match` fits its task type and
/// labels best.
pub fn matching<'a>(
    sets: &'a [RecipeSet],
    task_type: &str,
    labels: &[String],
) -> Option<&'a LoadedRecipe> {
    let mut best: Option<(u32, &LoadedRecipe)> = None;
    for set in sets {
        let Some(loaded) = set.current() else {
            continue;
        };
        let Some(score) = fit(&loaded.recipe.matches, task_type, labels) else {
            continue;
        };
        // Ties go to the first by name, since `sets` is sorted, so the pick never wobbles.
        if best.is_none_or(|(top, _)| score > top) {
            best = Some((score, loaded));
        }
    }
    best.map(|(_, loaded)| loaded)
}

/// How well a match fits: a task type counts more than any one label. `None` when it doesn't.
fn fit(matches: &Match, task_type: &str, labels: &[String]) -> Option<u32> {
    let type_fits = matches.task_types.iter().any(|t| t == task_type);
    if !matches.task_types.is_empty() && !type_fits {
        return None;
    }
    let shared = matches
        .labels_any
        .iter()
        .filter(|label| labels.contains(label))
        .count() as u32;
    if !matches.labels_any.is_empty() && shared == 0 {
        return None;
    }
    Some(u32::from(type_fits) * 100 + shared)
}

/// The folder a version file sits in, where its prompts and `.star` files are.
pub fn recipe_dir(loaded: &LoadedRecipe) -> &Path {
    loaded.path.parent().unwrap_or(Path::new("."))
}

/// A file beside the recipe as git names it from the repo root, `.argonauts/recipes/<name>/<file>`.
pub fn repo_path(recipe: &str, file: &str) -> String {
    format!(".argonauts/{RECIPES}/{recipe}/{file}")
}

/// The recipe's settings as a TOML table for the preset layer. Its workflow and prompt files
/// are named by their paths beside the version file, in your checkout's recipe folder or a
/// candidate's folder in the conductor's state, since a candidate's files are nowhere else and
/// a replay runs in another repository.
pub fn settings_overlay(loaded: &LoadedRecipe) -> Result<toml::Table> {
    let recipe = &loaded.recipe;
    let mut table = toml::Table::new();
    for (key, value) in &recipe.settings {
        let converted = json_to_toml(value).ok_or_else(|| LearningError::Parse {
            path: loaded.path.clone(),
            message: format!("`{key}` holds a value TOML can't carry, such as a null"),
        })?;
        table.insert(key.clone(), converted);
    }
    let beside = |file: &str| recipe_dir(loaded).join(file).to_string_lossy().into_owned();
    if let Some(workflow) = &recipe.workflow {
        let value = if is_workflow_file(workflow) {
            beside(workflow)
        } else {
            workflow.clone()
        };
        table.insert("workflow".to_owned(), toml::Value::String(value));
    }
    if !recipe.prompts.is_empty() {
        let mut prompts = toml::Table::new();
        for (key, file) in &recipe.prompts {
            prompts.insert(key.clone(), toml::Value::String(beside(file)));
        }
        table.insert("prompts".to_owned(), toml::Value::Table(prompts));
    }
    Ok(table)
}

fn json_to_toml(value: &serde_json::Value) -> Option<toml::Value> {
    use serde_json::Value as Json;
    Some(match value {
        Json::Null => return None,
        Json::Bool(flag) => toml::Value::Boolean(*flag),
        Json::Number(number) => match number.as_i64() {
            Some(int) => toml::Value::Integer(int),
            None => toml::Value::Float(number.as_f64()?),
        },
        Json::String(text) => toml::Value::String(text.clone()),
        Json::Array(items) => {
            toml::Value::Array(items.iter().map(json_to_toml).collect::<Option<_>>()?)
        }
        Json::Object(map) => {
            let mut table = toml::Table::new();
            for (key, value) in map {
                table.insert(key.clone(), json_to_toml(value)?);
            }
            toml::Value::Table(table)
        }
    })
}

/// A value written as TOML, such as a retro's proposed `["a", "b"]`, as the JSON a recipe holds.
pub fn toml_to_json(text: &str) -> std::result::Result<serde_json::Value, String> {
    let table: toml::Table = toml::from_str(&format!("value = {text}"))
        .map_err(|error| format!("`{text}` isn't a TOML value: {error}"))?;
    let value = table
        .get("value")
        .ok_or_else(|| format!("`{text}` isn't a TOML value"))?;
    serde_json::to_value(value).map_err(|error| error.to_string())
}

/// A class of change, as `approvals.recipe_promotion.rules.change_classes` and
/// `evolution.not_worse_classes` name it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChangeClass {
    /// An item added to or removed from `critique.checklist`.
    ChecklistItem,
    /// `critique.rounds`.
    CritiqueRounds,
    /// `critique.critic`.
    CriticPolicy,
    /// `prompts.drafting`.
    DraftingPrompt,
    /// `prompts.build`.
    BuildPrompt,
    /// `divide.*`.
    CardSplit,
    /// `budget.per_card_usd` and fan-out caps.
    CardBudget,
    /// Who is in the crew, keeping distinct model families.
    CrewComposition,
    /// A step of the workflow added, removed, or rewired.
    StepGraph,
    /// Any other key on your evolvable list.
    Setting,
}

/// One difference between two versions of a recipe, found by a structural diff.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Change {
    /// The settings key, `prompts.<name>`, or `workflow` for a step.
    pub key: String,
    pub class: ChangeClass,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub before: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub after: Option<serde_json::Value>,
}
