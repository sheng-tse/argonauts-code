//! Landing after your merge answer. Right before landing the merge head is re-read; if it
//! moved since the approval, landing is refused (`HardLimit::HeadMoved`) and the approval
//! opens again. Main unchanged: fast-forward to what `delivery` says lands, by default one
//! commit for the run, once nothing of yours is in the way, which waits on you and never fails
//! the run. Whenever it waited, everything is read again before landing,
//! since your checkout may have changed meanwhile. Main moved, even while landing: merge it in
//! and rerun the suite; the approval carries over only if the run's own diff is unchanged and
//! the suite passes. A refused landing gives back the auto-merge your rules took.
//!
//! Two kinds of run write your files and move no branch: one under `delivery.write = "files"`,
//! and one that started before your branch had a commit, which has nothing to move. Under
//! `merge.unborn = "first_commit"` the second kind gives your branch its first commit instead.

use std::path::Path;
use std::process::Stdio;

use argonauts_core::events::RunEventKind;
use argonauts_core::events::approval::{AnswerAction, ApprovalDecided, ApprovalEvent, PolicyCheck};
use argonauts_core::events::run::RunOutcome;
use argonauts_core::events::task::MergeEvent;
use argonauts_core::policy::{EvidenceSubject, HardLimit, Violation};
use argonauts_core::settings::delivery::DeliveryWrite;
use argonauts_core::{Note, NoteLevel};

use crate::approvals;
use crate::engine::host::Host;
use crate::error::{EngineError, EngineResult};
use crate::git::GitError;
use crate::merge::detached::{self, Named};
use crate::merge::in_the_way::{self, Blocked, Holding, Way};
use crate::merge::unborn::{self, Lands};
use crate::merge::your_folder::{self, Settled};
use crate::merge::{absorb, deliver, final_check, kept, merge_packet};

/// How many times main may move under a landing before you're asked again.
const LANDING_TRIES: usize = 3;

/// What follows from the decision.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Landing {
    /// Merged, a pull request, the branch kept, or abandoned.
    Done(RunOutcome),
    /// The approval opens again, for this reason.
    Again(String),
    /// Your files were in the way, and you chose to merge later.
    Later,
}

/// Acts on your answer to the merge approval that showed `approved`.
pub async fn act(
    host: &Host,
    decided: &ApprovalDecided,
    approved: &EvidenceSubject,
) -> EngineResult<Landing> {
    // A landing cut short may have left files of yours set aside, whatever comes next.
    absorb::put_back_leftovers(host);
    your_folder::clear_stale_lock(host);
    match &decided.action {
        AnswerAction::Merge | AnswerAction::Approve => {
            let landing = land_or_wait(host, decided, approved).await?;
            match landing {
                Landing::Done(RunOutcome::Merged) => approvals::merged(host, &decided.id)?,
                // Nothing merged, so an auto-merge your rules took goes back.
                Landing::Again(_)
                | Landing::Done(RunOutcome::BranchLeft | RunOutcome::PullRequest) => {
                    approvals::landing_refused(host, &decided.id);
                }
                _ => {}
            }
            Ok(landing)
        }
        action if your_folder::is(host) && your_folder::needs_a_branch(action) => {
            Ok(Landing::Again(NO_BRANCH_HERE.to_owned()))
        }
        AnswerAction::PullRequest => open_pull_request_for(host).await,
        AnswerAction::KeepBranch => Ok(Landing::Done(RunOutcome::BranchLeft)),
        AnswerAction::Abandon => abandon(host),
        other => Err(EngineError::Refused(format!(
            "the merge approval can't merge {other:?}"
        ))),
    }
}

/// The `gh pr create` command to run, recorded; run only when `gh` is installed. Without a
/// `body`, `gh` fills it from the branch's commits here.
pub fn pr_command(branch: &str, title: &str, body: Option<&str>) -> Vec<String> {
    let mut command = vec!["gh", "pr", "create", "--head", branch, "--title", title];
    match body {
        Some(body) => command.extend(["--body", body]),
        None => command.push("--fill"),
    }
    command.into_iter().map(str::to_owned).collect()
}

/// Lands, and when git fails on the way, says what it said and waits on you instead of
/// failing the run. The run's work stays on its branch meanwhile, and trying again reads
/// everything afresh.
async fn land_or_wait(
    host: &Host,
    decided: &ApprovalDecided,
    approved: &EvidenceSubject,
) -> EngineResult<Landing> {
    loop {
        let err = match land(host, decided, approved).await {
            Err(EngineError::Git(err @ (GitError::Failed { .. } | GitError::Spawn(_)))) => err,
            landing => return landing,
        };
        let target = match final_check::lands_on(host).await {
            Ok(Some(target)) => target,
            _ => "your branch".to_owned(),
        };
        let way = ask_about_refusal(host, decided, &target, &err).await?;
        if let Some(landing) = left_waiting(host, way).await? {
            return Ok(landing);
        }
    }
}

async fn land(
    host: &Host,
    decided: &ApprovalDecided,
    approved: &EvidenceSubject,
) -> EngineResult<Landing> {
    let git = host.git();
    let root = host.repo().root.clone();
    let (branch, worktree) = merge_packet::landing_branch(host).await?;
    let mut head = git.resolve(&worktree, "HEAD").await?;
    if head != approved.head_commit {
        let detail = format!(
            "{branch} moved from {} to {head} after the approval, so the approval doesn't cover \
             what would merge",
            approved.head_commit
        );
        refuse(host, decided, &detail)?;
        return Ok(Landing::Again(detail));
    }
    // Main can move again while the suite runs, or between reading it and landing. Each time it
    // does, the carry-over runs again; it never lands a combination the suite didn't run on.
    // Waiting on you doesn't count: once you answer, everything is read again.
    let mut moved = 0;
    loop {
        // How the work reaches you is read each time round too, since a first commit of your
        // own changes it.
        let lands = unborn::lands(host).await?;
        let target = match &lands {
            Some(lands) => lands.target().to_owned(),
            None => match final_check::target(host) {
                Some(target) => target,
                // Your checkout was on no branch when the run started: you name one, once.
                None => match detached::ask(host, &decided.id).await? {
                    Named::Branch(target) => target,
                    Named::KeepBranch => return Ok(Landing::Done(RunOutcome::BranchLeft)),
                    Named::Withdrawn => return Ok(Landing::Again(NO_TARGET.to_owned())),
                },
            },
        };
        let main = final_check::main_commit(host, &target).await?;
        if let Some(Lands::FirstCommitMade { commit, .. }) = &lands {
            // A landing cut short right after your branch got its first commit. Your files
            // were written before that, so nothing is looked for in their way.
            match git.index_under_first_commit(&root, &main, commit).await {
                Ok(()) => {
                    in_the_way::withdraw_open(host)?;
                    merged(host, &target, commit, true)?;
                    return Ok(Landing::Done(RunOutcome::Merged));
                }
                Err(err) => {
                    let way = ask_about_refusal(host, decided, &target, &err).await?;
                    if let Some(landing) = left_waiting(host, way).await? {
                        return Ok(landing);
                    }
                    continue;
                }
            }
        }
        let reach = Reach::of(host, lands.as_ref());
        if !git.is_ancestor(&root, &main, &head).await? {
            // A landing cut short right after your branch moved left it holding the run's
            // files already, in commits of their own. Under `delivery.write = "files"` those
            // are your commits, which the run review's notes don't follow.
            if git.tree_of(&root, &main).await? == git.tree_of(&root, &head).await? {
                merged(host, &target, &main, reach.moves_your_branch())?;
                return Ok(Landing::Done(RunOutcome::Merged));
            }
            // The same, for a run that merged beside your uncommitted edits: your branch
            // has its commit, and your index may not have followed yet.
            if reach == Reach::BesideYourEdits && is_the_runs_commit(host, &main).await? {
                if git.current_branch(&root).await?.as_deref() == Some(target.as_str()) {
                    follow_beside(host, &root, &main).await?;
                }
                merged(host, &target, &main, true)?;
                return Ok(Landing::Done(RunOutcome::Merged));
            }
            if moved == LANDING_TRIES {
                return Ok(Landing::Again(format!(
                    "{target} kept moving while the run merged. Answer again once it settles"
                )));
            }
            moved += 1;
            match carry_over(host, &worktree, &target, approved).await? {
                Ok(new_head) => head = new_head,
                Err(why) => return Ok(Landing::Again(why)),
            }
            continue;
        }
        // Work that started before your first commit is written over the files it started
        // from, whichever branch you're on by now.
        let elsewhere =
            lands.is_none() && git.current_branch(&root).await?.as_deref() != Some(target.as_str());
        if elsewhere {
            let refused = if reach.commits() {
                // Not checked out, so only the branch moves, and only if nobody moved it
                // meanwhile.
                let delivered = deliver::commit(host, &worktree, &main, &head).await?;
                match git.update_branch(&root, &target, &delivered, &main).await {
                    Ok(()) => {
                        merged(host, &target, &delivered, true)?;
                        return Ok(Landing::Done(RunOutcome::Merged));
                    }
                    Err(_)
                        if git.resolve(&root, &target).await? != main && moved < LANDING_TRIES =>
                    {
                        moved += 1;
                        continue;
                    }
                    Err(err) => git_words(&err),
                }
            } else {
                format!(
                    "{target} isn't checked out here, so there are no files to write the run \
                     into. Check it out, or set delivery.write to \"commit\""
                )
            };
            let blocked = Blocked {
                refused: Some(refused),
                ..Blocked::default()
            };
            let way = in_the_way::ask(host, &decided.id, &target, &blocked, None).await?;
            if let Some(landing) = left_waiting(host, way).await? {
                return Ok(landing);
            }
            continue;
        }
        // A landing cut short right after the writing left your files holding the run's work
        // already, which would read as edits of yours in its way.
        let files_from = reach.files_from(host, &main);
        let written = reach != Reach::Commits && already_written(host, &files_from, &head).await?;
        let range = (main.as_str(), head.as_str());
        let mut holding = Holding::default();
        if !written {
            let blocked = in_the_way::find(host, &main, &head).await?;
            if !blocked.is_empty() && your_folder::is(host) {
                // Nothing can be committed or stashed here, so your changes are merged with
                // the run's, and you're asked only about lines both changed.
                match your_folder::settle(host, &decided.id, &main, &head).await? {
                    Settled::Written(sha) => {
                        in_the_way::withdraw_open(host)?;
                        merged(host, &target, &sha, true)?;
                        return Ok(Landing::Done(RunOutcome::Merged));
                    }
                    Settled::Later => return Ok(Landing::Later),
                    Settled::InTheWay => {}
                }
            }
            if !blocked.is_empty() {
                let way =
                    in_the_way::ask(host, &decided.id, &target, &blocked, Some(range)).await?;
                if let Some(landing) = left_waiting(host, way).await? {
                    return Ok(landing);
                }
                continue;
            }
            holding = blocked.holding;
        }
        in_the_way::withdraw_open(host)?;
        let reached = match reach {
            Reach::Commits => {
                let delivered = deliver::commit(host, &worktree, &main, &head).await?;
                absorb::fast_forward(host, &root, &delivered, &holding)
                    .await
                    .map(|moved| moved.then_some(delivered))
            }
            Reach::Files(why) => {
                let files = (files_from.as_str(), head.as_str());
                write_files(host, &root, files, why, written, &holding).await
            }
            Reach::FirstCommit => {
                first_commit(host, &root, &target, range, written, &holding).await
            }
            Reach::BesideYourEdits => {
                let files = (files_from.as_str(), head.as_str());
                beside_your_edits(host, &worktree, &target, &main, files, written, &holding).await
            }
        };
        match reached {
            Ok(Some(sha)) => {
                in_the_way::tell_held(host, &holding)?;
                merged(host, &target, &sha, reach.moves_your_branch())?;
                return Ok(Landing::Done(RunOutcome::Merged));
            }
            // None only when your branch changed since it was read: it moved, or it got its
            // first commit from you.
            Ok(None) => {}
            // Something turned up after the look, or git has a reason of its own: you're asked,
            // in its words if nothing of yours is in the way.
            Err(err) => {
                // Once the run's files are written, they aren't yours in their own way.
                let written =
                    reach != Reach::Commits && already_written(host, &files_from, &head).await?;
                let mut blocked = if written {
                    Blocked::default()
                } else {
                    in_the_way::find(host, &main, &head).await?
                };
                let recheck = (!blocked.is_empty()).then_some(range);
                if blocked.is_empty() {
                    blocked.refused = Some(git_words(&err));
                }
                let way = in_the_way::ask(host, &decided.id, &target, &blocked, recheck).await?;
                if let Some(landing) = left_waiting(host, way).await? {
                    return Ok(landing);
                }
            }
        }
    }
}

/// Where your answer to the waiting question leaves the landing: `None` looks again.
async fn left_waiting(host: &Host, way: Way) -> EngineResult<Option<Landing>> {
    Ok(match way {
        Way::Again => None,
        Way::Later => Some(Landing::Later),
        Way::KeepBranch => Some(Landing::Done(RunOutcome::BranchLeft)),
        Way::PullRequest => Some(open_pull_request_for(host).await?),
    })
}

/// What you're told when an answer asks for a branch in a folder that has none.
const NO_BRANCH_HERE: &str = "this folder has no git, so there is no branch to keep and nowhere \
                              to open a pull request. The run can be written into your folder";

/// What you're told when the question about a run's branch went away unanswered.
const NO_TARGET: &str = "the run has no branch to merge into. Set merge.target, or keep the branch";

/// How approved work reaches your checkout.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Reach {
    /// Your branch moves to the run's commits.
    Commits,
    /// Your files change, and your branch stays where it is.
    Files(FilesOnly),
    /// Your branch has no commit yet and gets its first, as `merge.unborn` asks.
    FirstCommit,
    /// The run started from what you hadn't committed. Your branch gets one commit of the
    /// run's work, and your own edits stay in your files, uncommitted.
    BesideYourEdits,
}

impl Reach {
    /// A folder with a hidden history has no branch of yours to leave: its history follows
    /// the folder, so a run lands there as a commit whatever `delivery.write` says.
    fn of(host: &Host, lands: Option<&Lands>) -> Self {
        let asked =
            host.settings().delivery.write == DeliveryWrite::Files && !your_folder::is(host);
        match lands {
            Some(Lands::Files) => Self::Files(FilesOnly::NoCommitsYet),
            Some(Lands::FilesAfterYourCommit) => Self::Files(FilesOnly::YourFirstCommit),
            Some(Lands::FirstCommit { .. } | Lands::FirstCommitMade { .. }) => Self::FirstCommit,
            None if asked => Self::Files(FilesOnly::Asked),
            None if deliver::snapshot_of_yours(host).is_some() => Self::BesideYourEdits,
            None => Self::Commits,
        }
    }

    /// Whether a commit is made for a branch of yours that had commits when the run started.
    fn commits(self) -> bool {
        matches!(self, Self::Commits | Self::BesideYourEdits)
    }

    /// What your files are compared with, and written over: your branch at `main`, or the
    /// snapshot of your uncommitted edits the run started from.
    fn files_from(self, host: &Host, main: &str) -> String {
        match self {
            Self::BesideYourEdits | Self::Files(FilesOnly::Asked) => {
                deliver::snapshot_of_yours(host)
            }
            _ => None,
        }
        .unwrap_or_else(|| main.to_owned())
    }

    /// Whether a branch of yours ends up at what merged. The run review's notes follow only
    /// a branch that did.
    fn moves_your_branch(self) -> bool {
        !matches!(self, Self::Files(_))
    }
}

/// Why a run only writes your files and leaves your branch where it is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FilesOnly {
    /// `delivery.write = "files"` asks for it.
    Asked,
    /// The run started before your branch had a commit, so there's nothing to commit onto,
    /// and a first commit made for you would put your own files into history under the run's
    /// name.
    NoCommitsYet,
    /// `merge.unborn` asks for that first commit, but your branch got one from you while the
    /// run worked.
    YourFirstCommit,
}

/// Whether your checkout holds the run's changes, from `main` to `head`, already.
async fn already_written(host: &Host, main: &str, head: &str) -> Result<bool, GitError> {
    if main == head {
        return Ok(false);
    }
    let repo = host.repo();
    let scratch = repo.run_dir.join("landing.index");
    host.git()
        .holds_change(&repo.root, &scratch, main, head)
        .await
}

/// Writes the run's changes, from `from` to `head`, into your checkout without a commit,
/// unless they're `written` already, and says where its commits stay. Files of yours
/// `holding` what it brings stay as they are.
async fn write_files(
    host: &Host,
    root: &Path,
    (from, head): (&str, &str),
    why: FilesOnly,
    written: bool,
    holding: &Holding,
) -> Result<Option<String>, GitError> {
    if !written {
        let scratch = host.repo().run_dir.join("landing.index");
        let holding = holding.all();
        host.git()
            .write_files(root, &scratch, from, head, &holding)
            .await?;
    }
    say_written(host, why).await;
    Ok(Some(head.to_owned()))
}

/// Gives `branch`, which has no commit yet, its first: the run's changes go into your files
/// as for any landing as files, the branch starts at one commit of what the run ends with,
/// and your index follows it, so only what you changed since the run started shows as a
/// change. `None` when you made the branch's first commit in the same moment.
async fn first_commit(
    host: &Host,
    root: &Path,
    branch: &str,
    (main, head): (&str, &str),
    written: bool,
    holding: &Holding,
) -> Result<Option<String>, GitError> {
    let git = host.git();
    if !written {
        let scratch = host.repo().run_dir.join("landing.index");
        git.write_files(root, &scratch, main, head, &holding.all())
            .await?;
    }
    let commit = deliver::first_commit(host, root, main, head).await?;
    if let Err(err) = git.start_branch(root, branch, &commit).await {
        // Yours stays yours, and landing reads again how the work reaches you.
        return match git.head_commit(root).await? {
            Some(_) => Ok(None),
            None => Err(err),
        };
    }
    git.index_under_first_commit(root, main, &commit).await?;
    let note = Note {
        level: NoteLevel::Info,
        text: format!(
            "{branch} had no commits, so the run made its first one: your files as they were \
             when the run started, with the run's work, as merge.unborn asks. What you changed \
             since then shows as changes of yours."
        ),
    };
    // The commit is made either way, so a note that can't be recorded doesn't undo that.
    let _ = host.record(None, None, RunEventKind::Note(note));
    Ok(Some(commit))
}

/// Lands the run on `branch`, checked out here, beside what you hadn't committed when it
/// started: the run's work goes into your files over the snapshot it started from, `branch`
/// gets one commit that holds none of your edits, and your index follows that commit, so
/// your edits show as the uncommitted changes they were. `None` when `branch` moved in the
/// same moment.
async fn beside_your_edits(
    host: &Host,
    worktree: &Path,
    branch: &str,
    main: &str,
    (snapshot, head): (&str, &str),
    written: bool,
    holding: &Holding,
) -> Result<Option<String>, GitError> {
    let git = host.git();
    let root = &host.repo().root;
    if !written {
        let scratch = host.repo().run_dir.join("landing.index");
        git.write_files(root, &scratch, snapshot, head, &holding.left_alone())
            .await?;
    }
    let delivered = deliver::commit(host, worktree, main, head).await?;
    if delivered != main
        && git
            .update_branch(root, branch, &delivered, main)
            .await
            .is_err()
    {
        return Ok(None);
    }
    if delivered != main {
        follow_beside(host, root, &delivered).await?;
    }
    // Nothing to say once your branch holds those edits, committed by you meanwhile.
    if git.tree_of(root, snapshot).await? != git.tree_of(root, main).await? {
        let note = Note {
            level: NoteLevel::Info,
            text: format!(
                "The run started from files you hadn't committed. Its commit on {branch} \
                 holds only its own work, and what you still haven't committed stays in your \
                 files as it was."
            ),
        };
        // The commit is made either way, so a note that can't be recorded doesn't undo that.
        let _ = host.record(None, None, RunEventKind::Note(note));
    }
    Ok(Some(delivered))
}

/// Puts your index, and any file you took your edit back from, under `commit`, the one the
/// run made on your branch beside your edits.
async fn follow_beside(host: &Host, root: &Path, commit: &str) -> Result<(), GitError> {
    let git = host.git();
    let Some(snapshot) = deliver::snapshot_of_yours(host) else {
        return Ok(());
    };
    let Some(main) = git.parents(root, commit).await?.into_iter().next() else {
        return Ok(());
    };
    let scratch = host.repo().run_dir.join("landing.index");
    git.follow_beside(root, &scratch, &snapshot, &main, commit)
        .await
}

/// Whether `commit` is one this run made for your branch: its message names the run.
async fn is_the_runs_commit(host: &Host, commit: &str) -> Result<bool, GitError> {
    let log = host
        .git()
        .log_messages(&host.repo().root, &format!("{commit}^!"))
        .await?;
    Ok(log
        .first()
        .is_some_and(|(_, message)| deliver::names_run(message, host.run_id())))
}

/// Asks you what to do when git refused for a reason of its own, in its words.
async fn ask_about_refusal(
    host: &Host,
    decided: &ApprovalDecided,
    target: &str,
    err: &GitError,
) -> EngineResult<Way> {
    let blocked = Blocked {
        refused: Some(git_words(err)),
        ..Blocked::default()
    };
    in_the_way::ask(host, &decided.id, target, &blocked, None).await
}

/// Says the run's changes are in your files, why nothing was committed, and where the run's
/// own commits stay.
async fn say_written(host: &Host, why: FilesOnly) {
    let Ok((branch, _)) = merge_packet::landing_branch(host).await else {
        return;
    };
    let text = match why {
        FilesOnly::Asked => format!(
            "The run's changes are in your files, uncommitted, as delivery.write asks. Its \
             commits stay on {branch}."
        ),
        FilesOnly::NoCommitsYet => format!(
            "The run's changes are in your files for you to commit. Your branch had no \
             commit when the run started, so nothing was committed or staged. The run's own \
             commits stay on {branch}."
        ),
        FilesOnly::YourFirstCommit => format!(
            "The run's changes are in your files for you to commit. merge.unborn asks for \
             your branch's first commit, but you made that one while the run worked, so \
             nothing was committed or staged. The run's own commits stay on {branch}."
        ),
    };
    let note = Note {
        level: NoteLevel::Info,
        text,
    };
    // The files are written either way, so a note that can't be recorded doesn't undo that.
    let _ = host.record(None, None, RunEventKind::Note(note));
}

/// What git said, without the command around it.
fn git_words(err: &GitError) -> String {
    match err {
        GitError::Failed { stderr, .. } if !stderr.trim().is_empty() => stderr.trim().to_owned(),
        other => other.to_string(),
    }
}

/// Main moved while the merge waited: bring it in and rerun the suite. Returns the new
/// merge head, or why you must decide again.
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

fn refuse(host: &Host, decided: &ApprovalDecided, detail: &str) -> EngineResult<()> {
    let check = PolicyCheck {
        id: decided.id.clone(),
        violations: vec![Violation {
            limit: HardLimit::HeadMoved,
            detail: detail.to_owned(),
            paths: Vec::new(),
        }],
        verdict: None,
        by: None,
        judge: None,
        reasons: vec![detail.to_owned()],
        auto_preview: false,
    };
    host.record(
        None,
        None,
        RunEventKind::Approval(ApprovalEvent::Checked(check)),
    )?;
    Ok(())
}

fn merged(host: &Host, target: &str, sha: &str, fast_forward: bool) -> EngineResult<()> {
    host.record(
        None,
        None,
        RunEventKind::Merge(MergeEvent::RunMerged {
            target: target.to_owned(),
            sha: sha.to_owned(),
            fast_forward,
        }),
    )?;
    Ok(())
}

/// `merge.mode = "pull_request"`, as a learned workflow's canary runs use: the run lands as a pull
/// request instead of on the target.
pub async fn pull_request_by_rule(host: &Host) -> EngineResult<Landing> {
    open_pull_request_for(host).await
}

/// Pushes the run's branch and opens a pull request from it. Whoever answered, a pull request
/// is only ever chosen by you or by a setting of yours that names it, so the push is yours
/// either way. The run ends as a pull request only when one was opened: a push or `gh` that
/// failed says why and asks again, and without `gh` the pushed branch is left for you to open
/// one from.
async fn open_pull_request_for(host: &Host) -> EngineResult<Landing> {
    // A run on a GitHub issue ends as a draft that names it, shown to you first.
    if let Some(link) = crate::follow::link_of(host) {
        return crate::follow::draft::open(host, link).await;
    }
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
    let root = host.repo().root.clone();
    // A run that started from what you hadn't committed pushes one commit of its own work in
    // place of its history, and `gh` would fill the body from that history here.
    let kept = kept::without_yours(host).await?;
    let body = match &kept {
        Some(commit) => Some(kept::body_of(host, commit).await?),
        None => None,
    };
    let mut command = pr_command(&branch, &title, body.as_deref());
    if host.settings().merge.pr_draft {
        command.push("--draft".to_owned());
    }
    let pushed = match push(host, &root, &branch, kept.as_deref()).await? {
        Ok(remote) => remote,
        Err(why) => return not_opened(host, why),
    };
    let Some(gh) = host.git().installed("gh") else {
        let note = Note {
            level: NoteLevel::Info,
            text: format!(
                "Pushed {branch} to {pushed}. gh isn't installed, so the pull request is yours \
                 to open, and the run ends with its work on that branch."
            ),
        };
        host.record(None, None, RunEventKind::Note(note))?;
        return Ok(Landing::Done(RunOutcome::BranchLeft));
    };
    let url = match open_pull_request(host, &gh, &root, &command).await? {
        Ok(url) => url,
        Err(why) => {
            let why =
                format!("{why}. {branch} is pushed to {pushed}, and no pull request was opened");
            return not_opened(host, why);
        }
    };
    host.record(
        None,
        None,
        RunEventKind::Merge(MergeEvent::PullRequest {
            branch,
            url,
            command: Some(command),
        }),
    )?;
    Ok(Landing::Done(RunOutcome::PullRequest))
}

/// Nothing was opened, so the run doesn't end as a pull request: the transcript says why, and
/// the merge question opens again.
fn not_opened(host: &Host, why: String) -> EngineResult<Landing> {
    let note = Note {
        level: NoteLevel::Warn,
        text: format!("No pull request was opened: {why}."),
    };
    host.record(None, None, RunEventKind::Note(note))?;
    Ok(Landing::Again(why))
}

/// Pushes `branch`, which a pull request is opened from, and gives the remote it went to. With
/// `commit`, that commit is what the remote's branch holds. `Err` says why nothing was pushed.
async fn push(
    host: &Host,
    root: &Path,
    branch: &str,
    commit: Option<&str>,
) -> EngineResult<Result<String, String>> {
    let git = host.git();
    let remotes = git.remotes(root).await?;
    // `origin` is where `gh` looks first too.
    let Some(remote) = remotes
        .iter()
        .find(|remote| *remote == "origin")
        .or(remotes.first())
    else {
        return Ok(Err(format!(
            "this repository has no remote, so there is nowhere to open a pull request. \
             {branch} holds the run's work"
        )));
    };
    let pushed = match commit {
        Some(commit) => git.push_commit(root, remote, commit, branch).await,
        None => git.push(root, remote, branch).await,
    };
    Ok(match pushed {
        Ok(()) => Ok(remote.clone()),
        Err(err) => Err(format!(
            "pushing {branch} to {remote} failed, so no pull request was opened: {}",
            last_line(&git_words(&err))
        )),
    })
}

fn last_line(text: &str) -> &str {
    text.trim().lines().last().unwrap_or("no output").trim()
}

/// Runs `gh`, and gives the pull request's address, or what `gh` said when it failed.
async fn open_pull_request(
    host: &Host,
    gh: &Path,
    root: &Path,
    command: &[String],
) -> EngineResult<Result<Option<String>, String>> {
    let args = command.get(1..).unwrap_or_default();
    let env = host.git().env().iter().map(|(name, value)| (name, value));
    let out = tokio::process::Command::new(gh)
        .args(args)
        .envs(env)
        .current_dir(root)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .output()
        .await?;
    if out.status.success() {
        let stdout = String::from_utf8_lossy(&out.stdout);
        return Ok(Ok(stdout.lines().last().map(|line| line.trim().to_owned())));
    }
    let stderr = String::from_utf8_lossy(&out.stderr);
    Ok(Err(format!("gh pr create failed: {}", last_line(&stderr))))
}

fn abandon(host: &Host) -> EngineResult<Landing> {
    let branches = host.read(|state| {
        state
            .merge
            .branch
            .iter()
            .cloned()
            .chain(
                state
                    .tasks
                    .by_id
                    .values()
                    .filter_map(|record| record.branch.clone()),
            )
            .collect()
    });
    host.record(
        None,
        None,
        RunEventKind::Merge(MergeEvent::Abandoned { branches }),
    )?;
    Ok(Landing::Done(RunOutcome::Abandoned))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_pr_command_is_an_argument_vector() {
        let command = pr_command("argonauts/run-01/merged", "Add a Redis store; really", None);
        assert_eq!(
            command,
            [
                "gh",
                "pr",
                "create",
                "--head",
                "argonauts/run-01/merged",
                "--title",
                "Add a Redis store; really",
                "--fill"
            ]
        );
    }

    #[test]
    fn a_body_of_its_own_takes_the_place_of_the_branchs_commits() {
        let command = pr_command("argonauts/run-01/merged", "Add a Redis store", Some(""));
        assert_eq!(command[command.len() - 2..], ["--body", ""]);
        assert!(!command.contains(&"--fill".to_owned()));
    }
}
