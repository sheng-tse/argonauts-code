//! Once every card is in: merge main in if it moved (`MainMoved`, `MainMergedIn`), then the
//! final check (`gates.full_suite`, else `gates.run`) on the integration worktree. Its result
//! is kept with the heads it ran on, so the same heads never run the suite twice.

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};

use argonauts_core::CardId;
use argonauts_core::events::RunEventKind;
use argonauts_core::events::card::{GateOutcome, IntegrationEvent};
use argonauts_core::limits::GateRequest;
use argonauts_core::worker::Access;

use crate::engine::host::Host;
use crate::error::{EngineError, EngineResult};
use crate::git::GitError;
use crate::integrate::{conductor, merge_packet};

/// Used when `gates.timeout` doesn't parse or says `none`.
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30 * 60);

/// The name the final check's gates run under, since they belong to no card.
pub const GATE_SCOPE: &str = "integration";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FinalCheck {
    pub passed: bool,
    /// Empty when no suite is set, and then rules that need the suite don't hold.
    pub gates: Vec<GateOutcome>,
    /// Why it failed before any gate ran, such as main conflicting with the run.
    pub problem: Option<String>,
}

impl FinalCheck {
    pub fn suite_ran(&self) -> bool {
        !self.gates.is_empty()
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
    // Main's merge and the suite see the tree no conductor commit changes underneath.
    let _held = conductor::hold(&worktree).await;
    let check = match bring_in_main(host, &worktree).await? {
        Err(problem) => FinalCheck {
            passed: false,
            gates: Vec::new(),
            problem: Some(problem),
        },
        Ok(_) => {
            let gates = run_suite(host, &worktree).await?;
            FinalCheck {
                passed: gates.iter().all(|gate| gate.passed),
                gates,
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

/// The last final check, while the landing branch and the target are where it left them and
/// nothing recorded has changed the branch since. A suite can take half an hour, and a flaky one
/// would change the evidence of a checkpoint you already answered.
pub async fn reusable(host: &Host) -> EngineResult<Option<FinalCheck>> {
    let Some(passed) = host.read(|state| state.integration.final_check) else {
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

async fn heads(host: &Host, worktree: &Path) -> EngineResult<(String, Option<String>)> {
    let git = host.git();
    let head = git.resolve(worktree, "HEAD").await?;
    let target = match target(host) {
        Some(target) => Some(git.resolve(&host.repo().root, &target).await?),
        None => None,
    };
    Ok((head, target))
}

fn ran_path(host: &Host) -> PathBuf {
    host.repo().run_dir.join("merge").join("final-check.json")
}

/// The branch an approval lands on: `merge.target`, else the branch checked out at the start.
pub fn target(host: &Host) -> Option<String> {
    host.settings().merge.target.clone().or_else(|| {
        host.read(|state| {
            state
                .run
                .started
                .as_ref()
                .and_then(|started| started.base_branch.clone())
        })
    })
}

/// Merges the target into the integration branch when it moved. `Ok(Err(why))` when it
/// conflicts; the merge is then aborted and the branch left as it was.
pub async fn bring_in_main(host: &Host, worktree: &Path) -> EngineResult<Result<bool, String>> {
    let Some(target) = target(host) else {
        return Ok(Ok(false));
    };
    let git = host.git();
    let main = git.resolve(&host.repo().root, &target).await?;
    let head = git.resolve(worktree, "HEAD").await?;
    if git.is_ancestor(worktree, &main, &head).await? {
        return Ok(Ok(false));
    }
    let from = host
        .read(|state| state.integration.main_seen.clone())
        .or_else(|| run_base(host))
        .unwrap_or_else(|| main.clone());
    let commits = git
        .log_messages(worktree, &format!("{from}..{main}"))
        .await?;
    host.record(
        None,
        None,
        RunEventKind::Integration(IntegrationEvent::MainMoved {
            from,
            to: main.clone(),
            commits: u32::try_from(commits.len()).unwrap_or(u32::MAX),
        }),
    )?;
    let subject = format!("Merge {target} into the integration branch");
    let message = conductor::message(host.run_id(), &subject);
    match git.merge(worktree, &main, &message).await {
        Ok(()) => {}
        Err(GitError::Failed { stderr, .. }) => {
            return Ok(Err(format!(
                "{target} conflicts with the run's work: {stderr}"
            )));
        }
        Err(err) => return Err(err.into()),
    }
    let sha = git.resolve(worktree, "HEAD").await?;
    host.record(
        None,
        None,
        RunEventKind::Integration(IntegrationEvent::MainMergedIn { sha }),
    )?;
    Ok(Ok(true))
}

/// The commit the run started from.
pub fn run_base(host: &Host) -> Option<String> {
    host.read(|state| {
        state.integration.base.clone().or_else(|| {
            state
                .run
                .started
                .as_ref()
                .map(|started| started.base_commit.clone())
        })
    })
}

async fn run_suite(host: &Host, worktree: &Path) -> EngineResult<Vec<GateOutcome>> {
    let settings = host.settings();
    let commands = if settings.gates.full_suite.is_empty() {
        &settings.gates.run
    } else {
        &settings.gates.full_suite
    };
    if commands.is_empty() {
        return Ok(Vec::new());
    }
    let timeout = settings
        .gates
        .timeout
        .parse()
        .ok()
        .flatten()
        .unwrap_or(DEFAULT_TIMEOUT);
    let temp = host.repo().run_dir.join("tmp");
    let sandbox = crate::spec::sandbox(settings, Access::Build, worktree, &temp)?;
    let scope = CardId::new(GATE_SCOPE).map_err(|err| EngineError::Refused(err.to_string()))?;
    let mut outcomes = Vec::new();
    for gate in commands {
        let request = GateRequest {
            run: host.run_id().clone(),
            card: scope.clone(),
            name: gate.name.clone(),
            command: gate.command.clone(),
            workdir: worktree.to_path_buf(),
            sandbox: sandbox.clone(),
            timeout,
            setup: false,
        };
        outcomes.push(host.gate(request).await?);
    }
    Ok(outcomes)
}

fn record(host: &Host, passed: bool) -> EngineResult<()> {
    host.record(
        None,
        None,
        RunEventKind::Integration(IntegrationEvent::FinalCheck { passed }),
    )?;
    Ok(())
}
