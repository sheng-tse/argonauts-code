//! Cards changing hands: handoffs with their checkpoint tags, waits, reverts, reassignments,
//! rotations to a fresh session, cleared review debt, and an agent coming back.

use argonauts_core::RunId;
use argonauts_core::display_names::DisplayNames;
use argonauts_core::events::run::Party;
use argonauts_core::events::vendor::HandoffPlan;
use argonauts_core::names::{VendorId, WorkerRef};
use argonauts_core::settings::handoff::{HandoffContext, HandoffMode};
use time::OffsetDateTime;

use crate::cell::{Notice, Tone, ToolState};
use crate::format::WallClock;

use super::steps::{party_words, plural, step};
use super::{Cx, emit};

/// `Claude Code out until 3:45 PM`, or the stop's own reason when no reset time is known.
fn why(plan: &HandoffPlan, clock: &WallClock, names: &DisplayNames) -> String {
    let vendor = names.harness(&plan.stopped.worker);
    match plan.resets_at {
        Some(at) => format!("{vendor} out until {}", clock.clock(at)),
        None if plan.reason.trim().is_empty() => format!("{vendor} stopped"),
        None => format!("{vendor} stopped: {}", plan.reason.trim()),
    }
}

/// A handoff as the log records it, with the card's newest checkpoint tag.
pub(super) struct Handed<'a> {
    /// The card's label, such as `build:flag`.
    pub card: &'a str,
    pub to: &'a WorkerRef,
    pub mode: HandoffMode,
    /// What the new owner is told. Older logs don't say.
    pub context: Option<HandoffContext>,
    pub tag: Option<u32>,
}

/// An auto handoff is a step, `● Handoff(build:limiter Middleware · auto)` with who took it,
/// what it was told, and why. One you picked on the handoff card is one line, `● build:export
/// handed to GPT-6.1 Sol on Codex · note + transcript · checkpoint handoff-1 · Claude Code out
/// until 3:45 PM`.
pub(super) fn handed_off(
    handed: Handed<'_>,
    plan: Option<&HandoffPlan>,
    clock: &WallClock,
    cx: &mut Cx<'_>,
) {
    let Handed {
        card,
        to,
        mode,
        context,
        tag,
    } = handed;
    let told = context.map(|context| context.label().to_owned());
    let checkpoint = tag.map(|n| format!("checkpoint fallback-{n}"));
    let names = &cx.state.names;
    let why = plan.map(|plan| why(plan, clock, names));
    if mode == HandoffMode::Auto {
        let title = plan
            .map(|plan| plan.title.trim().to_owned())
            .filter(|title| !title.is_empty());
        let args = match title {
            Some(title) => format!("{card} {title} · auto"),
            None => format!("{card} · auto"),
        };
        let mut took = format!("{} took it", names.worker(to));
        for part in [told, checkpoint].into_iter().flatten() {
            took.push_str(&format!(" · {part}"));
        }
        let details: Vec<String> = [Some(took), why].into_iter().flatten().collect();
        emit(cx, step("Fallback", &args, ToolState::Done, &details));
        return;
    }
    let mut text = format!("{card} handed to {}", names.worker(to));
    for part in [told, checkpoint, why].into_iter().flatten() {
        text.push_str(&format!(" · {part}"));
    }
    emit(cx, Notice::new(Tone::Success, text));
}

/// A card that waits for its agent to come back instead of changing hands.
pub(super) fn waiting(
    card: &str,
    vendor: &VendorId,
    until: Option<OffsetDateTime>,
    clock: &WallClock,
    cx: &mut Cx<'_>,
) {
    let mut text = format!("{card} waits for {}", cx.state.names.harness(&vendor.0));
    if let Some(at) = until {
        text.push_str(&format!(" until {}", clock.clock(at)));
    }
    emit(cx, Notice::new(Tone::Normal, text));
}

/// `/rewind`: the card went back to a checkpoint, and the work after it was kept aside.
pub(super) fn reverted(card: &str, to: u32, kept_on: &str, cx: &mut Cx<'_>) {
    let mut notice = Notice::new(
        Tone::Success,
        format!("Reverted {card} to checkpoint fallback-{to}"),
    );
    if !kept_on.trim().is_empty() {
        notice = notice.with_detail(format!("The later work is kept on {}", kept_on.trim()));
    }
    emit(cx, notice);
}

pub(super) fn reassigned(
    card: &str,
    from: Option<&WorkerRef>,
    to: &WorkerRef,
    by: &Party,
    cx: &mut Cx<'_>,
) {
    let names = &cx.state.names;
    let text = format!("Reassigned {card} to {}", names.worker(to));
    let mut notice = Notice::new(Tone::Success, text);
    let parts: Vec<String> = [
        from.map(|from| format!("was {}", names.worker(from))),
        party_words(by, names).map(|who| format!("by {who}")),
    ]
    .into_iter()
    .flatten()
    .collect();
    if !parts.is_empty() {
        notice = notice.with_detail(parts.join(" · "));
    }
    emit(cx, notice);
}

/// A full session gave way to a fresh one of the same agent, which goes on from a handoff note.
pub(super) fn rotated(card: &str, worker: &WorkerRef, cx: &mut Cx<'_>) {
    let text = format!(
        "{card} rotated to a fresh {} session",
        cx.state.names.worker(worker)
    );
    let detail = "its context filled up, so the new session starts from a fallback note";
    emit(cx, Notice::new(Tone::Normal, text).with_detail(detail));
}

/// A review by the original, or a model at least as strong, cleared a replacement's debt.
pub(super) fn debt_cleared(card: &str, by: &WorkerRef, cx: &mut Cx<'_>) {
    let text = format!(
        "{card} needs no more review · checked by {}",
        cx.state.names.worker(by)
    );
    emit(cx, Notice::new(Tone::Success, text));
}

pub(super) fn conflict(card: &str, files: usize, assigned_to: &WorkerRef, cx: &mut Cx<'_>) {
    let text = format!(
        "Merging {card} conflicted in {} · {} resolves it",
        plural(files, "file", "files"),
        cx.state.names.worker(assigned_to)
    );
    emit(cx, Notice::new(Tone::Warning, text));
}

/// `Claude Code is back · build:limiter stays with GPT-6.1 Sol on Codex · /reassign card-003
/// claude-code/opus moves it back`, for each card another agent still holds for it. The
/// command keeps the id, since that's what you type.
pub(super) fn vendor_back(run: &RunId, vendor: &VendorId, cx: &mut Cx<'_>) {
    let Some(state) = cx.state.runs.get(run) else {
        return;
    };
    let names = &cx.state.names;
    let label = names.harness(&vendor.0);
    let lines: Vec<String> = state
        .cards
        .values()
        .filter_map(|card| {
            let original = card.standing_in_for.as_ref()?;
            let owner = card.owner.as_ref()?;
            let stands_in = original.vendor() == *vendor && owner != original;
            stands_in.then(|| {
                format!(
                    "{label} is back · {card} stays with {owner} · /reassign {id} {original} moves it back",
                    card = state.card_label(&card.id),
                    id = card.id,
                    owner = names.worker(owner),
                )
            })
        })
        .collect();
    for line in lines {
        emit(cx, Notice::new(Tone::Success, line));
    }
}
