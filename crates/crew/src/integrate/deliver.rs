//! What a merged run leaves on your branch. The run's own history, with every agent's commits
//! and the merges between them, stays on its integration branch. By default what lands is one
//! commit on top of your branch's tip that holds the same files, so your history gains one
//! entry you can read and revert. `delivery.commits` picks one commit for each task instead,
//! or the history as it happened.

use std::path::Path;

use argonauts_core::RunId;
use argonauts_core::settings::delivery::DeliveryCommits;

use crate::engine::host::Host;
use crate::git::{Git, GitError};
use crate::integrate::commit_message;
use crate::labels::card_label;

/// On the commits Argonauts makes for your branch, so a later reader can find the run.
pub const RUN_TRAILER: &str = "Argonauts-Run";

/// The commit to land for the approved `head`, whose history holds `main`. It always has
/// `head`'s files, and lands as a fast-forward from `main`.
pub async fn commit(host: &Host, dir: &Path, main: &str, head: &str) -> Result<String, GitError> {
    let git = host.git();
    let commits = host.settings().delivery.commits;
    if main == head || commits == DeliveryCommits::AsRun {
        return Ok(head.to_owned());
    }
    // Your branch holds the run's files already, as after a landing that was cut short and
    // taken up again, so there's nothing to add to it.
    if git.tree_of(dir, main).await? == git.tree_of(dir, head).await? {
        return Ok(main.to_owned());
    }
    match commits {
        DeliveryCommits::AsRun => Ok(head.to_owned()),
        DeliveryCommits::Squash => squash(host, dir, main, head).await,
        DeliveryCommits::PerTask => match per_step(git, dir, main, head).await? {
            Some(last) => Ok(last),
            // The run took your branch's newer commits in along the way, so its steps no
            // longer stand one on another from where your branch is now.
            None => squash(host, dir, main, head).await,
        },
    }
}

/// The first commit of a branch that has none, for the approved `head`, which grew from
/// `main`, the snapshot of your files the run started from: one commit with no parent that
/// holds `head`'s files, your own as the run found them among them. It's one commit whatever
/// `delivery.commits` says, since the run's history starts at a snapshot of your files that
/// was never a commit of yours.
pub async fn first_commit(
    host: &Host,
    dir: &Path,
    main: &str,
    head: &str,
) -> Result<String, GitError> {
    let git = host.git();
    let tree = git.tree_of(dir, head).await?;
    let message = landing_message(host, dir, main, head).await?;
    git.root_commit(dir, &tree, &message).await
}

/// Whether `message` is of a commit made to land `run`.
pub fn names_run(message: &str, run: &RunId) -> bool {
    let trailer = format!("{RUN_TRAILER}: {run}");
    message.lines().any(|line| line.trim() == trailer)
}

async fn squash(host: &Host, dir: &Path, main: &str, head: &str) -> Result<String, GitError> {
    let git = host.git();
    let tree = git.tree_of(dir, head).await?;
    let message = landing_message(host, dir, main, head).await?;
    git.commit_tree(dir, &tree, main, &message).await
}

/// The merge agent's message for the change from `main` to `head`, else the one plain code
/// writes, each ending with the run it came from.
async fn landing_message(
    host: &Host,
    dir: &Path,
    main: &str,
    head: &str,
) -> Result<String, GitError> {
    Ok(
        match commit_message::for_change(host, dir, main, head).await? {
            Some(written) => with_trailer(written, host.run_id()),
            None => message(host),
        },
    )
}

fn with_trailer(mut text: String, run: &RunId) -> String {
    text.push_str(&format!("\n\n{RUN_TRAILER}: {run}"));
    text
}

/// The run's title, then each task, then the run.
fn message(host: &Host) -> String {
    let run = host.run_id();
    let (title, tasks) = host.read(|state| {
        let title = state
            .run
            .started
            .as_ref()
            .map(|started| started.title.clone())
            .filter(|title| !title.trim().is_empty());
        let tasks: Vec<(String, String)> = state
            .cards
            .by_id
            .iter()
            .map(|(id, record)| (card_label(state, id), record.spec.title.clone()))
            .collect();
        (title, tasks)
    });
    message_of(run, title.as_deref(), &tasks)
}

fn message_of(run: &RunId, title: Option<&str>, tasks: &[(String, String)]) -> String {
    let mut text = title.map_or_else(|| format!("Work from {run}"), str::to_owned);
    // One task's title is the run's, more or less, so the list would only repeat it.
    if tasks.len() > 1 {
        text.push('\n');
        for (label, title) in tasks {
            text.push_str(&format!("\n- {label}: {title}"));
        }
    }
    with_trailer(text, run)
}

/// One commit for each step the run's branch took from `main` to `head`: a task's merge, or
/// a commit of the conductor's, each with the message it had. `None` when the branch didn't
/// grow from `main` step by step.
async fn per_step(
    git: &Git,
    dir: &Path,
    main: &str,
    head: &str,
) -> Result<Option<String>, GitError> {
    let mut steps = git
        .first_parent_log(dir, &format!("{main}..{head}"))
        .await?;
    steps.reverse();
    let Some((first, _)) = steps.first() else {
        return Ok(None);
    };
    if git.parents(dir, first).await?.first().map(String::as_str) != Some(main) {
        return Ok(None);
    }
    let mut parent = main.to_owned();
    let mut parent_tree = git.tree_of(dir, main).await?;
    for (step, message) in &steps {
        let tree = git.tree_of(dir, step).await?;
        if tree == parent_tree {
            continue;
        }
        parent = git.commit_tree(dir, &tree, &parent, message).await?;
        parent_tree = tree;
    }
    Ok((parent != main).then_some(parent))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::{TestRepo, git_test_env};

    fn run() -> RunId {
        RunId::new("run-07").unwrap()
    }

    #[test]
    fn the_one_commit_names_the_run_and_lists_several_tasks() {
        let one = [("build:add-due".to_owned(), "Add due dates".to_owned())];
        assert_eq!(
            message_of(&run(), Some("Add due dates"), &one),
            "Add due dates\n\nArgonauts-Run: run-07"
        );
        let two = [
            ("build:add-due".to_owned(), "Add due dates".to_owned()),
            ("build:sort".to_owned(), "Sort by due date".to_owned()),
        ];
        assert_eq!(
            message_of(&run(), Some("Due dates"), &two),
            "Due dates\n\n- build:add-due: Add due dates\n- build:sort: Sort by due date\n\n\
             Argonauts-Run: run-07"
        );
        assert!(message_of(&run(), None, &[]).starts_with("Work from run-07\n"));
    }

    #[test]
    fn a_landing_commit_is_known_by_the_run_it_names() {
        let message = message_of(&run(), Some("Due dates"), &[]);
        assert!(names_run(&message, &run()));
        assert!(!names_run(&message, &RunId::new("run-08").unwrap()));
        assert!(!names_run(
            "Mentions Argonauts-Run: run-07 in passing",
            &run()
        ));
    }

    /// Main, then a branch with two steps, each a merge of a task's own commits.
    async fn two_steps(repo: &TestRepo, git: &Git) -> (String, String) {
        let main = git.head_commit(&repo.root).await.unwrap().unwrap();
        repo.git(&["checkout", "-q", "-b", "integration"]);
        for (task, file) in [("task-001", "one.txt"), ("task-002", "two.txt")] {
            repo.git(&["checkout", "-q", "-b", task]);
            repo.write(file, "draft\n");
            git.commit_all(&repo.root, "a draft").await.unwrap();
            repo.write(file, "done\n");
            git.commit_all(&repo.root, "the work").await.unwrap();
            repo.git(&["checkout", "-q", "integration"]);
            git.merge_no_ff(&repo.root, task, &format!("Merge {task}"))
                .await
                .unwrap();
        }
        let head = git.head_commit(&repo.root).await.unwrap().unwrap();
        repo.git(&["checkout", "-q", "main"]);
        (main, head)
    }

    #[tokio::test]
    async fn each_step_of_the_run_becomes_one_commit_with_its_message() {
        let repo = TestRepo::new();
        let git = Git::with_env(git_test_env());
        let (main, head) = two_steps(&repo, &git).await;

        let last = per_step(&git, &repo.root, &main, &head)
            .await
            .unwrap()
            .unwrap();
        let log = git
            .log_messages(&repo.root, &format!("{main}..{last}"))
            .await
            .unwrap();
        let subjects: Vec<&str> = log.iter().map(|(_, message)| message.as_str()).collect();
        assert_eq!(subjects, ["Merge task-002", "Merge task-001"]);
        assert_eq!(
            git.tree_of(&repo.root, &last).await.unwrap(),
            git.tree_of(&repo.root, &head).await.unwrap()
        );
        assert!(git.is_ancestor(&repo.root, &main, &last).await.unwrap());
        for (sha, _) in &log {
            assert_eq!(git.parents(&repo.root, sha).await.unwrap().len(), 1);
        }
    }

    #[tokio::test]
    async fn steps_that_took_your_newer_commits_in_fall_back_to_one_commit() {
        let repo = TestRepo::new();
        let git = Git::with_env(git_test_env());
        let (_, head) = two_steps(&repo, &git).await;
        repo.write("yours.txt", "yours\n");
        let moved = git.commit_all(&repo.root, "yours").await.unwrap().unwrap();
        repo.git(&["checkout", "-q", "integration"]);
        git.merge_no_ff(&repo.root, "main", "Merge main")
            .await
            .unwrap();
        let merged = git.head_commit(&repo.root).await.unwrap().unwrap();
        repo.git(&["checkout", "-q", "main"]);
        assert_ne!(merged, head);
        assert_eq!(
            per_step(&git, &repo.root, &moved, &merged).await.unwrap(),
            None
        );
    }
}
