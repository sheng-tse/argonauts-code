//! Merging one approved card into the integration branch: no fast-forward, a conductor merge
//! commit naming the card, then its affected tests (the `gates.run` commands on the integration
//! worktree). Conflicts go to the interface's owner, semantic ones to the phase lead.

use std::path::Path;

use argonauts_core::CardId;
use argonauts_core::events::RunEventKind;
use argonauts_core::events::card::{CardKind, GateKind, IntegrationEvent};
use argonauts_core::events::session::{EndReason, InputPartKind};
use argonauts_core::names::{RoleName, WorkerRef};
use argonauts_core::steps::card::{ReportStatus, ResolveConflict};
use argonauts_core::steps::{StepName, output_schema};
use argonauts_core::worker::Access;

use crate::card::gates::{self, BuiltinChecks};
use crate::card::pipeline::dependencies_done;
use crate::card::prompt;
use crate::card::session::report_of;
use crate::card::support::{owner_of, part, record_of, wait_until};
use crate::card::worktree::{self, CardTree};
use crate::engine::host::{Host, Ran, SessionPlan, StandIns};
use crate::engine::stand_in::{self, Seat};
use crate::error::{EngineError, EngineResult};
use crate::git::StagedMerge;
use crate::idempotency;
use crate::integrate::CardMerge;
use crate::integrate::branch::{
    card_branch, ensure_integration, integration_branch, path_lock, run_base,
};
use crate::integrate::conductor;
use crate::lead;
use crate::prompts;
use crate::state::cards::CardRecord;

pub async fn merge(host: &Host, card: &CardId) -> EngineResult<CardMerge> {
    let integration = ensure_integration(host).await?;
    // Dependency order: a card merges only after what it builds on. Waited for before taking
    // the lock, which those merges need.
    wait_until(host, |state| dependencies_done(state, card)).await?;
    let lock = path_lock("merge", &integration);
    let _held = lock.lock().await;

    let git = host.git();
    if let Some(sha) = recorded_merge(host, card).await? {
        return Ok(CardMerge::Integrated { sha });
    }
    let mut conflicted = Vec::new();
    // One conflict resolution, then the card's owner takes over.
    for _ in 0..2 {
        // A merge a crash left half done.
        git.abort_merge(&integration).await?;
        let record = record_of(host, card)?;
        let head = card_head(host, &record).await?;
        let range = format!("{}..HEAD", run_base(host)?);
        if let Some(sha) =
            idempotency::merge_of_head(git, &integration, &range, card, &head).await?
        {
            note_merged(host, card, &sha)?;
            return Ok(CardMerge::Integrated { sha });
        }
        match git.merge_staged(&integration, &head, false).await? {
            StagedMerge::UpToDate => {
                let sha = git.head_commit(&integration).await?.unwrap_or(head);
                note_merged(host, card, &sha)?;
                return Ok(CardMerge::Integrated { sha });
            }
            StagedMerge::Staged => {
                let markers = git.leftover_markers(&integration).await?;
                if !markers.is_empty() {
                    git.abort_merge(&integration).await?;
                    if !resolve(host, card, &markers).await? {
                        return Ok(CardMerge::Conflict { files: markers });
                    }
                    conflicted = markers;
                    continue;
                }
                if let Some(output) = affected_tests(host, card, &integration).await? {
                    git.abort_merge(&integration).await?;
                    return Ok(CardMerge::TestsFailed { output });
                }
                let sha = git
                    .finish_merge(&integration, &merge_message(host, &record))
                    .await?;
                note_merged(host, card, &sha)?;
                return Ok(CardMerge::Integrated { sha });
            }
            StagedMerge::Conflict(files) => {
                if !resolve(host, card, &files).await? {
                    return Ok(CardMerge::Conflict { files });
                }
                conflicted = files;
            }
        }
    }
    Ok(CardMerge::Conflict { files: conflicted })
}

/// The merge the log already records for this card, when it holds the card's current head.
/// A replay then neither aborts nor merges again.
async fn recorded_merge(host: &Host, card: &CardId) -> EngineResult<Option<String>> {
    let merged = host.read(|state| {
        state
            .integration
            .merged
            .iter()
            .find(|(merged, _)| merged == card)
            .map(|(_, sha)| sha.clone())
    });
    let Some(merged) = merged else {
        return Ok(None);
    };
    let head = card_head(host, &record_of(host, card)?).await?;
    let holds = host
        .git()
        .is_ancestor(&host.repo().root, &head, &merged)
        .await?;
    Ok(holds.then_some(merged))
}

async fn card_head(host: &Host, record: &CardRecord) -> EngineResult<String> {
    let branch = record
        .branch
        .clone()
        .unwrap_or_else(|| card_branch(host.run_id(), &record.spec.id));
    host.git()
        .branch_head(&host.repo().root, &branch)
        .await?
        .ok_or_else(|| EngineError::Refused(format!("{branch} doesn't exist")))
}

/// Merges the integration branch into the card's worktree and leaves any conflict markers for
/// the next session there. Its commit at that session's end finishes the merge, and the
/// integration head it brought in becomes the card's base.
pub(crate) async fn bring_in_integration(host: &Host, card: &CardId) -> EngineResult<StagedMerge> {
    let tree = worktree::ensure(host, card).await?;
    let git = worktree::tree_git(host, &tree.path).await?;
    git.abort_merge(&tree.path).await?;
    let integration = integration_branch(host.run_id());
    Ok(git.merge_staged(&tree.path, &integration, true).await?)
}

/// The merge commit's message: it names the card, for idempotency, and carries the conductor
/// trailer.
fn merge_message(host: &Host, record: &CardRecord) -> String {
    let card = &record.spec.id;
    let subject = format!("Merge {card}: {}", record.spec.title);
    format!(
        "{}\n{}",
        conductor::message(host.run_id(), &subject),
        idempotency::card_line(card)
    )
}

fn note_merged(host: &Host, card: &CardId, sha: &str) -> EngineResult<()> {
    let known = host.read(|state| {
        state
            .integration
            .merged
            .iter()
            .any(|(c, s)| c == card && s == sha)
    });
    if !known {
        let merged = IntegrationEvent::Merged {
            card: card.clone(),
            sha: sha.to_owned(),
        };
        host.record(Some(card), None, RunEventKind::Integration(merged))?;
    }
    Ok(())
}

/// Runs `gates.run` on the integration worktree with the merge staged. The failures' output
/// when any fails. Acceptance tests whose build card isn't in yet are expected to fail, the
/// merging test card's own included, so only the rest of the suite must pass.
async fn affected_tests(
    host: &Host,
    card: &CardId,
    integration: &Path,
) -> EngineResult<Option<String>> {
    if let Some(output) = gates::setup(host, card, integration, "integration").await? {
        return Ok(Some(output));
    }
    let expected = gates::expected_failures(host, card, host.git(), integration).await?;
    let mut failed = Vec::new();
    for command in &host.settings().gates.run {
        let mut request = gates::request(host, card, command, integration, false)?;
        request.name = format!("affected: {}", command.name);
        let outcome = host.services().gates.run(request).await;
        let mut outcome = gates::judge(outcome, expected.get(&command.name));
        outcome.kind = GateKind::AffectedTests;
        host.record(Some(card), None, RunEventKind::Gate(outcome.clone()))?;
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
    /// The two sides disagree on behavior, which the phase lead decides.
    Semantic,
    /// The resolver's session ended for a vendor reason, even when asked again, so another
    /// agent takes the conflict.
    Unavailable(EndReason),
    Failed,
}

/// The conflict goes to the interface's owner (or the integration chore's), and a semantic one
/// on to the phase lead. A resolver whose vendor fails hands the conflict to the next, and after
/// the last to a stand-in by the owner role's handoff list. True when the card's
/// branch now holds the integration branch cleanly.
async fn resolve(host: &Host, card: &CardId, files: &[String]) -> EngineResult<bool> {
    let assignee = conflict_owner(host, card);
    let conflict = IntegrationEvent::Conflict {
        card: card.clone(),
        files: files.to_vec(),
        assigned_to: assignee.clone(),
    };
    host.record(Some(card), None, RunEventKind::Integration(conflict))?;
    let mut resolvers = vec![assignee];
    let lead = host.read(|state| lead::phase_lead(state, host.settings()));
    if let Some(lead) = lead.filter(|lead| !resolvers.contains(lead)) {
        resolvers.push(lead);
    }
    let mut unavailable: Vec<WorkerRef> = Vec::new();
    let mut next = 0;
    while let Some(worker) = resolvers.get(next).cloned() {
        next += 1;
        let reason = match resolve_with(host, card, &worker, files).await? {
            Resolution::Resolved => return Ok(true),
            Resolution::Semantic => continue,
            Resolution::Failed => return Ok(false),
            Resolution::Unavailable(reason) => reason,
        };
        unavailable.push(worker.clone());
        if next < resolvers.len() {
            continue;
        }
        let seat = Seat {
            at: None,
            role: RoleName::Owner,
            step: Some(StepName::ResolveConflict),
            card: Some(card.clone()),
            independent_of: Vec::new(),
            isolated: false,
        };
        let failed: Vec<&WorkerRef> = unavailable.iter().collect();
        match stand_in::after(host, &seat, &worker, &failed, &reason).await {
            Ok(taker) => resolvers.push(taker),
            Err(EngineError::Stalled(stall)) => {
                let line = format!("{} The card's owner resolves it instead.", stall.line);
                crate::plan::context::warn(host, Some(card), line)?;
                return Ok(false);
            }
            Err(err) => return Err(err),
        }
    }
    Ok(false)
}

/// The owner of the interface card, else of the integration chore, else the phase lead, else
/// the card's own owner.
fn conflict_owner(host: &Host, card: &CardId) -> WorkerRef {
    host.read(|state| {
        let owner_of_kind = |wanted: fn(&CardKind) -> bool| {
            state
                .cards
                .by_id
                .values()
                .find(|record| wanted(&record.spec.kind))
                .map(|record| owner_of(host, record))
        };
        owner_of_kind(|kind| *kind == CardKind::Interface)
            .or_else(|| owner_of_kind(|kind| *kind == CardKind::Chore))
            .or_else(|| lead::phase_lead(state, host.settings()))
            .or_else(|| state.cards.get(card).map(|record| owner_of(host, record)))
            .unwrap_or_else(|| host.settings().default_agent.clone())
    })
}

async fn resolve_with(
    host: &Host,
    card: &CardId,
    worker: &WorkerRef,
    files: &[String],
) -> EngineResult<Resolution> {
    let tree = worktree::ensure(host, card).await?;
    let git = worktree::tree_git(host, &tree.path).await?;
    let record = record_of(host, card)?;
    let mut plan = SessionPlan {
        at: None,
        role: RoleName::Owner,
        worker: worker.clone(),
        card: Some(card.clone()),
        access: Access::Build,
        step: Some(StepName::ResolveConflict),
        workdir: tree.path.clone(),
        parts: resolver_parts(&record, files),
        json_schema: Some(output_schema::<ResolveConflict>()),
        resume: None,
        slot: 0,
    };
    plan.slot = host.slot_for(&plan);
    // A resolver that a pause or a crash cut off picks up its own half-done merge.
    let cut_off = host.read(|state| {
        state
            .sessions
            .by_key
            .values()
            .filter(|session| {
                session.card.as_ref() == Some(card)
                    && session.started.step == Some(StepName::ResolveConflict)
                    && session.started.worker == *worker
            })
            .max_by_key(|session| session.since)
            .is_some_and(|session| session.resumable())
    });
    if !(cut_off && git.merge_in_progress(&tree.path).await?) {
        match bring_in_integration(host, card).await? {
            StagedMerge::UpToDate => return Ok(Resolution::Resolved),
            StagedMerge::Staged => {
                worktree::commit_in(host, card, &tree, None, None).await?;
                return Ok(Resolution::Resolved);
            }
            StagedMerge::Conflict(_) => {}
        }
    }
    // A vendor error has the resolver go again once on its own session.
    let Ran {
        key,
        worker: by,
        end,
    } = host.run_session(plan, StandIns::None).await?;
    if end.reason.is_vendor_failure() {
        git.abort_merge(&tree.path).await?;
        return Ok(Resolution::Unavailable(end.reason));
    }
    let report = report_of(&end);
    let status = report.as_ref().map(|r| r.status);
    if status == Some(ReportStatus::Done) && !has_markers(&tree, files) {
        worktree::commit_in(host, card, &tree, Some(&key), Some(by)).await?;
        if resolution_is_clean(host, card, &tree).await? {
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
/// integration branch. False while the merge is still open or when a check failed.
async fn resolution_is_clean(host: &Host, card: &CardId, tree: &CardTree) -> EngineResult<bool> {
    let git = worktree::tree_git(host, &tree.path).await?;
    if git.merge_in_progress(&tree.path).await? {
        return Ok(false);
    }
    let checks = gates::builtin_checks(host, card, tree, "HEAD^1", "HEAD").await?;
    Ok(matches!(checks, BuiltinChecks::Done(failed) if failed.is_empty()))
}

fn resolver_parts(
    record: &CardRecord,
    files: &[String],
) -> Vec<argonauts_core::events::session::InputPart> {
    let list = files
        .iter()
        .map(|file| format!("- {file}"))
        .collect::<Vec<_>>()
        .join("\n");
    vec![
        part(
            InputPartKind::Prompt,
            "resolver",
            prompts::card::RESOLVE_CONFLICT,
            None,
        ),
        part(
            InputPartKind::Card,
            record.spec.id.as_str(),
            prompt::card_text(&record.spec),
            None,
        ),
        part(
            InputPartKind::Message,
            "conflicts",
            format!("Files with conflicts:\n{list}"),
            None,
        ),
        part(InputPartKind::Schema, "report", prompts::card::BUILD, None),
    ]
}

/// Whether any of `files` in the tree still holds a conflict marker line.
fn has_markers(tree: &CardTree, files: &[String]) -> bool {
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
        let tree = CardTree {
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
