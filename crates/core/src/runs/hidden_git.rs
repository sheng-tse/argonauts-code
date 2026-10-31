//! Where the history of a folder with no git of its own is kept, and whether it's in use.
//!
//! That history is a git repository in the folder's state under Argonauts Code's home, with your
//! folder as its work tree. Every git call for the folder names both, so the folder gets no
//! `.git` and nothing in it points here. The app makes and records it; the engine only needs
//! to know where it is.

use std::path::{Path, PathBuf};

use crate::base::layout::repo_state;

/// In the history's folder while it's turned off. The history itself stays, so turning it
/// back on loses nothing.
pub const OFF: &str = "argonauts-off";

/// The history's one branch, which follows your folder. A run names the branch it starts from
/// and merges into wherever it speaks of them, so the name says what that is here.
pub const BRANCH: &str = "your-folder";

/// Where a merged run's work went, read from the branch its merge names: onto a branch of
/// yours, or, in a folder without git, into the folder itself. Screens word a run's ending
/// through this, so the hidden history's branch never reads as one of yours.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MergedInto<'a> {
    Branch(&'a str),
    YourFolder,
}

impl<'a> MergedInto<'a> {
    /// By the target a run's merge names.
    pub fn of(target: &'a str) -> Self {
        if target == BRANCH {
            Self::YourFolder
        } else {
            Self::Branch(target)
        }
    }

    /// How the run ended, as the end of its row says it, such as `merged onto main`.
    pub fn ended(self) -> String {
        match self {
            Self::Branch(branch) => format!("merged onto {branch}"),
            Self::YourFolder => "written into your folder".to_owned(),
        }
    }

    /// The same as a heading, such as `Merged onto main`.
    pub fn heading(self) -> String {
        match self {
            Self::Branch(branch) => format!("Merged onto {branch}"),
            Self::YourFolder => "Written into your folder".to_owned(),
        }
    }

    /// What the merge question's evidence is headed with, such as `Merge the run into main`.
    pub fn asked(self) -> String {
        match self {
            Self::Branch(branch) => format!("Merge the run into {branch}"),
            Self::YourFolder => "Write the run into your folder".to_owned(),
        }
    }

    /// What approving it is recorded as, about `subject`, such as `Approved merging the run
    /// into main`.
    pub fn approved(self, subject: &str) -> String {
        match self {
            Self::Branch(branch) => format!("Approved merging {subject} into {branch}"),
            Self::YourFolder => format!("Approved writing {subject} into your folder"),
        }
    }

    /// What a run that waits on your files is headed with.
    pub fn waiting(self) -> String {
        match self {
            Self::Branch(branch) => format!("Waiting to merge into {branch}"),
            Self::YourFolder => "Waiting to be written into your folder".to_owned(),
        }
    }
}

/// Said beside a run written into a folder without git, where a branch of yours would name
/// git: the history is Argonauts Code's own, and only `/rewind` reads it.
pub const HISTORY_KEPT: &str = "history kept by Argonauts";

/// How a run written into a folder without git is undone, said after its heading.
pub const PUT_BACK: &str = "/rewind puts it back";

/// Where the history of the project with this state folder is kept.
pub fn dir(state_dir: &Path) -> PathBuf {
    state_dir.join(repo_state::HIDDEN_GIT)
}

/// Whether the history at `dir` exists and is in use.
pub fn is_on(dir: &Path) -> bool {
    dir.join("HEAD").is_file() && !dir.join(OFF).exists()
}

/// The hidden history `folder` works with: none when it has a `.git` of its own, or keeps no
/// history at all.
pub fn of(folder: &Path, state_dir: &Path) -> Option<PathBuf> {
    let hidden = dir(state_dir);
    (!folder.join(".git").exists() && is_on(&hidden)).then_some(hidden)
}

/// How long a lock file may sit before it counts as left behind while a run host of the folder
/// is alive. Git holds one for the moment it writes an index or a ref, so a lock this old
/// belongs to a git that was stopped in the middle and will never take it away.
pub const STALE_LOCK: std::time::Duration = std::time::Duration::from_secs(5 * 60);

/// The same with no run host of the folder alive: nothing that writes for long is left, so a
/// lock that outlived this moment was left behind.
pub const UNHELD_LOCK: std::time::Duration = std::time::Duration::from_secs(2);

/// What a stale lock is said with, once it's cleared.
pub const CLEARED_STALE_LOCK: &str = "Cleared a lock on this folder's hidden history that a stop \
                                      in the middle of a write had left behind.";

/// Who else may be writing a folder's history while its locks are looked at.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Writers {
    /// No other run host of the folder is alive.
    None,
    /// Another run host of the folder is alive, or nobody could say.
    Maybe,
}

impl Writers {
    fn stale_after(self) -> std::time::Duration {
        match self {
            Self::None => UNHELD_LOCK,
            Self::Maybe => STALE_LOCK,
        }
    }
}

/// The lock files under `dir`, at any depth.
fn locks_under(dir: &Path, found: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if entry.file_type().is_ok_and(|kind| kind.is_dir()) {
            locks_under(&path, found);
        } else if path.extension().is_some_and(|ext| ext == "lock") {
            found.push(path);
        }
    }
}

/// Every lock git may hold in the history at `dir`: on its index, its head and its packed
/// refs, on each branch, a run's among them, and on the index and head of each copy an agent
/// works in.
fn lock_files(dir: &Path) -> Vec<PathBuf> {
    let mut locks: Vec<PathBuf> = ["index.lock", "HEAD.lock", "packed-refs.lock"]
        .into_iter()
        .map(|name| dir.join(name))
        .filter(|lock| lock.exists())
        .collect();
    locks_under(&dir.join("refs"), &mut locks);
    let copies = std::fs::read_dir(dir.join("worktrees"));
    for copy in copies.into_iter().flatten().flatten() {
        for name in ["index.lock", "HEAD.lock"] {
            let lock = copy.path().join(name);
            if lock.exists() {
                locks.push(lock);
            }
        }
    }
    locks
}

fn age(lock: &Path, now: std::time::SystemTime) -> Option<std::time::Duration> {
    let written = std::fs::metadata(lock)
        .and_then(|meta| meta.modified())
        .ok()?;
    // A lock dated after `now` was taken this instant.
    Some(now.duration_since(written).unwrap_or_default())
}

/// Removes the lock files in the history at `dir` that were left behind by `now`, and says
/// whether any went. With no other writer a lock goes once it is [`UNHELD_LOCK`] old, and
/// otherwise once it is [`STALE_LOCK`] old. A younger lock may belong to a git still writing,
/// so it stays, and [`standing_lock`] tells.
pub fn clear_stale_locks(dir: &Path, now: std::time::SystemTime, writers: Writers) -> bool {
    let stale = |lock: &PathBuf| age(lock, now).is_some_and(|age| age >= writers.stale_after());
    // A lock that can't be removed stays, and git's own words say what's wrong.
    lock_files(dir)
        .iter()
        .filter(|lock| stale(lock))
        .filter(|lock| std::fs::remove_file(lock).is_ok())
        .count()
        > 0
}

/// How long ago the newest lock still on the history at `dir` was taken, when one is there.
pub fn standing_lock(dir: &Path, now: std::time::SystemTime) -> Option<std::time::Duration> {
    lock_files(dir)
        .iter()
        .filter_map(|lock| age(lock, now))
        .min()
}

/// Why nothing can be written to a history that holds a lock taken `age` ago.
pub fn locked(age: std::time::Duration) -> String {
    let seconds = age.as_secs();
    let ago = match (seconds / 60, seconds % 60) {
        (0, seconds) => format!("{seconds}s"),
        (minutes, seconds) => format!("{minutes}m{seconds:02}s"),
    };
    format!(
        "this folder's hidden history is being written, or a stop left a lock {ago} ago · try \
         again in a moment"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_run_in_a_folder_without_git_is_written_into_it_and_never_merged_onto_a_branch() {
        let folder = MergedInto::of(BRANCH);
        assert_eq!(folder, MergedInto::YourFolder);
        assert_eq!(folder.ended(), "written into your folder");
        assert_eq!(folder.heading(), "Written into your folder");
        assert_eq!(folder.asked(), "Write the run into your folder");
        assert_eq!(
            folder.approved("the run"),
            "Approved writing the run into your folder"
        );
        assert_eq!(folder.waiting(), "Waiting to be written into your folder");

        let branch = MergedInto::of("main");
        assert_eq!(branch, MergedInto::Branch("main"));
        assert_eq!(branch.ended(), "merged onto main");
        assert_eq!(branch.heading(), "Merged onto main");
        assert_eq!(branch.asked(), "Merge the run into main");
        assert_eq!(
            branch.approved("the run"),
            "Approved merging the run into main"
        );
        assert_eq!(branch.waiting(), "Waiting to merge into main");
    }

    #[test]
    fn only_a_lock_left_behind_long_ago_is_cleared_while_a_run_host_lives() {
        let state = tempfile::tempdir().unwrap();
        let hidden = dir(state.path());
        std::fs::create_dir_all(hidden.join("refs/heads")).unwrap();
        let index = hidden.join("index.lock");
        let branch = hidden.join("refs/heads").join(format!("{BRANCH}.lock"));
        std::fs::write(&index, "").unwrap();
        std::fs::write(&branch, "").unwrap();
        let now = std::time::SystemTime::now();

        // A git that is writing right now keeps its lock.
        assert!(!clear_stale_locks(&hidden, now, Writers::Maybe));
        assert!(index.exists() && branch.exists());
        assert!(standing_lock(&hidden, now).is_some());

        let later = now + STALE_LOCK + std::time::Duration::from_secs(1);
        assert!(clear_stale_locks(&hidden, later, Writers::Maybe));
        assert!(!index.exists() && !branch.exists());
        assert_eq!(standing_lock(&hidden, later), None);
        assert!(
            !clear_stale_locks(&hidden, later, Writers::Maybe),
            "nothing is left to clear"
        );
    }

    #[test]
    fn with_no_run_host_alive_a_lock_goes_once_no_write_could_still_hold_it() {
        let state = tempfile::tempdir().unwrap();
        let hidden = dir(state.path());
        std::fs::create_dir_all(&hidden).unwrap();
        let index = hidden.join("index.lock");
        std::fs::write(&index, "").unwrap();
        let now = std::time::SystemTime::now();

        assert!(!clear_stale_locks(&hidden, now, Writers::None));
        assert!(index.exists(), "a write this instant keeps its lock");

        let soon = now + UNHELD_LOCK + std::time::Duration::from_secs(1);
        assert!(clear_stale_locks(&hidden, soon, Writers::None));
        assert!(!index.exists());
    }

    #[test]
    fn a_stop_in_a_tasks_commit_leaves_locks_that_are_found_too() {
        let state = tempfile::tempdir().unwrap();
        let hidden = dir(state.path());
        let copy = hidden.join("worktrees/task-001");
        let runs = hidden.join("refs/heads/argonauts/run-01");
        std::fs::create_dir_all(&copy).unwrap();
        std::fs::create_dir_all(&runs).unwrap();
        let locks = [copy.join("index.lock"), runs.join("task-001.lock")];
        for lock in &locks {
            std::fs::write(lock, "").unwrap();
        }
        std::fs::write(runs.join("merged"), "0000\n").unwrap();
        let later = std::time::SystemTime::now() + STALE_LOCK;

        assert!(clear_stale_locks(&hidden, later, Writers::None));
        assert!(locks.iter().all(|lock| !lock.exists()));
        assert!(runs.join("merged").exists(), "a branch is never a lock");
    }

    #[test]
    fn a_lock_that_stands_says_how_old_it_is() {
        assert_eq!(
            locked(std::time::Duration::from_secs(40)),
            "this folder's hidden history is being written, or a stop left a lock 40s ago · \
             try again in a moment"
        );
        assert!(locked(std::time::Duration::from_secs(125)).contains("a lock 2m05s ago"));
    }

    #[test]
    fn a_folder_uses_its_hidden_history_only_while_it_is_on_and_it_has_no_git() {
        let folder = tempfile::tempdir().unwrap();
        let state = tempfile::tempdir().unwrap();
        assert_eq!(of(folder.path(), state.path()), None);

        let hidden = dir(state.path());
        std::fs::create_dir_all(&hidden).unwrap();
        std::fs::write(hidden.join("HEAD"), "ref: refs/heads/main\n").unwrap();
        assert_eq!(of(folder.path(), state.path()), Some(hidden.clone()));

        std::fs::write(hidden.join(OFF), "").unwrap();
        assert_eq!(of(folder.path(), state.path()), None);
        std::fs::remove_file(hidden.join(OFF)).unwrap();

        std::fs::create_dir(folder.path().join(".git")).unwrap();
        assert_eq!(of(folder.path(), state.path()), None);
    }
}
