//! Where Argonauts keeps state, worktrees, and config: under `ARGONAUTS_HOME` (default
//! `~/.argonauts`), and in a repo's own `.argonauts/`.

use std::ffi::OsString;
use std::fs::{self, DirBuilder, OpenOptions, Permissions};
use std::io::{self, Write};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

use crate::ids::{CardId, RepoId, RunId};

pub const HOME_ENV: &str = "ARGONAUTS_HOME";

pub const EVENT_LOG_FILE: &str = "events.jsonl";

/// Set for everything cargo runs in this workspace, by its `.cargo/config.toml`: the tests, the
/// programs they start, and `cargo run`. None of them may touch your own `~/.argonauts`.
pub const TESTING_ENV: &str = "ARGONAUTS_TESTING";

/// The home tests share when they don't set `ARGONAUTS_HOME`, in the temp folder. Shared, so a
/// test and the supervisor it starts find the same runs.
const SCRATCH_HOME: &str = "argonauts-test-home";

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("could not find the home directory; set {HOME_ENV}")]
pub struct NoHomeDir;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Paths {
    home: PathBuf,
}

impl Paths {
    pub fn new(home: impl Into<PathBuf>) -> Self {
        Self { home: home.into() }
    }

    /// Paths rooted at `ARGONAUTS_HOME`, or `~/.argonauts` when it is unset or empty. Under
    /// test, never your own: see [`test_home`].
    pub fn from_env() -> Result<Self, NoHomeDir> {
        let env_home = std::env::var_os(HOME_ENV);
        if testing() {
            let home = test_home(env_home, &std::env::temp_dir(), account_home().as_deref());
            return Ok(Self { home });
        }
        Self::resolve(env_home, dirs::home_dir())
    }

    fn resolve(env_home: Option<OsString>, user_home: Option<PathBuf>) -> Result<Self, NoHomeDir> {
        let home = match env_home.filter(|h| !h.is_empty()) {
            Some(home) => absolute(PathBuf::from(home)),
            None => user_home.ok_or(NoHomeDir)?.join(".argonauts"),
        };
        Ok(Self { home })
    }

    pub fn home(&self) -> &Path {
        &self.home
    }

    pub fn state_dir(&self, repo: &RepoId) -> PathBuf {
        self.home.join("state").join(repo.as_str())
    }

    pub fn global_config(&self) -> PathBuf {
        self.home.join("config.toml")
    }

    /// Your additions to the model registry.
    pub fn global_models(&self) -> PathBuf {
        self.home.join("models.toml")
    }

    /// What model discovery last found.
    pub fn models_cache(&self) -> PathBuf {
        self.home.join(crate::layout::user_files::MODELS_CACHE)
    }

    pub fn global_presets(&self) -> PathBuf {
        self.home.join("presets")
    }

    pub fn run_dir(&self, repo: &RepoId, run: &RunId) -> PathBuf {
        self.state_dir(repo).join("runs").join(run.as_str())
    }

    pub fn event_log_path(&self, repo: &RepoId, run: &RunId) -> PathBuf {
        self.run_dir(repo, run).join(EVENT_LOG_FILE)
    }

    /// Private scratch space for one card, kept outside its worktree.
    pub fn card_dir(&self, repo: &RepoId, run: &RunId, card: &CardId) -> PathBuf {
        self.run_dir(repo, run).join("cards").join(card.as_str())
    }

    /// Creates the root and its `state` and `worktrees` folders readable only by the owner, and
    /// tightens them if they already exist. Logs hold prompts, transcripts, and diffs.
    pub fn prepare(&self) -> io::Result<()> {
        for dir in [
            self.home.clone(),
            self.home.join("state"),
            self.home.join("worktrees"),
        ] {
            create_private_dir_all(&dir)?;
            // Someone else's folder can't be tightened, and everything below is private anyway.
            let _ = fs::set_permissions(&dir, Permissions::from_mode(0o700));
        }
        Ok(())
    }

    pub fn worktree_dir(&self, repo: &RepoId, run: &RunId, card: &CardId) -> PathBuf {
        self.home
            .join("worktrees")
            .join(repo.as_str())
            .join(run.as_str())
            .join(card.as_str())
    }
}

pub fn create_private_dir_all(path: &Path) -> io::Result<()> {
    DirBuilder::new().recursive(true).mode(0o700).create(path)
}

pub fn write_private_file(path: &Path, contents: &[u8]) -> io::Result<()> {
    let mut file = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)?;
    file.write_all(contents)
}

fn testing() -> bool {
    cfg!(test) || std::env::var_os(TESTING_ENV).is_some_and(|value| !value.is_empty())
}

/// A test's home: `ARGONAUTS_HOME` when the test set one, else the scratch home in `temp`. One
/// inside your own `~/.argonauts` (under `account`, your home whatever `HOME` says) stops the
/// test with a panic, since it would write there. Prompt history from tests once piled up in
/// the real `state` folder this way.
fn test_home(env_home: Option<OsString>, temp: &Path, account: Option<&Path>) -> PathBuf {
    let Some(home) = env_home.filter(|home| !home.is_empty()) else {
        return temp.join(SCRATCH_HOME);
    };
    let home = absolute(PathBuf::from(home));
    if let Some(yours) = account.map(|account| account.join(".argonauts"))
        && home.starts_with(&yours)
    {
        panic!(
            "a test set {HOME_ENV} to {}, inside your own {}; tests must use a home of their own",
            home.display(),
            yours.display()
        );
    }
    home
}

/// Your home folder from the account database, which a test's `HOME` doesn't change.
fn account_home() -> Option<PathBuf> {
    let user = nix::unistd::User::from_uid(nix::unistd::getuid()).ok()??;
    Some(user.dir)
}

fn absolute(path: PathBuf) -> PathBuf {
    std::path::absolute(&path).unwrap_or(path)
}

pub fn argonauts_home() -> Result<PathBuf, NoHomeDir> {
    Ok(Paths::from_env()?.home)
}

pub fn state_dir(repo: &RepoId) -> Result<PathBuf, NoHomeDir> {
    Ok(Paths::from_env()?.state_dir(repo))
}

pub fn run_dir(repo: &RepoId, run: &RunId) -> Result<PathBuf, NoHomeDir> {
    Ok(Paths::from_env()?.run_dir(repo, run))
}

pub fn worktree_dir(repo: &RepoId, run: &RunId, card: &CardId) -> Result<PathBuf, NoHomeDir> {
    Ok(Paths::from_env()?.worktree_dir(repo, run, card))
}

pub fn global_config_path() -> Result<PathBuf, NoHomeDir> {
    Ok(Paths::from_env()?.global_config())
}

/// Where 0.0.1 read your settings. Nothing reads it now; the app offers once to move it to
/// [`global_config_path`].
pub fn old_global_config_path() -> Result<PathBuf, NoHomeDir> {
    old_global_config_path_from(std::env::var_os("XDG_CONFIG_HOME"), dirs::home_dir())
}

fn old_global_config_path_from(
    xdg: Option<OsString>,
    user_home: Option<PathBuf>,
) -> Result<PathBuf, NoHomeDir> {
    let base = match xdg.map(PathBuf::from).filter(|path| path.is_absolute()) {
        Some(xdg) => xdg,
        None => user_home.ok_or(NoHomeDir)?.join(".config"),
    };
    Ok(base.join("argonauts").join("config.toml"))
}

pub fn project_config_path(repo_root: &Path) -> PathBuf {
    repo_root.join(".argonauts").join("config.toml")
}

/// The first write adds it to `.git/info/exclude`, so it's never committed.
pub fn project_local_config_path(repo_root: &Path) -> PathBuf {
    repo_root.join(".argonauts").join("config.local.toml")
}

pub const LOCAL_CONFIG_EXCLUDE: &str = "/.argonauts/config.local.toml";

/// Adds `entry` to `.git/info/exclude`, in the main repository's file for a linked worktree.
/// Returns false when it's already listed, with or without its leading `/`, or when
/// `repo_root` isn't a git checkout.
pub fn exclude_from_git(repo_root: &Path, entry: &str) -> io::Result<bool> {
    let Some(git_dir) = shared_git_dir(repo_root)? else {
        return Ok(false);
    };
    let info = git_dir.join("info");
    let exclude = info.join("exclude");
    let existing = match fs::read_to_string(&exclude) {
        Ok(text) => text,
        Err(err) if err.kind() == io::ErrorKind::NotFound => String::new(),
        Err(err) => return Err(err),
    };
    let bare = entry.trim_start_matches('/');
    if existing
        .lines()
        .any(|line| line.trim().trim_start_matches('/') == bare)
    {
        return Ok(false);
    }
    fs::create_dir_all(&info)?;
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&exclude)?;
    if !existing.is_empty() && !existing.ends_with('\n') {
        file.write_all(b"\n")?;
    }
    writeln!(file, "{entry}")?;
    Ok(true)
}

/// Where git keeps a checkout's shared files: its `.git` folder, or for a linked worktree or a
/// submodule the folder its `.git` file points at, following `commondir` to the main repository.
fn shared_git_dir(repo_root: &Path) -> io::Result<Option<PathBuf>> {
    let dot_git = repo_root.join(".git");
    let metadata = match fs::metadata(&dot_git) {
        Ok(metadata) => metadata,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(err) => return Err(err),
    };
    if metadata.is_dir() {
        return Ok(Some(dot_git));
    }
    let pointer = fs::read_to_string(&dot_git)?;
    let Some(git_dir) = pointer
        .lines()
        .find_map(|line| line.strip_prefix("gitdir:"))
    else {
        return Ok(None);
    };
    let git_dir = repo_root.join(git_dir.trim());
    match fs::read_to_string(git_dir.join("commondir")) {
        Ok(common) => Ok(Some(git_dir.join(common.trim()))),
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(Some(git_dir)),
        Err(err) => Err(err),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ids() -> (RepoId, RunId, CardId) {
        (
            RepoId::new("api-3f9a1c2b4d5e").unwrap(),
            RunId::new("run-01").unwrap(),
            CardId::new("card-001").unwrap(),
        )
    }

    #[test]
    fn layout_under_home() {
        let paths = Paths::new("/h");
        let (repo, run, card) = ids();
        assert_eq!(
            paths.state_dir(&repo),
            Path::new("/h/state/api-3f9a1c2b4d5e")
        );
        assert_eq!(
            paths.run_dir(&repo, &run),
            Path::new("/h/state/api-3f9a1c2b4d5e/runs/run-01")
        );
        assert_eq!(
            paths.event_log_path(&repo, &run),
            Path::new("/h/state/api-3f9a1c2b4d5e/runs/run-01/events.jsonl")
        );
        assert_eq!(
            paths.card_dir(&repo, &run, &card),
            Path::new("/h/state/api-3f9a1c2b4d5e/runs/run-01/cards/card-001")
        );
        assert_eq!(
            paths.worktree_dir(&repo, &run, &card),
            Path::new("/h/worktrees/api-3f9a1c2b4d5e/run-01/card-001")
        );
    }

    #[test]
    fn env_home_wins_over_user_home() {
        let paths = Paths::resolve(Some("/custom".into()), Some("/u".into())).unwrap();
        assert_eq!(paths.home(), Path::new("/custom"));
    }

    #[test]
    fn empty_env_home_falls_back() {
        let paths = Paths::resolve(Some("".into()), Some("/u".into())).unwrap();
        assert_eq!(paths.home(), Path::new("/u/.argonauts"));
        assert_eq!(Paths::resolve(None, None), Err(NoHomeDir));
    }

    #[test]
    fn relative_env_home_is_made_absolute() {
        let paths = Paths::resolve(Some("rel/dir".into()), None).unwrap();
        assert!(paths.home().is_absolute());
        assert!(paths.home().ends_with("rel/dir"));
    }

    #[test]
    fn from_env_resolves_somewhere() {
        // The value depends on the machine, so only check that it is absolute.
        if let Ok(home) = argonauts_home() {
            assert!(home.is_absolute());
        }
    }

    #[test]
    fn tests_never_get_your_own_home() {
        let temp = Path::new("/tmp/t");
        let account = Some(Path::new("/Users/me"));
        assert_eq!(
            test_home(None, temp, account),
            temp.join("argonauts-test-home")
        );
        assert_eq!(
            test_home(Some("".into()), temp, account),
            temp.join("argonauts-test-home")
        );
        assert_eq!(
            test_home(Some("/tmp/x/.argonauts".into()), temp, account),
            PathBuf::from("/tmp/x/.argonauts")
        );
        let yours = std::panic::catch_unwind(|| {
            test_home(Some("/Users/me/.argonauts".into()), temp, account)
        });
        assert!(yours.is_err(), "your own home stops the test");
        let inside = std::panic::catch_unwind(|| {
            test_home(Some("/Users/me/.argonauts/state".into()), temp, account)
        });
        assert!(inside.is_err());
    }

    #[test]
    fn this_test_runs_in_a_scratch_home() {
        let home = argonauts_home().unwrap();
        if let Some(account) = account_home() {
            assert!(
                !home.starts_with(account.join(".argonauts")),
                "{}",
                home.display()
            );
        }
    }

    #[test]
    fn global_config_lives_in_the_state_root() {
        let paths = Paths::resolve(None, Some("/u".into())).unwrap();
        assert_eq!(
            paths.global_config(),
            Path::new("/u/.argonauts/config.toml")
        );
        assert_eq!(paths.global_presets(), Path::new("/u/.argonauts/presets"));
        let moved = Paths::resolve(Some("/x".into()), Some("/u".into())).unwrap();
        assert_eq!(moved.global_config(), Path::new("/x/config.toml"));
        assert_eq!(
            project_local_config_path(Path::new("/r")),
            Path::new("/r/.argonauts/config.local.toml")
        );
    }

    #[test]
    fn the_old_global_config_is_where_0_0_1_read_it() {
        let old = |xdg: Option<&str>, home: Option<&str>| {
            old_global_config_path_from(xdg.map(OsString::from), home.map(PathBuf::from))
        };
        assert_eq!(
            old(None, Some("/u")).unwrap(),
            Path::new("/u/.config/argonauts/config.toml")
        );
        assert_eq!(
            old(Some("/x"), Some("/u")).unwrap(),
            Path::new("/x/argonauts/config.toml")
        );
        // A relative XDG_CONFIG_HOME is ignored, as the XDG spec says.
        assert_eq!(
            old(Some("rel"), Some("/u")).unwrap(),
            Path::new("/u/.config/argonauts/config.toml")
        );
        assert_eq!(old(None, None), Err(NoHomeDir));
    }

    fn mode(path: &Path) -> u32 {
        fs::metadata(path).unwrap().permissions().mode() & 0o777
    }

    #[test]
    fn prepare_makes_the_roots_private() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("home");
        fs::create_dir_all(home.join("state")).unwrap();
        fs::set_permissions(home.join("state"), Permissions::from_mode(0o755)).unwrap();
        let paths = Paths::new(&home);
        paths.prepare().unwrap();
        for sub in ["", "state", "worktrees"] {
            assert_eq!(mode(&home.join(sub)), 0o700, "{sub}");
        }
    }

    #[test]
    fn private_files_and_dirs() {
        let dir = tempfile::tempdir().unwrap();
        let nested = dir.path().join("a/b");
        create_private_dir_all(&nested).unwrap();
        assert_eq!(mode(&nested), 0o700);
        assert_eq!(mode(&dir.path().join("a")), 0o700);
        let file = nested.join("diff.patch");
        write_private_file(&file, b"one").unwrap();
        write_private_file(&file, b"two").unwrap();
        assert_eq!(fs::read_to_string(&file).unwrap(), "two");
        assert_eq!(mode(&file), 0o600);
    }

    #[test]
    fn project_config_lives_in_dot_argonauts() {
        assert_eq!(
            project_config_path(Path::new("/r")),
            Path::new("/r/.argonauts/config.toml")
        );
    }

    #[test]
    fn exclude_adds_the_line_once() {
        let dir = tempfile::tempdir().unwrap();
        let info = dir.path().join(".git/info");
        fs::create_dir_all(&info).unwrap();
        // A template file without a final newline must not run into our line.
        fs::write(info.join("exclude"), "# git ls-files --others\n*.swp").unwrap();
        assert!(exclude_from_git(dir.path(), LOCAL_CONFIG_EXCLUDE).unwrap());
        assert!(!exclude_from_git(dir.path(), LOCAL_CONFIG_EXCLUDE).unwrap());
        assert!(!exclude_from_git(dir.path(), ".argonauts/config.local.toml").unwrap());
        assert_eq!(
            fs::read_to_string(info.join("exclude")).unwrap(),
            "# git ls-files --others\n*.swp\n/.argonauts/config.local.toml\n"
        );
    }

    #[test]
    fn exclude_creates_the_file_when_the_template_left_none() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir(dir.path().join(".git")).unwrap();
        assert!(exclude_from_git(dir.path(), LOCAL_CONFIG_EXCLUDE).unwrap());
        assert_eq!(
            fs::read_to_string(dir.path().join(".git/info/exclude")).unwrap(),
            "/.argonauts/config.local.toml\n"
        );
    }

    #[test]
    fn a_linked_worktree_excludes_in_the_main_repository() {
        let dir = tempfile::tempdir().unwrap();
        let main_git = dir.path().join("main/.git");
        let worktree_git = main_git.join("worktrees/wt");
        fs::create_dir_all(&worktree_git).unwrap();
        fs::write(worktree_git.join("commondir"), "../..\n").unwrap();
        let worktree = dir.path().join("wt");
        fs::create_dir_all(&worktree).unwrap();
        fs::write(
            worktree.join(".git"),
            format!("gitdir: {}\n", worktree_git.display()),
        )
        .unwrap();
        assert!(exclude_from_git(&worktree, LOCAL_CONFIG_EXCLUDE).unwrap());
        let text = fs::read_to_string(main_git.join("info/exclude")).unwrap();
        assert_eq!(text, "/.argonauts/config.local.toml\n");
        assert!(!worktree_git.join("info").exists());
    }

    #[test]
    fn exclude_leaves_a_folder_outside_git_alone() {
        let dir = tempfile::tempdir().unwrap();
        assert!(!exclude_from_git(dir.path(), LOCAL_CONFIG_EXCLUDE).unwrap());
        assert!(!dir.path().join(".git").exists());
    }
}
