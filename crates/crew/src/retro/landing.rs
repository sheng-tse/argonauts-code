//! A merged run's notes land after the run did, since the next run reads its notes from its
//! base. The notes commit goes on top of what landed, made again there when the run landed as
//! commits of its own, and moves the target only as a fast-forward from there. It holds nothing but notes and decisions, and the target must still
//! point where the run left it, so nothing lands that the merge checkpoint didn't cover apart
//! from scanned notes.

use std::path::Path;

use crate::git::{Git, GitError};
use crate::state::integration::Landed;

/// Where the retro may commit on its own after the run landed.
pub const MEMORY_DIRS: [&str; 2] = [".argonauts/notes/", ".argonauts/decisions/"];

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NotesLanding {
    /// The target now points at this notes commit.
    Landed(String),
    /// The notes stay on the integration branch, for this reason.
    Kept(String),
    /// No notes commit to land, or it landed before a crash.
    Nothing,
}

/// Brings the integration worktree up to what landed, so the notes commit sits on top of it.
/// `Err` says why the notes can't land.
pub async fn catch_up(
    git: &Git,
    worktree: &Path,
    landed: &Landed,
) -> Result<Result<(), String>, GitError> {
    let head = git.resolve(worktree, "HEAD").await?;
    if git.is_ancestor(worktree, &landed.sha, &head).await? {
        return Ok(Ok(()));
    }
    // Uncommitted files in the way fail the fast-forward. That keeps the notes back, but
    // shouldn't fail the retro.
    match git.fast_forward(worktree, &landed.sha).await {
        Ok(true) => Ok(Ok(())),
        Ok(false) => {
            // What landed can be the run's files in commits of their own
            // (`delivery.commits`), which the integration branch never holds. Its files are
            // all here when nothing but notes differs.
            let differs = git.changed_paths(worktree, &landed.sha, &head).await?;
            if differs.iter().all(|path| is_memory(path)) {
                return Ok(Ok(()));
            }
            Ok(Err(format!(
                "the integration branch moved away from what merged into {}",
                landed.target
            )))
        }
        Err(error) => Ok(Err(format!(
            "the integration worktree couldn't catch up with {}: {error}",
            landed.target
        ))),
    }
}

/// Moves `landed.target` in `root` to the integration head when only memory files changed
/// since the landing and the target hasn't moved.
pub async fn land(
    git: &Git,
    root: &Path,
    worktree: &Path,
    landed: &Landed,
) -> Result<NotesLanding, GitError> {
    let target = &landed.target;
    let head = git.resolve(worktree, "HEAD").await?;
    if head == landed.sha {
        return Ok(NotesLanding::Nothing);
    }
    let now = git.resolve(root, target).await?;
    if now == head {
        return Ok(NotesLanding::Nothing);
    }
    if now != landed.sha {
        // The notes landed before a crash, as a commit made for the target.
        if git.tree_of(root, &now).await? == git.tree_of(worktree, &head).await? {
            return Ok(NotesLanding::Nothing);
        }
        return Ok(NotesLanding::Kept(format!(
            "{target} moved after the run merged"
        )));
    }
    let changed = git.changed_paths(worktree, &landed.sha, &head).await?;
    if let Some(path) = changed.iter().find(|path| !is_memory(path)) {
        return Ok(NotesLanding::Kept(format!(
            "the integration branch changed {path} after the run merged"
        )));
    }
    let head = if git.is_ancestor(worktree, &landed.sha, &head).await? {
        head
    } else if changed.is_empty() {
        return Ok(NotesLanding::Nothing);
    } else {
        // What landed isn't in the integration branch's history, so the notes are committed
        // again on top of it, with the message they had.
        let tree = git.tree_of(worktree, &head).await?;
        let message = git
            .log_messages(worktree, &format!("{head}~1..{head}"))
            .await?
            .pop()
            .map_or_else(|| "Notes from the run review".to_owned(), |(_, text)| text);
        git.commit_tree(worktree, &tree, &landed.sha, &message)
            .await?
    };
    let moved = if git.current_branch(root).await?.as_deref() == Some(target.as_str()) {
        if !git.tracked_changes(root).await?.is_empty() {
            return Ok(NotesLanding::Kept(format!(
                "your checkout of {target} has uncommitted changes"
            )));
        }
        git.fast_forward(root, &head).await
    } else {
        // Git refuses when someone moved the target since the check above.
        git.update_branch(root, target, &head, &landed.sha)
            .await
            .map(|()| true)
    };
    // Your files in the way, or a target moved just now, keep the notes back without failing
    // the retro.
    Ok(match moved {
        Ok(true) => NotesLanding::Landed(head),
        Ok(false) => NotesLanding::Kept(format!("{target} moved after the run merged")),
        Err(error) => NotesLanding::Kept(format!("{target} couldn't move: {error}")),
    })
}

fn is_memory(path: &str) -> bool {
    MEMORY_DIRS.iter().any(|dir| path.starts_with(dir))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::{TestRepo, git_test_env};

    fn git() -> Git {
        Git::with_env(git_test_env())
    }

    /// `main` checked out in the repository, and the integration branch in its own worktree
    /// with one commit that landed on `main` by fast-forward.
    async fn landed_run(repo: &TestRepo, git: &Git) -> (std::path::PathBuf, Landed) {
        let worktree = repo.dir.path().join("integration");
        let base = repo.git(&["rev-parse", "HEAD"]);
        git.add_worktree(&repo.root, &worktree, "integration", &base)
            .await
            .unwrap();
        std::fs::write(worktree.join("src.rs"), "fn main() {}\n").unwrap();
        commit(git, &worktree, &["src.rs"], "The run's work").await;
        let head = git.resolve(&worktree, "HEAD").await.unwrap();
        assert!(git.fast_forward(&repo.root, &head).await.unwrap());
        let landed = Landed {
            target: "main".to_owned(),
            sha: head,
            fast_forward: true,
        };
        (worktree, landed)
    }

    async fn commit(git: &Git, worktree: &Path, paths: &[&str], message: &str) -> String {
        git.commit_paths(worktree, paths, message)
            .await
            .unwrap()
            .unwrap()
    }

    async fn write_note(git: &Git, worktree: &Path) -> String {
        let note = ".argonauts/notes/redis.md";
        std::fs::create_dir_all(worktree.join(".argonauts/notes")).unwrap();
        std::fs::write(worktree.join(note), "# Redis\n\n- Counters expire.\n").unwrap();
        commit(git, worktree, &[note], "Retro: notes and decisions").await
    }

    #[tokio::test]
    async fn the_notes_commit_lands_on_top_of_the_run() {
        let repo = TestRepo::new();
        let git = git();
        let (worktree, landed) = landed_run(&repo, &git).await;
        assert_eq!(
            land(&git, &repo.root, &worktree, &landed).await.unwrap(),
            NotesLanding::Nothing
        );

        assert_eq!(catch_up(&git, &worktree, &landed).await.unwrap(), Ok(()));
        let notes = write_note(&git, &worktree).await;
        assert_eq!(
            land(&git, &repo.root, &worktree, &landed).await.unwrap(),
            NotesLanding::Landed(notes.clone())
        );
        assert_eq!(repo.git(&["rev-parse", "main"]), notes);
        assert!(repo.root.join(".argonauts/notes/redis.md").exists());

        // Re-entered after a crash: the notes already landed.
        assert_eq!(catch_up(&git, &worktree, &landed).await.unwrap(), Ok(()));
        assert_eq!(
            land(&git, &repo.root, &worktree, &landed).await.unwrap(),
            NotesLanding::Nothing
        );
    }

    #[tokio::test]
    async fn a_merge_landing_is_caught_up_before_the_notes() {
        let repo = TestRepo::new();
        let git = git();
        let (worktree, mut landed) = landed_run(&repo, &git).await;
        // Main got a merge commit on top of the run, as when fast-forward wasn't possible.
        repo.git(&["commit", "--quiet", "--allow-empty", "-m", "merge"]);
        landed.sha = repo.git(&["rev-parse", "HEAD"]);
        landed.fast_forward = false;

        assert_eq!(catch_up(&git, &worktree, &landed).await.unwrap(), Ok(()));
        assert_eq!(git.resolve(&worktree, "HEAD").await.unwrap(), landed.sha);
        let notes = write_note(&git, &worktree).await;
        assert_eq!(
            land(&git, &repo.root, &worktree, &landed).await.unwrap(),
            NotesLanding::Landed(notes.clone())
        );
        assert_eq!(repo.git(&["rev-parse", "main"]), notes);
    }

    /// By default a run lands as one commit made for the target, which the integration
    /// branch's history never holds. The notes are committed again on top of it.
    #[tokio::test]
    async fn notes_land_on_top_of_a_run_that_landed_as_one_commit() {
        let repo = TestRepo::new();
        let git = git();
        let worktree = repo.dir.path().join("integration");
        let base = repo.git(&["rev-parse", "HEAD"]);
        git.add_worktree(&repo.root, &worktree, "integration", &base)
            .await
            .unwrap();
        for (file, subject) in [("a.rs", "First task"), ("b.rs", "Second task")] {
            std::fs::write(worktree.join(file), "fn f() {}\n").unwrap();
            commit(&git, &worktree, &[file], subject).await;
        }
        let head = git.resolve(&worktree, "HEAD").await.unwrap();
        let tree = git.tree_of(&worktree, &head).await.unwrap();
        let one = git
            .commit_tree(&repo.root, &tree, &base, "The run")
            .await
            .unwrap();
        assert!(git.fast_forward(&repo.root, &one).await.unwrap());
        let landed = Landed {
            target: "main".to_owned(),
            sha: one.clone(),
            fast_forward: true,
        };

        assert_eq!(catch_up(&git, &worktree, &landed).await.unwrap(), Ok(()));
        assert_eq!(git.resolve(&worktree, "HEAD").await.unwrap(), head);
        // Nothing to land until there are notes.
        assert_eq!(
            land(&git, &repo.root, &worktree, &landed).await.unwrap(),
            NotesLanding::Nothing
        );
        write_note(&git, &worktree).await;
        let NotesLanding::Landed(notes) = land(&git, &repo.root, &worktree, &landed).await.unwrap()
        else {
            panic!("the notes didn't land");
        };
        assert_eq!(repo.git(&["rev-parse", "main"]), notes);
        assert_eq!(git.parents(&repo.root, &notes).await.unwrap(), [one]);
        assert!(repo.root.join(".argonauts/notes/redis.md").exists());
        let log = repo.git(&["log", "--format=%s", "main"]);
        assert!(!log.contains("First task"), "{log}");

        // Re-entered after a crash: the notes already landed.
        assert_eq!(catch_up(&git, &worktree, &landed).await.unwrap(), Ok(()));
        assert_eq!(
            land(&git, &repo.root, &worktree, &landed).await.unwrap(),
            NotesLanding::Nothing
        );
    }

    #[tokio::test]
    async fn the_notes_stay_when_the_target_moved_or_code_changed() {
        let repo = TestRepo::new();
        let git = git();
        let (worktree, landed) = landed_run(&repo, &git).await;
        write_note(&git, &worktree).await;

        repo.write("yours.txt", "yours\n");
        repo.git(&["add", "yours.txt"]);
        repo.git(&["commit", "--quiet", "-m", "yours"]);
        let yours = repo.git(&["rev-parse", "main"]);
        let kept = land(&git, &repo.root, &worktree, &landed).await.unwrap();
        assert_eq!(
            kept,
            NotesLanding::Kept("main moved after the run merged".to_owned())
        );
        assert_eq!(repo.git(&["rev-parse", "main"]), yours);

        let repo = TestRepo::new();
        let (worktree, landed) = landed_run(&repo, &git).await;
        std::fs::write(worktree.join("src.rs"), "fn main() { todo!() }\n").unwrap();
        commit(&git, &worktree, &["src.rs"], "After the approval").await;
        write_note(&git, &worktree).await;
        let kept = land(&git, &repo.root, &worktree, &landed).await.unwrap();
        assert_eq!(
            kept,
            NotesLanding::Kept(
                "the integration branch changed src.rs after the run merged".to_owned()
            )
        );
        assert_eq!(repo.git(&["rev-parse", "main"]), landed.sha);
    }

    #[tokio::test]
    async fn a_target_not_checked_out_moves_by_its_ref() {
        let repo = TestRepo::new();
        let git = git();
        let (worktree, mut landed) = landed_run(&repo, &git).await;
        repo.git(&["branch", "release", &landed.sha]);
        repo.git(&["reset", "--quiet", "--hard", "HEAD~1"]);
        landed.target = "release".to_owned();
        let notes = write_note(&git, &worktree).await;
        assert_eq!(
            land(&git, &repo.root, &worktree, &landed).await.unwrap(),
            NotesLanding::Landed(notes.clone())
        );
        assert_eq!(repo.git(&["rev-parse", "release"]), notes);
    }
}
