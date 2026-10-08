//! `gates.setup`: a fresh worktree has no dependencies installed, so before a card's first gate
//! the conductor runs the setup commands once in that worktree, outside the agent's sandbox, with
//! the gate environment, installing from the lockfile with install scripts off where the tool
//! allows it. Only the untracked files `gates.copy_untracked` names are copied in.

use std::ffi::{OsStr, OsString};
use std::os::unix::ffi::OsStrExt;
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

use argonauts_core::settings::protocol::GateCommand;
use globset::{GlobBuilder, GlobSet, GlobSetBuilder};

use super::{GateProcess, Ran, execute};
use crate::control::StopHandle;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SetupError {
    #[error("setup command {name} failed: {detail}")]
    Failed { name: String, detail: String },
    #[error("setup: {0}")]
    Io(String),
}

/// Runs every setup command in order in `worktree`, stopping at the first failure.
pub async fn run_setup(
    commands: &[GateCommand],
    worktree: &Path,
    env: &[(String, String)],
    timeout: Duration,
) -> Result<(), SetupError> {
    for command in commands {
        let ran = run_setup_command(&command.command, worktree, env, timeout)
            .await
            .map_err(|err| SetupError::Failed {
                name: command.name.clone(),
                detail: err.to_string(),
            })?;
        if !ran.passed() {
            let detail = match (&ran.ended_early, ran.exit_code) {
                (Some(why), _) => why.clone(),
                (None, Some(code)) => format!("exit code {code}"),
                (None, None) => "ended by a signal".to_owned(),
            };
            return Err(SetupError::Failed {
                name: command.name.clone(),
                detail,
            });
        }
    }
    Ok(())
}

/// One setup command, unsandboxed, with its process group killed at the end.
pub(crate) async fn run_setup_command(
    argv: &[String],
    worktree: &Path,
    env: &[(String, String)],
    timeout: Duration,
) -> std::io::Result<Ran> {
    let env = env
        .iter()
        .map(|(k, v)| (OsString::from(k), OsString::from(v)))
        .collect();
    let process = GateProcess::new(argv, worktree, env)?;
    execute(&process, timeout, &StopHandle::new()).await
}

/// The main checkout of the repository `worktree` belongs to, from
/// `git rev-parse --git-common-dir`.
pub fn main_checkout(worktree: &Path) -> Result<PathBuf, SetupError> {
    let common = git(worktree, &["rev-parse", "--git-common-dir"])?;
    let common = String::from_utf8_lossy(&common).trim().to_owned();
    let common = worktree.join(common);
    let common = common
        .canonicalize()
        .map_err(|err| SetupError::Io(format!("{}: {err}", common.display())))?;
    match (common.file_name(), common.parent()) {
        (Some(name), Some(root)) if name == ".git" => Ok(root.to_path_buf()),
        _ => Err(SetupError::Io(format!(
            "{} has no main checkout to copy untracked files from",
            common.display()
        ))),
    }
}

/// Copies the untracked files of `repo_root` that match `globs` into `worktree`, and returns
/// what it copied. Ignored files count as untracked, since that's where files like `.env.test`
/// live. Symlinks and files the worktree already has are left alone.
pub fn copy_untracked(
    repo_root: &Path,
    worktree: &Path,
    globs: &[String],
) -> Result<Vec<PathBuf>, SetupError> {
    if globs.is_empty() {
        return Ok(Vec::new());
    }
    let wanted = glob_set(globs)?;
    let mut args = vec!["ls-files", "--others", "-z", "--"];
    let pathspecs: Vec<String> = globs.iter().map(|glob| format!(":(glob){glob}")).collect();
    args.extend(pathspecs.iter().map(String::as_str));
    let listed = git(repo_root, &args)?;

    let mut copied = Vec::new();
    for raw in listed.split(|&b| b == 0).filter(|raw| !raw.is_empty()) {
        let relative = PathBuf::from(OsStr::from_bytes(raw));
        if !stays_inside(&relative) || !wanted.is_match(&relative) {
            continue;
        }
        let source = repo_root.join(&relative);
        let target = worktree.join(&relative);
        let is_file = source
            .symlink_metadata()
            .is_ok_and(|meta| meta.file_type().is_file());
        if !is_file || target.symlink_metadata().is_ok() {
            continue;
        }
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|err| SetupError::Io(format!("{}: {err}", parent.display())))?;
        }
        std::fs::copy(&source, &target)
            .map_err(|err| SetupError::Io(format!("{}: {err}", relative.display())))?;
        copied.push(relative);
    }
    Ok(copied)
}

fn glob_set(globs: &[String]) -> Result<GlobSet, SetupError> {
    let mut set = GlobSetBuilder::new();
    for glob in globs {
        let glob = GlobBuilder::new(glob)
            .literal_separator(true)
            .build()
            .map_err(|err| SetupError::Io(format!("checks.copy_untracked: {err}")))?;
        set.add(glob);
    }
    set.build()
        .map_err(|err| SetupError::Io(format!("checks.copy_untracked: {err}")))
}

fn stays_inside(relative: &Path) -> bool {
    relative
        .components()
        .all(|part| matches!(part, Component::Normal(_)))
}

fn git(dir: &Path, args: &[&str]) -> Result<Vec<u8>, SetupError> {
    let output = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .stdin(Stdio::null())
        .env("GIT_TERMINAL_PROMPT", "0")
        .output()
        .map_err(|err| SetupError::Io(format!("could not run git: {err}")))?;
    if !output.status.success() {
        return Err(SetupError::Io(format!(
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }
    Ok(output.stdout)
}

#[cfg(test)]
mod tests {
    use super::*;
    use argonauts_testkit::repo::TempRepo;

    #[test]
    fn copy_untracked_takes_only_matching_untracked_files() {
        let repo = TempRepo::new().unwrap();
        repo.write(".gitignore", ".env*\n").unwrap();
        repo.write("tracked.env.test", "tracked").unwrap();
        repo.commit_all("base").unwrap();
        repo.write(".env.test", "SECRET_FREE=1").unwrap();
        repo.write(".env.local", "no").unwrap();
        repo.write("fixtures/data.json", "{}").unwrap();
        repo.write("notes.txt", "no").unwrap();
        std::os::unix::fs::symlink("/etc/hosts", repo.root().join("fixtures/link.json")).unwrap();

        let worktree = repo.scratch().join("wt");
        let path = worktree.display().to_string();
        repo.git(&["worktree", "add", "--detach", &path]).unwrap();
        assert_eq!(
            main_checkout(&worktree).unwrap(),
            repo.root().canonicalize().unwrap()
        );
        assert_eq!(
            main_checkout(repo.root()).unwrap(),
            repo.root().canonicalize().unwrap()
        );

        let globs = [".env.test".to_owned(), "fixtures/*.json".to_owned()];
        let mut copied = copy_untracked(repo.root(), &worktree, &globs).unwrap();
        copied.sort();
        assert_eq!(
            copied,
            [
                PathBuf::from(".env.test"),
                PathBuf::from("fixtures/data.json")
            ]
        );
        assert_eq!(
            std::fs::read_to_string(worktree.join(".env.test")).unwrap(),
            "SECRET_FREE=1"
        );
        assert!(!worktree.join(".env.local").exists());
        assert!(!worktree.join("notes.txt").exists());
        assert!(!worktree.join("fixtures/link.json").exists());

        // A second copy finds everything already there.
        assert!(
            copy_untracked(repo.root(), &worktree, &globs)
                .unwrap()
                .is_empty()
        );
        assert!(matches!(
            copy_untracked(repo.root(), &worktree, &["[".to_owned()]),
            Err(SetupError::Io(_))
        ));
    }

    #[tokio::test]
    async fn run_setup_stops_at_the_first_failure() {
        let dir = tempfile::tempdir().unwrap();
        let command = |name: &str, script: &str| GateCommand {
            name: name.to_owned(),
            command: vec!["/bin/sh".into(), "-c".into(), script.into()],
        };
        let commands = [
            command("first", "echo one >> log"),
            command("broken", "exit 3"),
            command("never", "echo three >> log"),
        ];
        let env = [("PATH".to_owned(), "/usr/bin:/bin".to_owned())];
        let err = run_setup(&commands, dir.path(), &env, Duration::from_secs(10))
            .await
            .unwrap_err();
        assert_eq!(
            err,
            SetupError::Failed {
                name: "broken".into(),
                detail: "exit code 3".into()
            }
        );
        assert_eq!(
            std::fs::read_to_string(dir.path().join("log")).unwrap(),
            "one\n"
        );
    }
}
