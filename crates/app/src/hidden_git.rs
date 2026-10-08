//! Making and recording the history of a folder that has no git of its own. Where it lives
//! and what it is: `argonauts_core::hidden_git`. It gives such a folder what a workflow needs,
//! branches and worktrees for agents working side by side, and what `/rewind` needs.

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context as _, bail};
use argonauts_core::hidden_git::OFF;
pub(crate) use argonauts_core::hidden_git::{dir, is_on};
use argonauts_core::paths::{create_private_dir_all, write_private_file};
use argonauts_core::settings::delivery::HistorySettings;

use crate::git::{self, git};

/// The history's one branch, which follows your folder. A run names the branch it starts from
/// and merges into wherever it speaks of them, so the name says what that is here.
pub(crate) const BRANCH: &str = "your-folder";

/// Makes the history for the folder with this state if there's none yet, and turns it on.
/// It's a bare repository made without a template, so it holds no hooks, and your folder is
/// named as its work tree on each call instead of being written into its config.
pub(crate) fn turn_on(state_dir: &Path) -> anyhow::Result<PathBuf> {
    let dir = dir(state_dir);
    if !dir.join("HEAD").is_file() {
        create_private_dir_all(state_dir)
            .with_context(|| format!("creating {}", state_dir.display()))?;
        init_bare(&dir)?;
        let head = format!("ref: refs/heads/{BRANCH}\n");
        std::fs::write(dir.join("HEAD"), head).context("naming the history's branch")?;
    }
    match std::fs::remove_file(dir.join(OFF)) {
        Err(err) if err.kind() != std::io::ErrorKind::NotFound => {
            Err(err).context("turning the hidden history back on")
        }
        _ => Ok(dir),
    }
}

/// Stops using the history. It stays where it is, so turning it on again loses nothing.
pub(crate) fn turn_off(state_dir: &Path) -> anyhow::Result<()> {
    let dir = dir(state_dir);
    if dir.is_dir() {
        write_private_file(&dir.join(OFF), b"").context("turning the hidden history off")?;
    }
    Ok(())
}

fn init_bare(dir: &Path) -> anyhow::Result<()> {
    let made = Command::new("git")
        .args(["init", "--quiet", "--bare", "--template="])
        .arg(dir)
        .output()
        .map_err(|err| match err.kind() {
            std::io::ErrorKind::NotFound => anyhow::anyhow!(
                "git isn't installed, and a workflow needs it. On macOS: xcode-select \
                 --install. On Linux: your package manager's git"
            ),
            _ => anyhow::Error::new(err).context("running git init"),
        })?;
    if !made.status.success() {
        bail!(
            "git init for Argonauts' own history failed: {}",
            String::from_utf8_lossy(&made.stderr).trim()
        );
    }
    Ok(())
}

/// Why `folder` can't be recorded at all: your home folder and a file system's top hold far
/// more than a project, so a workflow there asks for a project folder instead.
pub(crate) fn refused(folder: &Path, user_home: Option<&Path>) -> Option<&'static str> {
    let same = |other: &Path| other.canonicalize().is_ok_and(|other| other == folder);
    if folder.parent().is_none() {
        Some("the top of the file system")
    } else if user_home.is_some_and(same) {
        Some("your home folder")
    } else {
        None
    }
}

/// Your folder as one commit of its history.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Snapshot {
    pub commit: String,
    /// What stayed out, each with why, such as `data.bin (over 50 MB)`.
    pub left_out: Vec<String>,
}

/// The files of a folder that git doesn't hold yet, sorted into what a first commit or a
/// record takes and what stays out. Sizes and places decide, never file types: a file over
/// `history.max_file_mb` stays out, and so does a git repository nested inside, which has a
/// history of its own.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Survey {
    pub take: Vec<String>,
    /// The bytes `take` holds.
    pub bytes: u64,
    /// What stays out, each with why, such as `data.bin (over 50 MB)`.
    pub left_out: Vec<String>,
    /// The same paths, bare.
    excluded: Vec<String>,
}

impl Survey {
    /// From `new`, git's NUL-separated list of the files in `root` it doesn't track and your
    /// `.gitignore` files allow.
    pub(crate) fn of(root: &Path, new: &str, limits: &HistorySettings) -> Self {
        let max_file = limits.max_file_mb.saturating_mul(MB);
        let mut survey = Self::default();
        for path in new.split('\0').filter(|path| !path.is_empty()) {
            // Git names a nested repository as a folder, and can't add one with no commits.
            if let Some(nested) = path.strip_suffix('/') {
                survey
                    .left_out
                    .push(format!("{nested}/ (a git repository of its own)"));
                survey.excluded.push(nested.to_owned());
                continue;
            }
            let size = std::fs::symlink_metadata(root.join(path)).map_or(0, |meta| meta.len());
            if max_file > 0 && size > max_file {
                survey
                    .left_out
                    .push(format!("{path} (over {} MB)", limits.max_file_mb));
                survey.excluded.push(path.to_owned());
                continue;
            }
            survey.bytes = survey.bytes.saturating_add(size);
            survey.take.push(path.to_owned());
        }
        survey
    }

    /// Whether a first record may take this much: a folder past `history.max_files` or
    /// `history.max_total_mb` is refused, with what to do about it.
    pub(crate) fn fits(&self, limits: &HistorySettings) -> anyhow::Result<()> {
        too_big_to_start(self.take.len() as u64, self.bytes, limits)
    }

    /// Whether recording what this takes into the git folder of `root` leaves
    /// `history.min_free_gb` free on that disk. A disk whose free space can't be read isn't
    /// held against the record.
    pub(crate) fn leaves_room(&self, root: &Path, limits: &HistorySettings) -> anyhow::Result<()> {
        if limits.min_free_gb == 0 {
            return Ok(());
        }
        let git_dir = git(root, &["rev-parse", "--absolute-git-dir"])?;
        match free_bytes(Path::new(&git_dir)) {
            Some(free) => short_of_room(free, self.bytes, limits),
            None => Ok(()),
        }
    }

    /// `git add` for everything in the folder but what stays out.
    pub(crate) fn add_all(&self) -> Vec<String> {
        let mut add: Vec<String> = vec!["add".into(), "--all".into(), "--".into(), ".".into()];
        add.extend(
            self.excluded
                .iter()
                .map(|path| format!(":(exclude,literal){path}")),
        );
        add
    }
}

/// What making `root` a repository would put in its first commit, found without leaving
/// anything in the folder: a throwaway git folder under `state_dir` lists the files.
pub(crate) fn first_commit(
    root: &Path,
    state_dir: &Path,
    limits: &HistorySettings,
) -> anyhow::Result<Survey> {
    create_private_dir_all(state_dir)
        .with_context(|| format!("creating {}", state_dir.display()))?;
    let scratch = state_dir.join(format!("git-preview-{}.git", std::process::id()));
    let listed = (|| {
        init_bare(&scratch)?;
        let out = Command::new("git")
            .arg("--git-dir")
            .arg(&scratch)
            .arg("--work-tree")
            .arg(root)
            .args(["ls-files", "--others", "--exclude-standard", "-z"])
            .current_dir(root)
            .output()
            .context("listing the folder's files")?;
        if !out.status.success() {
            bail!(
                "listing the folder's files failed: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            );
        }
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    })();
    let _ = std::fs::remove_dir_all(&scratch);
    Ok(Survey::of(root, &listed?, limits))
}

/// Makes `root` a git repository as `git init` does, then stages what `survey` takes and,
/// with `commit`, commits it as your first commit, under your own name. Returns whether a
/// commit was made: git refuses one when it knows no name and email for you, and then the
/// files stay staged for you to commit.
pub(crate) fn make_repository(root: &Path, survey: &Survey, commit: bool) -> anyhow::Result<bool> {
    let made = Command::new("git")
        .arg("init")
        .arg("--quiet")
        .arg(root)
        .output()
        .context("running git init")?;
    if !made.status.success() {
        bail!(
            "git init failed: {}",
            String::from_utf8_lossy(&made.stderr).trim()
        );
    }
    if !commit {
        return Ok(false);
    }
    let add: Vec<String> = survey.add_all();
    let add: Vec<&str> = add.iter().map(String::as_str).collect();
    git(root, &add)?;
    if git::git_ok(root, &["diff", "--cached", "--quiet", "--no-ext-diff"])? {
        return Ok(false);
    }
    git::git_ok(root, &["commit", "--quiet", "-m", "First commit"])
}

/// Records `root` as it is now on the history's branch and returns that commit, the newest
/// one when nothing changed since. What it takes is what [`Survey`] takes, and your
/// `.gitignore` files hold. The first record of a folder past `history.max_files` or
/// `history.max_total_mb` is refused.
///
/// `root` must be using its hidden history, so git calls for it reach it.
pub(crate) fn record(
    root: &Path,
    limits: &HistorySettings,
    message: &str,
) -> anyhow::Result<Snapshot> {
    let first = git::head(root).is_err();
    let new = git(root, &["ls-files", "--others", "--exclude-standard", "-z"])?;
    let survey = Survey::of(root, &new, limits);
    if first {
        survey.fits(limits)?;
    }
    survey.leaves_room(root, limits)?;
    let add = survey.add_all();
    let add: Vec<&str> = add.iter().map(String::as_str).collect();
    git(root, &add)?;
    let unchanged = git::git_ok(root, &["diff", "--cached", "--quiet", "--no-ext-diff"])?;
    if first || !unchanged {
        commit(root, message, first)?;
    }
    Ok(Snapshot {
        commit: git::head(root)?,
        left_out: survey.left_out,
    })
}

const MB: u64 = 1024 * 1024;
const GB: u64 = 1024 * MB;

/// The bytes free for you on the disk that holds `path`.
fn free_bytes(path: &Path) -> Option<u64> {
    let stat = nix::sys::statvfs::statvfs(path).ok()?;
    Some(widen(stat.blocks_available()).saturating_mul(widen(stat.fragment_size())))
}

/// The counts statvfs gives are 32 bits on some systems and 64 on others.
fn widen<T: TryInto<u64>>(count: T) -> u64 {
    count.try_into().unwrap_or(u64::MAX)
}

fn short_of_room(free: u64, bytes: u64, limits: &HistorySettings) -> anyhow::Result<()> {
    let left = free.saturating_sub(bytes);
    if left < limits.min_free_gb.saturating_mul(GB) {
        bail!(
            "recording this folder would leave {:.1} GB free on its disk, less than \
             history.min_free_gb ({} GB). Free some space, or lower the setting",
            left as f64 / GB as f64,
            limits.min_free_gb
        );
    }
    Ok(())
}

fn too_big_to_start(files: u64, bytes: u64, limits: &HistorySettings) -> anyhow::Result<()> {
    if limits.max_files > 0 && files > limits.max_files {
        bail!(
            "this folder has {files} files, more than history.max_files ({}) lets Argonauts \
             record. Start in the project's own folder, add a .gitignore for what isn't part \
             of it, or raise the setting",
            limits.max_files
        );
    }
    let total = limits.max_total_mb.saturating_mul(MB);
    if total > 0 && bytes > total {
        bail!(
            "this folder holds {} MB, more than history.max_total_mb ({}) lets Argonauts \
             record. Start in the project's own folder, add a .gitignore for what isn't part \
             of it, or raise the setting",
            bytes / MB,
            limits.max_total_mb
        );
    }
    Ok(())
}

/// Commits what's staged as Argonauts, whatever identity your git has or lacks, since these
/// commits are its own record and never reach a branch of yours.
fn commit(root: &Path, message: &str, allow_empty: bool) -> anyhow::Result<()> {
    let mut command = git::command(root);
    command.args(["commit", "--quiet", "--no-verify", "-m", message]);
    if allow_empty {
        command.arg("--allow-empty");
    }
    let out = command
        .env("GIT_AUTHOR_NAME", "Argonauts")
        .env("GIT_AUTHOR_EMAIL", "argonauts@localhost")
        .env("GIT_COMMITTER_NAME", "Argonauts")
        .env("GIT_COMMITTER_EMAIL", "argonauts@localhost")
        .output()
        .context("running git commit")?;
    if !out.status.success() {
        bail!(
            "recording the folder failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use argonauts_core::Paths;

    use super::*;
    use crate::env::{AppEnv, FolderGit};

    /// A folder with files and no git, its environment, and the homes they live in.
    fn folder() -> (tempfile::TempDir, tempfile::TempDir, AppEnv) {
        let folder = tempfile::tempdir().unwrap();
        let home = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(folder.path().join("src")).unwrap();
        std::fs::write(folder.path().join("src/app.py"), "print('hi')\n").unwrap();
        std::fs::write(folder.path().join("notes.md"), "todo\n").unwrap();
        let env = AppEnv::for_repo(Paths::new(home.path()), folder.path(), false).unwrap();
        (folder, home, env)
    }

    fn files_in(root: &Path, commit: &str) -> Vec<String> {
        git(root, &["ls-tree", "-r", "--name-only", commit])
            .unwrap()
            .lines()
            .map(str::to_owned)
            .collect()
    }

    #[test]
    fn the_history_records_the_folder_and_puts_nothing_in_it() {
        let (_folder, _home, env) = folder();
        let root = &env.repo_root;
        assert_eq!(env.git(), FolderGit::Off);
        assert!(!git::has_git(root));

        let hidden = turn_on(&env.state_dir).unwrap();
        assert_eq!(env.git(), FolderGit::Hidden(hidden.clone()));
        assert!(hidden.starts_with(&env.state_dir));
        let limits = HistorySettings::default();
        let first = record(root, &limits, "first").unwrap();
        assert_eq!(files_in(root, &first.commit), ["notes.md", "src/app.py"]);
        assert!(first.left_out.is_empty());
        assert_eq!(
            git(root, &["symbolic-ref", "--short", "HEAD"]).unwrap(),
            BRANCH
        );
        // Nothing changed, so nothing new is recorded.
        assert_eq!(record(root, &limits, "again").unwrap().commit, first.commit);

        std::fs::write(root.join("notes.md"), "done\n").unwrap();
        let second = record(root, &limits, "second").unwrap();
        assert_ne!(second.commit, first.commit);
        assert_eq!(
            git(root, &["show", &format!("{}:notes.md", second.commit)]).unwrap(),
            "done"
        );
        assert!(!root.join(".git").exists(), "the folder got a .git");
        let entries: Vec<_> = std::fs::read_dir(root)
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        assert_eq!(entries.len(), 2, "{entries:?}");
    }

    #[test]
    fn big_files_and_nested_repositories_stay_out_and_are_named() {
        let (_folder, _home, env) = folder();
        let root = &env.repo_root;
        turn_on(&env.state_dir).unwrap();
        std::fs::write(root.join("data.bin"), vec![0u8; 2 * 1024 * 1024]).unwrap();
        let nested = root.join("vendor/lib");
        std::fs::create_dir_all(&nested).unwrap();
        crate::git::git(&nested, &["init", "--quiet"]).unwrap();
        std::fs::write(nested.join("lib.c"), "int x;\n").unwrap();
        std::fs::write(root.join(".gitignore"), "*.log\n").unwrap();
        std::fs::write(root.join("build.log"), "noise\n").unwrap();

        let limits = HistorySettings {
            max_file_mb: 1,
            ..HistorySettings::default()
        };
        let snapshot = record(root, &limits, "first").unwrap();
        assert_eq!(
            files_in(root, &snapshot.commit),
            [".gitignore", "notes.md", "src/app.py"]
        );
        assert_eq!(
            snapshot.left_out,
            [
                "data.bin (over 1 MB)",
                "vendor/lib/ (a git repository of its own)"
            ]
        );
        // They stay out the next time too, and stay in the folder.
        let again = record(root, &limits, "again").unwrap();
        assert_eq!(again.commit, snapshot.commit);
        assert_eq!(again.left_out, snapshot.left_out);
        assert!(root.join("data.bin").is_file() && nested.join("lib.c").is_file());
    }

    #[test]
    fn a_folder_too_big_to_start_with_is_refused_with_the_setting_to_change() {
        let (_folder, _home, env) = folder();
        turn_on(&env.state_dir).unwrap();
        let limits = HistorySettings {
            max_files: 1,
            ..HistorySettings::default()
        };
        let err = record(&env.repo_root, &limits, "first").unwrap_err();
        assert!(err.to_string().contains("history.max_files"), "{err}");
        assert!(git::head(&env.repo_root).is_err(), "nothing was recorded");
    }

    #[test]
    fn a_record_never_fills_the_disk_past_what_the_setting_keeps_free() {
        let limits = HistorySettings {
            min_free_gb: 5,
            ..HistorySettings::default()
        };
        assert!(short_of_room(6 * GB, MB, &limits).is_ok());
        let err = short_of_room(6 * GB, 2 * GB, &limits).unwrap_err();
        assert!(
            err.to_string().contains("4.0 GB free")
                && err.to_string().contains("history.min_free_gb (5 GB)"),
            "{err}"
        );

        // A real disk that can't spare a million gigabytes refuses before anything is
        // written, and 0 turns the check off.
        let (_folder, _home, env) = folder();
        turn_on(&env.state_dir).unwrap();
        let greedy = HistorySettings {
            min_free_gb: 1_000_000,
            ..HistorySettings::default()
        };
        let err = record(&env.repo_root, &greedy, "first").unwrap_err();
        assert!(err.to_string().contains("history.min_free_gb"), "{err}");
        assert!(git::head(&env.repo_root).is_err(), "nothing was recorded");
        let off = HistorySettings {
            min_free_gb: 0,
            ..greedy
        };
        assert!(record(&env.repo_root, &off, "first").is_ok());
    }

    #[test]
    fn your_home_and_the_top_of_the_disk_are_never_recorded() {
        let home = tempfile::tempdir().unwrap();
        let home = home.path().canonicalize().unwrap();
        assert_eq!(refused(&home, Some(&home)), Some("your home folder"));
        assert_eq!(
            refused(Path::new("/"), Some(&home)),
            Some("the top of the file system")
        );
        let project = home.join("project");
        std::fs::create_dir(&project).unwrap();
        assert_eq!(refused(&project, Some(&home)), None);
    }
}
