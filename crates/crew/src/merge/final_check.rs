//! Once every task is in: merge main in if it moved (`MainMoved`, `MainMergedIn`), with the
//! merger resolving what conflicts, then the final check (`checks.full_suite`, else
//! `checks.run`) on the run's worktree. A command that fails once main is in and held on the
//! run's own work was broken by main, which its summary says and the merger fixes. The result
//! is kept with the heads it ran on, so the same heads never run the suite twice.

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};

use argonauts_core::TaskId;
use argonauts_core::agents::session_runner::Access;
use argonauts_core::models::limits::CheckRequest;
use argonauts_core::runs::events::RunEventKind;
use argonauts_core::runs::events::task::{CheckOutcome, MergeEvent};
use argonauts_core::settings::protocol::CheckCommand;

use crate::engine::error::{EngineError, EngineResult};
use crate::engine::host::Host;
use crate::git::{GitError, StagedMerge};
use crate::merge::{conductor, detached, merge_packet, merger, unborn};

use crate::task::worktree::TaskTree;
use crate::task::{at_start, checks};

/// Used when `checks.timeout` doesn't parse or says `none`.
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30 * 60);

/// The name the final check's checks run under, since they belong to no task.
pub const CHECK_SCOPE: &str = "merge";

/// The worktree the run's own work is checked in, once the target's commits are merged into
/// the run's.
const OWN_WORK: &str = "before-main";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FinalCheck {
    pub passed: bool,
    /// Empty when no suite is set, and then rules that need the suite don't hold.
    pub checks: Vec<CheckOutcome>,
    /// Why it failed before any check ran, such as main conflicting with the run.
    pub problem: Option<String>,
}

impl FinalCheck {
    pub fn suite_ran(&self) -> bool {
        !self.checks.is_empty()
    }
}

/// A final check and the heads it ran on.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct Ran {
    /// The landing branch's head, with main already merged in.
    head: String,
    /// The target's head, when the run has a target.
    target: Option<String>,
    check: FinalCheck,
}

pub async fn run(host: &Host) -> EngineResult<FinalCheck> {
    let (_, worktree) = merge_packet::landing_branch(host).await?;
    // Main's merge and the suite see the tree no commit of Argonauts Code's changes underneath.
    let _held = conductor::hold(&worktree).await;
    let own_head = host.git().resolve(&worktree, "HEAD").await?;
    let passed_before = passed_on(host, &own_head).await;
    let check = match bring_in_main(host, &worktree).await? {
        Err(problem) => FinalCheck {
            passed: false,
            checks: Vec::new(),
            problem: Some(problem),
        },
        Ok(merged) => {
            let came_in = target(host).filter(|_| merged).map(|target| CameIn {
                target,
                own_head,
                passed_before,
            });
            let mut suite = run_suite(host, &worktree, came_in.as_ref()).await?;
            if let Some(came_in) = &came_in
                && !suite.broken_by_main.is_empty()
                && merger::fix_after_main(host, &worktree, &came_in.target, &suite.broken_by_main)
                    .await?
            {
                suite = run_suite(host, &worktree, None).await?;
            }
            FinalCheck {
                passed: suite.checks.iter().all(|check| check.passed),
                checks: suite.checks,
                problem: None,
            }
        }
    };
    let (head, target) = heads(host, &worktree).await?;
    let ran = Ran {
        head,
        target,
        check: check.clone(),
    };
    let path = ran_path(host);
    if let Some(dir) = path.parent() {
        tokio::fs::create_dir_all(dir).await?;
    }
    tokio::fs::write(&path, serde_json::to_vec_pretty(&ran)?).await?;
    record(host, check.passed)?;
    Ok(check)
}

/// The target's new commits, merged into the run's branch by this final check.
struct CameIn {
    target: String,
    /// The run's own head, before they came in.
    own_head: String,
    /// The checks of the last final check, when it passed on that head.
    passed_before: Option<Vec<CheckOutcome>>,
}

/// The suite's outcomes, and those of them that held on the run's own work and fail now
/// that the target's commits are in.
struct Suite {
    checks: Vec<CheckOutcome>,
    broken_by_main: Vec<CheckOutcome>,
}

/// The last final check, while the landing branch and the target are where it left them and
/// nothing recorded has changed the branch since. A suite can take half an hour, and a flaky one
/// would change the evidence of an approval you already answered.
pub async fn reusable(host: &Host) -> EngineResult<Option<FinalCheck>> {
    let Some(passed) = host.read(|state| state.merge.final_check) else {
        return Ok(None);
    };
    // A missing or unreadable record only means the check runs again.
    let Some(ran) = tokio::fs::read(ran_path(host))
        .await
        .ok()
        .and_then(|bytes| serde_json::from_slice::<Ran>(&bytes).ok())
    else {
        return Ok(None);
    };
    if ran.check.passed != passed {
        return Ok(None);
    }
    let (_, worktree) = merge_packet::landing_branch(host).await?;
    let (head, target) = heads(host, &worktree).await?;
    Ok((ran.head == head && ran.target == target).then_some(ran.check))
}

/// The checks of the last final check, when it passed on `head`.
async fn passed_on(host: &Host, head: &str) -> Option<Vec<CheckOutcome>> {
    let bytes = tokio::fs::read(ran_path(host)).await.ok()?;
    let ran: Ran = serde_json::from_slice(&bytes).ok()?;
    (ran.check.passed && ran.head == head).then_some(ran.check.checks)
}

async fn heads(host: &Host, worktree: &Path) -> EngineResult<(String, Option<String>)> {
    let git = host.git();
    let head = git.resolve(worktree, "HEAD").await?;
    let target = match target(host) {
        Some(target) => Some(main_commit(host, &target).await?),
        None => None,
    };
    Ok((head, target))
}

fn ran_path(host: &Host) -> PathBuf {
    host.repo().run_dir.join("merge").join("final-check.json")
}

/// The branch an approval lands on: `merge.target`, else the branch checked out at the start,
/// else the one you named for a run that started on no branch.
pub fn target(host: &Host) -> Option<String> {
    host.settings().merge.target.clone().or_else(|| {
        host.read(|state| {
            state
                .run
                .started
                .as_ref()
                .and_then(|started| started.base_branch.clone())
                .or_else(|| detached::chosen(state))
        })
    })
}

/// What the run is said to merge into: its target branch, or, when it started before your
/// branch had a commit, your files or the branch its first commit goes on.
pub async fn lands_on(host: &Host) -> Result<Option<String>, GitError> {
    match unborn::lands(host).await? {
        Some(lands) => Ok(Some(lands.target().to_owned())),
        None => Ok(target(host)),
    }
}

/// Where `target` stands for this run: the branch's tip, or the snapshot the run started from
/// when the branch had no commit then. That snapshot stays the run's main whatever you commit
/// meanwhile, since your first commit shares no history with it.
pub async fn main_commit(host: &Host, target: &str) -> Result<String, GitError> {
    match unborn::base(host).await? {
        Some(snapshot) => Ok(snapshot),
        None => host.git().resolve(&host.repo().root, target).await,
    }
}

/// Merges the target into the run branch when it moved. A conflict goes to the
/// merger, and the final check then runs on its merge. `Ok(Err(why))` when nobody resolved it;
/// the merge is then aborted and the branch left as it was.
pub async fn bring_in_main(host: &Host, worktree: &Path) -> EngineResult<Result<bool, String>> {
    let Some(target) = target(host) else {
        return Ok(Ok(false));
    };
    let git = host.git();
    let main = main_commit(host, &target).await?;
    let head = git.resolve(worktree, "HEAD").await?;
    if git.is_ancestor(worktree, &main, &head).await? {
        return Ok(Ok(false));
    }
    let from = host
        .read(|state| state.merge.main_seen.clone())
        .or_else(|| run_base(host))
        .unwrap_or_else(|| main.clone());
    let commits = git
        .log_messages(worktree, &format!("{from}..{main}"))
        .await?;
    host.record(
        None,
        None,
        RunEventKind::Merge(MergeEvent::MainMoved {
            from,
            to: main.clone(),
            commits: u32::try_from(commits.len()).unwrap_or(u32::MAX),
        }),
    )?;
    let subject = format!("Merge {target} into the run branch");
    let message = conductor::message(host.run_id(), &subject);
    // A merge a crash left half done, such as one the merger was resolving.
    git.abort_merge(worktree).await?;
    let resolved = match git.merge_staged(worktree, &main, true).await {
        Ok(StagedMerge::Staged | StagedMerge::UpToDate) => Vec::new(),
        Ok(StagedMerge::Conflict(files)) => {
            match merger::resolve_main(host, worktree, &target, &main, &files).await? {
                Ok(()) => files,
                Err(why) => {
                    git.abort_merge(worktree).await?;
                    return Ok(Err(why));
                }
            }
        }
        Err(GitError::Failed { stderr, .. }) => {
            return Ok(Err(format!(
                "{target} conflicts with the run's work: {stderr}"
            )));
        }
        Err(err) => return Err(err.into()),
    };
    if git.merge_in_progress(worktree).await? {
        git.finish_merge_of(worktree, &resolved, &message).await?;
    }
    let sha = git.resolve(worktree, "HEAD").await?;
    host.record(
        None,
        None,
        RunEventKind::Merge(MergeEvent::MainMergedIn { sha }),
    )?;
    Ok(Ok(true))
}

/// The commit the run started from.
pub fn run_base(host: &Host) -> Option<String> {
    host.read(|state| {
        state.merge.base.clone().or_else(|| {
            state
                .run
                .started
                .as_ref()
                .map(|started| started.base_commit.clone())
        })
    })
}

/// Runs the suite in `worktree`. With `came_in`, a command that fails is checked against the
/// run's own work, so a failure the target's commits caused says so and is listed apart.
async fn run_suite(host: &Host, worktree: &Path, came_in: Option<&CameIn>) -> EngineResult<Suite> {
    let mut suite = Suite {
        checks: Vec::new(),
        broken_by_main: Vec::new(),
    };
    let scope = TaskId::new(CHECK_SCOPE).map_err(|err| EngineError::Refused(err.to_string()))?;
    for check in commands(host) {
        let request = request(host, &scope, check, worktree)?;
        let ran = host.services().checks.run(request).await;
        let mut outcome = compared_with_the_start(host, check, ran.clone()).await?;
        if let Some(came_in) = came_in.filter(|_| !outcome.passed)
            && let Some(before) = held_before(host, check, came_in).await?
        {
            outcome = broke_after(ran, &before, &came_in.target);
            suite.broken_by_main.push(outcome.clone());
        }
        host.record(Some(&scope), None, RunEventKind::Check(outcome.clone()))?;
        suite.checks.push(outcome);
    }
    Ok(suite)
}

/// The final check's commands: `checks.full_suite`, else `checks.run`.
fn commands(host: &Host) -> &[CheckCommand] {
    let checks = &host.settings().checks;
    if checks.full_suite.is_empty() {
        &checks.run
    } else {
        &checks.full_suite
    }
}

fn request(
    host: &Host,
    scope: &TaskId,
    check: &CheckCommand,
    workdir: &Path,
) -> EngineResult<CheckRequest> {
    let settings = host.settings();
    let timeout = settings
        .checks
        .timeout
        .parse()
        .ok()
        .flatten()
        .unwrap_or(DEFAULT_TIMEOUT);
    let temp = host.repo().run_dir.join("tmp");
    Ok(CheckRequest {
        run: host.run_id().clone(),
        task: scope.clone(),
        name: check.name.clone(),
        command: check.command.clone(),
        workdir: workdir.to_path_buf(),
        sandbox: crate::sessions::spec::sandbox(settings, Access::Build, workdir, &temp)?,
        timeout,
        setup: false,
    })
}

/// A command that fails here as it failed before the run changed anything counts as passed,
/// with that in its summary: the run didn't break it, so it doesn't keep the run's work out.
/// One that fails otherwise, or that can't be compared, fails.
async fn compared_with_the_start(
    host: &Host,
    command: &CheckCommand,
    outcome: CheckOutcome,
) -> EngineResult<CheckOutcome> {
    if outcome.passed {
        return Ok(outcome);
    }
    let before = at_start::outcome_of(host, command).await?;
    Ok(at_start::judge(outcome, before.as_ref()))
}

/// How `command` did on the run's own work, when it held there: as the last final check ran it
/// on that head, else run now in a clean worktree at that head. `None` when it fails there too,
/// or the worktree can't be set up, so the failure isn't known to be the target's doing.
async fn held_before(
    host: &Host,
    command: &CheckCommand,
    came_in: &CameIn,
) -> EngineResult<Option<CheckOutcome>> {
    let earlier = came_in.passed_before.as_ref().and_then(|checks| {
        let same = |check: &&CheckOutcome| check.name == command.name;
        checks.iter().find(same).cloned()
    });
    if earlier.is_some() {
        return Ok(earlier);
    }
    let Some(copy) = own_work(host, came_in).await? else {
        return Ok(None);
    };
    let scope = TaskId::new(CHECK_SCOPE).map_err(|err| EngineError::Refused(err.to_string()))?;
    let ran = host
        .services()
        .checks
        .run(request(host, &scope, command, &copy)?)
        .await;
    let outcome = compared_with_the_start(host, command, ran).await?;
    Ok(outcome.passed.then_some(outcome))
}

/// The run's work as it was before the target's commits came in, in a clean worktree set up
/// for checks. `None` when the setup fails there.
async fn own_work(host: &Host, came_in: &CameIn) -> EngineResult<Option<PathBuf>> {
    let tree = TaskTree {
        path: host.repo().worktrees.join(OWN_WORK),
        branch: format!("the run before {} came in", came_in.target),
        base: came_in.own_head.clone(),
    };
    let copy = checks::clean_copy_at(host, &tree, &came_in.own_head).await?;
    let scope = TaskId::new(CHECK_SCOPE).map_err(|err| EngineError::Refused(err.to_string()))?;
    let failed = checks::setup(host, &scope, &copy, &tree.branch).await?;
    Ok(failed.is_none().then_some(copy))
}

/// The failure of a command that held on the run's own work, said as the target's doing:
/// how many more tests fail than did there, where both runs counted them.
fn broke_after(mut outcome: CheckOutcome, before: &CheckOutcome, target: &str) -> CheckOutcome {
    let more = match (
        at_start::failures(&before.summary),
        at_start::failures(&outcome.summary),
    ) {
        (Some(before), Some(now)) => now.checked_sub(before).filter(|more| *more > 0),
        _ => None,
    };
    outcome.summary = match more {
        Some(1) => format!("1 test broke after {target} came in"),
        Some(more) => format!("{more} tests broke after {target} came in"),
        None => format!("{}, after {target} came in", outcome.summary.trim()),
    };
    outcome
}

fn record(host: &Host, passed: bool) -> EngineResult<()> {
    host.record(
        None,
        None,
        RunEventKind::Merge(MergeEvent::FinalCheck { passed }),
    )?;
    Ok(())
}
