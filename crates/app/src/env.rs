//! Where this process's files are, and whether a person or an agent is driving it.

use std::io::IsTerminal;
use std::path::{Path, PathBuf};

use anyhow::{Context as _, anyhow, bail};
use argonauts_core::layout::{project_files, repo_state};
use argonauts_core::paths::{project_config_path, project_local_config_path};
use argonauts_core::{Paths, RepoId, RunId};

use crate::{git, hidden_git};

/// Set in every agent session Argonauts starts.
pub(crate) const SESSION_ENV: &str = "ARGONAUTS_SESSION";

/// Where a folder's history lives.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum FolderGit {
    /// The folder is a git repository.
    Repository,
    /// No `.git` in the folder. Argonauts keeps its history at this path under its own home.
    Hidden(PathBuf),
    /// No git at all. Chat and solo write your files directly, and the first workflow that
    /// needs branches turns the hidden history on.
    Off,
}

impl FolderGit {
    /// Git has commits, branches and worktrees for this folder.
    pub(crate) fn has_history(&self) -> bool {
        !matches!(self, Self::Off)
    }
}

#[derive(Debug, Clone)]
pub(crate) struct AppEnv {
    pub paths: Paths,
    /// The project's folder: a git repository's top, or any folder that isn't in one.
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
    /// For the project holding `dir`, or the current directory, under `ARGONAUTS_HOME`. Any
    /// folder is a project: git is only needed where work runs side by side.
    pub(crate) fn discover(dir: Option<&Path>) -> anyhow::Result<Self> {
        let paths = Paths::from_env()
            .map_err(|_| anyhow!("can't find your home directory; set ARGONAUTS_HOME"))?;
        let start = match dir {
            Some(dir) => dir.to_owned(),
            None => std::env::current_dir().context("reading the current directory")?,
        };
        if !start.is_dir() {
            bail!("{} isn't a folder", start.display());
        }
        let root = git::project_root(&start);
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
        git::know_folder(&repo_root, &hidden_git::dir(&state_dir));
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

    /// What the folder has for git right now: its own `.git`, else the hidden history when
    /// it's on. Read each time, since `/git`, a run, or your own `git init` can change it.
    pub(crate) fn git(&self) -> FolderGit {
        let hidden = hidden_git::dir(&self.state_dir);
        if self.repo_root.join(".git").exists() {
            FolderGit::Repository
        } else if hidden_git::is_on(&hidden) {
            FolderGit::Hidden(hidden)
        } else {
            FolderGit::Off
        }
    }

    /// The settings a project shares. Started in your home folder, that path is your own
    /// settings file, which is read as yours already, so the project's is kept with its state.
    pub(crate) fn project_config(&self) -> PathBuf {
        let shared = project_config_path(&self.repo_root);
        let own = self.global_config();
        let folder = |file: &Path| file.parent().and_then(|dir| dir.canonicalize().ok());
        if folder(&shared).is_some_and(|dir| Some(dir) == folder(&own)) {
            return self.state_dir.join(project_files::CONFIG);
        }
        shared
    }

    /// The settings that are just yours for this project. A repository hides the file through
    /// `.git/info/exclude`. A folder without git has nothing to hide it with, and stays as it
    /// is, so the file is kept with the folder's state.
    pub(crate) fn project_local_config(&self) -> PathBuf {
        if self.git() == FolderGit::Repository {
            project_local_config_path(&self.repo_root)
        } else {
            self.state_dir.join(project_files::CONFIG_LOCAL)
        }
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

    /// A file in the repo by its path from the repo, such as `.argonauts/config.toml`, and any
    /// other as [`Self::display`] shows it.
    pub(crate) fn shown(&self, path: &Path) -> String {
        match path.strip_prefix(&self.repo_root) {
            Ok(relative) if !relative.as_os_str().is_empty() => relative.display().to_string(),
            _ => self.display(path),
        }
    }

    /// `text` with each file in the repo named by its path from the repo, for messages from
    /// parts that only know full paths.
    pub(crate) fn shown_in(&self, text: &str) -> String {
        let root = format!("{}/", self.repo_root.display());
        if root == "/" {
            return text.to_owned();
        }
        let mut out = String::with_capacity(text.len());
        let mut rest = text;
        while let Some(at) = rest.find(&root) {
            let (before, after) = rest.split_at(at);
            out.push_str(before);
            // Only a whole path, not the tail of a longer one.
            let starts_a_path = before
                .chars()
                .next_back()
                .is_none_or(|c| !(c.is_alphanumeric() || matches!(c, '/' | '.' | '-' | '_')));
            if !starts_a_path {
                out.push_str(&root);
            }
            rest = &after[root.len()..];
        }
        out.push_str(rest);
        out
    }

    pub(crate) fn display_repo(&self) -> String {
        self.display(&self.repo_root)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn files_in_the_repo_are_named_from_the_repo() {
        let repo = tempfile::tempdir().unwrap();
        let home = tempfile::tempdir().unwrap();
        let mut env = AppEnv::for_repo(Paths::new(home.path()), repo.path(), false).unwrap();
        let root = env.repo_root.clone();
        let outside = root.parent().unwrap().to_owned();
        env.user_home = Some(outside.clone());
        assert_eq!(
            env.shown(&root.join(".argonauts/config.toml")),
            ".argonauts/config.toml"
        );
        let name = root.file_name().unwrap().to_string_lossy();
        assert_eq!(env.shown(&root), format!("~/{name}"));
        assert_eq!(env.shown(&outside.join("x.toml")), "~/x.toml");
        let root = root.display();
        assert_eq!(
            env.shown_in(&format!(
                "invalid config in {root}/.argonauts/config.local.toml: see `{root}/a.rs` and \
                 /x{root}/b.rs"
            )),
            format!("invalid config in .argonauts/config.local.toml: see `a.rs` and /x{root}/b.rs")
        );
    }

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
        assert!(env.project_config().starts_with(&env.repo_root));
    }

    #[test]
    fn a_repository_keeps_your_own_settings_in_it_and_a_plain_folder_with_its_state() {
        let repo = argonauts_testkit::TempRepo::new().unwrap();
        let env = AppEnv::for_repo(Paths::new(repo.home()), repo.root(), false).unwrap();
        assert_eq!(env.git(), FolderGit::Repository);
        assert!(env.project_local_config().starts_with(&env.repo_root));

        let folder = tempfile::tempdir().unwrap();
        let home = tempfile::tempdir().unwrap();
        let env = AppEnv::for_repo(Paths::new(home.path()), folder.path(), false).unwrap();
        assert_eq!(env.git(), FolderGit::Off);
        assert!(!env.git().has_history());
        assert!(env.project_config().starts_with(&env.repo_root));
        assert!(env.project_local_config().starts_with(&env.state_dir));
    }

    #[test]
    fn started_in_your_home_the_project_settings_are_not_your_own_file() {
        let account = tempfile::tempdir().unwrap();
        let paths = Paths::new(account.path().join(".argonauts"));
        std::fs::create_dir_all(paths.home()).unwrap();
        let env = AppEnv::for_repo(paths, account.path(), false).unwrap();
        assert_ne!(env.project_config(), env.global_config());
        assert!(env.project_config().starts_with(&env.state_dir));
    }

    #[test]
    fn any_folder_is_a_project_and_a_repository_is_one_from_its_top() {
        let repo = argonauts_testkit::TempRepo::new().unwrap();
        let top = repo.root().canonicalize().unwrap();
        let inside = top.join("src/deep");
        std::fs::create_dir_all(&inside).unwrap();
        assert_eq!(git::project_root(&inside), top);

        // A folder its repository ignores would show a run none of its files.
        repo.write(".gitignore", "scratch/\n").unwrap();
        let scratch = top.join("scratch");
        std::fs::create_dir_all(&scratch).unwrap();
        assert_eq!(git::project_root(&scratch), scratch);
        let env = AppEnv::for_repo(Paths::new(repo.home()), &scratch, false).unwrap();
        assert_eq!(env.git(), FolderGit::Off);
        assert!(
            !git::has_git(&env.repo_root),
            "git must not walk up into the repository that ignores the folder"
        );
        assert!(git::has_git(&top));

        let plain = tempfile::tempdir().unwrap();
        let plain = plain.path().canonicalize().unwrap();
        assert_eq!(git::project_root(&plain), plain);
    }
}
