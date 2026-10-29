//! How a run's work reaches your folder, with git or without it.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// `[commit]`: which new files a task's commit takes. Changes to files git already tracks
/// are always committed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
pub struct CommitSettings {
    /// What happens to a new file no agent meant to create, one that no edit tool wrote and
    /// no report lists, such as a cache or build output a command left behind: it's left out
    /// of the commit and named, or kept like any other file.
    pub unclaimed: Unclaimed,
    /// New files that are always committed, whoever made them: files a tool outside the
    /// agent's view makes and the project needs, such as `*.meta`. Patterns as in
    /// `merge.leave_out`, which wins where both name a file.
    pub always_keep: Vec<String>,
    /// A new file bigger than this many megabytes is left out of the commit whoever made it,
    /// and named with its size. Files your `.gitattributes` give to Git LFS are exempt. 0
    /// turns the limit off.
    pub max_new_file_mb: u64,
}

impl Default for CommitSettings {
    fn default() -> Self {
        Self {
            unclaimed: Unclaimed::default(),
            always_keep: Vec::new(),
            max_new_file_mb: 50,
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Unclaimed {
    /// Left out of the commit, kept in the task's worktree, and named in the transcript.
    #[default]
    Leave,
    /// Committed, as every new file git doesn't ignore.
    Keep,
}

/// `[delivery]`: what a merged run leaves on your branch.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
pub struct DeliverySettings {
    /// What lands when a run merges: one commit for the whole run, one for each task, or the
    /// run's own history with every agent's commits and the merges between them.
    pub commits: DeliveryCommits,
    /// Whether a merged run is committed on your branch, or only written into your files for
    /// you to look over and commit yourself.
    pub write: DeliveryWrite,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum DeliveryCommits {
    /// One commit on top of your branch, holding everything the run changed.
    #[default]
    Squash,
    /// One commit for each task, in the order they merged.
    PerTask,
    /// The run's history as it happened.
    AsRun,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum DeliveryWrite {
    /// Your branch moves to the run's commits.
    #[default]
    Commit,
    /// Your files change and your branch stays where it is. The run's commits stay on its
    /// own branch.
    Files,
}

/// `merge.unborn`: how a run lands when your branch had no commit at its start, so there was
/// nothing to move forward from.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum UnbornLanding {
    /// Its work is written into your files, and nothing is committed or staged: your first
    /// commit is yours to make.
    #[default]
    Files,
    /// It makes your branch's first commit, holding your files as the run found them and
    /// its work. Files you changed meanwhile show as changes of yours.
    FirstCommit,
}

/// `[history]`: the history Argonauts Code keeps of a folder that has no git of its own, under its
/// own home, so a workflow there has branches to work on and your folder gets no `.git`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
pub struct HistorySettings {
    /// Whether a folder without git may keep that history: from a workflow's first record, or
    /// from Captain's first write, so `/rewind` works there. Off leaves such a folder to
    /// conversation and Captain with nothing to rewind to, and a workflow there says git is
    /// needed.
    pub enabled: bool,
    /// A new file bigger than this many megabytes stays out of the history, and is listed. 0
    /// turns the limit off.
    pub max_file_mb: u64,
    /// A folder holding more megabytes than this isn't recorded the first time. 0 turns the
    /// limit off.
    pub max_total_mb: u64,
    /// Nor is a folder with more files than this. 0 turns the limit off.
    pub max_files: u64,
    /// A record that would leave less than this many gigabytes free on the disk it goes to
    /// isn't made, the first or any later one. 0 turns the check off.
    pub min_free_gb: u64,
    /// Changes to the list of what the history never records, which starts from secrets
    /// (`.env`, `*.pem`), packages (`node_modules/`, `.venv/`) and the built-in build output.
    /// A pattern adds to the list, and one after `!` takes that pattern from it.
    pub leave_out: Vec<String>,
}

impl Default for HistorySettings {
    fn default() -> Self {
        Self {
            enabled: true,
            max_file_mb: 50,
            max_total_mb: 2048,
            max_files: 100_000,
            min_free_gb: 5,
            leave_out: Vec::new(),
        }
    }
}
