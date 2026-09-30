//! A throwaway git repository with its own Argonauts home, config home, and git config, so no
//! test ever reads or writes yours.

use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Mutex, MutexGuard, PoisonError};

use anyhow::{Context as _, bail};
use tempfile::TempDir;

/// A git repository with one commit on `main` and private homes, all removed on drop.
pub struct TempRepo {
    dir: TempDir,
    root: PathBuf,
    home: PathBuf,
    config_home: PathBuf,
    user_home: PathBuf,
    git_config: PathBuf,
}

impl TempRepo {
    pub fn new() -> anyhow::Result<Self> {
        let dir = tempfile::Builder::new()
            .prefix("argonauts-test-")
            .tempdir()
            .context("creating a temp directory")?;
        // Canonical paths, because macOS reaches /tmp through /private and git reports that.
        let base = dir
            .path()
            .canonicalize()
            .context("resolving the temp directory")?;
        let repo = Self {
            root: base.join("repo"),
            home: base.join("argonauts-home"),
            config_home: base.join("config"),
            user_home: base.join("home"),
            git_config: base.join("gitconfig"),
            dir,
        };
        for path in [&repo.root, &repo.home, &repo.config_home, &repo.user_home] {
            fs::create_dir_all(path).with_context(|| format!("creating {}", path.display()))?;
        }
        fs::write(
            &repo.git_config,
            "[user]\n\tname = Argonauts Test\n\temail = test@argonauts.invalid\n\
             [commit]\n\tgpgsign = false\n[tag]\n\tgpgsign = false\n\
             [init]\n\tdefaultBranch = main\n[core]\n\thooksPath = /dev/null\n",
        )
        .context("writing the test git config")?;
        repo.git(&["init", "--quiet"])?;
        repo.write("README.md", "# Test repo\n")?;
        repo.commit_all("initial commit")?;
        Ok(repo)
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// The private `ARGONAUTS_HOME`.
    pub fn home(&self) -> &Path {
        &self.home
    }

    /// The private `XDG_CONFIG_HOME`. Argonauts' own global config lives in `ARGONAUTS_HOME`.
    pub fn config_home(&self) -> &Path {
        &self.config_home
    }

    /// The private `HOME`.
    pub fn user_home(&self) -> &Path {
        &self.user_home
    }

    /// The temp directory holding everything, for other test files.
    pub fn scratch(&self) -> &Path {
        self.dir.path()
    }

    pub fn global_config(&self) -> PathBuf {
        self.home.join("config.toml")
    }

    pub fn envs(&self) -> Vec<(String, OsString)> {
        vec![
            (
                "ARGONAUTS_HOME".to_owned(),
                self.home.clone().into_os_string(),
            ),
            (
                "XDG_CONFIG_HOME".to_owned(),
                self.config_home.clone().into_os_string(),
            ),
            ("HOME".to_owned(), self.user_home.clone().into_os_string()),
            (
                "GIT_CONFIG_GLOBAL".to_owned(),
                self.git_config.clone().into_os_string(),
            ),
            ("GIT_CONFIG_NOSYSTEM".to_owned(), OsString::from("1")),
        ]
    }

    /// Returns git's trimmed stdout.
    pub fn git(&self, args: &[&str]) -> anyhow::Result<String> {
        let output = Command::new("git")
            .args(args)
            .current_dir(&self.root)
            .envs(self.envs())
            .output()
            .with_context(|| format!("running git {}", args.join(" ")))?;
        if !output.status.success() {
            bail!(
                "git {} failed: {}",
                args.join(" "),
                String::from_utf8_lossy(&output.stderr).trim()
            );
        }
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
    }

    pub fn write(&self, relative: &str, contents: &str) -> anyhow::Result<PathBuf> {
        let path = self.root.join(relative);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).with_context(|| format!("creating {}", parent.display()))?;
        }
        fs::write(&path, contents).with_context(|| format!("writing {}", path.display()))?;
        Ok(path)
    }

    /// Returns the new commit's id.
    pub fn commit_all(&self, message: &str) -> anyhow::Result<String> {
        self.git(&["add", "-A"])?;
        self.git(&["commit", "--quiet", "-m", message])?;
        self.head()
    }

    pub fn head(&self) -> anyhow::Result<String> {
        self.git(&["rev-parse", "HEAD"])
    }

    /// Sets this repo's environment in the test process until the guard drops.
    pub fn env_guard(&self) -> EnvGuard {
        EnvGuard::set(&self.envs())
    }
}

/// Every test that changes the process environment holds this, one at a time.
static ENV_LOCK: Mutex<()> = Mutex::new(());

/// Sets environment variables for in-process code under test and restores them on drop. Code
/// reading the environment on other threads can still race, so prefer passing `envs()` to child
/// processes.
pub struct EnvGuard {
    saved: Vec<(String, Option<OsString>)>,
    _lock: MutexGuard<'static, ()>,
}

impl EnvGuard {
    pub fn set(vars: &[(String, OsString)]) -> Self {
        let lock = ENV_LOCK.lock().unwrap_or_else(PoisonError::into_inner);
        let saved = vars
            .iter()
            .map(|(name, _)| (name.clone(), std::env::var_os(name)))
            .collect();
        for (name, value) in vars {
            // SAFETY: ENV_LOCK serializes every guard in this process, and tests that use a
            // guard don't read the environment from other threads while it changes.
            unsafe { std::env::set_var(name, value) };
        }
        Self { saved, _lock: lock }
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        for (name, value) in self.saved.drain(..) {
            // SAFETY: as in `set`; the lock is still held.
            unsafe {
                match value {
                    Some(value) => std::env::set_var(&name, value),
                    None => std::env::remove_var(&name),
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_temp_repo_has_one_commit_and_private_homes() {
        let repo = TempRepo::new().unwrap();
        assert_eq!(repo.git(&["rev-list", "--count", "HEAD"]).unwrap(), "1");
        assert_eq!(repo.git(&["branch", "--show-current"]).unwrap(), "main");
        repo.write("src/a.txt", "a\n").unwrap();
        let before = repo.head().unwrap();
        let after = repo.commit_all("add a").unwrap();
        assert_ne!(before, after);
        assert!(repo.home().is_dir() && repo.config_home().is_dir());
        assert!(repo.global_config().starts_with(repo.home()));
    }

    #[test]
    fn the_env_guard_restores_what_it_changed() {
        let name = "ARGONAUTS_TESTKIT_GUARD_CHECK";
        {
            let _guard = EnvGuard::set(&[(name.to_owned(), OsString::from("1"))]);
            assert_eq!(std::env::var(name).unwrap(), "1");
        }
        assert!(std::env::var_os(name).is_none());
    }
}
