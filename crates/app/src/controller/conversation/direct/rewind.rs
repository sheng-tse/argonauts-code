//! `/rewind` for direct turns: a snapshot of your folder before and after each one, and putting
//! back what turns changed. A snapshot is a git tree written through a private index, so it
//! holds tracked and untracked files alike as git objects no branch points to. Untracked files
//! over `captain.rewind_max_mb` stay out of it, and a turn that changes one lists it as one
//! `/rewind` can't restore.
//!
//! Ignored files are left out too, since a folder's build output can be huge, except the ones
//! turns wrote, which every later snapshot keeps. One a turn writes before any snapshot kept it
//! is listed as one `/rewind` can't put back, as nothing says what it held.

use std::collections::{BTreeMap, BTreeSet};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use anyhow::{Context as _, bail};

/// Your folder at one moment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Snapshot {
    tree: String,
    /// Untracked files too big to keep, by path from the root, with their size and when they
    /// last changed, so one changed in place is still seen.
    too_big: BTreeMap<String, Stamp>,
    /// Ignored files it keeps, because a turn wrote them.
    kept_ignored: BTreeSet<String>,
}

/// A big file's size and modification time, which say whether it changed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Stamp {
    len: u64,
    modified: Option<std::time::SystemTime>,
}

impl Stamp {
    fn of(meta: &std::fs::Metadata) -> Self {
        Self {
            len: meta.len(),
            modified: meta.modified().ok(),
        }
    }
}

/// What putting files back did.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct Restored {
    /// As the turn found them, or gone when the turn made them.
    pub put_back: Vec<String>,
    /// You changed them after the turn, so they stay as you left them.
    pub kept: Vec<String>,
    /// Too big to have been kept.
    pub cant: Vec<String>,
}

/// Snapshots the folder at `root` without touching your index or any branch, with the ignored
/// files of `watched` that are there and small enough.
pub(crate) fn take(
    root: &Path,
    max_bytes: u64,
    watched: &BTreeSet<String>,
) -> anyhow::Result<Snapshot> {
    let index = ScratchIndex::new();
    let index = index.path.as_path();
    let own_index = git_path(root, "index")?;
    // A copy of yours keeps git's file stats, so only what changed is read again.
    if own_index.is_file() {
        crate::git::copy_index(&own_index, index).context("copying the index")?;
    }
    let too_big: BTreeMap<String, Stamp> = untracked(root)?
        .into_iter()
        .filter_map(|path| {
            let meta = std::fs::metadata(root.join(&path)).ok()?;
            (meta.len() > max_bytes).then(|| (path, Stamp::of(&meta)))
        })
        .collect();
    let mut add: Vec<String> = vec!["add".into(), "-A".into(), "--".into(), ".".into()];
    add.extend(
        too_big
            .keys()
            .map(|path| format!(":(exclude,literal){path}")),
    );
    with_index(root, index, &add)?;
    let mut kept_ignored = BTreeSet::new();
    for path in watched {
        let small = std::fs::metadata(root.join(path))
            .is_ok_and(|meta| meta.is_file() && meta.len() <= max_bytes);
        if small && ignored(root, path) {
            let literal = format!(":(literal){path}");
            with_index(
                root,
                index,
                &["add".into(), "-f".into(), "--".into(), literal],
            )?;
            kept_ignored.insert(path.clone());
        }
    }
    let tree = with_index(root, index, &["write-tree".to_owned()])?;
    Ok(Snapshot {
        tree,
        too_big,
        kept_ignored,
    })
}

/// Whether git ignores `path` in the folder at `root`, so `add -A` leaves it out.
pub(crate) fn ignored(root: &Path, path: &str) -> bool {
    crate::git::command(root)
        .args(["check-ignore", "-q", "--", path])
        .status()
        .is_ok_and(|status| status.success())
}

/// Paths a turn wrote that `before` couldn't have kept: ignored and not watched yet, or an
/// untracked file too big to keep. Nothing says what they held, so `/rewind` can't put them
/// back. A tracked file is kept whatever its size.
pub(crate) fn unkept(
    root: &Path,
    before: &Snapshot,
    written: &BTreeSet<String>,
    max_bytes: u64,
) -> Vec<String> {
    written
        .iter()
        .filter(|path| {
            let big = std::fs::metadata(root.join(path)).is_ok_and(|meta| meta.len() > max_bytes)
                && entry(root, before, path).ok().flatten().is_none();
            let missed = !before.kept_ignored.contains(*path) && ignored(root, path);
            big || missed
        })
        .cloned()
        .collect()
}

/// The paths that are different between two snapshots.
pub(crate) fn changed(
    root: &Path,
    before: &Snapshot,
    after: &Snapshot,
) -> anyhow::Result<Vec<String>> {
    let mut paths: BTreeSet<String> = git(
        root,
        &[
            "diff-tree",
            "-r",
            "-z",
            "--name-only",
            &before.tree,
            &after.tree,
        ],
    )?
    .split('\0')
    .filter(|path| !path.is_empty())
    .map(str::to_owned)
    .collect();
    let big = before.too_big.keys().chain(after.too_big.keys());
    paths.extend(
        big.filter(|path| before.too_big.get(*path) != after.too_big.get(*path))
            .cloned(),
    );
    Ok(paths.into_iter().collect())
}

/// Lines added and removed in each changed text file, such as `invoice/money.py (+9 -3)`, and
/// the rest by name.
pub(crate) fn summary(
    root: &Path,
    before: &Snapshot,
    after: &Snapshot,
    paths: &[String],
) -> String {
    let counted: Vec<(String, String)> = git(
        root,
        &["diff-tree", "-r", "--numstat", &before.tree, &after.tree],
    )
    .unwrap_or_default()
    .lines()
    .filter_map(|line| {
        let mut fields = line.splitn(3, '\t');
        let (added, removed, path) = (fields.next()?, fields.next()?, fields.next()?);
        let counts = if added == "-" {
            String::new()
        } else {
            format!(" (+{added} -{removed})")
        };
        Some((path.to_owned(), counts))
    })
    .collect();
    let named: Vec<String> = paths
        .iter()
        .map(|path| {
            let counts = counted
                .iter()
                .find(|(counted, _)| counted == path)
                .map_or("", |(_, counts)| counts.as_str());
            format!("{path}{counts}")
        })
        .collect();
    match named.as_slice() {
        [] => String::new(),
        [one] => format!("{one} changed"),
        [first, second] => format!("{first} and {second} changed"),
        [first, rest @ ..] => format!("{first} and {} more files changed", rest.len()),
    }
}

/// Puts each of `paths` back as `target` holds it, where the file still matches `last`, the
/// snapshot after the newest turn. A file you changed since stays as it is.
pub(crate) fn restore(
    root: &Path,
    target: &Snapshot,
    last: &Snapshot,
    paths: &[String],
    max_bytes: u64,
) -> anyhow::Result<Restored> {
    let now = take(root, max_bytes, &last.kept_ignored)?;
    let mut restored = Restored::default();
    for path in paths {
        let big = [target, last, &now]
            .iter()
            .any(|snapshot| snapshot.too_big.contains_key(path));
        // An ignored file the target never kept may have held anything, so it's never removed.
        let unknown = !target.kept_ignored.contains(path) && ignored(root, path);
        if big || unknown {
            restored.cant.push(path.clone());
            continue;
        }
        if entry(root, &now, path)? != entry(root, last, path)? {
            restored.kept.push(path.clone());
            continue;
        }
        put(root, &entry(root, target, path)?, path)?;
        restored.put_back.push(path.clone());
    }
    Ok(restored)
}

/// A path's mode and blob in a snapshot, or `None` where it doesn't exist.
fn entry(root: &Path, snapshot: &Snapshot, path: &str) -> anyhow::Result<Option<(String, String)>> {
    let listed = git(root, &["ls-tree", "-z", &snapshot.tree, "--", path])?;
    let line = listed.trim_end_matches('\0');
    if line.is_empty() {
        return Ok(None);
    }
    // `<mode> blob <sha>\t<path>`
    let mut fields = line.split(['\t', ' ']);
    match (fields.next(), fields.next(), fields.next()) {
        (Some(mode), Some("blob"), Some(sha)) => Ok(Some((mode.to_owned(), sha.to_owned()))),
        _ => Ok(None),
    }
}

fn put(root: &Path, entry: &Option<(String, String)>, path: &str) -> anyhow::Result<()> {
    let file = root.join(path);
    let Some((mode, sha)) = entry else {
        if file.exists() {
            std::fs::remove_file(&file).with_context(|| format!("removing {path}"))?;
        }
        return Ok(());
    };
    let output = crate::git::command(root)
        .args(["cat-file", "blob", sha])
        .output()
        .context("reading a file back from git")?;
    if !output.status.success() {
        bail!("git couldn't read {path} back");
    }
    if let Some(parent) = file.parent() {
        std::fs::create_dir_all(parent).with_context(|| format!("making {}", parent.display()))?;
    }
    std::fs::write(&file, &output.stdout).with_context(|| format!("writing {path}"))?;
    let executable = mode == "100755";
    let mut permissions = std::fs::metadata(&file)?.permissions();
    let bits = permissions.mode();
    permissions.set_mode(if executable {
        bits | 0o111
    } else {
        bits & !0o111
    });
    std::fs::set_permissions(&file, permissions)?;
    Ok(())
}

/// A private index in temp, gone when dropped.
struct ScratchIndex {
    path: PathBuf,
}

impl ScratchIndex {
    fn new() -> Self {
        let name = format!("argonauts-rewind-{}.index", uuid::Uuid::new_v4().simple());
        Self {
            path: std::env::temp_dir().join(name),
        }
    }
}

impl Drop for ScratchIndex {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

fn untracked(root: &Path) -> anyhow::Result<Vec<String>> {
    Ok(
        git(root, &["ls-files", "--others", "--exclude-standard", "-z"])?
            .split('\0')
            .filter(|path| !path.is_empty())
            .map(str::to_owned)
            .collect(),
    )
}

fn git_path(root: &Path, name: &str) -> anyhow::Result<PathBuf> {
    let path = PathBuf::from(git(root, &["rev-parse", "--git-path", name])?);
    Ok(if path.is_absolute() {
        path
    } else {
        root.join(path)
    })
}

fn with_index(root: &Path, index: &Path, args: &[String]) -> anyhow::Result<String> {
    let output = crate::git::command(root)
        .args(args)
        .env("GIT_INDEX_FILE", index)
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

fn git(root: &Path, args: &[&str]) -> anyhow::Result<String> {
    crate::git::git(root, args)
}

#[cfg(test)]
mod tests {
    use super::*;
    use argonauts_testkit::TempRepo;

    const MB: u64 = 1024 * 1024;

    fn read(root: &Path, path: &str) -> Option<String> {
        std::fs::read_to_string(root.join(path)).ok()
    }

    /// A folder with no git rewinds through its hidden history once that's on, and gets no
    /// `.git` for it.
    #[test]
    fn a_folder_with_a_hidden_history_rewinds_like_a_repository() {
        let folder = tempfile::tempdir().unwrap();
        let home = tempfile::tempdir().unwrap();
        let env = crate::startup::env::AppEnv::for_repo(
            argonauts_core::Paths::new(home.path()),
            folder.path(),
            false,
        )
        .unwrap();
        let root = env.repo_root.as_path();
        std::fs::write(root.join("notes.txt"), "before\n").unwrap();
        assert!(take(root, MB, &BTreeSet::new()).is_err(), "no git yet");

        crate::git::hidden_git::turn_on(&env.state_dir).unwrap();
        let before = take(root, MB, &BTreeSet::new()).unwrap();
        std::fs::write(root.join("notes.txt"), "after\n").unwrap();
        std::fs::write(root.join("made.txt"), "new\n").unwrap();
        let after = take(root, MB, &BTreeSet::new()).unwrap();
        let paths = changed(root, &before, &after).unwrap();
        assert_eq!(paths, ["made.txt", "notes.txt"]);
        let restored = restore(root, &before, &after, &paths, MB).unwrap();
        assert_eq!(restored.put_back, ["made.txt", "notes.txt"]);
        assert_eq!(read(root, "notes.txt").as_deref(), Some("before\n"));
        assert_eq!(read(root, "made.txt"), None);
        assert!(!root.join(".git").exists());
    }

    /// A tracked edit goes back, a file the turn made goes, an untracked file under the limit
    /// comes back, one over it is listed, and a file you changed after the turn stays yours.
    #[test]
    fn rewind_puts_back_what_the_turn_changed_and_nothing_you_did() {
        let repo = TempRepo::new().unwrap();
        let root = repo.root();
        let write = |path: &str, text: &str| {
            repo.write(path, text).unwrap();
        };
        write("tracked.txt", "before\n");
        repo.commit_all("start").unwrap();
        write("notes.txt", "untracked before\n");
        write("big.bin", &"x".repeat(2 * 1024));
        write("yours.txt", "yours before\n");
        let limit = 1024;
        let before = take(root, limit, &BTreeSet::new()).unwrap();

        // The turn.
        write("tracked.txt", "after\n");
        write("made.txt", "new\n");
        write("notes.txt", "untracked after\n");
        write("big.bin", &"y".repeat(2 * 1024));
        write("yours.txt", "turn's\n");
        let after = take(root, limit, &BTreeSet::new()).unwrap();
        let paths = changed(root, &before, &after).unwrap();
        assert_eq!(
            paths,
            [
                "big.bin",
                "made.txt",
                "notes.txt",
                "tracked.txt",
                "yours.txt"
            ],
            "a big file changed in place is seen, by its size and time"
        );

        // You, after the turn.
        write("yours.txt", "yours after\n");
        let restored = restore(root, &before, &after, &paths, limit).unwrap();
        assert_eq!(restored.put_back, ["made.txt", "notes.txt", "tracked.txt"]);
        assert_eq!(restored.kept, ["yours.txt"]);
        assert_eq!(restored.cant, ["big.bin"]);
        assert_eq!(read(root, "tracked.txt").as_deref(), Some("before\n"));
        assert_eq!(
            read(root, "notes.txt").as_deref(),
            Some("untracked before\n")
        );
        assert_eq!(read(root, "made.txt"), None);
        assert_eq!(read(root, "yours.txt").as_deref(), Some("yours after\n"));
    }

    /// An ignored file a turn wrote isn't one the snapshot before it kept, so it's listed as one
    /// `/rewind` can't put back, never removed. Snapshots keep it from then on, so a later turn
    /// that changes it again can be put back.
    #[test]
    fn ignored_files_a_turn_writes_are_kept_from_then_on() {
        let repo = TempRepo::new().unwrap();
        let root = repo.root();
        repo.write(".gitignore", ".env\nconfig/local.toml\n")
            .unwrap();
        repo.write("app.py", "print(1)\n").unwrap();
        repo.commit_all("start").unwrap();
        repo.write(".env", "TOKEN=yours\n").unwrap();
        let none = BTreeSet::new();
        let before = take(root, MB, &none).unwrap();

        // The first turn writes .env and app.py.
        repo.write(".env", "TOKEN=changed-by-agent\n").unwrap();
        repo.write("app.py", "print(2)\n").unwrap();
        let written: BTreeSet<String> = [".env", "app.py"].map(str::to_owned).into();
        let after = take(root, MB, &written).unwrap();
        assert_eq!(unkept(root, &before, &written, MB), [".env"]);
        let mut paths = changed(root, &before, &after).unwrap();
        paths.extend(unkept(root, &before, &written, MB));
        paths.sort();
        paths.dedup();
        assert_eq!(paths, [".env", "app.py"]);
        let restored = restore(root, &before, &after, &paths, MB).unwrap();
        assert_eq!(restored.put_back, ["app.py"]);
        assert_eq!(restored.cant, [".env"]);
        assert_eq!(
            read(root, ".env").as_deref(),
            Some("TOKEN=changed-by-agent\n")
        );

        // From then on .env is watched, so a turn that changes it again can be put back.
        let watched: BTreeSet<String> = [".env".to_owned()].into();
        let second = take(root, MB, &watched).unwrap();
        repo.write(".env", "TOKEN=second\n").unwrap();
        let later = take(root, MB, &watched).unwrap();
        assert!(unkept(root, &second, &watched, MB).is_empty());
        let paths = changed(root, &second, &later).unwrap();
        assert_eq!(paths, [".env"]);
        let restored = restore(root, &second, &later, &paths, MB).unwrap();
        assert_eq!(restored.put_back, [".env"]);
        assert_eq!(
            read(root, ".env").as_deref(),
            Some("TOKEN=changed-by-agent\n")
        );
    }

    #[test]
    fn a_big_file_the_turn_made_is_listed_as_one_it_cant_restore() {
        let repo = TempRepo::new().unwrap();
        let root = repo.root();
        let before = take(root, 10, &BTreeSet::new()).unwrap();
        repo.write("huge.log", &"z".repeat(100)).unwrap();
        let after = take(root, 10, &BTreeSet::new()).unwrap();
        let paths = changed(root, &before, &after).unwrap();
        assert_eq!(paths, ["huge.log"]);
        let restored = restore(root, &before, &after, &paths, 10).unwrap();
        assert_eq!(restored.cant, ["huge.log"]);
        assert!(root.join("huge.log").exists());
    }

    #[test]
    fn a_repo_with_no_commits_still_snapshots() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        crate::git::git(root, &["init", "--quiet"]).unwrap();
        std::fs::write(root.join("first.txt"), "1\n").unwrap();
        let before = take(root, MB, &BTreeSet::new()).unwrap();
        std::fs::write(root.join("first.txt"), "2\n").unwrap();
        let after = take(root, MB, &BTreeSet::new()).unwrap();
        let paths = changed(root, &before, &after).unwrap();
        restore(root, &before, &after, &paths, MB).unwrap();
        assert_eq!(read(root, "first.txt").as_deref(), Some("1\n"));
    }
}
