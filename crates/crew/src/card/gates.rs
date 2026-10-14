//! A card's gates, before review: the built-in checks on the agent's own commits (never the
//! conductor's) with a secret stopping the card at once, then whether a done report's changes
//! are on the branch, then each `gates.run` command in the card's sandbox, then the stub check
//! for test cards. Acceptance tests whose build card's work isn't in the tree yet are expected
//! to fail, and so are the commands that run them.
//!
//! The commands run on a clean copy of the card's commit, which holds exactly its change
//! (`crate::change`), never in the task's own folder: they see none of the leftovers beside the
//! work, and what they write, such as caches and compiled files, stays in the copy.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::time::Duration;

use argonauts_core::CardId;
use argonauts_core::events::RunEventKind;
use argonauts_core::events::card::{CardKind, CardState, GateKind, GateOutcome};
use argonauts_core::limits::GateRequest;
use argonauts_core::policy::{HardLimit, Violation};
use argonauts_core::settings::protocol::GateCommand;
use argonauts_core::steps::card::{CardReport, ReportStatus};
use argonauts_core::worker::Access;

use crate::card::support::record_of;
use crate::card::worktree::{self, CardTree};
use crate::engine::host::Host;
use crate::error::EngineResult;
use crate::git::Git;
use crate::integrate::branch::integration_branch;

/// What the gates cell says when there are no command gates.
pub const NO_COMMAND_GATES: &str = "No command checks set · /config checks.run";

/// Used when `gates.timeout` can't be read.
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30 * 60);
/// `gates.timeout = "none"` still ends a gate after a day.
const NO_TIMEOUT: Duration = Duration::from_secs(24 * 60 * 60);

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GateVerdict {
    Passed,
    /// The output goes back to the owner's session the first time.
    Failed {
        output: String,
    },
    /// A secret or another hard stop: the card stops at once.
    Stopped {
        reason: String,
    },
}

pub async fn run(host: &Host, card: &CardId) -> EngineResult<GateVerdict> {
    let tree = worktree::ensure(host, card).await?;
    let report = record_of(host, card)?.reports.pop();
    run_in(host, card, &tree, report.as_ref()).await
}

/// The gates in one tree of the card: its own, or an attempt's, with the report that ended
/// the work in it.
pub async fn run_in(
    host: &Host,
    card: &CardId,
    tree: &CardTree,
    report: Option<&CardReport>,
) -> EngineResult<GateVerdict> {
    let spec = record_of(host, card)?.spec;
    let git = worktree::tree_git(host, &tree.path).await?;
    let copy = clean_copy(host, &git, tree).await?;
    let label = format!("{} (clean copy)", tree.branch);
    if let Some(output) = setup(host, card, &copy, &label).await? {
        return Ok(GateVerdict::Stopped {
            reason: format!("checks.setup failed in {}: {output}", tree.branch),
        });
    }

    if git.merge_in_progress(&tree.path).await? {
        let files = git.leftover_markers(&tree.path).await?;
        return Ok(GateVerdict::Failed {
            output: unfinished_merge_text(&files),
        });
    }
    let mut failed = match builtin_checks(host, card, tree, &tree.base, "HEAD").await? {
        BuiltinChecks::Secret(reason) => return Ok(GateVerdict::Stopped { reason }),
        BuiltinChecks::Done(failed) => failed,
    };
    if let Some(outcome) = reported_changes(&git, tree, report).await? {
        host.record(Some(card), None, RunEventKind::Gate(outcome.clone()))?;
        if !outcome.passed {
            failed.push(outcome);
        }
    }

    let commands = &host.settings().checks.run;
    let tests_card = matches!(spec.kind, CardKind::AcceptanceTests { .. });
    if commands.is_empty() {
        host.record(
            Some(card),
            None,
            RunEventKind::Gate(no_command_gates(GateKind::Command)),
        )?;
    } else {
        // A test card's commands are the stub check's input, so they stay as they ran.
        let expected = if tests_card {
            BTreeMap::new()
        } else {
            expected_failures(host, card, &git, &tree.path).await?
        };
        let mut outcomes = Vec::new();
        for command in commands {
            let request = request(host, card, command, &copy, false)?;
            let outcome = host.services().gates.run(request).await;
            let outcome = judge(outcome, expected.get(&command.name));
            host.record(Some(card), None, RunEventKind::Gate(outcome.clone()))?;
            outcomes.push(outcome);
        }
        if tests_card {
            let check = stub_check(&outcomes);
            host.record(Some(card), None, RunEventKind::Gate(check.clone()))?;
            if !check.passed {
                failed.push(check);
            }
        } else {
            failed.extend(outcomes.into_iter().filter(|o| !o.passed));
        }
    }

    if failed.is_empty() {
        Ok(GateVerdict::Passed)
    } else {
        Ok(GateVerdict::Failed {
            output: failure_text(&failed),
        })
    }
}

/// One check's result as reviewers read it, such as `tests (node --test) passed: 9 pass`. A
/// summary that only repeats the result is left out.
pub(crate) fn result_line(gate: &GateOutcome) -> String {
    let result = if gate.passed { "passed" } else { "failed" };
    let ran = gate
        .command
        .as_ref()
        .map(|command| format!(" ({})", command.join(" ")))
        .unwrap_or_default();
    let summary = gate.summary.trim();
    if summary.is_empty() || summary == result {
        format!("{}{ran} {result}", gate.name)
    } else {
        format!("{}{ran} {result}: {summary}", gate.name)
    }
}

/// The clean copy of `tree`'s commit the checks run in: a detached worktree beside the run's
/// other worktrees, made once and put back to the commit before each run of the checks. Only
/// what git ignores survives from one run to the next, such as installed dependencies.
async fn clean_copy(host: &Host, git: &Git, tree: &CardTree) -> EngineResult<PathBuf> {
    let head = git.resolve(&tree.path, "HEAD").await?;
    clean_copy_at(host, tree, &head).await
}

/// A clean copy of the task's change as it stands, committed or not, for checks asked for
/// while its agents still work in its folder, and the label its setup commands run under.
pub(crate) async fn clean_copy_of_change(
    host: &Host,
    card: &CardId,
) -> EngineResult<(PathBuf, String)> {
    let tree = worktree::ensure(host, card).await?;
    let commit = crate::change::commit_of_task(host, card).await?;
    let copy = clean_copy_at(host, &tree, &commit).await?;
    Ok((copy, format!("{} (clean copy)", tree.branch)))
}

/// The checks' copy of `tree`, put back to `head`.
async fn clean_copy_at(host: &Host, tree: &CardTree, head: &str) -> EngineResult<PathBuf> {
    let name = tree
        .path
        .file_name()
        .map_or_else(|| "card".into(), |name| name.to_string_lossy());
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

/// The `gates.run` commands expected to fail in the tree at `dir`, each with the build cards
/// whose acceptance tests make it fail: tests already integrated (or the test card being
/// merged, when `card` is one) for another card whose work isn't in that tree. Commands run the
/// whole suite, so the rest of such a command goes unchecked until that work is in; a card's
/// own acceptance tests are never excused.
pub(crate) async fn expected_failures(
    host: &Host,
    card: &CardId,
    git: &Git,
    dir: &Path,
) -> EngineResult<BTreeMap<String, Vec<CardId>>> {
    let pending: Vec<(CardId, Option<String>, BTreeSet<String>)> = host.read(|state| {
        state
            .cards
            .by_id
            .values()
            .filter_map(|tests| {
                let CardKind::AcceptanceTests { for_card } = &tests.spec.kind else {
                    return None;
                };
                let in_tree = tests.state == CardState::Integrated || &tests.spec.id == card;
                if !in_tree || for_card == card {
                    return None;
                }
                let built = state
                    .integration
                    .merged
                    .iter()
                    .find(|(merged, _)| merged == for_card)
                    .map(|(_, sha)| sha.clone());
                Some((for_card.clone(), built, failed_commands(&tests.gates)))
            })
            .collect()
    });
    let mut expected: BTreeMap<String, Vec<CardId>> = BTreeMap::new();
    for (for_card, built, commands) in pending {
        let work_in = match &built {
            Some(sha) => git.is_ancestor(dir, sha, "HEAD").await?,
            None => false,
        };
        if work_in {
            continue;
        }
        for command in commands {
            let waiting = expected.entry(command).or_default();
            if !waiting.contains(&for_card) {
                waiting.push(for_card.clone());
            }
        }
    }
    Ok(expected)
}

/// The `gates.run` commands whose newest run in `gates` failed.
fn failed_commands(gates: &[GateOutcome]) -> BTreeSet<String> {
    let mut newest: BTreeMap<&str, bool> = BTreeMap::new();
    for gate in gates
        .iter()
        .filter(|gate| gate.kind == GateKind::Command && gate.command.is_some())
    {
        newest.insert(&gate.name, gate.passed);
    }
    newest
        .into_iter()
        .filter(|(_, passed)| !passed)
        .map(|(name, _)| name.to_owned())
        .collect()
}

/// A failure in a command expected to fail, for the cards `waiting` names, counts as passed,
/// with why in its summary. The exit code and output stay as they ran.
pub(crate) fn judge(mut outcome: GateOutcome, waiting: Option<&Vec<CardId>>) -> GateOutcome {
    let Some(waiting) = waiting.filter(|_| !outcome.passed) else {
        return outcome;
    };
    let cards = waiting
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join(", ");
    outcome.passed = true;
    outcome.summary = format!(
        "failed as expected, since the acceptance tests for {cards} wait for that work ({})",
        outcome.summary
    );
    outcome
}

/// Runs `gates.setup` once in the worktree on `branch` before its first gate, outside the
/// agent's sandbox. The failing command's output when one fails.
pub(crate) async fn setup(
    host: &Host,
    card: &CardId,
    workdir: &Path,
    branch: &str,
) -> EngineResult<Option<String>> {
    for command in &host.settings().checks.setup {
        let name = format!("setup: {} · {branch}", command.name);
        let done = host.read(|state| {
            state
                .cards
                .by_id
                .values()
                .flat_map(|record| &record.gates)
                .any(|gate| gate.name == name && gate.passed)
        });
        if done {
            continue;
        }
        let mut request = request(host, card, command, workdir, true)?;
        request.name = name;
        let outcome = host.gate(request).await?;
        if !outcome.passed {
            return Ok(Some(failure_text(&[outcome])));
        }
    }
    Ok(None)
}

pub(crate) fn request(
    host: &Host,
    card: &CardId,
    command: &GateCommand,
    workdir: &Path,
    setup: bool,
) -> EngineResult<GateRequest> {
    let settings = host.settings();
    let temp = host.repo().run_dir.join("tmp").join(card.as_str());
    std::fs::create_dir_all(&temp)?;
    let sandbox = crate::spec::sandbox(settings, Access::Build, workdir, &temp)?;
    let timeout = match settings.checks.timeout.parse() {
        Ok(Some(timeout)) => timeout,
        Ok(None) => NO_TIMEOUT,
        Err(_) => DEFAULT_TIMEOUT,
    };
    Ok(GateRequest {
        run: host.run_id().clone(),
        card: card.clone(),
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
    /// A secret, which stops the card at once.
    Secret(String),
    /// The checks that failed, if any.
    Done(Vec<GateOutcome>),
}

/// Runs the built-in checks on every commit between `base` and `head` in `tree` that isn't
/// the engine's own, and records one outcome per check.
pub(crate) async fn builtin_checks(
    host: &Host,
    card: &CardId,
    tree: &CardTree,
    base: &str,
    head: &str,
) -> EngineResult<BuiltinChecks> {
    let spec = record_of(host, card)?.spec;
    let git = worktree::tree_git(host, &tree.path).await?;
    let history = CardHistory {
        dir: &tree.path,
        conductor: worktree::conductor_commits(host, card)?,
        integration: integration_branch(host.run_id()),
    };
    let diff = agent_diff(&git, &history, base, head).await?;
    let violations = host
        .services()
        .policy
        .hard_limits()
        .check_card_tree_diff(&spec, &diff, &tree.path);
    let builtin = builtin_outcomes(&violations);
    for outcome in &builtin {
        host.record(Some(card), None, RunEventKind::Gate(outcome.clone()))?;
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
pub struct CardHistory<'a> {
    pub dir: &'a Path,
    /// Commits the engine made as the conductor, by SHA.
    pub conductor: BTreeSet<String>,
    /// The branch whose merges into the card only bring in work already checked.
    pub integration: String,
}

/// The unified diff of every change between `base` and `head` on the card's own line of
/// commits that the engine didn't make as the conductor, with build output shown as text. A
/// commit counts as the conductor's only by its recorded SHA, never by its message. A merge of
/// the integration branch counts only for what its resolution changed; any other merge counts
/// in full against its first parent.
pub async fn agent_diff(
    git: &Git,
    history: &CardHistory<'_>,
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
                if merges_integration(git, dir, &history.integration, merged).await? {
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

/// Whether every merged parent is already on the integration branch.
async fn merges_integration(
    git: &Git,
    dir: &Path,
    integration: &str,
    merged: &[String],
) -> EngineResult<bool> {
    let Some(head) = git.branch_head(dir, integration).await? else {
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
        "the merge of the integration branch isn't finished: resolve it and report done again"
            .to_owned()
    } else {
        format!(
            "the merge of the integration branch isn't finished: conflict markers are left in {}",
            files.join(", ")
        )
    }
}

/// One outcome per built-in check, each passed unless a violation of its kind was found.
fn builtin_outcomes(violations: &[Violation]) -> Vec<GateOutcome> {
    let mut by_kind: BTreeMap<u8, (GateKind, &str, Vec<Violation>)> = BTreeMap::new();
    for (order, kind, name) in [
        (0, GateKind::ProtectedPaths, "protected paths"),
        (1, GateKind::SecretScan, "secret scan"),
        (2, GateKind::TestTamper, "test tampering"),
        (3, GateKind::Dependencies, "dependencies"),
    ] {
        by_kind.insert(order, (kind, name, Vec::new()));
    }
    for violation in violations {
        let order = match builtin_kind(violation.limit) {
            GateKind::SecretScan => 1,
            GateKind::TestTamper => 2,
            GateKind::Dependencies => 3,
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
            GateOutcome {
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
pub fn builtin_kind(limit: HardLimit) -> GateKind {
    match limit {
        HardLimit::Secret => GateKind::SecretScan,
        HardLimit::TestDeleted
        | HardLimit::SkipMarker
        | HardLimit::TestConfigChanged
        | HardLimit::TamperFlag => GateKind::TestTamper,
        HardLimit::Dependency | HardLimit::Lockfile => GateKind::Dependencies,
        _ => GateKind::ProtectedPaths,
    }
}

/// Acceptance tests must fail against the interface stubs: the check passes when a command
/// gate failed, and fails when every command passed.
fn stub_check(outcomes: &[GateOutcome]) -> GateOutcome {
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
    GateOutcome {
        name: "stub check".to_owned(),
        kind: GateKind::StubCheck,
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

/// The outcome recorded when `gates.run` is empty: skipped, since nothing ran. It fails
/// nothing, and rules that need the suite still see it as not run.
pub fn no_command_gates(kind: GateKind) -> GateOutcome {
    GateOutcome {
        name: "checks.run".to_owned(),
        kind,
        passed: true,
        command: None,
        exit_code: None,
        duration_ms: 0,
        summary: NO_COMMAND_GATES.to_owned(),
        output_tail: String::new(),
        violations: Vec::new(),
        sandboxed: false,
        skipped: true,
    }
}

/// Journals from before `skipped` recorded the marker as a failure with no command.
pub fn is_no_command_gates(outcome: &GateOutcome) -> bool {
    outcome.skipped || (outcome.command.is_none() && outcome.summary == NO_COMMAND_GATES)
}

/// Whether the card's newest run of each gate passed, for `EvidenceFacts.gates_passed`. `None`
/// when no command gate ever ran, so merge rules that need the suite don't hold.
pub fn gates_passed(outcomes: &[GateOutcome]) -> Option<bool> {
    let mut newest: BTreeMap<&str, &GateOutcome> = BTreeMap::new();
    for outcome in outcomes {
        if outcome.name.starts_with("setup: ") {
            continue;
        }
        newest.insert(&outcome.name, outcome);
    }
    let suite_ran = newest
        .values()
        .any(|o| o.kind == GateKind::Command && o.command.is_some());
    if !suite_ran {
        return None;
    }
    Some(
        newest
            .values()
            .filter(|o| !is_no_command_gates(o))
            .all(|o| o.passed),
    )
}

/// Whether a done report matches the branch when the branch has no changes. A live owner once
/// reported `calc.py` changed with nothing on its branch, and with no review or command gates
/// the card would have merged nothing as done. `None` when there's no done report or the
/// branch has changes.
async fn reported_changes(
    git: &Git,
    tree: &CardTree,
    report: Option<&CardReport>,
) -> EngineResult<Option<GateOutcome>> {
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
    Ok(Some(GateOutcome {
        name: "reported changes".to_owned(),
        kind: GateKind::ReportedChanges,
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

fn failure_text(failed: &[GateOutcome]) -> String {
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

    use crate::integrate::conductor;
    use crate::testutil::{TestRepo, git_test_env};

    fn outcome(name: &str, kind: GateKind, passed: bool) -> GateOutcome {
        GateOutcome {
            name: name.into(),
            kind,
            passed,
            command: (kind == GateKind::Command).then(|| vec!["cargo".into(), "test".into()]),
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
        let kinds: Vec<(GateKind, bool)> = outcomes.iter().map(|o| (o.kind, o.passed)).collect();
        assert_eq!(
            kinds,
            [
                (GateKind::ProtectedPaths, true),
                (GateKind::SecretScan, false),
                (GateKind::TestTamper, false),
                (GateKind::Dependencies, true),
            ]
        );
        assert_eq!(outcomes[1].summary, "an AWS key");
        assert!(outcomes[1].output_tail.contains("src/config.rs"));
    }

    #[test]
    fn the_stub_check_wants_the_tests_to_fail() {
        let failing = stub_check(&[outcome("test", GateKind::Command, false)]);
        assert!(failing.passed);
        let passing = stub_check(&[outcome("test", GateKind::Command, true)]);
        assert!(!passing.passed);
        assert_eq!(passing.violations[0].limit, HardLimit::TestPassesOnStubs);
        assert_eq!(passing.kind, GateKind::StubCheck);
    }

    #[test]
    fn no_command_gates_means_the_suite_did_not_run() {
        let marker = no_command_gates(GateKind::Command);
        assert!(is_no_command_gates(&marker));
        assert_eq!(marker.summary, NO_COMMAND_GATES);
        // Recorded as skipped, not as a failure.
        assert!(marker.skipped && marker.passed);
        let recorded = serde_json::to_value(&marker).unwrap();
        assert_eq!(recorded["skipped"], true);
        let older = serde_json::json!({
            "name": "checks.run", "kind": "command", "passed": false, "command": null,
            "exit_code": null, "duration_ms": 0, "summary": NO_COMMAND_GATES,
            "output_tail": "", "violations": [], "sandboxed": false
        });
        let older: GateOutcome = serde_json::from_value(older).unwrap();
        assert!(!older.skipped && is_no_command_gates(&older));
        let secret = outcome("secret scan", GateKind::SecretScan, true);
        assert_eq!(gates_passed(&[secret.clone(), marker.clone()]), None);
        assert_eq!(gates_passed(&[]), None);

        let failed = outcome("test", GateKind::Command, false);
        let passed = outcome("test", GateKind::Command, true);
        assert_eq!(gates_passed(&[secret.clone(), failed.clone()]), Some(false));
        assert_eq!(
            gates_passed(&[secret, failed, passed]),
            Some(true),
            "the newest run of each check counts"
        );
    }

    fn history<'a>(repo: &'a TestRepo, conductor: &[&str]) -> CardHistory<'a> {
        CardHistory {
            dir: &repo.root,
            conductor: conductor.iter().map(|sha| (*sha).to_owned()).collect(),
            integration: "integration".to_owned(),
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
    async fn a_merge_of_integration_counts_only_its_resolution() {
        let repo = TestRepo::new();
        let git = Git::with_env(git_test_env());
        repo.write("shared.txt", "one\n");
        repo.git(&["add", "shared.txt"]);
        repo.git(&["commit", "--quiet", "-m", "shared"]);
        let base = git.head_commit(&repo.root).await.unwrap().unwrap();
        repo.git(&["checkout", "--quiet", "-b", "integration"]);
        repo.write("shared.txt", "theirs\n");
        repo.write("other.rs", "other task line\n");
        repo.git(&["add", "--all"]);
        repo.git(&["commit", "--quiet", "-m", "another task"]);
        repo.git(&["checkout", "--quiet", "main"]);
        repo.write("shared.txt", "ours\n");
        git.commit_all(&repo.root, "task-001: work").await.unwrap();

        let staged = git
            .merge_staged(&repo.root, "integration", true)
            .await
            .unwrap();
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
