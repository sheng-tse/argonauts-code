//! Files of yours that already hold what a landing brings: the same bytes at a path the run
//! writes, or nothing at a path it removes. They aren't in its way. Git would stop at them all
//! the same, since it never compares what it would overwrite, so landing stages such an edit
//! and sets such an untracked file aside for the moment it writes.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use super::{Git, GitError, PATHS_PER_CALL};

/// A path's entry in a commit or an index.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Entry {
    pub(crate) mode: String,
    pub(crate) object: String,
}

impl Git {
    /// `rev`'s entries for `paths`. A path `rev` doesn't have is left out.
    pub(crate) async fn entries(
        &self,
        dir: &Path,
        rev: &str,
        paths: &[&str],
    ) -> Result<BTreeMap<String, Entry>, GitError> {
        let mut found = BTreeMap::new();
        for chunk in paths.chunks(PATHS_PER_CALL) {
            let mut args = vec!["--literal-pathspecs", "ls-tree", "-z", "--full-tree", rev];
            args.push("--");
            args.extend(chunk);
            let listed = self.run(dir, &args).await?;
            found.extend(parse_entries(&listed, IN_A_TREE));
        }
        Ok(found)
    }

    /// Of `paths` in `checkout`, the ones that already hold what `rev` has there: a file with
    /// the same bytes, after the filters its path is given, and the same executable bit, or
    /// nothing where `rev` has nothing. A folder, a link or a submodule never does.
    pub async fn holding(
        &self,
        checkout: &Path,
        rev: &str,
        paths: &[String],
    ) -> Result<Vec<String>, GitError> {
        let files: Vec<&str> = paths
            .iter()
            .map(String::as_str)
            .filter(|path| !path.ends_with('/'))
            .collect();
        if files.is_empty() {
            return Ok(Vec::new());
        }
        let entries = self.entries(checkout, rev, &files).await?;
        let mut holding = Vec::new();
        let mut to_hash: Vec<(&str, &str)> = Vec::new();
        for path in files {
            let on_disk = std::fs::symlink_metadata(checkout.join(path));
            match (entries.get(path), on_disk) {
                (None, Err(err)) if err.kind() == std::io::ErrorKind::NotFound => {
                    holding.push(path.to_owned());
                }
                (Some(entry), Ok(meta)) if meta.is_file() && same_mode(&entry.mode, &meta) => {
                    to_hash.push((path, &entry.object));
                }
                _ => {}
            }
        }
        for chunk in to_hash.chunks(PATHS_PER_CALL) {
            let mut args = vec!["hash-object", "--"];
            args.extend(chunk.iter().map(|(path, _)| *path));
            let hashed = self.run(checkout, &args).await?;
            for ((path, object), hashed) in chunk.iter().zip(hashed.lines()) {
                if hashed.trim() == *object {
                    holding.push((*path).to_owned());
                }
            }
        }
        holding.sort();
        Ok(holding)
    }

    /// Of `paths`, the ones your index holds in a version that is neither `from`'s nor `to`'s:
    /// something you staged on the way that taking the file as it is would lose.
    pub async fn staged_apart(
        &self,
        checkout: &Path,
        from: &str,
        to: &str,
        paths: &[String],
    ) -> Result<BTreeSet<String>, GitError> {
        let paths: Vec<&str> = paths.iter().map(String::as_str).collect();
        if paths.is_empty() {
            return Ok(BTreeSet::new());
        }
        let before = self.entries(checkout, from, &paths).await?;
        let after = self.entries(checkout, to, &paths).await?;
        let mut staged = BTreeMap::new();
        for chunk in paths.chunks(PATHS_PER_CALL) {
            let mut args = vec!["--literal-pathspecs", "ls-files", "--stage", "-z", "--"];
            args.extend(chunk);
            let listed = self.run(checkout, &args).await?;
            staged.extend(parse_entries(&listed, IN_THE_INDEX));
        }
        let object = |entries: &BTreeMap<String, Entry>, path: &str| {
            entries.get(path).map(|entry| entry.object.clone())
        };
        Ok(paths
            .into_iter()
            .filter(|path| {
                let yours = object(&staged, path);
                yours != object(&before, path) && yours != object(&after, path)
            })
            .map(str::to_owned)
            .collect())
    }

    /// Makes the index `with_index` names hold what `rev` has at `paths`, which your files
    /// there hold already, so that writing `rev` out takes them as they are.
    pub(crate) async fn take_as_they_are(
        &self,
        checkout: &Path,
        rev: &str,
        paths: &[String],
        with_index: &[(&str, &str)],
    ) -> Result<(), GitError> {
        let paths: Vec<&str> = paths.iter().map(String::as_str).collect();
        let entries = self.entries(checkout, rev, &paths).await?;
        let infos: Vec<String> = entries
            .iter()
            .map(|(path, entry)| format!("{},{},{path}", entry.mode, entry.object))
            .collect();
        for chunk in infos.chunks(PATHS_PER_CALL) {
            let mut args = vec!["update-index", "--add"];
            for info in chunk {
                args.extend(["--cacheinfo", info.as_str()]);
            }
            self.in_index(checkout, &args, with_index).await?;
        }
        let gone: Vec<&str> = paths
            .into_iter()
            .filter(|path| !entries.contains_key(*path))
            .collect();
        for chunk in gone.chunks(PATHS_PER_CALL) {
            let mut args = vec!["update-index", "--force-remove", "--"];
            args.extend(chunk);
            self.in_index(checkout, &args, with_index).await?;
        }
        Ok(())
    }

    /// Stages your files at `paths` as they are, a deletion too. Landing does it for files
    /// that already hold what lands, so that git's fast-forward takes them.
    pub async fn stage_as_they_are(
        &self,
        checkout: &Path,
        paths: &[String],
    ) -> Result<(), GitError> {
        for chunk in paths.chunks(PATHS_PER_CALL) {
            let mut args = vec!["--literal-pathspecs", "add", "--all", "--"];
            args.extend(chunk.iter().map(String::as_str));
            self.run(checkout, &args).await?;
        }
        Ok(())
    }
}

/// Where the object is among the fields before the tab: `ls-tree` puts the type between.
const IN_A_TREE: usize = 2;
const IN_THE_INDEX: usize = 1;

/// `ls-tree -z` or `ls-files --stage -z` lines: the mode first, the object at `object_field`,
/// then the path after a tab.
fn parse_entries(listed: &str, object_field: usize) -> impl Iterator<Item = (String, Entry)> + '_ {
    listed
        .split('\0')
        .filter(|line| !line.is_empty())
        .filter_map(move |line| {
            let (meta, path) = line.split_once('\t')?;
            let fields: Vec<&str> = meta.split(' ').collect();
            let entry = Entry {
                mode: (*fields.first()?).to_owned(),
                object: (*fields.get(object_field)?).to_owned(),
            };
            Some((path.to_owned(), entry))
        })
}

/// Whether a file on disk has the executable bit a commit's `mode` gives it.
fn same_mode(mode: &str, meta: &std::fs::Metadata) -> bool {
    #[cfg(unix)]
    let executable = {
        use std::os::unix::fs::PermissionsExt;
        meta.permissions().mode() & 0o111 != 0
    };
    #[cfg(not(unix))]
    let executable = {
        let _ = meta;
        false
    };
    match mode {
        "100644" => !executable,
        "100755" => executable,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::{TestRepo, git_test_env};

    fn git() -> Git {
        Git::with_env(git_test_env())
    }

    /// Main with `a.txt`, `gone.txt` and `run.sh`, and a run that changes `a.txt`, adds
    /// `new.txt` and `odd*name.txt`, and removes `gone.txt`. Returns both commits.
    async fn a_run(repo: &TestRepo) -> (String, String) {
        repo.write("a.txt", "a\n");
        repo.write("gone.txt", "gone\n");
        repo.git(&["add", "--all"]);
        repo.git(&["commit", "-qm", "main"]);
        let main = repo.git(&["rev-parse", "HEAD"]);
        repo.git(&["checkout", "-qb", "run"]);
        repo.write("a.txt", "a, by the run\n");
        repo.write("new.txt", "new\n");
        repo.write("odd*name.txt", "odd\n");
        repo.git(&["rm", "-q", "gone.txt"]);
        repo.git(&["add", "--all"]);
        repo.git(&["commit", "-qm", "run"]);
        let run = repo.git(&["rev-parse", "HEAD"]);
        repo.git(&["checkout", "-q", "main"]);
        (main, run)
    }

    fn paths(names: &[&str]) -> Vec<String> {
        names.iter().map(|name| (*name).to_owned()).collect()
    }

    #[tokio::test]
    async fn files_that_already_hold_what_lands_are_told_from_the_rest() {
        let repo = TestRepo::new();
        let git = git();
        let (_, run) = a_run(&repo).await;
        let all = paths(&["a.txt", "new.txt", "odd*name.txt", "gone.txt", "folder/"]);
        assert!(
            git.holding(&repo.root, &run, &all)
                .await
                .unwrap()
                .is_empty()
        );

        repo.write("a.txt", "a, by the run\n");
        repo.write("new.txt", "new\n");
        repo.write("odd*name.txt", "not the same\n");
        std::fs::remove_file(repo.root.join("gone.txt")).unwrap();
        assert_eq!(
            git.holding(&repo.root, &run, &all).await.unwrap(),
            ["a.txt", "gone.txt", "new.txt"]
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn the_same_bytes_with_another_executable_bit_do_not_hold() {
        use std::os::unix::fs::PermissionsExt;
        let repo = TestRepo::new();
        let git = git();
        let (_, run) = a_run(&repo).await;
        repo.write("new.txt", "new\n");
        let file = repo.root.join("new.txt");
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o755)).unwrap();
        let new = paths(&["new.txt"]);
        assert!(
            git.holding(&repo.root, &run, &new)
                .await
                .unwrap()
                .is_empty()
        );
    }

    #[tokio::test]
    async fn something_staged_on_the_way_is_told_apart() {
        let repo = TestRepo::new();
        let git = git();
        let (main, run) = a_run(&repo).await;
        let a = paths(&["a.txt"]);
        let apart = || git.staged_apart(&repo.root, &main, &run, &a);
        assert!(apart().await.unwrap().is_empty(), "nothing staged");
        repo.write("a.txt", "a, by the run\n");
        repo.git(&["add", "a.txt"]);
        assert!(
            apart().await.unwrap().is_empty(),
            "the run's version staged"
        );
        repo.write("a.txt", "a draft of yours\n");
        repo.git(&["add", "a.txt"]);
        repo.write("a.txt", "a, by the run\n");
        assert_eq!(apart().await.unwrap().into_iter().collect::<Vec<_>>(), a);
    }

    #[tokio::test]
    async fn staged_as_they_are_a_fast_forward_takes_them() {
        let repo = TestRepo::new();
        let git = git();
        let (_, run) = a_run(&repo).await;
        repo.write("a.txt", "a, by the run\n");
        std::fs::remove_file(repo.root.join("gone.txt")).unwrap();
        assert!(git.fast_forward(&repo.root, &run).await.is_err());

        git.stage_as_they_are(&repo.root, &paths(&["a.txt", "gone.txt"]))
            .await
            .unwrap();
        assert!(git.fast_forward(&repo.root, &run).await.unwrap());
        assert_eq!(repo.git(&["status", "--porcelain"]), "");
    }
}
