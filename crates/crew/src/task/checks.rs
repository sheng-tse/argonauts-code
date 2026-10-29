//! A task's checks, before review: the built-in checks on the agent's own commits (never
//! Argonauts Code's) with a secret stopping the task at once, then whether a done report's changes
//! are on the branch, then each `checks.run` command in the task's sandbox, then the stub check
//! for test tasks. Acceptance tests whose build task's work isn't in the tree yet are expected
//! to fail, and so are the commands that run them.
//!
//! The commands run on a clean copy of the task's commit, which holds exactly its change
//! (`crate::change`), never in the task's own folder: they see none of the leftovers beside the
//! work, and what they write, such as caches and compiled files, stays in the copy.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::time::Duration;

use argonauts_core::TaskId;
use argonauts_core::events::RunEventKind;
use argonauts_core::events::task::{CheckKind, CheckOutcome, TaskKind, TaskState};
use argonauts_core::limits::CheckRequest;
use argonauts_core::policy::{HardLimit, Violation};
use argonauts_core::project_checks::untrusted_checks_file;
use argonauts_core::session_runner::Access;
use argonauts_core::settings::protocol::CheckCommand;
use argonauts_core::steps::task::{ReportStatus, TaskReport};

use crate::engine::host::Host;
use crate::error::EngineResult;
use crate::git::Git;
use crate::merge::branch::run_branch;
use crate::task::at_start;
use crate::task::support::record_of;
use crate::task::worktree::{self, TaskTree};

/// What the checks cell says when there are no command checks.
pub const NO_COMMAND_CHECKS: &str = "No command checks set · /config checks.run";

/// Used when `checks.timeout` can't be read.
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30 * 60);
/// `checks.timeout = "none"` still ends a check after a day.
const NO_TIMEOUT: Duration = Duration::from_secs(24 * 60 * 60);

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CheckVerdict {
    Passed,
    /// The output goes back to the worker's session the first time.
    Failed {
        output: String,
    },
    /// A secret or another hard stop: the task stops at once.
    Stopped {
        reason: String,
    },
}

pub async fn run(host: &Host, task: &TaskId) -> EngineResult<CheckVerdict> {
    let tree = worktree::ensure(host, task).await?;
    let report = record_of(host, task)?.reports.pop();
    run_in(host, task, &tree, report.as_ref()).await
}

/// The checks in one tree of the task: its own, or a try's, with the report that ended
/// the work in it.
pub async fn run_in(
    host: &Host,
    task: &TaskId,
    tree: &TaskTree,
    report: Option<&TaskReport>,
) -> EngineResult<CheckVerdict> {
    let spec = record_of(host, task)?.spec;
    let git = worktree::tree_git(host, &tree.path).await?;
    let copy = clean_copy(host, &git, tree).await?;
    let label = format!("{} (clean copy)", tree.branch);
    if let Some(output) = setup(host, task, &copy, &label).await? {
        return Ok(CheckVerdict::Stopped {
            reason: format!("checks.setup failed in {}: {output}", tree.branch),
        });
    }

    if git.merge_in_progress(&tree.path).await? {
        let files = git.leftover_markers(&tree.path).await?;
        return Ok(CheckVerdict::Failed {
            output: unfinished_merge_text(&files),
        });
    }
    let mut failed = match builtin_checks(host, task, tree, &tree.base, "HEAD").await? {
        BuiltinChecks::Secret(reason) => return Ok(CheckVerdict::Stopped { reason }),
        BuiltinChecks::Done(failed) => failed,
    };
    if let Some(outcome) = reported_changes(&git, tree, report).await? {
        host.record(Some(task), None, RunEventKind::Check(outcome.clone()))?;
        if !outcome.passed {
            failed.push(outcome);
        }
    }

    let commands = &host.settings().checks.run;
    let tests_task = matches!(spec.kind, TaskKind::AcceptanceTests { .. });
    if commands.is_empty() {
        host.record(Some(task), None, RunEventKind::Check(none_set(host)))?;
    } else {
        // A test task's commands are the stub check's input, so they stay as they ran.
        let expected = if tests_task {
            BTreeMap::new()
        } else {
            expected_failures(host, task, &git, &tree.path).await?
        };
        // What already failed before the run isn't this task's to answer for.
        at_start::ensure(host).await?;
        let mut outcomes = Vec::new();
        for command in commands {
            let request = request(host, task, command, &copy, false)?;
            let outcome = host.services().checks.run(request).await;
            let outcome = judge(outcome, expected.get(&command.name));
            let outcome = host.read(|state| {
                let failing = at_start::failing(state);
                at_start::judge(outcome, failing.get(command.name.as_str()).copied())
            });
            host.record(Some(task), None, RunEventKind::Check(outcome.clone()))?;
            outcomes.push(outcome);
        }
        if tests_task {
            let check = stub_check(&outcomes);
            host.record(Some(task), None, RunEventKind::Check(check.clone()))?;
            if !check.passed {
                failed.push(check);
            }
        } else {
            failed.extend(outcomes.into_iter().filter(|o| !o.passed));
        }
    }

    if failed.is_empty() {
        Ok(CheckVerdict::Passed)
    } else {
        Ok(CheckVerdict::Failed {
            output: failure_text(&failed),
        })
    }
}

/// One check's result as reviewers read it, such as `tests (node --test) passed: 9 pass`. A
/// summary that only repeats the result is left out.
pub(crate) fn result_line(check: &CheckOutcome) -> String {
    let result = if check.passed { "passed" } else { "failed" };
    let ran = check
        .command
        .as_ref()
        .map(|command| format!(" ({})", command.join(" ")))
        .unwrap_or_default();
    let summary = check.summary.trim();
    if summary.is_empty() || summary == result {
        format!("{}{ran} {result}", check.name)
    } else {
        format!("{}{ran} {result}: {summary}", check.name)
    }
}

/// The clean copy of `tree`'s commit the checks run in: a detached worktree beside the run's
/// other worktrees, made once and put back to the commit before each run of the checks. Only
/// what git ignores survives from one run to the next, such as installed dependencies.
async fn clean_copy(host: &Host, git: &Git, tree: &TaskTree) -> EngineResult<PathBuf> {
    let head = git.resolve(&tree.path, "HEAD").await?;
    clean_copy_at(host, tree, &head).await
}

/// A clean copy of the task's change as it stands, committed or not, for checks asked for
/// while its agents still work in its folder, and the label its setup commands run under.
pub(crate) async fn clean_copy_of_change(
    host: &Host,
    task: &TaskId,
) -> EngineResult<(PathBuf, String)> {
    let tree = worktree::ensure(host, task).await?;
    let commit = crate::change::commit_of_task(host, task).await?;
    let copy = clean_copy_at(host, &tree, &commit).await?;
    Ok((copy, format!("{} (clean copy)", tree.branch)))
}

/// The checks' copy of `tree`, put back to `head`.
pub(crate) async fn clean_copy_at(
    host: &Host,
    tree: &TaskTree,
    head: &str,
) -> EngineResult<PathBuf> {
    let name = tree
        .path
        .file_name()
        .map_or_else(|| "task".into(), |name| name.to_string_lossy());
    let copy = host.repo().worktrees.join(format!("{name}-checks"));
    let made = match host
        .git()
        .worktree_git_dir(&host.repo().root, &copy)
        .await?
    {
        Some(admin) if copy.is_dir() => Some(host.git().bound_to(admin, copy.clone())),
        _ => None,
    };
    match made {
        Some(copy_git) => copy_git.restore_clean(&copy, head).await?,
        None => {
            // A folder git no longer knows, after a crash or a cleanup, is made again.
            let _ = std::fs::remove_dir_all(&copy);
            host.git()
                .add_detached_worktree(&host.repo().root, &copy, head)
                .await?;
        }
    }
    Ok(copy)
}

/// The `checks.run` commands expected to fail in the tree at `dir`, each with the build tasks
/// whose acceptance tests make it fail: tests already merged (or the test task being
/// merged, when `task` is one) for another task whose work isn't in that tree. Commands run the
/// whole suite, so the rest of such a command goes unchecked until that work is in; a task's
/// own acceptance tests are never excused.
pub(crate) async fn expected_failures(
    host: &Host,
    task: &TaskId,
    git: &Git,
    dir: &Path,
) -> EngineResult<BTreeMap<String, Vec<TaskId>>> {
    let pending: Vec<(TaskId, Option<String>, BTreeSet<String>)> = host.read(|state| {
        state
            .tasks
            .by_id
            .values()
            .filter_map(|tests| {
                let TaskKind::AcceptanceTests { for_task } = &tests.spec.kind else {
                    return None;
                };
                let in_tree = tests.state == TaskState::Merged || &tests.spec.id == task;
                if !in_tree || for_task == task {
                    return None;
                }
                let built = state
                    .merge
                    .merged
                    .iter()
                    .find(|(merged, _)| merged == for_task)
                    .map(|(_, sha)| sha.clone());
                Some((for_task.clone(), built, failed_commands(&tests.checks)))
            })
            .collect()
    });
    let mut expected: BTreeMap<String, Vec<TaskId>> = BTreeMap::new();
    for (for_task, built, commands) in pending {
        let work_in = match &built {
            Some(sha) => git.is_ancestor(dir, sha, "HEAD").await?,
            None => false,
        };
        if work_in {
            continue;
        }
        for command in commands {
            let waiting = expected.entry(command).or_default();
            if !waiting.contains(&for_task) {
                waiting.push(for_task.clone());
            }
        }
    }
    Ok(expected)
}

/// The `checks.run` commands whose newest run in `checks` failed.
fn failed_commands(checks: &[CheckOutcome]) -> BTreeSet<String> {
    let mut newest: BTreeMap<&str, bool> = BTreeMap::new();
    for check in checks
        .iter()
        .filter(|check| check.kind == CheckKind::Command && check.command.is_some())
    {
        newest.insert(&check.name, check.passed);
    }
    newest
        .into_iter()
        .filter(|(_, passed)| !passed)
        .map(|(name, _)| name.to_owned())
        .collect()
}

/// A failure in a command expected to fail, for the tasks `waiting` names, counts as passed,
/// with why in its summary. The exit code and output stay as they ran.
pub(crate) fn judge(mut outcome: CheckOutcome, waiting: Option<&Vec<TaskId>>) -> CheckOutcome {
    let Some(waiting) = waiting.filter(|_| !outcome.passed) else {
        return outcome;
    };
    let tasks = waiting
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join(", ");
    outcome.passed = true;
    outcome.summary = format!(
        "failed as expected, since the acceptance tests for {tasks} wait for that work ({})",
        outcome.summary
    );
    outcome
}

/// Runs `checks.setup` once in the worktree on `branch` before its first check, outside the
/// agent's sandbox. The failing command's output when one fails.
pub(crate) async fn setup(
    host: &Host,
    task: &TaskId,
    workdir: &Path,
    branch: &str,
) -> EngineResult<Option<String>> {
    for command in &host.settings().checks.setup {
        let name = format!("setup: {} · {branch}", command.name);
        let done = host.read(|state| {
            state
                .tasks
                .by_id
                .values()
                .flat_map(|record| &record.checks)
                .any(|check| check.name == name && check.passed)
        });
        if done {
            continue;
        }
        let mut request = request(host, task, command, workdir, true)?;
        request.name = name;
        let outcome = host.check(request).await?;
        if !outcome.passed {
            return Ok(Some(failure_text(&[outcome])));
        }
    }
    Ok(None)
}

pub(crate) fn request(
    host: &Host,
    task: &TaskId,
    command: &CheckCommand,
    workdir: &Path,
    setup: bool,
) -> EngineResult<CheckRequest> {
    let settings = host.settings();
    let temp = host.repo().run_dir.join("tmp").join(task.as_str());
    std::fs::create_dir_all(&temp)?;
    let sandbox = crate::spec::sandbox(settings, Access::Build, workdir, &temp)?;
    let timeout = match settings.checks.timeout.parse() {
        Ok(Some(timeout)) => timeout,
        Ok(None) => NO_TIMEOUT,
        Err(_) => DEFAULT_TIMEOUT,
    };
    Ok(CheckRequest {
        run: host.run_id().clone(),
        task: task.clone(),
        name: command.name.clone(),
        command: command.command.clone(),
        workdir: workdir.to_path_buf(),
        sandbox,
        timeout,
        setup,
    })
}

/// What the built-in checks found.
pub(crate) enum BuiltinChecks {
    /// A secret, which stops the task at once.
    Secret(String),
    /// The checks that failed, if any.
    Done(Vec<CheckOutcome>),
}

/// Runs the built-in checks on every commit between `base` and `head` in `tree` that isn't
/// the engine's own, and records one outcome per check.
pub(crate) async fn builtin_checks(
    host: &Host,
    task: &TaskId,
    tree: &TaskTree,
    base: &str,
    head: &str,
) -> EngineResult<BuiltinChecks> {
    let spec = record_of(host, task)?.spec;
    let git = worktree::tree_git(host, &tree.path).await?;
    let history = TaskHistory {
        dir: &tree.path,
        conductor: worktree::conductor_commits(host, task)?,
        run_branch: run_branch(host.run_id()),
    };
    let diff = agent_diff(&git, &history, base, head).await?;
    let violations = host
        .services()
        .policy
        .hard_limits()
        .check_task_tree_diff(&spec, &diff, &tree.path);
    let builtin = builtin_outcomes(&violations);
    for outcome in &builtin {
        host.record(Some(task), None, RunEventKind::Check(outcome.clone()))?;
    }
    if let Some(secret) = violations.iter().find(|v| v.limit == HardLimit::Secret) {
        // The detail says whether it found a secret or a file it can't read.
        let reason = format!("secret scan: {}", secret.detail);
        return Ok(BuiltinChecks::Secret(reason));
    }
    Ok(BuiltinChecks::Done(
        builtin.into_iter().filter(|o| !o.passed).collect(),
    ))
}

/// What [`agent_diff`] needs to tell the engine's commits from anyone else's.
pub struct TaskHistory<'a> {
    pub dir: &'a Path,
    /// Commits the engine made as Argonauts Code, by SHA.
    pub conductor: BTreeSet<String>,
    /// The branch whose merges into the task only bring in work already checked.
    pub run_branch: String,
}

/// The unified diff of every change between `base` and `head` on the task's own line of
/// commits that the engine didn't make as Argonauts Code, with build output shown as text. A
/// commit counts as Argonauts Code's only by its recorded SHA, never by its message. A merge of
/// the run branch counts only for what its resolution changed; any other merge counts
/// in full against its first parent.
pub async fn agent_diff(
    git: &Git,
    history: &TaskHistory<'_>,
    base: &str,
    head: &str,
) -> EngineResult<String> {
    let dir = history.dir;
    let mut diff = String::new();
    let mut log = git
        .first_parent_log(dir, &format!("{base}..{head}"))
        .await?;
    log.reverse();
    for (sha, _) in log {
        if history.conductor.contains(&sha) {
            continue;
        }
        let parents = git.parents(dir, &sha).await?;
        let text = match parents.as_slice() {
            [] => continue,
            [parent] => git.scan_diff(dir, parent, &sha).await?,
            [first, merged @ ..] => {
                if merges_run_branch(git, dir, &history.run_branch, merged).await? {
                    git.remerge_diff(dir, &sha).await?
                } else {
                    git.scan_diff(dir, first, &sha).await?
                }
            }
        };
        diff.push_str(&text);
    }
    Ok(diff)
}

/// Whether every merged parent is already on the run branch.
async fn merges_run_branch(
    git: &Git,
    dir: &Path,
    run_branch: &str,
    merged: &[String],
) -> EngineResult<bool> {
    let Some(head) = git.branch_head(dir, run_branch).await? else {
        return Ok(false);
    };
    for parent in merged {
        if !git.is_ancestor(dir, parent, &head).await? {
            return Ok(false);
        }
    }
    Ok(true)
}

fn unfinished_merge_text(files: &[String]) -> String {
    if files.is_empty() {
        "the merge of the run branch isn't finished: resolve it and report done again".to_owned()
    } else {
        format!(
            "the merge of the run branch isn't finished: conflict markers are left in {}",
            files.join(", ")
        )
    }
}

/// One outcome per built-in check, each passed unless a violation of its kind was found.
fn builtin_outcomes(violations: &[Violation]) -> Vec<CheckOutcome> {
    let mut by_kind: BTreeMap<u8, (CheckKind, &str, Vec<Violation>)> = BTreeMap::new();
    for (order, kind, name) in [
        (0, CheckKind::ProtectedPaths, "protected paths"),
        (1, CheckKind::SecretScan, "secret scan"),
        (2, CheckKind::TestTamper, "test tampering"),
        (3, CheckKind::Dependencies, "dependencies"),
    ] {
        by_kind.insert(order, (kind, name, Vec::new()));
    }
    for violation in violations {
        let order = match builtin_kind(violation.limit) {
            CheckKind::SecretScan => 1,
            CheckKind::TestTamper => 2,
            CheckKind::Dependencies => 3,
            _ => 0,
        };
        if let Some((_, _, found)) = by_kind.get_mut(&order) {
            found.push(violation.clone());
        }
    }
    by_kind
        .into_values()
        .map(|(kind, name, found)| {
            let summary = match found.len() {
                0 => "passed".to_owned(),
                1 => found[0].detail.clone(),
                n => format!("{n} problems: {}", found[0].detail),
            };
            CheckOutcome {
                name: name.to_owned(),
                kind,
                passed: found.is_empty(),
                command: None,
                exit_code: None,
                duration_ms: 0,
                summary,
                output_tail: found
                    .iter()
                    .map(|v| format!("{}: {}", v.detail, v.paths.join(", ")))
                    .collect::<Vec<_>>()
                    .join("\n"),
                violations: found,
                sandboxed: false,
                skipped: false,
            }
        })
        .collect()
}

/// Which built-in check a hard limit belongs to.
pub fn builtin_kind(limit: HardLimit) -> CheckKind {
    match limit {
        HardLimit::Secret => CheckKind::SecretScan,
        HardLimit::TestDeleted
        | HardLimit::SkipMarker
        | HardLimit::TestConfigChanged
        | HardLimit::TamperFlag => CheckKind::TestTamper,
        HardLimit::Dependency | HardLimit::Lockfile => CheckKind::Dependencies,
        _ => CheckKind::ProtectedPaths,
    }
}

/// Acceptance tests must fail against the interface stubs: the check passes when a command
/// check failed, and fails when every command passed.
fn stub_check(outcomes: &[CheckOutcome]) -> CheckOutcome {
    let failing = outcomes.iter().any(|o| !o.passed);
    let (summary, violations) = if failing {
        ("the tests fail against the stubs".to_owned(), Vec::new())
    } else {
        let detail = "the tests pass against the stubs, so they test nothing yet".to_owned();
        let violation = Violation {
            limit: HardLimit::TestPassesOnStubs,
            detail: detail.clone(),
            paths: Vec::new(),
        };
        (detail, vec![violation])
    };
    CheckOutcome {
        name: "stub check".to_owned(),
        kind: CheckKind::StubCheck,
        passed: failing,
        command: None,
        exit_code: None,
        duration_ms: outcomes.iter().map(|o| o.duration_ms).sum(),
        summary,
        output_tail: outcomes
            .iter()
            .map(|o| format!("{}: {}", o.name, o.summary))
            .collect::<Vec<_>>()
            .join("\n"),
        violations,
        sandboxed: outcomes.iter().all(|o| o.sandboxed),
        skipped: false,
    }
}

/// The outcome recorded when `checks.run` is empty: skipped, since nothing ran. It fails
/// nothing, and rules that need the suite still see it as not run.
pub fn no_command_checks(kind: CheckKind) -> CheckOutcome {
    CheckOutcome {
        name: "checks.run".to_owned(),
        kind,
        passed: true,
        command: None,
        exit_code: None,
        duration_ms: 0,
        summary: NO_COMMAND_CHECKS.to_owned(),
        output_tail: String::new(),
        violations: Vec::new(),
        sandboxed: false,
        skipped: true,
    }
}

/// The marker for a run with no checks, saying why there are none: none are set, or the only
/// file that sets them is a project file you haven't trusted, which the run left out.
fn none_set(host: &Host) -> CheckOutcome {
    let mut marker = no_command_checks(CheckKind::Command);
    let overrides = host.read(|state| {
        state
            .run
            .started
            .as_ref()
            .map(|started| started.overrides.clone())
    });
    let root = &host.repo().root;
    if let Some(file) = overrides.and_then(|set| untrusted_checks_file(root, &set)) {
        marker.summary = held_back_text(&file);
    }
    marker
}

/// What the checks cell says when `file` sets the checks the run left out.
pub fn held_back_text(file: &str) -> String {
    format!("{HELD_BACK_IN}{file}{HELD_BACK_WHY}")
}

/// The file a no-checks marker says holds the checks the run left out, if it says one.
pub fn held_back_file(marker: &CheckOutcome) -> Option<&str> {
    let file = marker.summary.strip_prefix(HELD_BACK_IN)?;
    file.strip_suffix(HELD_BACK_WHY)
}

const HELD_BACK_IN: &str = "checks.run is set in ";
const HELD_BACK_WHY: &str = ", a file you haven't trusted · /setup";

/// Journals from before `skipped` recorded the marker as a failure with no command.
pub fn is_no_command_checks(outcome: &CheckOutcome) -> bool {
    outcome.skipped || (outcome.command.is_none() && outcome.summary == NO_COMMAND_CHECKS)
}

/// Whether the task's newest run of each check passed, for `EvidenceFacts.checks_passed`. `None`
/// when no command check ever ran, so merge rules that need the suite don't hold.
pub fn checks_passed(outcomes: &[CheckOutcome]) -> Option<bool> {
    let mut newest: BTreeMap<&str, &CheckOutcome> = BTreeMap::new();
    for outcome in outcomes {
        if outcome.name.starts_with("setup: ") {
            continue;
        }
        newest.insert(&outcome.name, outcome);
    }
    let suite_ran = newest
        .values()
        .any(|o| o.kind == CheckKind::Command && o.command.is_some());
    if !suite_ran {
        return None;
    }
    Some(
        newest
            .values()
            .filter(|o| !is_no_command_checks(o))
            .all(|o| o.passed),
    )
}

/// Whether a done report matches the branch when the branch has no changes. A live worker once
/// reported `calc.py` changed with nothing on its branch, and with no review or command checks
/// the task would have merged nothing as done. `None` when there's no done report or the
/// branch has changes.
async fn reported_changes(
    git: &Git,
    tree: &TaskTree,
    report: Option<&TaskReport>,
) -> EngineResult<Option<CheckOutcome>> {
    let Some(report) = report.filter(|report| report.status == ReportStatus::Done) else {
        return Ok(None);
    };
    if !git
        .changed_paths(&tree.path, &tree.base, "HEAD")
        .await?
        .is_empty()
    {
        return Ok(None);
    }
    let (passed, summary, output_tail) = if report.files_changed.is_empty() {
        (
            true,
            "no changes on the branch, as the report says".to_owned(),
            String::new(),
        )
    } else {
        (
            false,
            format!(
                "the report lists {}, but the branch has no changes",
                report.files_changed.join(", ")
            ),
            "Make the changes in this worktree, or report done again with files_changed empty \
             and say in the summary why nothing needed to change."
                .to_owned(),
        )
    };
    Ok(Some(CheckOutcome {
        name: "reported changes".to_owned(),
        kind: CheckKind::ReportedChanges,
        passed,
        command: None,
        exit_code: None,
        duration_ms: 0,
        summary,
        output_tail,
        violations: Vec::new(),
        sandboxed: false,
        skipped: false,
    }))
}

fn failure_text(failed: &[CheckOutcome]) -> String {
    failed
        .iter()
        .map(|o| {
            let tail = o.output_tail.trim();
            if tail.is_empty() {
                format!("{}: {}", o.name, o.summary)
            } else {
                format!("{}: {}\n{tail}", o.name, o.summary)
            }
        })
        .collect::<Vec<_>>()
        .join("\n\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use argonauts_core::RunId;

    use crate::merge::conductor;
    use crate::testutil::{TestRepo, git_test_env};

    fn outcome(name: &str, kind: CheckKind, passed: bool) -> CheckOutcome {
        CheckOutcome {
            name: name.into(),
            kind,
            passed,
            command: (kind == CheckKind::Command).then(|| vec!["cargo".into(), "test".into()]),
            exit_code: Some(if passed { 0 } else { 101 }),
            duration_ms: 5,
            summary: if passed {
                "ok".into()
            } else {
                "1 failed".into()
            },
            output_tail: String::new(),
            violations: Vec::new(),
            sandboxed: true,
            skipped: false,
        }
    }

    #[test]
    fn built_in_checks_group_violations_by_kind() {
        let violations = vec![
            Violation {
                limit: HardLimit::Secret,
                detail: "an AWS key".into(),
                paths: vec!["src/config.rs".into()],
            },
            Violation {
                limit: HardLimit::SkipMarker,
                detail: "a skip marker".into(),
                paths: vec!["tests/a.rs".into()],
            },
        ];
        let outcomes = builtin_outcomes(&violations);
        let kinds: Vec<(CheckKind, bool)> = outcomes.iter().map(|o| (o.kind, o.passed)).collect();
        assert_eq!(
            kinds,
            [
                (CheckKind::ProtectedPaths, true),
                (CheckKind::SecretScan, false),
                (CheckKind::TestTamper, false),
                (CheckKind::Dependencies, true),
            ]
        );
        assert_eq!(outcomes[1].summary, "an AWS key");
        assert!(outcomes[1].output_tail.contains("src/config.rs"));
    }

    #[test]
    fn the_stub_check_wants_the_tests_to_fail() {
        let failing = stub_check(&[outcome("test", CheckKind::Command, false)]);
        assert!(failing.passed);
        let passing = stub_check(&[outcome("test", CheckKind::Command, true)]);
        assert!(!passing.passed);
        assert_eq!(passing.violations[0].limit, HardLimit::TestPassesOnStubs);
        assert_eq!(passing.kind, CheckKind::StubCheck);
    }

    #[test]
    fn checks_held_back_name_the_file_that_holds_them() {
        let text = held_back_text(".argonauts/config.toml");
        assert_eq!(
            text,
            "checks.run is set in .argonauts/config.toml, a file you haven't trusted · /setup"
        );
        let mut marker = no_command_checks(CheckKind::Command);
        assert_eq!(held_back_file(&marker), None);
        marker.summary = text;
        assert_eq!(held_back_file(&marker), Some(".argonauts/config.toml"));
    }

    #[test]
    fn no_command_checks_means_the_suite_did_not_run() {
        let marker = no_command_checks(CheckKind::Command);
        assert!(is_no_command_checks(&marker));
        assert_eq!(marker.summary, NO_COMMAND_CHECKS);
        // Recorded as skipped, not as a failure.
        assert!(marker.skipped && marker.passed);
        let recorded = serde_json::to_value(&marker).unwrap();
        assert_eq!(recorded["skipped"], true);
        let older = serde_json::json!({
            "name": "checks.run", "kind": "command", "passed": false, "command": null,
            "exit_code": null, "duration_ms": 0, "summary": NO_COMMAND_CHECKS,
            "output_tail": "", "violations": [], "sandboxed": false
        });
        let older: CheckOutcome = serde_json::from_value(older).unwrap();
        assert!(!older.skipped && is_no_command_checks(&older));
        let secret = outcome("secret scan", CheckKind::SecretScan, true);
        assert_eq!(checks_passed(&[secret.clone(), marker.clone()]), None);
        assert_eq!(checks_passed(&[]), None);

        let failed = outcome("test", CheckKind::Command, false);
        let passed = outcome("test", CheckKind::Command, true);
        assert_eq!(
            checks_passed(&[secret.clone(), failed.clone()]),
            Some(false)
        );
        assert_eq!(
            checks_passed(&[secret, failed, passed]),
            Some(true),
            "the newest run of each check counts"
        );
    }

    fn history<'a>(repo: &'a TestRepo, conductor: &[&str]) -> TaskHistory<'a> {
        TaskHistory {
            dir: &repo.root,
            conductor: conductor.iter().map(|sha| (*sha).to_owned()).collect(),
            run_branch: "merge".to_owned(),
        }
    }

    #[tokio::test]
    async fn only_recorded_conductor_commits_are_left_out_of_the_agent_diff() {
        let repo = TestRepo::new();
        let git = Git::with_env(git_test_env());
        let run = RunId::new("run-01").unwrap();
        let base = git.head_commit(&repo.root).await.unwrap().unwrap();
        repo.write("src/agent.rs", "agent line\n");
        git.commit_all(
            &repo.root,
            "task-001: work from s-001\n\nArgonauts-Session: s-001",
        )
        .await
        .unwrap();
        repo.write("notes/yours.md", "your line\n");
        let yours = git
            .commit_all(
                &repo.root,
                &conductor::message(&run, "task-001: your changes"),
            )
            .await
            .unwrap()
            .unwrap();
        repo.write(".github/workflows/ci.yml", "forged line\n");
        git.commit_all(&repo.root, &conductor::message(&run, "task-001: tidy up"))
            .await
            .unwrap();
        let diff = agent_diff(&git, &history(&repo, &[&yours]), &base, "HEAD")
            .await
            .unwrap();
        assert!(diff.contains("+agent line"));
        assert!(!diff.contains("your line"));
        assert!(
            diff.contains("+forged line"),
            "a forged trailer is still checked"
        );
    }

    #[tokio::test]
    async fn the_secret_scan_reads_build_output_an_agent_committed() {
        use argonauts_policy::limits::{diff, secrets};

        let repo = TestRepo::new();
        let git = Git::with_env(git_test_env());
        let base = git.head_commit(&repo.root).await.unwrap().unwrap();
        let pyc = |text: &str| {
            [
                b"\x6f\x0d\x0d\x0a\0\0\0\0\xe3".as_slice(),
                text.as_bytes(),
                b"\0",
            ]
            .concat()
        };
        let cache = repo.root.join("stats/__pycache__");
        std::fs::create_dir_all(&cache).unwrap();
        let scan = async |repo: &TestRepo| {
            let text = agent_diff(&git, &history(repo, &[]), &base, "HEAD")
                .await
                .unwrap();
            secrets::check(&diff::parse(&text).unwrap())
        };

        // The agent's own `git add -A` takes the cache along.
        std::fs::write(cache.join("core.cpython-312.pyc"), pyc("\u{5}median")).unwrap();
        repo.git(&["add", "--all"]);
        repo.git(&["commit", "-qm", "task-001: work"]);
        assert_eq!(scan(&repo).await, Vec::new(), "harmless bytecode passes");

        let key = format!("AKIA{}", "Q".repeat(16));
        std::fs::write(cache.join("settings.cpython-312.pyc"), pyc(&key)).unwrap();
        repo.git(&["add", "--all"]);
        repo.git(&["commit", "-qm", "task-001: more"]);
        let found = scan(&repo).await;
        assert_eq!(found.len(), 1, "{found:?}");
        assert_eq!(found[0].limit, HardLimit::Secret);
        assert!(
            found[0].detail.contains("adds secrets"),
            "{}",
            found[0].detail
        );
        assert_eq!(
            found[0].paths,
            ["stats/__pycache__/settings.cpython-312.pyc"]
        );
        assert!(!found[0].detail.contains(&key));

        // Any other binary is still one the scan can't read.
        repo.git(&["rm", "-q", "stats/__pycache__/settings.cpython-312.pyc"]);
        std::fs::write(repo.root.join("data.bin"), pyc("blob")).unwrap();
        repo.git(&["add", "--all"]);
        repo.git(&["commit", "-qm", "task-001: data"]);
        let found = scan(&repo).await;
        assert!(
            found
                .iter()
                .any(|v| v.detail.contains("can't read") && v.paths == ["data.bin"]),
            "{found:?}"
        );
    }

    #[tokio::test]
    async fn a_merge_of_the_run_branch_counts_only_its_resolution() {
        let repo = TestRepo::new();
        let git = Git::with_env(git_test_env());
        repo.write("shared.txt", "one\n");
        repo.git(&["add", "shared.txt"]);
        repo.git(&["commit", "--quiet", "-m", "shared"]);
        let base = git.head_commit(&repo.root).await.unwrap().unwrap();
        repo.git(&["checkout", "--quiet", "-b", "merge"]);
        repo.write("shared.txt", "theirs\n");
        repo.write("other.rs", "other task line\n");
        repo.git(&["add", "--all"]);
        repo.git(&["commit", "--quiet", "-m", "another task"]);
        repo.git(&["checkout", "--quiet", "main"]);
        repo.write("shared.txt", "ours\n");
        git.commit_all(&repo.root, "task-001: work").await.unwrap();

        let staged = git.merge_staged(&repo.root, "merge", true).await.unwrap();
        assert!(matches!(staged, crate::git::StagedMerge::Conflict(_)));
        repo.write("shared.txt", "resolved\n");
        repo.write("sneaky.rs", "sneaky line\n");
        git.finish_merge(&repo.root, "task-001: work from s-002")
            .await
            .unwrap();

        repo.git(&["checkout", "--quiet", "-b", "side", &base]);
        repo.write("side.rs", "side line\n");
        repo.git(&["add", "side.rs"]);
        repo.git(&["commit", "--quiet", "-m", "side"]);
        repo.git(&["checkout", "--quiet", "main"]);
        repo.git(&["merge", "--quiet", "--no-ff", "-m", "merge side", "side"]);

        let diff = agent_diff(&git, &history(&repo, &[]), &base, "HEAD")
            .await
            .unwrap();
        assert!(diff.contains("+ours"));
        assert!(diff.contains("+resolved"));
        assert!(diff.contains("+sneaky line"));
        assert!(!diff.contains("other task line"), "{diff}");
        assert!(
            diff.contains("+side line"),
            "a merge of anything else counts in full"
        );
    }
}
