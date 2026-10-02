//! Where this process's files are, and whether a person or an agent is driving it.

use std::io::IsTerminal;
use std::path::{Path, PathBuf};

use anyhow::{Context as _, anyhow};
use argonauts_core::layout::repo_state;
use argonauts_core::paths::{project_config_path, project_local_config_path};
use argonauts_core::{Paths, RepoId, RunId};

/// Set in every agent session Argonauts starts.
pub(crate) const SESSION_ENV: &str = "ARGONAUTS_SESSION";

#[derive(Debug, Clone)]
pub(crate) struct AppEnv {
    pub paths: Paths,
    pub repo_root: PathBuf,
    pub repo_id: RepoId,
    /// `~/.argonauts/state/<repo>/`.
    pub state_dir: PathBuf,
    /// A person at a terminal: stdin and stdout are both terminals.
    pub interactive: bool,
    /// `ARGONAUTS_SESSION` is set, so this process runs inside an agent session.
    pub agent_session: bool,
    /// For showing paths with `~`.
    pub user_home: Option<PathBuf>,
}

impl AppEnv {
    /// For the repository holding `dir`, or the current directory, under `ARGONAUTS_HOME`.
    pub(crate) fn discover(dir: Option<&Path>) -> anyhow::Result<Self> {
        let paths = Paths::from_env()
            .map_err(|_| anyhow!("can't find your home directory; set ARGONAUTS_HOME"))?;
        let start = match dir {
            Some(dir) => dir.to_owned(),
            None => std::env::current_dir().context("reading the current directory")?,
        };
        let root = crate::git::toplevel(&start)?;
        let interactive = std::io::stdin().is_terminal() && std::io::stdout().is_terminal();
        Self::for_repo(paths, &root, interactive)
    }

    /// From explicit paths, as tests and the supervisor build it.
    pub(crate) fn for_repo(
        paths: Paths,
        repo_root: &Path,
        interactive: bool,
    ) -> anyhow::Result<Self> {
        let repo_root = repo_root
            .canonicalize()
            .with_context(|| format!("opening {}", repo_root.display()))?;
        let repo_id = RepoId::for_repo(&repo_root)?;
        let state_dir = paths.state_dir(&repo_id);
        Ok(Self {
            paths,
            repo_root,
            repo_id,
            state_dir,
            interactive,
            agent_session: std::env::var_os(SESSION_ENV).is_some(),
            user_home: std::env::var_os("HOME").map(PathBuf::from),
        })
    }

    pub(crate) fn runs_dir(&self) -> PathBuf {
        self.state_dir.join(repo_state::RUNS)
    }

    pub(crate) fn run_dir(&self, run: &RunId) -> PathBuf {
        self.paths.run_dir(&self.repo_id, run)
    }

    pub(crate) fn chat_dir(&self) -> PathBuf {
        self.state_dir.join(repo_state::CHAT)
    }

    pub(crate) fn global_config(&self) -> PathBuf {
        self.paths.global_config()
    }

    pub(crate) fn project_config(&self) -> PathBuf {
        project_config_path(&self.repo_root)
    }

    pub(crate) fn project_local_config(&self) -> PathBuf {
        project_local_config_path(&self.repo_root)
    }

    /// `path` with the user's home shown as `~`.
    pub(crate) fn display(&self, path: &Path) -> String {
        if let Some(home) = &self.user_home
            && let Ok(rest) = path.strip_prefix(home)
        {
            if rest.as_os_str().is_empty() {
                return "~".to_owned();
            }
            return format!("~/{}", rest.display());
        }
        path.display().to_string()
    }

    pub(crate) fn display_repo(&self) -> String {
        self.display(&self.repo_root)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn home_shows_as_a_tilde() {
        let repo = tempfile::tempdir().unwrap();
        let home = tempfile::tempdir().unwrap();
        let mut env = AppEnv::for_repo(Paths::new(home.path()), repo.path(), false).unwrap();
        env.user_home = Some(PathBuf::from("/Users/you"));
        assert_eq!(env.display(Path::new("/Users/you/code/app")), "~/code/app");
        assert_eq!(env.display(Path::new("/tmp/x")), "/tmp/x");
    }

    #[test]
    fn paths_under_argonauts_home() {
        let repo = tempfile::tempdir().unwrap();
        let home = tempfile::tempdir().unwrap();
        let env = AppEnv::for_repo(Paths::new(home.path()), repo.path(), false).unwrap();
        let run = RunId::new("run-03").unwrap();
        for path in [
            env.state_dir.clone(),
            env.runs_dir(),
            env.run_dir(&run),
            env.chat_dir(),
            env.global_config(),
            env.paths.global_presets(),
        ] {
            assert!(path.starts_with(home.path()), "{}", path.display());
            assert!(!path.starts_with(&env.repo_root), "{}", path.display());
        }
        assert!(env.run_dir(&run).starts_with(env.runs_dir()));
        for path in [env.project_config(), env.project_local_config()] {
            assert!(path.starts_with(&env.repo_root), "{}", path.display());
        }
    }
}
