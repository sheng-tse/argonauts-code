//! Where a run stands: resumed, paused, parked, and finished, with the turn end after it.

use std::time::Duration;

use argonauts_core::events::RunEvent;
use argonauts_core::events::run::{ParkReason, ResumeCause, RunOutcome, RunResumed};
use argonauts_core::settings::ui::VerbTheme;
use argonauts_core::{CardId, RunId};
use time::OffsetDateTime;

use crate::bottom::verbs::VerbStyle;
use crate::cell::{Notice, Tone, ToolState, TurnEnd};
use crate::format::{self, WallClock};
use crate::state::RunState;

use super::milestones::RunStory;
use super::steps::{plural, step};
use super::{Cx, emit, quiet, run_name};

/// The glyph of a run that stopped moving, paused or parked.
const STOPPED: &str = "■";

/// `● Resume(Add a --json flag)` and what changed while it waited. A run that waited on a card
/// prints nothing when it stops, so it prints nothing when your answer moves it on either,
/// unless something changed meanwhile.
pub(super) fn resumed(run: &RunId, resumed: &RunResumed, cx: &mut Cx<'_>) {
    let mut details = Vec::new();
    if resumed.settings_changed {
        details.push("Settings changed since it stopped".to_owned());
    }
    if !resumed.harness_versions_changed.is_empty() {
        details.push(format!(
            "New versions of {}",
            resumed.harness_versions_changed.join(", ")
        ));
    }
    if let Some(commits) = resumed.main_moved_by {
        let commits = usize::try_from(commits).unwrap_or(usize::MAX);
        details.push(format!(
            "main moved {} · the final check runs again before the merge",
            plural(commits, "commit", "commits")
        ));
    }
    let again = resumed.sessions.iter().filter(|s| s.resumed).count();
    let fresh = resumed.sessions.len() - again;
    let mut sessions = Vec::new();
    if again > 0 {
        sessions.push(format!("{} resumed", plural(again, "session", "sessions")));
    }
    if fresh > 0 {
        sessions.push(format!(
            "{} fresh with a handoff note",
            plural(fresh, "session started", "sessions started")
        ));
    }
    if !sessions.is_empty() {
        details.push(sessions.join(" · "));
    }
    let cause = match resumed.cause {
        ResumeCause::User => None,
        ResumeCause::Answer if details.is_empty() => return,
        ResumeCause::Answer => Some("After your answer"),
        ResumeCause::VendorBack => Some("An agent it waited for is back"),
        ResumeCause::AskTimeout => {
            Some("The handoff card waited too long, so the fallback applies")
        }
        ResumeCause::Crash => Some("After a crash, from its log"),
    };
    if let Some(cause) = cause {
        details.insert(0, cause.to_owned());
    }
    if details.is_empty() {
        details.push("Nothing changed while it waited".to_owned());
    }
    let name = run_name(run, cx);
    emit(cx, step("Resume", &name, ToolState::Done, &details));
}

/// `■ Paused the run at a safe point · 2 sessions stopped after their tool calls · work
/// committed`, and how to go on.
pub(super) fn paused(now: bool, sessions_stopped: u32, committed: &[CardId], cx: &mut Cx<'_>) {
    let mut text = if now {
        "Paused the run".to_owned()
    } else {
        "Paused the run at a safe point".to_owned()
    };
    if sessions_stopped > 0 {
        let sessions = plural(
            usize::try_from(sessions_stopped).unwrap_or(usize::MAX),
            "session",
            "sessions",
        );
        if now {
            text.push_str(&format!(" · {sessions} stopped"));
        } else {
            text.push_str(&format!(" · {sessions} stopped after their tool calls"));
        }
    }
    if !committed.is_empty() {
        text.push_str(" · work committed");
    }
    let notice = Notice::new(Tone::Normal, text)
        .with_glyph(STOPPED)
        .with_detail("Enter resumes · /stop ends the run · /resume <run> switches to another run");
    emit(cx, notice);
}

/// A run parked because the agents it needs are out. Pauses and checkpoints have their own
/// lines.
pub(super) fn parked(reason: &ParkReason, clock: &WallClock, cx: &mut Cx<'_>) {
    let until = |at: &Option<OffsetDateTime>| {
        at.map(|at| format!(" until {}", clock.clock(at)))
            .unwrap_or_default()
    };
    let text = match reason {
        ParkReason::AllVendorsOut { until: at } => {
            format!("Parked the run · every agent is out{}", until(at))
        }
        ParkReason::WaitingForVendor { vendor, until: at } => format!(
            "Parked the run · its cards wait for {}{}",
            cx.state.names.harness(&vendor.0),
            until(at)
        ),
        ParkReason::Paused | ParkReason::Checkpoint => return,
    };
    let notice = Notice::new(Tone::Normal, text)
        .with_glyph(STOPPED)
        .with_detail("It resumes when one is back · /resume tries now");
    emit(cx, notice);
}

/// `● Merged the run into main (4 cards, 7 files, +120 -18)`, then `⎿  Run finished · 42s ·
/// $0.06 · retro saved`, then the turn end.
pub(super) fn run_end(
    event: &RunEvent,
    story: &RunStory,
    outcome: RunOutcome,
    detail: Option<&str>,
    clock: &WallClock,
    cx: &mut Cx<'_>,
) {
    let run = &event.run_id;
    let state = cx.state.runs.get(run);
    let started = story.started.or(state.and_then(|run| run.started));
    let elapsed = started.map(|at| {
        let seconds = (event.ts - at).whole_seconds().max(0);
        Duration::from_secs(u64::try_from(seconds).unwrap_or(0))
    });
    let totals = state.and_then(totals).unwrap_or_default();
    let target = story.landed.as_deref().unwrap_or("main");
    let (tone, glyph, title, verb) = match outcome {
        RunOutcome::Merged => (
            Tone::Success,
            None,
            format!("Merged the run into {target}{totals}"),
            "finished",
        ),
        RunOutcome::PullRequest => {
            let opened = story
                .pull_request
                .as_ref()
                .is_some_and(|pr| pr.url.is_some());
            let title = if opened {
                format!("Opened a pull request for the run{totals}")
            } else {
                format!("The run is ready for a pull request{totals}")
            };
            (Tone::Success, None, title, "finished")
        }
        RunOutcome::BranchLeft => (
            Tone::Success,
            None,
            format!("Left the run on its branches{totals}"),
            "finished",
        ),
        RunOutcome::Abandoned => (
            Tone::Normal,
            None,
            "Abandoned the run · its branches are kept".to_owned(),
            "ended",
        ),
        RunOutcome::Stopped => (
            Tone::Normal,
            Some(STOPPED),
            "Stopped the run · work committed".to_owned(),
            "stopped",
        ),
        RunOutcome::Failed => (Tone::Error, None, "The run failed".to_owned(), "ended"),
    };
    let mut notice = Notice::new(tone, title);
    if let Some(glyph) = glyph {
        notice = notice.with_glyph(glyph);
    }
    if let Some(detail) = detail.map(str::trim).filter(|detail| !detail.is_empty()) {
        notice = notice.with_detail(detail);
    }
    if outcome == RunOutcome::PullRequest
        && let Some(pr) = &story.pull_request
    {
        if let Some(url) = &pr.url {
            notice = notice.with_detail(url.clone());
        } else if let Some(command) = &pr.command {
            notice = notice.with_detail(format!("Open it with: {}", shell_words::join(command)));
        }
    }
    let mut finished = format!("Run {verb}");
    if let Some(elapsed) = elapsed {
        finished.push_str(&format!(" · {}", format::duration(elapsed)));
    }
    if let Some(run) = state {
        finished.push_str(&format!(" · {}", format::spend(&run.spend())));
    }
    if story.retro {
        finished.push_str(" · retro saved");
    }
    emit(cx, notice.with_detail(finished));
    if let Some(elapsed) = elapsed
        && cx.settings.turn_duration
    {
        let style = match cx.settings.verbs {
            VerbTheme::Voyage => VerbStyle::Voyage,
            VerbTheme::Plain => VerbStyle::Plain,
        };
        let label = format!("{} for", cx.model.verb_picker.done(style));
        emit(
            cx,
            TurnEnd::new(label, elapsed, Some(clock.clock(event.ts))),
        );
    }
}

/// ` (4 cards, 7 files, +120 -18)`: what the merge card put up, or without one, the cards'
/// diffs.
fn totals(run: &RunState) -> Option<String> {
    if let Some(merge) = &run.merge_totals {
        let combined = &merge.combined;
        return Some(format!(
            " ({}, {}, +{} -{})",
            plural(
                usize::try_from(merge.cards).unwrap_or(usize::MAX),
                "card",
                "cards"
            ),
            plural(
                usize::try_from(combined.files_changed).unwrap_or(usize::MAX),
                "file",
                "files"
            ),
            combined.insertions,
            combined.deletions
        ));
    }
    if run.cards.is_empty() {
        return None;
    }
    let mut parts = vec![plural(run.cards.len(), "card", "cards")];
    let diffs: Vec<_> = run.cards.values().filter_map(|card| card.diff).collect();
    if !diffs.is_empty() {
        let files: u32 = diffs.iter().map(|diff| diff.files_changed).sum();
        let added: u32 = diffs.iter().map(|diff| diff.insertions).sum();
        let removed: u32 = diffs.iter().map(|diff| diff.deletions).sum();
        let files = usize::try_from(files).unwrap_or(usize::MAX);
        parts.push(plural(files, "file", "files"));
        parts.push(format!("+{added} -{removed}"));
    }
    Some(format!(" ({})", parts.join(", ")))
}
pub(super) fn main_moved(commits: u32, cx: &mut Cx<'_>) {
    if quiet(cx) {
        return;
    }
    let commits = usize::try_from(commits).unwrap_or(usize::MAX);
    let text = format!(
        "main moved {} since the run started · the final check brings them in",
        plural(commits, "commit", "commits")
    );
    emit(cx, Notice::new(Tone::Dim, text));
}
