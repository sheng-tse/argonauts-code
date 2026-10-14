//! Where learning keeps its files. Private state, such as the replay set and the hidden tests,
//! lives in the repo's state folder outside every sandbox's read scope. What you review, such as
//! learned workflows, skills, and notes, lives in the repo and changes only through commits.

use std::path::{Path, PathBuf};

use argonauts_core::layout::{project_files, repo_state};

/// The repo's state folder, `~/.argonauts/state/<repo-id>/`, and its main checkout.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Store {
    pub state_dir: PathBuf,
    pub repo_root: PathBuf,
}

/// Exported skills, the folder most CLIs read.
pub const SKILLS_DIR: &str = ".agents/skills";
/// Where Claude Code reads project skills; each exported skill is linked here.
pub const CLAUDE_SKILLS_DIR: &str = ".claude/skills";
pub const AGENTS_FILE: &str = "AGENTS.md";
pub const CLAUDE_FILE: &str = "CLAUDE.md";

impl Store {
    pub fn new(state_dir: impl Into<PathBuf>, repo_root: impl Into<PathBuf>) -> Self {
        Self {
            state_dir: state_dir.into(),
            repo_root: repo_root.into(),
        }
    }

    /// `outcomes/<run>.jsonl`, written by the engine's run review.
    pub fn outcomes_dir(&self) -> PathBuf {
        self.state_dir.join(repo_state::OUTCOMES)
    }

    pub fn replay_dir(&self) -> PathBuf {
        self.state_dir.join(repo_state::REPLAY)
    }

    pub fn evolution_dir(&self) -> PathBuf {
        self.state_dir.join(repo_state::EVOLUTION)
    }

    pub fn imports_dir(&self) -> PathBuf {
        self.state_dir.join(repo_state::IMPORTS)
    }

    pub fn argonauts_dir(&self) -> PathBuf {
        self.repo_root.join(".argonauts")
    }

    pub fn learned_workflows_dir(&self) -> PathBuf {
        self.argonauts_dir().join(project_files::LEARNED_WORKFLOWS)
    }

    /// Shared outcome logs, when `outcomes.share` is on.
    pub fn shared_outcomes_dir(&self) -> PathBuf {
        self.argonauts_dir().join(project_files::OUTCOMES)
    }

    pub fn notes_dir(&self) -> PathBuf {
        self.argonauts_dir().join(project_files::NOTES)
    }

    pub fn conventions(&self) -> PathBuf {
        self.argonauts_dir().join(project_files::CONVENTIONS)
    }

    pub fn skills_dir(&self) -> PathBuf {
        self.repo_root.join(SKILLS_DIR)
    }

    pub fn claude_skills_dir(&self) -> PathBuf {
        self.repo_root.join(CLAUDE_SKILLS_DIR)
    }

    /// A path inside the repo, as git names it.
    pub fn repo_relative<'a>(&self, path: &'a Path) -> Option<&'a Path> {
        path.strip_prefix(&self.repo_root).ok()
    }
}
