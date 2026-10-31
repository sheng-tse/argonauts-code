//! The run's worktree a carried-on agent's session works in. Once a run merged its worktree is
//! gone, so it's made again at the same path from the task's branch, touching no other worktree of
//! yours. Argonauts Code removes a worktree it made again once the conversation is done with it,
//! unless it holds work not committed yet; the agent's commits stay on the task's branch either
//! way.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use anyhow::{Context as _, bail};
use argonauts_core::NoteLevel;

use super::ConversationState;
use crate::controller::Ctx;
use crate::conversation::store::{ConversationPlace, ConversationStore};
use crate::git;

/// Makes the run's worktree at `folder` again from `branch` when it's gone. True when it did.
pub(super) fn ensure_worktree(
    repo: &Path,
    folder: &Path,
    branch: Option<&str>,
) -> anyhow::Result<bool> {
    if folder.is_dir() {
        return Ok(false);
    }
    let shown = folder.display();
    let Some(branch) = branch else {
        bail!("its folder {shown} is gone, and it ran on no branch to make it again from");
    };
    let path = folder
        .to_str()
        .with_context(|| format!("git can't take the path {shown}"))?;
    let reference = format!("refs/heads/{branch}");
    if !git::git_ok(repo, &["rev-parse", "--verify", "--quiet", &reference])? {
        bail!("its worktree {shown} is gone, and so is its branch {branch}");
    }
    let trees = worktrees(repo)?;
    let elsewhere = trees.iter().find(|tree| {
        tree.branch.as_deref() == Some(reference.as_str()) && !same_path(&tree.path, folder)
    });
    if let Some(other) = elsewhere {
        bail!(
            "its branch {branch} is checked out in {}, and git checks a branch out in one \
             folder at a time",
            other.path.display()
        );
    }
    // A worktree deleted outside git stays registered, and `add` refuses its path. `--force` clears
    // that one entry, and no other worktree's.
    let stale = trees.iter().any(|tree| same_path(&tree.path, folder));
    let mut args = vec!["worktree", "add"];
    if stale {
        args.push("--force");
    }
    args.extend([path, branch]);
    git::git(repo, &args)
        .with_context(|| format!("making its worktree {shown} again from {branch}"))?;
    Ok(true)
}

/// What became of a worktree made again, once its conversation was done with it.
#[derive(Debug, PartialEq, Eq)]
enum Left {
    /// Removed. `ahead` counts the commits on its branch that your checkout doesn't have.
    Removed { ahead: usize },
    /// Kept, since it holds changes not committed yet.
    Kept,
    /// Already gone.
    Gone,
}

/// Removes the worktree at `folder` that Argonauts Code made again from `branch`, unless it holds
/// changes not committed yet. The agent's commits stay on the branch.
fn remove_worktree(repo: &Path, folder: &Path, branch: Option<&str>) -> anyhow::Result<Left> {
    if !folder.is_dir() {
        return Ok(Left::Gone);
    }
    if !git::uncommitted_paths(folder)?.is_empty() {
        return Ok(Left::Kept);
    }
    let path = folder
        .to_str()
        .with_context(|| format!("git can't take the path {}", folder.display()))?;
    git::git(repo, &["worktree", "remove", path])?;
    let ahead = branch
        .and_then(|branch| {
            let range = format!("HEAD..refs/heads/{branch}");
            git::git(repo, &["rev-list", "--count", &range]).ok()
        })
        .and_then(|count| count.trim().parse().ok())
        .unwrap_or(0);
    Ok(Left::Removed { ahead })
}

/// One of the repo's worktrees, as `git worktree list --porcelain` lists it.
struct Worktree {
    path: PathBuf,
    /// Such as `refs/heads/main`; `None` with a detached head.
    branch: Option<String>,
}

fn worktrees(repo: &Path) -> anyhow::Result<Vec<Worktree>> {
    let text = git::git(repo, &["worktree", "list", "--porcelain"])?;
    let mut trees: Vec<Worktree> = Vec::new();
    for line in text.lines() {
        if let Some(path) = line.strip_prefix("worktree ") {
            trees.push(Worktree {
                path: PathBuf::from(path),
                branch: None,
            });
        } else if let (Some(branch), Some(tree)) = (line.strip_prefix("branch "), trees.last_mut())
        {
            tree.branch = Some(branch.to_owned());
        }
    }
    Ok(trees)
}

/// Whether two paths name the same place, though one may not exist and either may go through
/// a symlink, as macOS's `/var` does.
fn same_path(a: &Path, b: &Path) -> bool {
    a == b || resolved(a) == resolved(b)
}

/// `path` with its longest start that exists resolved.
fn resolved(path: &Path) -> PathBuf {
    let mut rest = Vec::new();
    let mut base = path;
    loop {
        if let Ok(real) = base.canonicalize() {
            return rest.iter().rev().fold(real, |path, part| path.join(part));
        }
        match (base.parent(), base.file_name()) {
            (Some(parent), Some(name)) => {
                rest.push(name.to_owned());
                base = parent;
            }
            _ => return path.to_owned(),
        }
    }
}

/// The worktrees made again for earlier conversations, at start: each goes once it holds nothing
/// uncommitted, since `/resume` makes it again. The conversation on screen keeps its own.
pub(crate) fn tidy_worktrees(ctx: &Ctx, conversation: &ConversationState) {
    let mut seen = HashSet::new();
    for (id, place) in ConversationStore::places(&ctx.env.conversation_dir()) {
        let on_screen = conversation
            .place
            .as_ref()
            .is_some_and(|ours| ours.folder == place.folder);
        if !place.made_again || on_screen || !seen.insert(place.folder.clone()) {
            continue;
        }
        let branch = place.branch.as_deref();
        match remove_worktree(&ctx.env.repo_root, &place.folder, branch) {
            Ok(Left::Removed { .. }) => {
                tracing::info!("removed {}, made again for {id}", place.folder.display());
            }
            Ok(_) => {}
            Err(err) => tracing::warn!("removing {}: {err:#}", place.folder.display()),
        }
    }
}

/// The conversation is done with the worktree made again for it: it goes, unless it holds changes
/// not committed yet. The agent's commits stay on its branch.
pub(super) fn leave(ctx: &Ctx, place: &ConversationPlace) {
    let folder = ctx.env.display(&place.folder);
    let agent = &place.agent;
    let branch = place.branch.as_deref().unwrap_or_default();
    let text = match remove_worktree(&ctx.env.repo_root, &place.folder, place.branch.as_deref()) {
        Ok(Left::Removed { ahead: 0 }) => {
            format!("Removed the worktree made again for {agent} in {folder}.")
        }
        Ok(Left::Removed { ahead }) => {
            let (commits, them) = if ahead == 1 {
                ("1 commit".to_owned(), "it")
            } else {
                (format!("{ahead} commits"), "them")
            };
            format!(
                "Removed the worktree made again for {agent} in {folder}. Its branch {branch} \
                 keeps {commits} your checkout doesn't have, and git merge {branch} brings {them} \
                 in."
            )
        }
        Ok(Left::Kept) => format!(
            "The worktree made again for {agent} in {folder} has changes not committed yet, so it \
             stays. Commit them there, and git merge {branch} brings them into your checkout."
        ),
        Ok(Left::Gone) => return,
        Err(err) => {
            ctx.notice(
                NoteLevel::Warn,
                format!("Couldn't remove the worktree in {folder}: {err:#}"),
            );
            return;
        }
    };
    ctx.notice(NoteLevel::Info, text);
}

#[cfg(test)]
mod tests {
    use argonauts_testkit::TempRepo;

    use super::*;

    fn branch_head(repo: &TempRepo, folder: &Path) -> String {
        let path = folder.to_str().unwrap();
        repo.git(&["-C", path, "branch", "--show-current"])
            .unwrap()
            .trim()
            .to_owned()
    }

    /// A worktree deleted by hand clears only its own stale entry, never one of yours, and a branch
    /// checked out elsewhere is never checked out twice.
    #[test]
    fn a_worktree_made_again_clears_only_its_own_stale_entry() {
        let repo = TempRepo::new().unwrap();
        let root = repo.root();
        let base = repo.home().join("worktrees");
        let (worktree, yours) = (base.join("task-001"), base.join("yours"));
        repo.git(&["branch", "task"]).unwrap();
        repo.git(&["branch", "mine"]).unwrap();
        for (folder, branch) in [(&worktree, "task"), (&yours, "mine")] {
            repo.git(&["worktree", "add", folder.to_str().unwrap(), branch])
                .unwrap();
        }
        // Both folders go missing: the worktree as a run's cleanup leaves it, and yours as an
        // unmounted drive would.
        std::fs::remove_dir_all(&worktree).unwrap();
        std::fs::remove_dir_all(&yours).unwrap();

        assert!(ensure_worktree(root, &worktree, Some("task")).unwrap());
        assert_eq!(branch_head(&repo, &worktree), "task");
        let listed = repo.git(&["worktree", "list", "--porcelain"]).unwrap();
        assert!(
            listed.contains("yours"),
            "your missing worktree stays registered: {listed}"
        );
        // Made already, so nothing to do.
        assert!(!ensure_worktree(root, &worktree, Some("task")).unwrap());

        // Your own checkout of the branch keeps it.
        std::fs::remove_dir_all(&worktree).unwrap();
        repo.git(&["worktree", "prune"]).unwrap();
        let elsewhere = base.join("elsewhere");
        repo.git(&["worktree", "add", elsewhere.to_str().unwrap(), "task"])
            .unwrap();
        let err = ensure_worktree(root, &worktree, Some("task")).unwrap_err();
        assert!(format!("{err:#}").contains("is checked out in"), "{err:#}");
        assert!(!worktree.exists());
    }

    #[test]
    fn a_worktree_made_again_goes_once_clean_and_its_commits_stay_on_its_branch() {
        let repo = TempRepo::new().unwrap();
        let root = repo.root();
        let worktree = repo.home().join("worktrees/task-001");
        repo.git(&["branch", "task"]).unwrap();
        assert!(ensure_worktree(root, &worktree, Some("task")).unwrap());
        let path = worktree.to_str().unwrap();
        std::fs::write(worktree.join("added.txt"), "new\n").unwrap();
        assert_eq!(
            remove_worktree(root, &worktree, Some("task")).unwrap(),
            Left::Kept
        );
        assert!(worktree.is_dir(), "uncommitted work keeps the worktree");

        repo.git(&["-C", path, "add", "added.txt"]).unwrap();
        repo.git(&["-C", path, "commit", "-qm", "add a file"])
            .unwrap();
        assert_eq!(
            remove_worktree(root, &worktree, Some("task")).unwrap(),
            Left::Removed { ahead: 1 }
        );
        assert!(!worktree.exists());
        // The branch is free again, so it can be deleted.
        repo.git(&["branch", "-D", "task"]).unwrap();
        assert_eq!(remove_worktree(root, &worktree, None).unwrap(), Left::Gone);
    }
}
