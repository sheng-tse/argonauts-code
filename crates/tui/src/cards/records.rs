//! One line in scrollback after a card is answered, handed on, or withdrawn, such as
//! `◆ Approved merging build:redis into main · you, 10:58 AM`.

use argonauts_core::app::NOT_CHANGED;
use argonauts_core::display_names::DisplayNames;
use argonauts_core::events::checkpoint::{
    AnswerAction, CheckpointDecided, CheckpointKind, CheckpointOpened, CheckpointPacket,
};
use argonauts_core::events::run::Party;
use argonauts_core::{CardId, RunId};
use ratatui::text::{Line, Span};
use time::{OffsetDateTime, UtcOffset};

use super::words;
use crate::cell::{Cell, HistoryCell, Notice, Tone};
use crate::render::RenderCtx;
use crate::text::{fit_lines, sanitize_line, str_width, wrap_spans};
use crate::theme::Role;

/// The record with agents by the names their ids give and cards by their ids, for callers that
/// don't know the app's names or the run.
pub fn record_cell(opened: &CheckpointOpened, decided: &CheckpointDecided) -> Cell {
    let names = DisplayNames::default();
    Cell::Notice(record(
        opened,
        decided,
        None,
        None,
        UtcOffset::UTC,
        &names,
        &by_id,
    ))
}

/// A card by its id, for a record made where its run isn't on screen to give its label.
pub fn by_id(card: &CardId) -> String {
    card.to_string()
}

/// The record, naming the run for a merge without a card when `run` is given (lists that
/// span runs) or as "the run", each card as `cards` names it, and the time it was decided in
/// the terminal's zone.
pub fn record(
    opened: &CheckpointOpened,
    decided: &CheckpointDecided,
    run: Option<&RunId>,
    at: Option<OffsetDateTime>,
    offset: UtcOffset,
    names: &DisplayNames,
    cards: &dyn Fn(&CardId) -> String,
) -> Notice {
    let mut who = words::party(&decided.by, names);
    if let Some(at) = at {
        who.push_str(&format!(", {}", words::clock(at, offset)));
    }
    // Esc: "carry on without me". What the agent was told isn't your words, so it isn't quoted.
    if decided.by == Party::Declined {
        let left = match opened.kind {
            CheckpointKind::Question | CheckpointKind::Goal => {
                "Question left to the agent".to_owned()
            }
            _ => format!(
                "Left without you: {}",
                outcome(opened, decided, run, names, cards)
            ),
        };
        return Notice::answered(left, who);
    }
    let mut notice = Notice::answered(outcome(opened, decided, run, names, cards), who);
    let own_words = decided.action == AnswerAction::Answer;
    if let Some(text) = decided.text.as_deref().map(str::trim)
        && !text.is_empty()
        && !own_words
    {
        notice = notice.with_detail(format!("“{text}”"));
    }
    notice
}

/// What the answer did, in a few words: `Approved merging build:redis into main`, `Plan
/// approved`, each card as `cards` names it.
pub fn outcome(
    opened: &CheckpointOpened,
    decided: &CheckpointDecided,
    run: Option<&RunId>,
    names: &DisplayNames,
    cards: &dyn Fn(&CardId) -> String,
) -> String {
    use AnswerAction as A;
    use CheckpointKind as K;

    let subject = subject(opened, run, cards);
    let card = packet_card(opened)
        .map(cards)
        .unwrap_or_else(|| subject.clone());
    let title = opened.title.trim();
    let text = decided.text.as_deref().map(str::trim).unwrap_or_default();
    match (&decided.action, opened.kind) {
        // The decision, not the landing: the run's last line says when it merged.
        (A::Merge, _) => format!("Approved merging {subject} into {}", merge_target(opened)),
        (A::PullRequest, _) => format!("Opened a pull request for {subject}"),
        (A::RequestChanges, _) if decided.cards.is_empty() => {
            "Sent every task back with changes".to_owned()
        }
        (A::RequestChanges, _) => {
            let sent: Vec<String> = decided.cards.iter().map(cards).collect();
            format!("Sent {} back with changes", words::joined(&sent))
        }
        (A::KeepBranch, _) => format!("Kept {subject}'s work on its branch, not merged"),
        (A::Abandon, K::Goal) => "Abandoned the goal".to_owned(),
        (A::Abandon, _) => format!("Abandoned {subject}"),
        (A::Approve, K::Goal) => "Goal approved".to_owned(),
        (A::Approve, K::Plan) => "Plan approved".to_owned(),
        (A::Approve, K::GoalChange) => "Goal change accepted".to_owned(),
        (A::Approve, K::ChangeRequest) => format!("Change to {card} accepted"),
        (A::Approve, K::WorkflowPromotion) => "Workflow promoted".to_owned(),
        (A::Approve, _) => format!("{title} · approved"),
        (A::ApproveWithEdits, K::Goal) => "Goal approved with edits".to_owned(),
        (A::ApproveWithEdits, K::Plan) => "Plan approved with edits".to_owned(),
        (A::ApproveWithEdits, _) => format!("{title} · approved with edits"),
        (A::SendBack, K::Goal) => "Goal sent back for another round".to_owned(),
        (A::SendBack, K::Plan) => "Plan sent back".to_owned(),
        (A::SendBack, _) => format!("{title} · sent back"),
        (A::Stop, _) => format!("Stopped {subject}"),
        (A::Choose { value }, K::Goal) => format!("Goal question answered · {value}"),
        // A question is titled by the asking agent's label, never its card id.
        (A::Choose { value }, K::Question) => format!("{title} answered · {value}"),
        (A::Choose { value }, _) => format!("{title} · {value}"),
        (A::Answer, K::Goal) => format!("Goal question answered · {text}"),
        (A::Answer, _) => format!("{title} answered · {text}"),
        (A::Candidate { worker }, _) => format!("{card} handed to {}", names.worker(worker)),
        (A::OtherCandidates, _) => format!("Listing every agent for {card}"),
        (A::TakeOver, K::TakeOverReturn) => format!("You keep {card}"),
        (A::TakeOver, _) => format!("You took over {card}"),
        (A::Wait, K::Handoff) => format!("{card} waits for the reset"),
        (A::Wait, K::VendorDown) => format!("Waiting for {}", vendor(opened, names)),
        (A::Wait, K::Merge) => "The merge waits for a stronger review".to_owned(),
        (A::Wait, _) => format!("{title} · waiting"),
        (A::OneForAll, _) => "One agent takes every paused task".to_owned(),
        (A::PerTask, _) => "Choosing an agent per paused task".to_owned(),
        (A::MoreBudget, _) => format!("More limit for {card}"),
        (A::HandOff, _) => format!("{card} handed off"),
        (A::Escalate, _) => format!("Stopped and needs you {card}"),
        (A::Retry, _) => format!("Retrying {card}"),
        (A::Split, _) => format!("Splitting {card}"),
        (A::Cancel, K::GoalChange) => "Kept the goal".to_owned(),
        (A::Cancel, K::ChangeRequest) => format!("Change to {card} declined"),
        (A::Cancel, K::Escalation) => format!("Cancelled {card}"),
        (A::Cancel, _) => format!("{title} · declined"),
        (A::Defer, _) => format!("Deferred {card}"),
        (A::LetContinue, _) => match &opened.packet {
            CheckpointPacket::TakeOverReturn(back) => {
                format!("{} continues {card}", names.worker(&back.owner))
            }
            _ => format!("{card} continues"),
        },
        (A::SwitchToSolo, _) => "Running the plan solo".to_owned(),
        (A::ShowDiff, _) => title.to_owned(),
        (A::UpgradeReview, _) => match &opened.packet {
            CheckpointPacket::OriginalReturned(back) => {
                format!("{} reviews {card}", names.worker(&back.original))
            }
            _ => format!("{subject} waits for a stronger review"),
        },
        (A::HandBack, _) => match &opened.packet {
            CheckpointPacket::OriginalReturned(back) => {
                format!("{card} handed back to {}", names.worker(&back.original))
            }
            _ => format!("{card} handed back"),
        },
        (A::Redo, _) => match &opened.packet {
            CheckpointPacket::OriginalReturned(back) => format!(
                "{} redoes {card} from fallback-{}",
                names.worker(&back.original),
                back.since
            ),
            _ => format!("{card} redone"),
        },
        (A::KeepReplacement, _) => match &opened.packet {
            CheckpointPacket::OriginalReturned(back) => {
                format!("{} keeps {card}", names.worker(&back.replacement))
            }
            _ => format!("{card} keeps its worker"),
        },
        // Esc is never recorded as itself: the option taken in your place is.
        (A::CarryOn, _) => format!("Left {title} without you"),
    }
}

/// Esc on a question or an approval a run waits on: who decides it now, and when you said
/// so. An agent's own question goes back to that agent, and the rest to the main agent. The
/// `◆` stays dim, since nothing is settled yet.
pub fn handed_on_cell(kind: CheckpointKind, at: OffsetDateTime, offset: UtcOffset) -> Cell {
    let what = match kind {
        CheckpointKind::Question => "Question left to the agent".to_owned(),
        CheckpointKind::Goal => "Goal question left to the main agent".to_owned(),
        CheckpointKind::Plan => "Plan approval left to the main agent".to_owned(),
        CheckpointKind::Merge => "Merge approval left to the main agent".to_owned(),
        _ => "Question left to the main agent".to_owned(),
    };
    Cell::Notice(Notice {
        tone: Tone::Dim,
        ..Notice::answered(what, format!("you, {}", words::clock(at, offset)))
    })
}

/// The dim line left when a card no longer applies.
pub fn withdrawn_cell(title: &str, reason: &str) -> Cell {
    let reason = reason.trim();
    let text = if reason.is_empty() {
        format!("{} · no longer needed", title.trim())
    } else {
        format!("{} · no longer needed: {reason}", title.trim())
    };
    Cell::Custom(Box::new(DimRecord { text }))
}

/// A closed app choice: its title, then the controller's outcome line. The `◆` takes the
/// success color for an answer, and stays dim when nothing changed.
pub fn choice_closed_cell(title: Option<&str>, outcome: &str) -> Cell {
    let outcome = outcome.trim();
    let tone = if outcome == NOT_CHANGED {
        Tone::Dim
    } else {
        Tone::Success
    };
    let notice = match title.map(str::trim).filter(|title| !title.is_empty()) {
        Some(title) => Notice {
            tone,
            ..Notice::answered(title, outcome)
        },
        None => Notice {
            card: true,
            ..Notice::new(tone, outcome)
        },
    };
    Cell::Notice(notice)
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct DimRecord {
    text: String,
}

impl HistoryCell for DimRecord {
    fn lines(&self, ctx: &RenderCtx<'_>) -> Vec<Line<'static>> {
        let theme = ctx.theme;
        let width = usize::from(ctx.width);
        let dim = theme.style(Role::Dim);
        let glyph = theme.glyphs.card;
        let first = [Span::styled(format!("{glyph} "), dim)];
        let rest = [Span::raw(" ".repeat(str_width(glyph) + 1))];
        let spans = [Span::styled(sanitize_line(&self.text).into_owned(), dim)];
        let mut lines = wrap_spans(&spans, width, &first, &rest);
        fit_lines(&mut lines, width);
        lines
    }
}

/// What a merge or a stop acts on: the card, or else the run.
fn subject(
    opened: &CheckpointOpened,
    run: Option<&RunId>,
    cards: &dyn Fn(&CardId) -> String,
) -> String {
    match (&opened.card, run) {
        (Some(card), _) => cards(card),
        (None, Some(run)) => run.to_string(),
        (None, None) => "the run".to_owned(),
    }
}

fn packet_card(opened: &CheckpointOpened) -> Option<&CardId> {
    let card = match &opened.packet {
        CheckpointPacket::Handoff(plan) => &plan.card,
        CheckpointPacket::Escalation(escalation) => &escalation.card,
        CheckpointPacket::Budget(budget) => budget.card.as_ref()?,
        CheckpointPacket::ChangeRequest(request) => &request.card,
        CheckpointPacket::TakeOverReturn(back) => &back.card,
        CheckpointPacket::Question(question) => &question.card,
        CheckpointPacket::OriginalReturned(back) => &back.card,
        _ => opened.card.as_ref()?,
    };
    Some(card)
}

fn merge_target(opened: &CheckpointOpened) -> String {
    match &opened.packet {
        CheckpointPacket::Merge(merge) => merge.target.clone().unwrap_or_else(|| "main".into()),
        _ => "main".into(),
    }
}

fn vendor(opened: &CheckpointOpened, names: &DisplayNames) -> String {
    match &opened.packet {
        CheckpointPacket::VendorDown(down) => names.harness(&down.vendor.0),
        _ => "the agent".into(),
    }
}
