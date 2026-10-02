//! Fresh repositories whose history stops at a commit.

use std::fs;
use std::path::{Path, PathBuf};

use argonauts_core::create_private_dir_all;

use super::files::{nonce, remove_if_present};
use crate::error::{LearningError, Result};
use crate::git;

/// Where the temporary ref lives in the source while its bundle is written.
const TEMP_REF_PREFIX: &str = "refs/argonauts/replay";

/// The branch the fresh repository checks out.
const BRANCH: &str = "main";

const NO_REFLOG: &str = "core.logAllRefUpdates=false";

/// A fresh repository at `into` whose history stops at `base`, so no agent can read the commit
/// that solved the task.
///
/// A clone would carry the source's other refs, tags, and remote, and a local clone shares its
/// objects, any of which can reach a later commit. So a temporary ref at `base` is bundled
/// alone, the bundle is fetched into a new repository, and the ref, the bundle, `FETCH_HEAD`,
/// and the reflog are removed. What's left reaches `base` and its ancestors, nothing else.
pub fn isolated_repo(source: &Path, base: &str, into: &Path) -> Result<PathBuf> {
    if base.starts_with('-') {
        return Err(LearningError::Refused(format!("{base:?} isn't a commit")));
    }
    let commit = git::run(
        source,
        &[
            "rev-parse",
            "--verify",
            "--quiet",
            &format!("{base}^{{commit}}"),
        ],
    )
    .map_err(|_| {
        LearningError::Refused(format!("{base} isn't a commit in {}", source.display()))
    })?;
    if into.exists() && fs::read_dir(into)?.next().is_some() {
        return Err(LearningError::Refused(format!(
            "{} already has files in it",
            into.display()
        )));
    }
    let parent = into.parent().ok_or_else(|| {
        LearningError::Refused(format!("{} has no parent folder", into.display()))
    })?;
    create_private_dir_all(parent)?;

    let id = nonce();
    let temp_ref = format!("{TEMP_REF_PREFIX}/{id}");
    let bundle = parent.join(format!(".{id}.bundle"));
    git::run(source, &["update-ref", &temp_ref, &commit])?;
    let built = build(source, parent, &temp_ref, &bundle, into);
    let removed = git::run(source, &["update-ref", "-d", &temp_ref]);
    let _ = fs::remove_file(&bundle);
    if built.is_err() || removed.is_err() {
        let _ = fs::remove_dir_all(into);
    }
    built?;
    removed?;
    Ok(into.to_path_buf())
}

fn build(source: &Path, parent: &Path, temp_ref: &str, bundle: &Path, into: &Path) -> Result<()> {
    let bundle_text = bundle.to_string_lossy();
    let into_text = into.to_string_lossy();
    git::run(source, &["bundle", "create", &bundle_text, temp_ref])?;
    git::run(parent, &["init", "--quiet", "-b", BRANCH, &into_text])?;
    // No reflog is started, and FETCH_HEAD goes, because both say where the commit came from.
    git::run(
        into,
        &[
            "-c",
            NO_REFLOG,
            "fetch",
            "--quiet",
            "--no-tags",
            "--update-head-ok",
            &bundle_text,
            &format!("{temp_ref}:refs/heads/{BRANCH}"),
        ],
    )?;
    git::run(
        into,
        &["-c", NO_REFLOG, "checkout", "--quiet", "--force", BRANCH],
    )?;
    remove_if_present(&into.join(".git").join("FETCH_HEAD"))
}
