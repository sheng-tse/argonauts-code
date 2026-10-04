//! When the original vendor is healthy again or its quota resets, `on_return` decides for each
//! card with debt: `ask` (the "original is back" card), `keep`, `review` (the original reviews
//! everything since the checkpoint, then approves it or sends it back, clearing the debt),
//! `hand_back`, or `redo` (reset to the checkpoint, the replacement's work kept on a side
//! branch). If a step is running, the choice applies after it.
//!
//! A card at rest takes a new owner or a reset branch at once and goes back in line. Between
//! its session and integration it goes back to `Running`, so its gates and review never run on
//! work its new owner hasn't done. The review waits for the card's own review step, after its
//! gates pass, and runs once: its verdict clears the debt or escalates the card, and a review
//! that couldn't happen leaves the debt to the merge rule.
//!
//! Nothing here is held in memory: what to do is worked out again from the log and the health
//! board at every safe point, so a choice survives a crash, and applying it twice is harmless.

use argonauts_core::events::RunEventKind;
use argonauts_core::events::card::{CardEvent, CardState};
use argonauts_core::events::checkpoint::{
    AnswerAction, CheckpointKind, CheckpointPacket, FreeText, OriginalReturnedPacket,
};
use argonauts_core::events::run::Party;
use argonauts_core::handoff::{CheckpointTag, QualityDebt};
use argonauts_core::names::{VendorId, WorkerRef};
use argonauts_core::policy::EvidenceFacts;
use argonauts_core::settings::handoff::OnReturn;
use argonauts_core::{CardId, DiffStat, Note, NoteLevel};

use crate::card::machine::Trigger;
use crate::card::{pipeline, takeover};
use crate::checkpoints::options;
use crate::checkpoints::{self, CheckpointRequest};
use crate::engine::host::Host;
use crate::error::{EngineError, EngineResult};
use crate::handoff::{assign, revert, rules, upgrade};
use crate::review::Verdict;
use crate::state::RunState;
use crate::state::cards::CardRecord;
use crate::workflow::StepAt;

/// A card whose original owner can work again while someone else holds it.
#[derive(Debug, Clone, PartialEq)]
pub struct Returning {
    pub original: WorkerRef,
    pub replacement: WorkerRef,
    pub debt: Option<QualityDebt>,
    pub rule: OnReturn,
}

/// What happens to the card, once decided.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Choice {
    Keep,
    Review,
    HandBack,
    Redo,
    TakeOver,
}

/// What applying a choice did, for the card's pipeline to go on from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Applied {
    Nothing,
    /// The card moved on its own: you took it over, or the original's review escalated it.
    Moved,
    /// The owner or the branch changed: the next session is a fresh one for the original.
    NewOwner {
        why: String,
    },
    /// The original's review asked for changes, and the card is back in `Running`.
    Changes {
        round: u32,
        comments: Vec<String>,
    },
}

impl Choice {
    pub fn from_answer(action: &AnswerAction) -> Option<Self> {
        match action {
            AnswerAction::UpgradeReview => Some(Self::Review),
            AnswerAction::HandBack => Some(Self::HandBack),
            AnswerAction::Redo => Some(Self::Redo),
            AnswerAction::KeepReplacement => Some(Self::Keep),
            AnswerAction::TakeOver => Some(Self::TakeOver),
            _ => None,
        }
    }

    /// The setting's choice, `None` for `ask` (the card decides) and for a card without debt
    /// under any rule but `hand_back`.
    pub fn from_rule(rule: OnReturn, has_debt: bool) -> Option<Self> {
        match (rule, has_debt) {
            (OnReturn::HandBack, _) => Some(Self::HandBack),
            (_, false) | (OnReturn::Ask, true) => None,
            (OnReturn::Keep, true) => Some(Self::Keep),
            (OnReturn::Review, true) => Some(Self::Review),
            (OnReturn::Redo, true) => Some(Self::Redo),
        }
    }
}

pub async fn vendor_back(host: &Host, vendor: &VendorId) -> EngineResult<()> {
    reopen_merge(host, vendor)?;
    let cards: Vec<CardRecord> = host.read(|state| state.cards.by_id.values().cloned().collect());
    for record in cards {
        let Some(back) = returning(host, &record) else {
            continue;
        };
        if back.original.vendor() != *vendor {
            continue;
        }
        let card = record.spec.id.clone();
        match (&back.debt, back.rule) {
            (None, OnReturn::HandBack) => {}
            (None, _) => {
                notice(host, &card, &back)?;
                continue;
            }
            (Some(debt), OnReturn::Ask) => {
                ask_in_background(host, &card, debt.since);
                continue;
            }
            _ => {}
        }
        if idle(host, &card) {
            apply_pending(host, &card).await?;
        }
    }
    Ok(())
}

/// A merge card whose debt waits on `vendor` offered to wait for it, which is moot now. It's
/// withdrawn, so the merge step decides again with the original back.
fn reopen_merge(host: &Host, vendor: &VendorId) -> EngineResult<()> {
    let waiting = host.read(|state| {
        let open = state.checkpoints.open_for(CheckpointKind::Merge, None)?;
        let CheckpointPacket::Merge(packet) = &open.opened.packet else {
            return None;
        };
        let waits = packet
            .rows
            .iter()
            .flat_map(|row| &row.debt)
            .any(|debt| debt.needs.vendor() == *vendor);
        waits.then(|| open.opened.id.clone())
    });
    match waiting {
        Some(id) => checkpoints::withdraw(host, &id, &format!("{} is back", vendor_label(vendor))),
        None => Ok(()),
    }
}

/// Called by the card pipeline at each safe point between steps, and for a card at rest when
/// the choice is made: applies a return choice decided for this card while a step ran.
pub async fn apply_pending(host: &Host, card: &CardId) -> EngineResult<Applied> {
    let record = rules::record(host, card)?;
    let Some(back) = returning(host, &record) else {
        return Ok(Applied::Nothing);
    };
    let (choice, by) = match (
        &back.debt,
        Choice::from_rule(back.rule, back.debt.is_some()),
    ) {
        (Some(debt), None) if back.rule == OnReturn::Ask => {
            match host.read(|state| answered(state, card, debt.since)) {
                Some(answer) => answer,
                None => return Ok(Applied::Nothing),
            }
        }
        (_, Some(choice)) => (choice, Party::Rules),
        (_, None) => return Ok(Applied::Nothing),
    };
    act(host, &record, &back, choice, by).await
}

/// The original of a card someone else holds, when it can work again. Cards already in the
/// integration branch are left to the merge checkpoint, in `merge_return`.
pub fn returning(host: &Host, record: &CardRecord) -> Option<Returning> {
    if matches!(
        record.state,
        CardState::Integrating
            | CardState::Integrated
            | CardState::Cancelled
            | CardState::TakenOver
    ) {
        return None;
    }
    let replacement = rules::owner(record)?;
    // A card planned without an owner, such as the solo card, is the default agent's.
    let original = record
        .debt
        .as_ref()
        .map(|debt| debt.needs.clone())
        .or_else(|| record.spec.owner.clone())
        .unwrap_or_else(|| host.settings().default_agent.clone());
    if original.vendor() == replacement.vendor() || !rules::vendor_usable(host, &original.vendor())
    {
        return None;
    }
    Some(Returning {
        rule: rules::applied_for(host, &record.spec, &original).on_return,
        original,
        replacement,
        debt: record.debt.clone(),
    })
}

/// Your answer on this card's "original is back" card for the debt that starts at `since`.
pub fn answered(state: &RunState, card: &CardId, since: u32) -> Option<(Choice, Party)> {
    let record = state
        .checkpoints
        .last_decided(CheckpointKind::OriginalReturned, Some(card))?;
    let CheckpointPacket::OriginalReturned(packet) = &record.opened.packet else {
        return None;
    };
    let decided = record.decided.as_ref()?;
    if packet.since != since {
        return None;
    }
    Some((Choice::from_answer(&decided.action)?, decided.by.clone()))
}

fn idle(host: &Host, card: &CardId) -> bool {
    host.read(|state| {
        state
            .cards
            .get(card)
            .is_some_and(|record| rules::at_rest(record.state))
            && !rules::session_running(state, card)
    })
}

async fn act(
    host: &Host,
    record: &CardRecord,
    back: &Returning,
    choice: Choice,
    by: Party,
) -> EngineResult<Applied> {
    let card = &record.spec.id;
    match choice {
        Choice::Keep => Ok(Applied::Nothing),
        Choice::Review => review(host, record, back).await,
        Choice::HandBack => {
            let since = back
                .debt
                .as_ref()
                .map_or(record.handoffs, |debt| debt.since);
            let why = hand_back_why(back, since);
            give_back(host, record, back, &why, by).await?;
            Ok(Applied::NewOwner { why })
        }
        Choice::Redo => {
            let since = back.debt.as_ref().map_or(0, |debt| debt.since);
            let kept_on = revert::reset_to(host, record, since).await?;
            host.record(
                Some(card),
                None,
                RunEventKind::Card(CardEvent::Reverted { to: since, kept_on }),
            )?;
            // Folding `Reverted` clears the debt: the work it was for is off the branch.
            let why = redo_why(back, since);
            let record = rules::record(host, card)?;
            give_back(host, &record, back, &why, by).await?;
            Ok(Applied::NewOwner { why })
        }
        // Only a running or escalated card can be taken over; anywhere else the choice waits
        // for the card to get there.
        Choice::TakeOver if matches!(record.state, CardState::Running | CardState::Escalated) => {
            takeover::take_over(host, card)
                .await
                .map_err(EngineError::Refused)?;
            Ok(Applied::Moved)
        }
        Choice::TakeOver => Ok(Applied::Nothing),
    }
}

/// The original reviews the card once its gates passed, unless its review is already the
/// newest. After an approval the card's own review still runs; changes go back to the owner,
/// and a rejection escalates the card to you.
async fn review(host: &Host, record: &CardRecord, back: &Returning) -> EngineResult<Applied> {
    let card = &record.spec.id;
    if record.state != CardState::InReview || upgrade::reviewed_last(record, &back.original) {
        return Ok(Applied::Nothing);
    }
    let round = upgrade::next_round(&record.reviews);
    let (verdict, _) = upgrade::review(host, card, &back.original).await?;
    let reviewed = host.read(|state| {
        state
            .cards
            .get(card)
            .is_some_and(|record| upgrade::reviewed_last(record, &back.original))
    });
    let change_rounds = host.read(|state| state.cards.progress(card).change_rounds);
    match verdict {
        Verdict::Approved => Ok(Applied::Nothing),
        Verdict::ChangesRequested { comments }
            if change_rounds < host.settings().review.max_change_rounds =>
        {
            let reason = format!("{}'s review asked for changes", back.original);
            pipeline::transition(host, card, Trigger::ChangesRequested, &reason)?;
            Ok(Applied::Changes { round, comments })
        }
        Verdict::ChangesRequested { .. } => {
            let reason = format!(
                "{}'s review asked for changes again after {change_rounds} rounds",
                back.original
            );
            pipeline::transition(host, card, Trigger::ChangesRequestedAgain, &reason)?;
            Ok(Applied::Moved)
        }
        // The review didn't happen: the card's own review goes on, and the debt stays for
        // the merge rule.
        Verdict::Escalated { .. } if !reviewed => Ok(Applied::Nothing),
        Verdict::Escalated { reasons } => {
            let reason = format!(
                "{} rejected the work: {}",
                back.original,
                reasons.join("; ")
            );
            pipeline::transition(host, card, Trigger::ChangesRequestedAgain, &reason)?;
            Ok(Applied::Moved)
        }
    }
}

pub(crate) fn hand_back_why(back: &Returning, since: u32) -> String {
    format!(
        "{} is back and continues the card from its current state. {} worked on it since \
         handoff-{since}.",
        back.original, back.replacement
    )
}

pub(crate) fn redo_why(back: &Returning, since: u32) -> String {
    format!(
        "{} is back and redoes the card from handoff-{since}. The work {} did after it is \
         kept on a side branch.",
        back.original, back.replacement
    )
}

async fn give_back(
    host: &Host,
    record: &CardRecord,
    back: &Returning,
    why: &str,
    by: Party,
) -> EngineResult<()> {
    reassign(host, record, back, why, by).await?;
    rules::back_in_line(host, &record.spec.id, &format!("{} is back", back.original))
}

/// Gives the card to its original, with a packet that says why.
pub(crate) async fn reassign(
    host: &Host,
    record: &CardRecord,
    back: &Returning,
    why: &str,
    by: Party,
) -> EngineResult<()> {
    let card = &record.spec.id;
    let mut packet = host.services().handoff.packet(&record.spec, &[]);
    packet.why_stopped = why.to_owned();
    assign::write_packet(host, card, &packet).await?;
    host.record(
        Some(card),
        None,
        RunEventKind::Card(CardEvent::Reassigned {
            from: Some(back.replacement.clone()),
            to: back.original.clone(),
            by,
            rule_override: None,
        }),
    )?;
    Ok(())
}

fn notice(host: &Host, card: &CardId, back: &Returning) -> EngineResult<()> {
    let text = format!(
        "{} is back · {card} stays with {} · /reassign {card} {} moves it back",
        vendor_label(&back.original.vendor()),
        back.replacement,
        back.original
    );
    host.record(
        Some(card),
        None,
        RunEventKind::Note(Note {
            level: NoteLevel::Info,
            text,
        }),
    )?;
    Ok(())
}

/// How a vendor is named in a sentence, the way the screen names it elsewhere. Unknown ids stay
/// as they are.
pub fn vendor_label(vendor: &VendorId) -> String {
    match vendor.0.as_str() {
        "claude-code" | "claude" => "Claude Code".to_owned(),
        "codex" => "Codex".to_owned(),
        "opencode" => "OpenCode".to_owned(),
        "openrouter" => "OpenRouter".to_owned(),
        "ollama" => "Ollama".to_owned(),
        other => other.to_owned(),
    }
}

/// Opens the "original is back" card without holding up the caller, which is the engine's
/// loop. Your answer applies at once when the card is at rest, else at its next safe point.
fn ask_in_background(host: &Host, card: &CardId, since: u32) {
    let already = host.read(|state| {
        state.checkpoints.by_id.values().any(|record| {
            record.withdrawn.is_none()
                && record.opened.card.as_ref() == Some(card)
                && matches!(
                    &record.opened.packet,
                    CheckpointPacket::OriginalReturned(packet) if packet.since == since
                )
        })
    });
    if already {
        return;
    }
    let host = host.clone();
    let card = card.clone();
    tokio::spawn(async move {
        let result = tokio::select! {
            () = host.interrupted().cancelled() => Ok(()),
            result = ask(&host, &card) => result,
        };
        if let Err(err) = result {
            let text = format!("The \"original is back\" card for {card} failed: {err}");
            let note = RunEventKind::Note(Note {
                level: NoteLevel::Error,
                text,
            });
            if let Err(err) = host.record(Some(&card), None, note) {
                tracing::error!(%card, %err, "could not record a failed return card");
            }
        }
    });
}

async fn ask(host: &Host, card: &CardId) -> EngineResult<()> {
    let record = rules::record(host, card)?;
    let Some(back) = returning(host, &record) else {
        return Ok(());
    };
    let Some(debt) = back.debt.clone() else {
        return Ok(());
    };
    let at = host.read(rules::current_step);
    let request = request(host, &record, &back, &debt, at).await?;
    // Withdrawn, the debt went with the work it was for, or the merge checkpoint asks instead.
    if checkpoints::open(host, request)?
        .decided_or_withdrawn(host)
        .await?
        .is_err()
    {
        return Ok(());
    }
    if idle(host, card) {
        apply_pending(host, card).await?;
    }
    Ok(())
}

/// The "original is back" card for the debt.
pub(crate) async fn request(
    host: &Host,
    record: &CardRecord,
    back: &Returning,
    debt: &QualityDebt,
    at: Option<StepAt>,
) -> EngineResult<CheckpointRequest> {
    let card = &record.spec.id;
    let packet = packet(host, record, back, debt).await?;
    let summary = format!(
        "{} commits by {} since handoff-{}, {} files changed",
        packet.commits, back.replacement, debt.since, packet.changed.files_changed
    );
    Ok(CheckpointRequest {
        at,
        kind: CheckpointKind::OriginalReturned,
        title: format!(
            "{} is back · {}",
            vendor_label(&back.original.vendor()),
            host.read(|state| crate::labels::card_label(state, card))
        ),
        card: Some(card.clone()),
        packet: CheckpointPacket::OriginalReturned(packet),
        facts: EvidenceFacts::default(),
        agent_outputs: vec![summary],
        documents: Vec::new(),
        options: Some(options::original_returned(
            &back.original,
            &back.replacement,
        )),
        free_text: FreeText::None,
        authors: vec![back.replacement.clone()],
    })
}

/// The card's work since the checkpoint the debt starts at.
async fn packet(
    host: &Host,
    record: &CardRecord,
    back: &Returning,
    debt: &QualityDebt,
) -> EngineResult<OriginalReturnedPacket> {
    let card = &record.spec.id;
    let git = host.git();
    let root = host.repo().root.clone();
    let since_commit = if debt.since == 0 {
        record.base.clone()
    } else {
        let tag = CheckpointTag {
            run: host.run_id().clone(),
            card: card.clone(),
            n: debt.since,
        };
        git.tag_target(&root, &tag.tag_name()).await?
    };
    let (commits, changed) = match (since_commit, &record.branch) {
        (Some(from), Some(branch)) => {
            let head = git.resolve(&root, branch).await?;
            let commits = git.log_messages(&root, &format!("{from}..{head}")).await?;
            let changed = git.diff_stat(&root, &from, &head).await?;
            (u32::try_from(commits.len()).unwrap_or(u32::MAX), changed)
        }
        _ => (0, DiffStat::default()),
    };
    Ok(OriginalReturnedPacket {
        card: card.clone(),
        title: record.spec.title.clone(),
        original: back.original.clone(),
        replacement: back.replacement.clone(),
        since: debt.since,
        commits,
        changed,
        tests_passed: record.gates.last().map(|gate| gate.passed),
        debt: vec![debt.clone()],
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rules_and_answers_map_to_choices() {
        assert_eq!(
            Choice::from_rule(OnReturn::HandBack, false),
            Some(Choice::HandBack)
        );
        assert_eq!(Choice::from_rule(OnReturn::Review, false), None);
        assert_eq!(Choice::from_rule(OnReturn::Ask, true), None);
        assert_eq!(Choice::from_rule(OnReturn::Keep, true), Some(Choice::Keep));
        assert_eq!(Choice::from_rule(OnReturn::Redo, true), Some(Choice::Redo));
        assert_eq!(
            Choice::from_answer(&AnswerAction::UpgradeReview),
            Some(Choice::Review)
        );
        assert_eq!(
            Choice::from_answer(&AnswerAction::KeepReplacement),
            Some(Choice::Keep)
        );
        assert_eq!(Choice::from_answer(&AnswerAction::ShowDiff), None);
    }
}
