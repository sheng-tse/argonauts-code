//! Whether a finished run's task belongs in the replay set: its tests fail at the base commit,
//! pass with the merged change, and lean on nothing the request and the base tree don't give.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

use argonauts_core::events::task::CheckKind;
use argonauts_core::events::{MergeEvent, ReviewEvent, RunLifecycle, SpendEvent};
use argonauts_core::events::{RunEvent, RunEventKind};
use argonauts_core::layout::{repo_state, run_files};
use argonauts_core::limits::{CheckRequest, CheckRunner};
use argonauts_core::session_runner::{SandboxMode, SandboxProfile};
use argonauts_core::settings::Settings;
use argonauts_core::settings::evolution::EntryAcceptance;
use argonauts_core::{RunId, TaskId, journal};

use super::files::{
    Scratch, nonce, place, read_regular, remove_if_present, safe_relative, write_json,
};
use super::isolated::isolated_repo;
use super::{Check, Entry, Proposal, ReplaySet, names};
use crate::error::{LearningError, Result};

/// A check with no timeout of its own gets the default `checks.timeout`.
const DEFAULT_TIMEOUT_SECS: u64 = 30 * 60;

/// What a scoring checkout needs besides the tests: the `checks.setup` commands, such as an
/// install from the lockfile, and how long a check may run.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(super) struct ScoringSetup {
    pub setup: Vec<Vec<String>>,
    pub timeout_secs: u64,
}

impl Default for ScoringSetup {
    fn default() -> Self {
        Self {
            setup: Vec::new(),
            timeout_secs: DEFAULT_TIMEOUT_SECS,
        }
    }
}

/// The entry's tests in one checkout. `ran` is false when a setup command failed first, which
/// says nothing about the tests.
pub(super) struct TestRun {
    pub ran: bool,
    pub passed: bool,
    pub summary: String,
}

/// What the journal says about a run that merged.
struct MergedRun {
    request: String,
    task_type: String,
    base: String,
    merged: String,
    tests: Vec<String>,
    test_command: Option<Vec<String>>,
    spent_usd: f64,
}

pub(super) async fn consider(
    set: &ReplaySet,
    run: &RunId,
    settings: &Settings,
    runner: Arc<dyn CheckRunner>,
) -> Result<Option<Proposal>> {
    let id = run.as_str();
    set.refresh_rule(&settings.evolution.replay)?;
    if set.entry(id)?.is_some() {
        return Ok(None);
    }
    let journal = set
        .store()
        .state_dir
        .join(repo_state::RUNS)
        .join(id)
        .join(run_files::EVENTS);
    let Some(facts) = read_run(&journal)? else {
        return Ok(None);
    };

    let source = set.store().repo_root.clone();
    let work = set.work_dir();
    let merged_dir = Scratch(isolated_repo(
        &source,
        &facts.merged,
        &work.join(format!("merged-{}", nonce())),
    )?);
    let base = git_commit(&merged_dir.0, &facts.base)?;
    let merged = git_commit(&merged_dir.0, &facts.merged)?;
    let tests = if facts.tests.is_empty() {
        names::changed_test_files(&merged_dir.0, &base, &merged)?
    } else {
        facts.tests.clone()
    };
    if tests.is_empty() {
        return Ok(None);
    }

    let (hidden, kept) = keep_hidden_tests(set, id, &merged_dir.0, &tests)?;
    let test_command = test_command(&facts, settings);
    let scoring = scoring_setup(settings);
    write_json(&set.scoring_setup_path(id), &scoring)?;
    let entry = Entry {
        id: id.to_owned(),
        run: run.clone(),
        task: facts.request.clone(),
        task_type: facts.task_type.clone(),
        base: base.clone(),
        merged: merged.clone(),
        tests: hidden,
        test_command: test_command.clone().unwrap_or_default(),
        limit_usd: facts.spent_usd,
        holdout: false,
        accepted: OffsetDateTime::now_utc(),
    };

    let mut checks = vec![kept];
    if facts.spent_usd <= 0.0 {
        checks.push(Check {
            name: "limit_known".to_owned(),
            passed: false,
            detail: "the run recorded no cost in dollars, so there's no limit to replay it at"
                .to_owned(),
        });
    }
    if test_command.is_none() {
        checks.push(Check {
            name: "test_command".to_owned(),
            passed: false,
            detail:
                "the run recorded no full suite, and checks.full_suite and checks.run are empty"
                    .to_owned(),
        });
    }
    if checks.iter().all(|check| check.passed) {
        let base_dir = Scratch(isolated_repo(
            &source,
            &base,
            &work.join(format!("base-{}", nonce())),
        )?);
        // Before the tests are copied into the base checkout, so their own names don't count
        // as the base tree's.
        let unseen = names::unseen(
            &facts.request,
            &base_dir.0,
            &merged_dir.0,
            &base,
            &merged,
            &tests,
            &entry.tests,
        )?;
        let replay_dir = set.dir();
        let at_merged = run_tests(&runner, &entry, &merged_dir.0, &scoring, &replay_dir).await?;
        let at_base = run_tests(&runner, &entry, &base_dir.0, &scoring, &replay_dir).await?;
        checks.extend([
            Check {
                name: "fails_at_base".to_owned(),
                passed: at_base.ran && !at_base.passed,
                detail: at_base.summary,
            },
            Check {
                name: "passes_when_merged".to_owned(),
                passed: at_merged.ran && at_merged.passed,
                detail: at_merged.summary,
            },
            Check {
                name: "needs_only_given_names".to_owned(),
                passed: unseen.is_empty(),
                detail: if unseen.is_empty() {
                    "every name the tests take from the change is in the request".to_owned()
                } else {
                    format!(
                        "the tests need {}, which neither the request nor the base tree gives",
                        unseen.join(", ")
                    )
                },
            },
        ]);
    }

    let proposal = Proposal { entry, checks };
    write_json(&set.proposal_path(id), &proposal)?;
    if settings.evolution.replay.accept_entries == EntryAcceptance::Auto && proposal.valid() {
        let entry = set.accept(id, super::set::draw_seed())?;
        return Ok(Some(Proposal {
            entry,
            checks: proposal.checks,
        }));
    }
    Ok(Some(proposal))
}

/// Copies the tests out of the merged checkout into the replay folder, refusing links and
/// paths that leave the repository.
fn keep_hidden_tests(
    set: &ReplaySet,
    id: &str,
    merged_dir: &Path,
    tests: &[String],
) -> Result<(Vec<PathBuf>, Check)> {
    let hidden_dir = set.hidden_tests_dir(id);
    remove_if_present(&hidden_dir)?;
    let mut hidden = Vec::new();
    let mut refused = Vec::new();
    for test in tests {
        let relative = Path::new(test);
        let copied = if safe_relative(relative) {
            read_regular(merged_dir, relative)
                .and_then(|bytes| place(&hidden_dir, relative, &bytes))
        } else {
            Err(LearningError::Refused(format!(
                "{test} leaves the repository"
            )))
        };
        match copied {
            Ok(()) => hidden.push(hidden_dir.join(relative)),
            Err(err) => refused.push(format!("{test}: {err}")),
        }
    }
    let check = Check {
        name: "tests_readable".to_owned(),
        passed: refused.is_empty(),
        detail: if refused.is_empty() {
            format!("{} test files kept out of every worktree", hidden.len())
        } else {
            refused.join("; ")
        },
    };
    Ok((hidden, check))
}

/// The run's own full suite, else the one your checks name.
fn test_command(facts: &MergedRun, settings: &Settings) -> Option<Vec<String>> {
    let checks = &settings.checks;
    facts
        .test_command
        .clone()
        .or_else(|| checks.full_suite.last().map(|check| check.command.clone()))
        .or_else(|| checks.run.last().map(|check| check.command.clone()))
        .filter(|command| !command.is_empty())
}

fn scoring_setup(settings: &Settings) -> ScoringSetup {
    let checks = &settings.checks;
    ScoringSetup {
        setup: checks
            .setup
            .iter()
            .map(|check| check.command.clone())
            .collect(),
        timeout_secs: checks
            .timeout
            .parse()
            .ok()
            .flatten()
            .map_or(DEFAULT_TIMEOUT_SECS, |timeout| timeout.as_secs()),
    }
}

/// Copies the hidden tests into `dir` and runs the entry's tests there through the check
/// runner, after the setup commands. The tests run sandboxed, with the replay folder unreadable
/// except for the checkout itself.
pub(super) async fn run_tests(
    checks: &Arc<dyn CheckRunner>,
    entry: &Entry,
    dir: &Path,
    scoring: &ScoringSetup,
    replay_dir: &Path,
) -> Result<TestRun> {
    let root = hidden_root(entry)?;
    for test in &entry.tests {
        let relative = test.strip_prefix(&root).map_err(|_| {
            LearningError::Refused(format!(
                "{} isn't among {}'s hidden tests",
                test.display(),
                entry.id
            ))
        })?;
        place(dir, relative, &std::fs::read(test)?)?;
    }
    let sandbox = |network: bool| SandboxProfile {
        mode: SandboxMode::Os,
        writable: vec![dir.to_path_buf()],
        readable: vec![dir.to_path_buf()],
        deny_read: vec![replay_dir.to_path_buf()],
        network,
        proxy_port: None,
        ask_port: None,
        closed: Vec::new(),
        listable: Vec::new(),
        loopback: false,
        outside: Default::default(),
    };
    let request = |name: &str, command: &[String], setup: bool| CheckRequest {
        run: entry.run.clone(),
        task: TaskId::numbered(0),
        name: name.to_owned(),
        command: command.to_vec(),
        workdir: dir.to_path_buf(),
        sandbox: sandbox(setup),
        timeout: std::time::Duration::from_secs(scoring.timeout_secs),
        setup,
    };
    for command in &scoring.setup {
        let outcome = checks.run(request("replay-setup", command, true)).await;
        if !outcome.passed {
            return Ok(TestRun {
                ran: false,
                passed: false,
                summary: format!("setup failed: {}", outcome.summary),
            });
        }
    }
    let outcome = checks
        .run(request("replay-tests", &entry.test_command, false))
        .await;
    Ok(TestRun {
        ran: true,
        passed: outcome.passed,
        summary: outcome.summary,
    })
}

/// The folder holding an entry's hidden tests at their repo paths: `tests/<id>` in the replay
/// folder.
pub(super) fn hidden_root(entry: &Entry) -> Result<PathBuf> {
    let missing = || LearningError::Refused(format!("{} has no hidden tests", entry.id));
    let first = entry.tests.first().ok_or_else(missing)?;
    first
        .ancestors()
        .find(|dir| {
            dir.file_name()
                .is_some_and(|name| name == entry.id.as_str())
                && dir
                    .parent()
                    .and_then(Path::file_name)
                    .is_some_and(|name| name == "tests")
        })
        .map(Path::to_path_buf)
        .ok_or_else(missing)
}

fn git_commit(repo: &Path, rev: &str) -> Result<String> {
    crate::git::run(
        repo,
        &[
            "rev-parse",
            "--verify",
            "--quiet",
            &format!("{rev}^{{commit}}"),
        ],
    )
}

fn read_run(path: &Path) -> Result<Option<MergedRun>> {
    if !path.exists() {
        return Ok(None);
    }
    let back = journal::read_all(path).map_err(|err| LearningError::Parse {
        path: path.to_owned(),
        message: err.to_string(),
    })?;
    Ok(merged_run(&back.events))
}

fn merged_run(events: &[RunEvent]) -> Option<MergedRun> {
    let mut started = None;
    let mut merged = None;
    let mut tests: Vec<String> = Vec::new();
    let mut test_command = None;
    let mut spent_usd = 0.0;
    for event in events {
        match &event.kind {
            RunEventKind::Run(RunLifecycle::Started(run)) => started = Some(run.as_ref()),
            RunEventKind::Merge(MergeEvent::RunMerged { sha, .. }) => {
                merged = Some(sha.clone());
            }
            RunEventKind::Review(ReviewEvent::TestsWritten { files, .. }) => {
                for file in files {
                    if !tests.contains(file) {
                        tests.push(file.clone());
                    }
                }
            }
            RunEventKind::Check(check) if check.kind == CheckKind::FullSuite && check.passed => {
                if let Some(command) = &check.command {
                    test_command = Some(command.clone());
                }
            }
            RunEventKind::Spend(SpendEvent::Run(limit)) => spent_usd = limit.spent.cost_usd,
            _ => {}
        }
    }
    let started = started?;
    Some(MergedRun {
        request: started.request.clone(),
        task_type: started.estimate.as_ref().map_or_else(
            || "unknown".to_owned(),
            |estimate| estimate.task_type.clone(),
        ),
        base: started.base_commit.clone(),
        merged: merged?,
        tests,
        test_command,
        spent_usd,
    })
}
