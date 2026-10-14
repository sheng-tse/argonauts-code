//! The merge agent, `[crew] merger`: a role like the reviewers, by default the orchestrator's
//! model, and the agent of every workflow's Merge step. It takes on what merging asks judgement
//! for, such as two tasks that changed the same lines, or your branch moving while the run
//! worked. Plain code still decides which files make up a change and whether a task passed, so
//! no agent decides on its own what goes into your history, and nothing touches your own
//! uncommitted work.

use std::path::Path;

use argonauts_core::events::card::GateOutcome;
use argonauts_core::events::session::{InputPart, InputPartKind};
use argonauts_core::names::{RoleName, WorkerRef};
use argonauts_core::settings::Settings;
use argonauts_core::steps::card::{ReportStatus, ResolveConflict};
use argonauts_core::steps::{StepName, output_schema};
use argonauts_core::worker::Access;
use argonauts_core::workflow::StepKind;

use crate::card::session::report_of;
use crate::card::support::part;
use crate::engine::host::{Host, SessionPlan, StandIns};
use crate::error::{EngineError, EngineResult};
use crate::git::StagedMerge;
use crate::integrate::conductor;
use crate::lead;
use crate::prompts::{self, instructions};
use crate::state::RunState;
use crate::workflow::StepAt;

/// Who merges: the first of `[crew] merger`, else the orchestrator.
pub fn seat(settings: &Settings) -> WorkerRef {
    settings
        .crew
        .merger
        .first()
        .cloned()
        .unwrap_or_else(|| instructions::orchestrator(settings))
}

/// The Merge step under way, which the merge agent's sessions belong to, so they show as its
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

/// `target` moved while the run worked, and merging `main`, its commit, into the run's branch
/// left conflicts in `files`, still marked in `worktree` where the merge waits. The merge agent
/// resolves them there; the phase lead takes over when the merge agent's vendor fails, each
/// from the merge as git left it. `Ok(())` leaves the merge ready to commit. `Err(why)` says
/// why nobody resolved it, and the merge is then left for the caller to abort.
pub async fn resolve_main(
    host: &Host,
    worktree: &Path,
    target: &str,
    main: &str,
    files: &[String],
) -> EngineResult<Result<(), String>> {
    let git = host.git();
    let mut resolvers = vec![seat(host.settings())];
    let lead = host.read(|state| lead::phase_lead(state, host.settings()));
    if let Some(lead) = lead.filter(|lead| !resolvers.contains(lead)) {
        resolvers.push(lead);
    }
    let conflict = format!(
        "{target} moved and conflicts with the run's work in {}",
        files.join(", ")
    );
    let mut why = conflict.clone();
    for (n, worker) in resolvers.into_iter().enumerate() {
        if n > 0 {
            git.abort_merge(worktree).await?;
            if !matches!(
                git.merge_staged(worktree, main, true).await?,
                StagedMerge::Conflict(_)
            ) {
                return Ok(Ok(()));
            }
        }
        let mut plan = SessionPlan {
            at: host.read(merge_step),
            role: RoleName::Worker,
            worker: worker.clone(),
            card: None,
            access: Access::Build,
            step: Some(StepName::ResolveConflict),
            workdir: worktree.to_path_buf(),
            parts: main_parts(host, target, files),
            json_schema: Some(output_schema::<ResolveConflict>()),
            resume: None,
            slot: 0,
            label: Some(format!("resolve:{target}")),
            reviews: false,
            fresh: true,
        };
        plan.slot = host.slot_for(&plan);
        let ran = host.run_session(plan, StandIns::None).await?;
        if ran.end.reason.is_vendor_failure() {
            why = format!("{conflict}, and {worker} couldn't take the merge");
            continue;
        }
        let report = report_of(&ran.end);
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
                "{conflict}, and {} still holds conflict markers after the merge agent",
                markers.join(", ")
            )),
            _ => Err(format!("{conflict}, and the merge agent didn't finish")),
        });
    }
    Ok(Err(why))
}

/// The checks passed on the run's branch, and `failed` fail once `target`'s new commits are
/// merged in, so the merge broke them. The merge agent fixes that in `worktree`, and its fix is
/// committed there. True when it committed one, for the checks to run again; a fix it didn't
/// finish is undone, so the failure stands as it was.
pub async fn fix_after_main(
    host: &Host,
    worktree: &Path,
    target: &str,
    failed: &[GateOutcome],
) -> EngineResult<bool> {
    let git = host.git();
    let mut plan = SessionPlan {
        at: host.read(merge_step),
        role: RoleName::Worker,
        worker: seat(host.settings()),
        card: None,
        access: Access::Build,
        step: None,
        workdir: worktree.to_path_buf(),
        parts: after_main_parts(host, target, failed),
        json_schema: Some(output_schema::<ResolveConflict>()),
        resume: None,
        slot: 0,
        label: Some(format!("fix:{target}")),
        reviews: false,
        fresh: true,
    };
    plan.slot = host.slot_for(&plan);
    let ran = match host.run_session(plan, StandIns::None).await {
        Ok(ran) => ran,
        // A merge agent that can't start leaves the failure to you, like one that fails.
        Err(EngineError::Worker(_)) => return Ok(false),
        Err(err) => return Err(err),
    };
    let Some(report) = report_of(&ran.end).filter(|report| report.status == ReportStatus::Done)
    else {
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

fn after_main_parts(host: &Host, target: &str, failed: &[GateOutcome]) -> Vec<InputPart> {
    let checks = failed
        .iter()
        .map(|gate| format!("## {}\n{}\n\n{}", gate.name, gate.summary, gate.output_tail))
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
            prompts::files::text(host, prompts::card::BUILD),
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
            prompts::files::text(host, prompts::card::BUILD),
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
        assert_eq!(seat(&settings), instructions::orchestrator(&settings));
        let named: WorkerRef = "codex/gpt-5.6-luna".parse().unwrap();
        settings.crew.merger = vec![named.clone()];
        assert_eq!(seat(&settings), named);
    }
}
