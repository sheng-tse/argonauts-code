//! `/rewind` to `handoff-<n>`: resets the card's branch to that tag after a confirm card,
//! keeping the work after it on `argonauts/<run>/<card>-replaced-<n>`. A card at rest goes back
//! in line. A card already in the run's branch has that work undone there too, by one commit,
//! and stays integrated as it was at the tag; the merge card opens again on the new branch.
//!
//! Refused once the run's work has landed, gone out as a pull request, or been abandoned, since
//! a new change undoes it then, and for a card that isn't at rest, whose gates, review, merge,
//! or your own edits would lose the branch under them.

use argonauts_core::CardId;
use argonauts_core::events::RunEventKind;
use argonauts_core::events::card::{CardEvent, CardState};
use argonauts_core::events::checkpoint::CheckpointKind;
use argonauts_core::handoff::CheckpointTag;
use argonauts_core::names::CheckpointId;

use crate::checkpoints;
use crate::engine::host::Host;
use crate::error::{EngineError, EngineResult};
use crate::handoff::rules;
use crate::integrate::{branch, conductor, merge};
use crate::state::RunState;
use crate::state::cards::CardRecord;

/// Called once you confirmed. Records `Card::Reverted`.
pub async fn revert(host: &Host, card: &CardId, checkpoint: u32) -> Result<(), String> {
    revert_card(host, card, checkpoint)
        .await
        .map_err(|err| err.to_string())
}

async fn revert_card(host: &Host, card: &CardId, checkpoint: u32) -> EngineResult<()> {
    let record = rules::record(host, card)?;
    if let Some(why) = host.read(|state| refuse(state, &record)) {
        return Err(EngineError::Refused(why));
    }
    if checkpoint == 0 || checkpoint > record.handoffs {
        return Err(EngineError::Refused(format!(
            "{card} has no checkpoint handoff-{checkpoint}"
        )));
    }
    let kept_on = reset_to(host, &record, checkpoint).await?;
    host.record(
        Some(card),
        None,
        RunEventKind::Card(CardEvent::Reverted {
            to: checkpoint,
            kept_on,
        }),
    )?;
    let reason = format!("you reverted {card} to handoff-{checkpoint}");
    withdraw_stale(host, card, &reason)?;
    rules::back_in_line(host, card, &reason)
}

/// Why the card can't be reverted now, if it can't.
fn refuse(state: &RunState, record: &CardRecord) -> Option<String> {
    let card = &record.spec.id;
    if state.integration.is_merged(card) && merge::ended(&state.integration).is_some() {
        return Some(format!(
            "{card}'s work is merged, so it can't be reverted. Undo it with a new change."
        ));
    }
    if record.state == CardState::Integrated {
        if rules::session_running(state, card) {
            return Some(format!(
                "{card} is being reviewed. Revert it once the review is done."
            ));
        }
        return branch_busy(state, card);
    }
    rules::refuse_reset(state, card)
}

/// Why the run's branch can't take an undo of `card`'s work now, if it can't: something is
/// being merged into it, or a started card builds on that work.
fn branch_busy(state: &RunState, card: &CardId) -> Option<String> {
    if let Some(merging) = state
        .cards
        .by_id
        .values()
        .find(|record| record.state == CardState::Integrating)
    {
        return Some(format!(
            "{} is being merged into the run's branch. Revert {card} once it's in.",
            merging.spec.id
        ));
    }
    if state.integration.main_moving.is_some() {
        return Some(format!(
            "Main is being merged into the run's branch. Revert {card} once it's in."
        ));
    }
    let builds_on = state.cards.by_id.values().find(|record| {
        record.spec.depends_on.contains(card)
            && !matches!(
                record.state,
                CardState::Waiting | CardState::Ready | CardState::Cancelled
            )
    })?;
    Some(format!(
        "{} builds on {card}'s work, so {card} can't be reverted on its own.",
        builds_on.spec.id
    ))
}

/// The branch moved under the merge card, and the card's debt may have gone with the work it
/// was for, so what was open over them opens again with what's true now.
fn withdraw_stale(host: &Host, card: &CardId, reason: &str) -> EngineResult<()> {
    let stale: Vec<CheckpointId> = host.read(|state| {
        let integrated = state
            .cards
            .get(card)
            .is_some_and(|record| record.state == CardState::Integrated);
        let debt_gone = state
            .cards
            .get(card)
            .is_some_and(|record| record.debt.is_none());
        state
            .checkpoints
            .open()
            .filter(|record| match record.opened.kind {
                CheckpointKind::Merge => integrated,
                CheckpointKind::OriginalReturned => {
                    debt_gone && record.opened.card.as_ref() == Some(card)
                }
                _ => false,
            })
            .map(|record| record.opened.id.clone())
            .collect()
    });
    for id in stale {
        checkpoints::withdraw(host, &id, reason)?;
    }
    Ok(())
}

/// Resets the card's branch to `handoff-<n>` (its base for 0), keeping the work after it on
/// the replaced branch. When the card's work is in the integration branch, the work after the
/// tag is undone there too, or nothing changes. Returns the replaced branch.
pub async fn reset_to(host: &Host, record: &CardRecord, n: u32) -> EngineResult<String> {
    let card = &record.spec.id;
    let git = host.git();
    let root = host.repo().root.clone();
    let tag = CheckpointTag {
        run: host.run_id().clone(),
        card: card.clone(),
        n,
    };
    let target = if n == 0 {
        record.base.clone()
    } else {
        git.tag_target(&root, &tag.tag_name()).await?
    };
    let target =
        target.ok_or_else(|| EngineError::Refused(format!("{card} has no {}", tag.label())))?;
    let branch = record
        .branch
        .clone()
        .ok_or_else(|| EngineError::Refused(format!("{card} has no branch yet")))?;
    let head = git.resolve(&root, &branch).await?;
    let kept_on = tag.replaced_branch();
    if head != target {
        keep(host, &kept_on, &head).await?;
    }
    move_branch(host, record, &branch, &target, &head).await?;
    if host.read(|state| state.integration.is_merged(card)) {
        let undone = undo_merged(host, card, &tag.label(), &target, &head).await;
        if let Err(err) = undone {
            move_branch(host, record, &branch, &head, &target).await?;
            return Err(err);
        }
    }
    Ok(kept_on)
}

/// Points the card's branch at `to`, through its worktree when it has one.
async fn move_branch(
    host: &Host,
    record: &CardRecord,
    branch: &str,
    to: &str,
    from: &str,
) -> EngineResult<()> {
    let git = host.git();
    match &record.worktree {
        Some(path) => git.reset_hard(path, to).await?,
        None => {
            git.update_branch(&host.repo().root, branch, to, from)
                .await?
        }
    }
    Ok(())
}

/// Undoes, on the integration branch, the card's commits after `target` that were merged
/// there, with one conductor commit. Refused with nothing changed when that work came in
/// through a merge, whose other side the undo can't tell apart, or when later work on the
/// branch conflicts with undoing it.
async fn undo_merged(
    host: &Host,
    card: &CardId,
    label: &str,
    target: &str,
    head: &str,
) -> EngineResult<()> {
    let git = host.git();
    let worktree = branch::ensure_integration(host).await?;
    let _held = conductor::hold(&worktree).await;
    let integrated = git.resolve(&worktree, "HEAD").await?;
    // The newest of the card's commits the branch holds.
    let Some(merged) = git.merge_base(&worktree, head, &integrated).await? else {
        return Ok(());
    };
    if git.is_ancestor(&worktree, &merged, target).await? {
        return Ok(());
    }
    let refused = |why: String| {
        Err(EngineError::Refused(format!(
            "{why}, so nothing was reverted. Undo it with a new change."
        )))
    };
    if !git.tracked_changes(&worktree).await?.is_empty() {
        return refused("The run's branch has uncommitted changes".to_owned());
    }
    let commits = git
        .log_messages(&worktree, &format!("{target}..{merged}"))
        .await?;
    let mut undo = Vec::new();
    for (sha, _) in commits {
        if git.parents(&worktree, &sha).await?.len() > 1 {
            return refused(format!(
                "{card}'s work after {label} came in through a merge"
            ));
        }
        undo.push(sha);
    }
    let conflicts = git.revert_staged(&worktree, &undo).await?;
    if !conflicts.is_empty() {
        return refused(format!(
            "Undoing {card}'s work after {label} conflicts with later work on the run's branch \
             in {}",
            conflicts.join(", ")
        ));
    }
    let message = conductor::message(host.run_id(), &format!("Revert {card} to {label}"));
    git.commit_index(&worktree, &message).await?;
    Ok(())
}

/// Creates `branch` at `head`. One that already holds exactly this work is fine; one that
/// holds other work is never overwritten.
async fn keep(host: &Host, branch: &str, head: &str) -> EngineResult<()> {
    let git = host.git();
    let root = host.repo().root.clone();
    // An empty old value makes git create the branch only if it doesn't exist.
    if git.update_branch(&root, branch, head, "").await.is_ok() {
        return Ok(());
    }
    if git.resolve(&root, branch).await? == head {
        return Ok(());
    }
    Err(EngineError::Refused(format!(
        "{branch} already holds other replaced work, so nothing was reset"
    )))
}
