//! Landing after your merge answer. Right before landing the integration head is re-read; if it
//! moved since the approval, landing is refused (`HardLimit::HeadMoved`) and the checkpoint
//! opens again. Main unchanged: fast-forward, once nothing of yours is in the way, which waits
//! on you and never fails the run. Whenever it waited, everything is read again before landing,
//! since your checkout may have changed meanwhile. Main moved, even while landing: merge it in
//! and rerun the suite; the approval carries over only if the run's own diff is unchanged and
//! the suite passes. A refused landing gives back the auto-merge your rules took.

use std::path::Path;
use std::process::Stdio;

use argonauts_core::events::RunEventKind;
use argonauts_core::events::card::IntegrationEvent;
use argonauts_core::events::checkpoint::{
    AnswerAction, CheckpointDecided, CheckpointEvent, PolicyCheck,
};
use argonauts_core::events::run::{Party, RunOutcome};
use argonauts_core::policy::{EvidenceSubject, HardLimit, Violation};
use argonauts_core::{Note, NoteLevel};

use crate::checkpoints;
use crate::engine::host::Host;
use crate::error::{EngineError, EngineResult};
use crate::git::GitError;
use crate::integrate::in_the_way::{self, Blocked, Way};
use crate::integrate::{final_check, merge_packet};

/// How many times main may move under a landing before you're asked again.
const LANDING_TRIES: usize = 3;

/// What the decision leads to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Landing {
    /// Landed, a pull request, the branch kept, or abandoned.
    Done(RunOutcome),
    /// The checkpoint opens again, for this reason.
    Again(String),
    /// Your files were in the way, and you chose to merge later.
    Later,
}

/// Acts on your answer to the merge checkpoint that showed `approved`.
pub async fn act(
    host: &Host,
    decided: &CheckpointDecided,
    approved: &EvidenceSubject,
) -> EngineResult<Landing> {
    match &decided.action {
        AnswerAction::Merge | AnswerAction::Approve => {
            let landing = land(host, decided, approved).await?;
            if matches!(landing, Landing::Again(_)) {
                // Nothing merged, so an auto-merge your rules took goes back.
                checkpoints::landing_refused(host, &decided.id);
            }
            Ok(landing)
        }
        AnswerAction::PullRequest => pull_request(host, &decided.by).await,
        AnswerAction::KeepBranch => Ok(Landing::Done(RunOutcome::BranchLeft)),
        AnswerAction::Abandon => abandon(host),
        other => Err(EngineError::Refused(format!(
            "the merge checkpoint can't land {other:?}"
        ))),
    }
}

/// The `gh pr create` command to run, recorded; run only when `gh` is installed and you chose
/// it.
pub fn pr_command(branch: &str, title: &str) -> Vec<String> {
    [
        "gh", "pr", "create", "--head", branch, "--title", title, "--fill",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect()
}

async fn land(
    host: &Host,
    decided: &CheckpointDecided,
    approved: &EvidenceSubject,
) -> EngineResult<Landing> {
    let git = host.git();
    let root = host.repo().root.clone();
    let (branch, worktree) = merge_packet::landing_branch(host).await?;
    let mut head = git.resolve(&worktree, "HEAD").await?;
    if head != approved.head_commit {
        let detail = format!(
            "{branch} moved from {} to {head} after the approval, so the approval doesn't cover \
             what would land",
            approved.head_commit
        );
        refuse(host, decided, &detail)?;
        return Ok(Landing::Again(detail));
    }
    let Some(target) = final_check::target(host) else {
        return Ok(Landing::Again(
            "the run has no branch to land on. Set merge.target, or keep the branch".to_owned(),
        ));
    };
    // Main can move again while the suite runs, or between reading it and landing. Each time it
    // does, the carry-over runs again; it never lands a combination the suite didn't run on.
    // Waiting on you doesn't count: once you answer, everything is read again.
    let mut moved = 0;
    loop {
        let main = git.resolve(&root, &target).await?;
        if !git.is_ancestor(&root, &main, &head).await? {
            if moved == LANDING_TRIES {
                break;
            }
            moved += 1;
            match carry_over(host, &worktree, &target, approved).await? {
                Ok(new_head) => head = new_head,
                Err(why) => return Ok(Landing::Again(why)),
            }
            continue;
        }
        if git.current_branch(&root).await?.as_deref() != Some(target.as_str()) {
            // Not checked out, so only the branch moves, and only if nobody moved it meanwhile.
            let refused = match git.update_branch(&root, &target, &head, &main).await {
                Ok(()) => {
                    landed(host, &target, &head, true)?;
                    return Ok(Landing::Done(RunOutcome::Merged));
                }
                Err(_) if git.resolve(&root, &target).await? != main && moved < LANDING_TRIES => {
                    moved += 1;
                    continue;
                }
                Err(err) => git_words(&err),
            };
            let blocked = Blocked {
                refused: Some(refused),
                ..Blocked::default()
            };
            if in_the_way::ask(host, &decided.id, &target, &blocked).await? == Way::Later {
                return Ok(Landing::Later);
            }
            continue;
        }
        let blocked = in_the_way::find(host, &main, &head).await?;
        if !blocked.is_empty() {
            if in_the_way::ask(host, &decided.id, &target, &blocked).await? == Way::Later {
                return Ok(Landing::Later);
            }
            continue;
        }
        in_the_way::withdraw_open(host)?;
        match git.fast_forward(&root, &head).await {
            Ok(true) => {
                landed(host, &target, &head, true)?;
                return Ok(Landing::Done(RunOutcome::Merged));
            }
            // False only when main is no longer an ancestor, so it moved since it was read.
            Ok(false) => {}
            // Something turned up after the look, or git has a reason of its own: you're asked,
            // in its words if nothing of yours is in the way.
            Err(err) => {
                let mut blocked = in_the_way::find(host, &main, &head).await?;
                if blocked.is_empty() {
                    blocked.refused = Some(git_words(&err));
                }
                if in_the_way::ask(host, &decided.id, &target, &blocked).await? == Way::Later {
                    return Ok(Landing::Later);
                }
            }
        }
    }
    Ok(Landing::Again(format!(
        "{target} kept moving while the run landed. Answer again once it settles"
    )))
}

/// What git said, without the command around it.
fn git_words(err: &GitError) -> String {
    match err {
        GitError::Failed { stderr, .. } if !stderr.trim().is_empty() => stderr.trim().to_owned(),
        other => other.to_string(),
    }
}

/// Main moved while the merge waited: bring it in and rerun the suite. Returns the new
/// integration head, or why you must decide again.
async fn carry_over(
    host: &Host,
    worktree: &Path,
    target: &str,
    approved: &EvidenceSubject,
) -> EngineResult<Result<String, String>> {
    let git = host.git();
    let before = git
        .diff(worktree, &approved.base_commit, &approved.head_commit)
        .await?;
    let check = final_check::run(host).await?;
    if !check.passed {
        let why = check
            .problem
            .unwrap_or_else(|| "the suite failed with the new main".to_owned());
        return Ok(Err(format!(
            "{target} moved after your approval, and {why}"
        )));
    }
    let head = git.resolve(worktree, "HEAD").await?;
    let main = git.resolve(&host.repo().root, target).await?;
    let base = git
        .merge_base(worktree, &main, &head)
        .await?
        .unwrap_or_else(|| main.clone());
    let after = git.diff(worktree, &base, &head).await?;
    if before != after {
        return Ok(Err(format!(
            "{target} moved after your approval and the run's own diff changed with it"
        )));
    }
    Ok(Ok(head))
}

fn refuse(host: &Host, decided: &CheckpointDecided, detail: &str) -> EngineResult<()> {
    let check = PolicyCheck {
        id: decided.id.clone(),
        violations: vec![Violation {
            limit: HardLimit::HeadMoved,
            detail: detail.to_owned(),
            paths: Vec::new(),
        }],
        verdict: None,
        by: None,
        approver: None,
        reasons: vec![detail.to_owned()],
        shadow: false,
    };
    host.record(
        None,
        None,
        RunEventKind::Checkpoint(CheckpointEvent::Checked(check)),
    )?;
    Ok(())
}

fn landed(host: &Host, target: &str, sha: &str, fast_forward: bool) -> EngineResult<()> {
    host.record(
        None,
        None,
        RunEventKind::Integration(IntegrationEvent::Landed {
            target: target.to_owned(),
            sha: sha.to_owned(),
            fast_forward,
        }),
    )?;
    Ok(())
}

async fn pull_request(host: &Host, by: &Party) -> EngineResult<Landing> {
    open_for(host, matches!(by, Party::User)).await
}

/// `merge.mode = "pull_request"`, as a recipe's canary runs use: the run lands as a pull request
/// instead of on the target, opened with `gh` where a remote exists, since your settings chose
/// it.
pub async fn pull_request_by_rule(host: &Host) -> EngineResult<Landing> {
    open_for(host, true).await
}

async fn open_for(host: &Host, chosen: bool) -> EngineResult<Landing> {
    let (branch, _) = merge_packet::landing_branch(host).await?;
    let title = host
        .read(|state| {
            state
                .run
                .started
                .as_ref()
                .map(|started| started.title.clone())
        })
        .unwrap_or_else(|| host.run_id().to_string());
    let mut command = pr_command(&branch, &title);
    if host.settings().merge.pr_draft {
        command.push("--draft".to_owned());
    }
    let root = host.repo().root.clone();
    // Only when you chose it, and only where a pull request is possible at all.
    let run_it =
        chosen && which::which("gh").is_ok() && !host.git().remotes(&root).await?.is_empty();
    let url = if run_it {
        open_pull_request(host, &root, &command).await?
    } else {
        None
    };
    host.record(
        None,
        None,
        RunEventKind::Integration(IntegrationEvent::PullRequest {
            branch,
            url,
            command: Some(command),
        }),
    )?;
    Ok(Landing::Done(RunOutcome::PullRequest))
}

/// Runs `gh`. Its failure is shown, and the command stays recorded to run by hand.
async fn open_pull_request(
    host: &Host,
    root: &Path,
    command: &[String],
) -> EngineResult<Option<String>> {
    let Some((program, args)) = command.split_first() else {
        return Ok(None);
    };
    let out = tokio::process::Command::new(program)
        .args(args)
        .current_dir(root)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .output()
        .await?;
    if out.status.success() {
        let stdout = String::from_utf8_lossy(&out.stdout);
        return Ok(stdout.lines().last().map(|line| line.trim().to_owned()));
    }
    let stderr = String::from_utf8_lossy(&out.stderr);
    let text = format!(
        "gh pr create failed: {}",
        stderr.trim().lines().last().unwrap_or("no output")
    );
    host.record(
        None,
        None,
        RunEventKind::Note(Note {
            level: NoteLevel::Warn,
            text,
        }),
    )?;
    Ok(None)
}

fn abandon(host: &Host) -> EngineResult<Landing> {
    let branches = host.read(|state| {
        state
            .integration
            .branch
            .iter()
            .cloned()
            .chain(
                state
                    .cards
                    .by_id
                    .values()
                    .filter_map(|record| record.branch.clone()),
            )
            .collect()
    });
    host.record(
        None,
        None,
        RunEventKind::Integration(IntegrationEvent::Abandoned { branches }),
    )?;
    Ok(Landing::Done(RunOutcome::Abandoned))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_pr_command_is_an_argument_vector() {
        let command = pr_command("argonauts/run-01/integration", "Add a Redis store; really");
        assert_eq!(
            command,
            [
                "gh",
                "pr",
                "create",
                "--head",
                "argonauts/run-01/integration",
                "--title",
                "Add a Redis store; really",
                "--fill"
            ]
        );
    }
}
