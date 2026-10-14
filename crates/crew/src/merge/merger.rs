//! The merger, `[crew] merger`: a role like the reviewers, by default the orchestrator's
//! model, and the agent of every workflow's Merge step. It takes on what merging asks judgement
//! for, such as two tasks that changed the same lines, or your branch moving while the run
//! worked. Plain code still decides which files make up a change and whether a task passed, so
//! no agent decides on its own what goes into your history, and nothing touches your own
//! uncommitted work.

use std::path::Path;

use argonauts_core::events::session::{EndReason, InputPart, InputPartKind, SessionEnd};
use argonauts_core::events::task::CheckOutcome;
use argonauts_core::names::{ModelRef, RoleName};
use argonauts_core::session_runner::Access;
use argonauts_core::settings::Settings;
use argonauts_core::steps::task::{ReportStatus, ResolveConflict};
use argonauts_core::steps::{StepName, output_schema};
use argonauts_core::workflow::StepKind;

use crate::engine::host::{Fallbacks, Host, SessionPlan, crashed_end};
use crate::engine::job_fallback::{self, Job};
use crate::error::{EngineError, EngineResult};
use crate::git::StagedMerge;
use crate::merge::conductor;
use crate::prompts::{self, instructions};
use crate::state::RunState;
use crate::task::session::report_of;
use crate::task::support::part;
use crate::tie_breaker;
use crate::workflow::StepAt;

/// What the merger's sessions go by, each named for its job and never for the branch, so a row
/// can't read as the merge itself: code merges, and these only settle what needs judgement.
pub const COMMIT_MESSAGE: &str = "commit message";
pub const SETTLE_CONFLICT: &str = "settle conflict";

/// `fix after main moved`, for the branch the run lands on.
pub fn fix_after_move(target: &str) -> String {
    format!("fix after {target} moved")
}

/// Who merges: the first of `[crew] merger`, else the orchestrator.
pub fn model(settings: &Settings) -> ModelRef {
    settings
        .crew
        .merger
        .first()
        .cloned()
        .unwrap_or_else(|| instructions::orchestrator(settings))
}

/// The Merge step under way, which the merger's sessions belong to, so they show as its
/// agents.
pub fn merge_step(state: &RunState) -> Option<StepAt> {
    state
        .workflows
        .by_id
        .values()
        .flat_map(|record| &record.steps)
        .filter(|progress| progress.kind == StepKind::Merge && progress.end.is_none())
        .max_by_key(|progress| progress.since)
        .map(|progress| StepAt {
            step: progress.step.clone(),
            round: progress.round,
        })
}

/// Who takes a merge job next, once every agent in `tried` ended on its own side, the last for
/// `reason`: the phase's tie-breaker when it hasn't tried, then a fallback by the worker
/// role's list, the order a conflict between two tasks follows too. `None` once nobody may
/// take it, after one line that says so.
async fn next_taker(
    host: &Host,
    step: Option<StepName>,
    tried: &[ModelRef],
    reason: &EndReason,
) -> EngineResult<Option<ModelRef>> {
    let tie_breaker = host.read(|state| tie_breaker::tie_breaker(state, host.settings()));
    if let Some(tie_breaker) = tie_breaker.filter(|model| !tried.contains(model)) {
        return Ok(Some(tie_breaker));
    }
    let Some(last) = tried.last() else {
        return Ok(None);
    };
    let job = Job {
        at: None,
        role: RoleName::Worker,
        step,
        task: None,
        independent_of: Vec::new(),
        isolated: false,
    };
    let failed: Vec<&ModelRef> = tried.iter().collect();
    match job_fallback::after(host, &job, last, &failed, reason).await {
        Ok(taker) => Ok((!tried.contains(&taker)).then_some(taker)),
        Err(EngineError::Stalled(stall)) => {
            crate::plan::context::warn(host, None, stall.line)?;
            Ok(None)
        }
        Err(err) => Err(err),
    }
}

/// Runs one of the merger's sessions for `worker`. An agent that can't start ends it like one
/// that failed, so the next taker gets the job.
async fn run(host: &Host, mut plan: SessionPlan, worker: &ModelRef) -> EngineResult<SessionEnd> {
    plan.worker = worker.clone();
    plan.slot = host.slot_for(&plan);
    match host.run_session(plan, Fallbacks::None).await {
        Ok(ran) => Ok(ran.end),
        Err(EngineError::Runner(err)) => Ok(SessionEnd {
            reason: EndReason::StartFailed {
                message: err.to_string(),
            },
            ..crashed_end("")
        }),
        Err(err) => Err(err),
    }
}

/// `target` moved while the run worked, and merging `main`, its commit, into the run's branch
/// left conflicts in `files`, still marked in `worktree` where the merge waits. The merger
/// resolves them there. When its agent fails, the tie-breaker and then a fallback take the
/// conflict, each from the merge as git left it. `Ok(())` leaves the merge ready to commit.
/// `Err(why)` says why nobody resolved it, and the merge is then left for the caller to abort.
pub async fn resolve_main(
    host: &Host,
    worktree: &Path,
    target: &str,
    main: &str,
    files: &[String],
) -> EngineResult<Result<(), String>> {
    let git = host.git();
    let conflict = format!(
        "{target} moved and conflicts with the run's work in {}",
        files.join(", ")
    );
    let mut tried: Vec<ModelRef> = Vec::new();
    let mut worker = model(host.settings());
    loop {
        if !tried.is_empty() {
            git.abort_merge(worktree).await?;
            if !matches!(
                git.merge_staged(worktree, main, true).await?,
                StagedMerge::Conflict(_)
            ) {
                return Ok(Ok(()));
            }
        }
        let plan = SessionPlan {
            at: host.read(merge_step),
            role: RoleName::Worker,
            worker: worker.clone(),
            task: None,
            access: Access::Build,
            step: Some(StepName::ResolveConflict),
            workdir: worktree.to_path_buf(),
            parts: main_parts(host, target, files),
            json_schema: Some(output_schema::<ResolveConflict>()),
            resume: None,
            slot: 0,
            label: Some(SETTLE_CONFLICT.to_owned()),
            reviews: false,
            fresh: true,
        };
        let end = run(host, plan, &worker).await?;
        if end.reason.is_agent_failure() {
            tried.push(worker.clone());
            match next_taker(host, Some(StepName::ResolveConflict), &tried, &end.reason).await? {
                Some(next) => {
                    worker = next;
                    continue;
                }
                None => {
                    return Ok(Err(format!(
                        "{conflict}, and {} couldn't take the merge",
                        names(&tried)
                    )));
                }
            }
        }
        let report = report_of(&end);
        let markers = git.leftover_markers(worktree).await?;
        return Ok(match report {
            Some(report) if report.status == ReportStatus::Done && markers.is_empty() => Ok(()),
            Some(report) if report.status == ReportStatus::NeedsDecision => {
                let because = report.concerns.join(" ");
                let because = if because.trim().is_empty() {
                    report.summary
                } else {
                    because
                };
                Err(format!(
                    "{conflict}, and the two can't both hold: {because}"
                ))
            }
            _ if !markers.is_empty() => Err(format!(
                "{conflict}, and {} still holds conflict markers after the merger",
                markers.join(", ")
            )),
            _ => Err(format!("{conflict}, and the merger didn't finish")),
        });
    }
}

/// The checks passed on the run's branch, and `failed` fail once `target`'s new commits are
/// merged in, so the merge broke them. The merger fixes that in `worktree`, and its fix is
/// committed there. When its agent fails, the tie-breaker and then a fallback take the fix,
/// each from the tree as the merge left it. True when one committed a fix, for the checks to
/// run again; a fix nobody finished is undone, so the failure stands as it was.
pub async fn fix_after_main(
    host: &Host,
    worktree: &Path,
    target: &str,
    failed: &[CheckOutcome],
) -> EngineResult<bool> {
    let git = host.git();
    let mut tried: Vec<ModelRef> = Vec::new();
    let mut worker = model(host.settings());
    let report = loop {
        let plan = SessionPlan {
            at: host.read(merge_step),
            role: RoleName::Worker,
            worker: worker.clone(),
            task: None,
            access: Access::Build,
            step: None,
            workdir: worktree.to_path_buf(),
            parts: after_main_parts(host, target, failed),
            json_schema: Some(output_schema::<ResolveConflict>()),
            resume: None,
            slot: 0,
            label: Some(fix_after_move(target)),
            reviews: false,
            fresh: true,
        };
        let end = run(host, plan, &worker).await?;
        if !end.reason.is_agent_failure() {
            break report_of(&end).filter(|report| report.status == ReportStatus::Done);
        }
        // The next taker starts from the merge, never from a fix left half done.
        git.discard_tracked_changes(worktree).await?;
        tried.push(worker.clone());
        match next_taker(host, None, &tried, &end.reason).await? {
            Some(next) => worker = next,
            None => return Ok(false),
        }
    };
    let Some(report) = report else {
        git.discard_tracked_changes(worktree).await?;
        return Ok(false);
    };
    // What it changed of the tracked files, and the new files it names: never anything else a
    // check left in the tree.
    let mut paths: Vec<String> = git
        .tracked_changes(worktree)
        .await?
        .iter()
        .filter_map(|line| line.get(3..))
        .map(str::to_owned)
        .chain(report.files_changed)
        .collect();
    paths.sort();
    paths.dedup();
    let paths: Vec<&str> = paths.iter().map(String::as_str).collect();
    let message = conductor::message(host.run_id(), &format!("Fix what merging {target} broke"));
    Ok(git
        .commit_paths(worktree, &paths, &message)
        .await?
        .is_some())
}

/// `Opus on Claude Code and GPT X on Codex`, the agents that tried a job.
fn names(tried: &[ModelRef]) -> String {
    let names: Vec<String> = tried.iter().map(crate::roles::display_name).collect();
    names.join(" and ")
}

fn after_main_parts(host: &Host, target: &str, failed: &[CheckOutcome]) -> Vec<InputPart> {
    let checks = failed
        .iter()
        .map(|check| {
            format!(
                "## {}\n{}\n\n{}",
                check.name, check.summary, check.output_tail
            )
        })
        .collect::<Vec<_>>()
        .join("\n\n");
    vec![
        part(
            InputPartKind::Prompt,
            "merger",
            prompts::files::text(host, prompts::merge::AFTER_MAIN),
            None,
        ),
        part(
            InputPartKind::Message,
            "checks",
            format!("Merged {target} into the run's branch. Checks that fail now:\n\n{checks}"),
            None,
        ),
        part(
            InputPartKind::Schema,
            "report",
            prompts::files::text(host, prompts::task::BUILD),
            None,
        ),
    ]
}

fn main_parts(host: &Host, target: &str, files: &[String]) -> Vec<InputPart> {
    let list = files
        .iter()
        .map(|file| format!("- {file}"))
        .collect::<Vec<_>>()
        .join("\n");
    vec![
        part(
            InputPartKind::Prompt,
            "merger",
            prompts::files::text(host, prompts::merge::MAIN_MOVED),
            None,
        ),
        part(
            InputPartKind::Message,
            "conflicts",
            format!("Merging {target} into the run's branch. Files with conflicts:\n{list}"),
            None,
        ),
        part(
            InputPartKind::Schema,
            "report",
            prompts::files::text(host, prompts::task::BUILD),
            None,
        ),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_merger_follows_the_orchestrator_until_one_is_named() {
        let mut settings = Settings::default();
        assert_eq!(model(&settings), instructions::orchestrator(&settings));
        let named: ModelRef = "codex/gpt-5.6-luna".parse().unwrap();
        settings.crew.merger = vec![named.clone()];
        assert_eq!(model(&settings), named);
    }
}
