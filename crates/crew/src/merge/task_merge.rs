//! Merging one approved task into the run branch: no fast-forward, a merge commit of Argonauts
//! Code's naming the task, then its affected tests (the `checks.run` commands on the merge
//! worktree). Conflicts go to the interface's worker, semantic ones to the tie-breaker.

use std::path::Path;

use argonauts_core::TaskId;
use argonauts_core::agents::session_runner::Access;
use argonauts_core::base::names::{ModelRef, RoleName};
use argonauts_core::runs::events::RunEventKind;
use argonauts_core::runs::events::session::{EndReason, InputPartKind};
use argonauts_core::runs::events::task::{CheckKind, MergeEvent};
use argonauts_core::workflows::steps::task::{ReportStatus, ResolveConflict};
use argonauts_core::workflows::steps::{StepName, output_schema};

use crate::engine::error::{EngineError, EngineResult};
use crate::engine::host::{Fallbacks, Host, Ran, SessionPlan};
use crate::engine::idempotency;
use crate::engine::job_fallback::{self, Job};
use crate::git::{Git, StagedMerge};
use crate::merge::TaskMerge;
use crate::merge::branch::{ensure_run_branch, path_lock, run_base, run_branch, task_branch};
use crate::merge::{conductor, merger};
use crate::prompts;
use crate::roles::tie_breaker;
use crate::state::tasks::TaskEntry;
use crate::task::checks::{self, BuiltinChecks};
use crate::task::claims;
use crate::task::pipeline::dependencies_done;
use crate::task::prompt;
use crate::task::session::report_of;
use crate::task::support::{part, record_of, wait_until};
use crate::task::worktree::{self, TaskTree};

pub async fn merge(host: &Host, task: &TaskId) -> EngineResult<TaskMerge> {
    let run_tree = ensure_run_branch(host).await?;
    // Dependency order: a task merges only after what it builds on. Waited for before taking
    // the lock, which those merges need.
    wait_until(host, |state| dependencies_done(state, task)).await?;
    let lock = path_lock("merge", &run_tree);
    let _held = lock.lock().await;

    let git = host.git();
    if let Some(sha) = recorded_merge(host, task).await? {
        return Ok(TaskMerge::Merged { sha });
    }
    let mut conflicted = Vec::new();
    // One conflict resolution, then it goes to the task's worker.
    for _ in 0..2 {
        // A merge a crash left half done.
        git.abort_merge(&run_tree).await?;
        let record = record_of(host, task)?;
        let head = task_head(host, &record).await?;
        let range = format!("{}..HEAD", run_base(host)?);
        if let Some(sha) = idempotency::merge_of_head(git, &run_tree, &range, task, &head).await? {
            note_merged(host, task, &sha)?;
            return Ok(TaskMerge::Merged { sha });
        }
        clear_leftovers(git, &run_tree, &head).await?;
        match git.merge_staged(&run_tree, &head, false).await? {
            StagedMerge::UpToDate => {
                let sha = git.head_commit(&run_tree).await?.unwrap_or(head);
                note_merged(host, task, &sha)?;
                return Ok(TaskMerge::Merged { sha });
            }
            StagedMerge::Staged => {
                let markers = git.leftover_markers(&run_tree).await?;
                if !markers.is_empty() {
                    git.abort_merge(&run_tree).await?;
                    if !resolve(host, task, &markers).await? {
                        return Ok(TaskMerge::Conflict { files: markers });
                    }
                    conflicted = markers;
                    continue;
                }
                if let Some(output) = affected_tests(host, task, &run_tree).await? {
                    git.abort_merge(&run_tree).await?;
                    return Ok(TaskMerge::TestsFailed { output });
                }
                let (sha, _) = git
                    .finish_merge_work(
                        &run_tree,
                        &merge_message(host, &record),
                        claims::of_a_merge(host).as_ref(),
                    )
                    .await?;
                note_merged(host, task, &sha)?;
                return Ok(TaskMerge::Merged { sha });
            }
            StagedMerge::Conflict(files) => {
                if !resolve(host, task, &files).await? {
                    return Ok(TaskMerge::Conflict { files });
                }
                conflicted = files;
            }
        }
    }
    Ok(TaskMerge::Conflict { files: conflicted })
}

/// Removes what earlier checks left in the run's own worktree at a path `head` brings a file
/// to. Git refuses a merge over such a file, and nothing of yours is ever in this worktree:
/// it's Argonauts Code's own, and a merge commit there never takes what a check left behind.
async fn clear_leftovers(git: &Git, run_tree: &Path, head: &str) -> EngineResult<()> {
    for path in git.in_the_way(run_tree, "HEAD", head).await? {
        let at = run_tree.join(path.trim_end_matches('/'));
        // If it can't be removed, git's own refusal says so next.
        let _ = match path.ends_with('/') {
            true => std::fs::remove_dir_all(&at),
            false => std::fs::remove_file(&at),
        };
    }
    Ok(())
}

/// The merge the log already records for this task, when it holds the task's current head.
/// A replay then neither aborts nor merges again.
async fn recorded_merge(host: &Host, task: &TaskId) -> EngineResult<Option<String>> {
    let merged = host.read(|state| {
        state
            .merge
            .merged
            .iter()
            .find(|(merged, _)| merged == task)
            .map(|(_, sha)| sha.clone())
    });
    let Some(merged) = merged else {
        return Ok(None);
    };
    let head = task_head(host, &record_of(host, task)?).await?;
    let holds = host
        .git()
        .is_ancestor(&host.repo().root, &head, &merged)
        .await?;
    Ok(holds.then_some(merged))
}

async fn task_head(host: &Host, record: &TaskEntry) -> EngineResult<String> {
    let branch = record
        .branch
        .clone()
        .unwrap_or_else(|| task_branch(host.run_id(), &record.spec.id));
    host.git()
        .branch_head(&host.repo().root, &branch)
        .await?
        .ok_or_else(|| EngineError::Refused(format!("{branch} doesn't exist")))
}

/// Merges the run branch into the task's worktree and leaves any conflict markers for
/// the next session there. Its commit at that session's end finishes the merge, and the
/// merge head it brought in becomes the task's base.
pub(crate) async fn bring_in_run_branch(host: &Host, task: &TaskId) -> EngineResult<StagedMerge> {
    let tree = worktree::ensure(host, task).await?;
    let git = worktree::tree_git(host, &tree.path).await?;
    git.abort_merge(&tree.path).await?;
    let run_branch = run_branch(host.run_id());
    Ok(git.merge_staged(&tree.path, &run_branch, true).await?)
}

/// The merge commit's message: it names the task, for idempotency, and carries the Argonauts
/// Code trailer.
fn merge_message(host: &Host, record: &TaskEntry) -> String {
    let task = &record.spec.id;
    let subject = format!("Merge {task}: {}", record.spec.title);
    format!(
        "{}\n{}",
        conductor::message(host.run_id(), &subject),
        idempotency::task_line(task)
    )
}

fn note_merged(host: &Host, task: &TaskId, sha: &str) -> EngineResult<()> {
    let known = host.read(|state| {
        state
            .merge
            .merged
            .iter()
            .any(|(c, s)| c == task && s == sha)
    });
    if !known {
        let merged = MergeEvent::Merged {
            task: task.clone(),
            sha: sha.to_owned(),
        };
        host.record(Some(task), None, RunEventKind::Merge(merged))?;
    }
    Ok(())
}

/// Runs `checks.run` on the run's worktree with the merge staged. The failures' output
/// when any fails. Acceptance tests whose build task isn't in yet are expected to fail, the
/// merging test task's own included, so only the rest of the suite must pass.
async fn affected_tests(
    host: &Host,
    task: &TaskId,
    run_tree: &Path,
) -> EngineResult<Option<String>> {
    if let Some(output) = checks::setup(host, task, run_tree, "merge").await? {
        return Ok(Some(output));
    }
    let expected = checks::expected_failures(host, task, host.git(), run_tree).await?;
    let mut failed = Vec::new();
    for command in &host.settings().checks.run {
        let mut request = checks::request(host, task, command, run_tree, false)?;
        request.name = format!("affected: {}", command.name);
        let outcome = host.services().checks.run(request).await;
        let mut outcome = checks::judge(outcome, expected.get(&command.name));
        outcome.kind = CheckKind::AffectedTests;
        host.record(Some(task), None, RunEventKind::Check(outcome.clone()))?;
        if !outcome.passed {
            failed.push(format!(
                "{}: {}\n{}",
                outcome.name,
                outcome.summary,
                outcome.output_tail.trim()
            ));
        }
    }
    Ok((!failed.is_empty()).then(|| failed.join("\n\n")))
}

/// How one resolver's session went.
#[derive(Debug, Clone, PartialEq)]
enum Resolution {
    Resolved,
    /// The two sides disagree on behavior, which the tie-breaker decides.
    Semantic,
    /// The resolver's session ended for an agent reason, even when asked again, so another
    /// agent takes the conflict.
    Unavailable(EndReason),
    Failed,
}

/// The conflict goes to the interface's worker (or the merge chore's), and a semantic one
/// on to the tie-breaker. A resolver whose agent fails hands the conflict to the next, and after
/// the last to a fallback by the worker role's fallback list. True when the task's
/// branch now holds the run branch cleanly.
async fn resolve(host: &Host, task: &TaskId, files: &[String]) -> EngineResult<bool> {
    let assignee = conflict_worker(host, task);
    let conflict = MergeEvent::Conflict {
        task: task.clone(),
        files: files.to_vec(),
        assigned_to: assignee.clone(),
    };
    host.record(Some(task), None, RunEventKind::Merge(conflict))?;
    let mut resolvers = vec![assignee];
    let tie_breaker = host.read(|state| tie_breaker::tie_breaker(state, host.settings()));
    if let Some(tie_breaker) = tie_breaker.filter(|model| !resolvers.contains(model)) {
        resolvers.push(tie_breaker);
    }
    let mut unavailable: Vec<ModelRef> = Vec::new();
    let mut next = 0;
    while let Some(worker) = resolvers.get(next).cloned() {
        next += 1;
        let reason = match resolve_with(host, task, &worker, files).await? {
            Resolution::Resolved => return Ok(true),
            Resolution::Semantic => continue,
            Resolution::Failed => return Ok(false),
            Resolution::Unavailable(reason) => reason,
        };
        unavailable.push(worker.clone());
        if next < resolvers.len() {
            continue;
        }
        let job = Job {
            at: None,
            role: RoleName::Worker,
            step: Some(StepName::ResolveConflict),
            task: Some(task.clone()),
            independent_of: Vec::new(),
            isolated: false,
        };
        let failed: Vec<&ModelRef> = unavailable.iter().collect();
        match job_fallback::after(host, &job, &worker, &failed, &reason).await {
            Ok(taker) => resolvers.push(taker),
            Err(EngineError::Stalled(stall)) => {
                let line = format!("{} The task's worker resolves it instead.", stall.line);
                crate::plan::context::warn(host, Some(task), line)?;
                return Ok(false);
            }
            Err(err) => return Err(err),
        }
    }
    Ok(false)
}

/// The merger, which resolves conflicts between tasks. When it can't take the session,
/// the tie-breaker and then a fallback do.
fn conflict_worker(host: &Host, _task: &TaskId) -> ModelRef {
    merger::model(host.settings())
}

async fn resolve_with(
    host: &Host,
    task: &TaskId,
    worker: &ModelRef,
    files: &[String],
) -> EngineResult<Resolution> {
    let tree = worktree::ensure(host, task).await?;
    let git = worktree::tree_git(host, &tree.path).await?;
    let record = record_of(host, task)?;
    let mut plan = SessionPlan {
        without_status: false,
        at: None,
        role: RoleName::Worker,
        worker: worker.clone(),
        task: Some(task.clone()),
        access: Access::Build,
        step: Some(StepName::ResolveConflict),
        workdir: tree.path.clone(),
        parts: resolver_parts(host, &record, files),
        json_schema: Some(output_schema::<ResolveConflict>()),
        resume: None,
        slot: 0,
        label: None,
        reviews: false,
        fresh: true,
        independent_of: Vec::new(),
        in_place_of: None,
        fork_of: None,
    };
    plan.slot = host.slot_for(&plan);
    // A resolver that a pause or a crash cut off picks up its own half-done merge.
    let cut_off = host.read(|state| {
        state
            .sessions
            .by_key
            .values()
            .filter(|session| {
                session.task.as_ref() == Some(task)
                    && session.started.step == Some(StepName::ResolveConflict)
                    && session.started.worker == *worker
            })
            .max_by_key(|session| session.since)
            .is_some_and(|session| session.resumable())
    });
    if !(cut_off && git.merge_in_progress(&tree.path).await?) {
        match bring_in_run_branch(host, task).await? {
            StagedMerge::UpToDate => return Ok(Resolution::Resolved),
            StagedMerge::Staged => {
                worktree::commit_in(host, task, &tree, None, None).await?;
                return Ok(Resolution::Resolved);
            }
            StagedMerge::Conflict(_) => {}
        }
    }
    // A CLI error has the resolver go again once on its own session.
    let Ran {
        key,
        worker: by,
        end,
    } = host.run_session(plan, Fallbacks::None).await?;
    if end.reason.is_agent_failure() {
        git.abort_merge(&tree.path).await?;
        return Ok(Resolution::Unavailable(end.reason));
    }
    let report = report_of(&end);
    let status = report.as_ref().map(|r| r.status);
    if status == Some(ReportStatus::Done) && !has_markers(&tree, files) {
        worktree::commit_in(host, task, &tree, Some(&key), Some(by)).await?;
        if resolution_is_clean(host, task, &tree).await? {
            return Ok(Resolution::Resolved);
        }
        return Ok(Resolution::Failed);
    }
    git.abort_merge(&tree.path).await?;
    Ok(if status == Some(ReportStatus::NeedsDecision) {
        Resolution::Semantic
    } else {
        Resolution::Failed
    })
}

/// The built-in checks on the resolver's merge commit, before its work can reach the
/// run branch. False while the merge is still open or when a check failed.
async fn resolution_is_clean(host: &Host, task: &TaskId, tree: &TaskTree) -> EngineResult<bool> {
    let git = worktree::tree_git(host, &tree.path).await?;
    if git.merge_in_progress(&tree.path).await? {
        return Ok(false);
    }
    let checks = checks::builtin_checks(host, task, tree, "HEAD^1", "HEAD").await?;
    Ok(matches!(checks, BuiltinChecks::Done(failed) if failed.is_empty()))
}

fn resolver_parts(
    host: &Host,
    record: &TaskEntry,
    files: &[String],
) -> Vec<argonauts_core::runs::events::session::InputPart> {
    let list = files
        .iter()
        .map(|file| format!("- {file}"))
        .collect::<Vec<_>>()
        .join("\n");
    vec![
        part(
            InputPartKind::Prompt,
            "resolver",
            prompts::task::RESOLVE_CONFLICT,
            None,
        ),
        part(
            InputPartKind::Task,
            record.spec.id.as_str(),
            prompt::task_text(&record.spec),
            None,
        ),
        part(
            InputPartKind::Message,
            "conflicts",
            format!("Files with conflicts:\n{list}"),
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

/// Whether any of `files` in the tree still holds a conflict marker line.
fn has_markers(tree: &TaskTree, files: &[String]) -> bool {
    files.iter().any(|file| {
        std::fs::read_to_string(tree.path.join(file)).is_ok_and(|text| {
            text.lines().any(|line| {
                line.starts_with("<<<<<<< ") || line.starts_with(">>>>>>> ") || line == "======="
            })
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn conflict_markers_are_found_line_by_line() {
        let dir = tempfile::tempdir().unwrap();
        let tree = TaskTree {
            path: dir.path().to_path_buf(),
            branch: "b".into(),
            base: "x".into(),
        };
        std::fs::write(
            dir.path().join("a.rs"),
            "<<<<<<< HEAD\nours\n=======\ntheirs\n>>>>>>> b\n",
        )
        .unwrap();
        std::fs::write(
            dir.path().join("b.rs"),
            "let s = \"<<<<<<< not a marker\";\n",
        )
        .unwrap();
        assert!(has_markers(&tree, &["a.rs".into()]));
        assert!(!has_markers(&tree, &["b.rs".into(), "missing.rs".into()]));
    }
}
