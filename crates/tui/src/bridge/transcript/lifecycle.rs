//! Where a run stands: resumed, paused, waiting, and finished, with the turn end after it.

use std::time::Duration;

use argonauts_core::events::RunEvent;
use argonauts_core::events::run::{
    ClosingAnswer, Party, ResumeCause, RunOutcome, RunResumed, WaitReason,
};
use argonauts_core::events::session::Billing;
use argonauts_core::hidden_git::{HISTORY_KEPT, MergedInto};
use argonauts_core::meter::WhoPaid;
use argonauts_core::settings::ui::{PaidByPlace, UiSettings, VerbTheme};
use argonauts_core::{RunId, Spend, TaskId, Verbosity};
use time::OffsetDateTime;

use crate::bottom::WorkflowRowState;
use crate::bottom::verbs::VerbStyle;
use crate::bridge::failure;
use crate::cell::{AssistantMessage, Cell, DecidedOnAutopilot, Notice, Tone, ToolState, TurnEnd};
use crate::format::{self, WallClock};
use crate::state::RunState;

use super::milestones::RunStory;
use super::steps::{plural, step};
use super::{Cx, emit, quiet, run_name};

/// The glyph of a run that waits out an outage. `■` takes two cells in some fonts.
const WAITING: &str = "▪";
/// The glyph of a run you paused, as its row has it.
const PAUSED: &str = "⏸";
/// The glyph of a run that stopped fully.
const STOPPED: &str = "✘";
/// How a run that stopped or that you paused goes on.
const CARRIES_ON: &str = "carries it on";
/// At normal verbosity the closing answer names this many files, then counts the rest.
const FILES_NAMED: usize = 4;
/// And keeps this many of what's left.
const LEFT_NAMED: usize = 2;
/// And this many of what autopilot decided, then counts the rest.
const DECIDED_NAMED: usize = 4;

/// `● Resume(Add a --json flag)` and what changed while it waited. A run that waited on a question
/// prints nothing when it stops, so it prints nothing when your answer moves it on either,
/// unless something changed meanwhile. A run that waited for nobody on a timer recorded every
/// wake as an agent coming back, so its try says only when it began (`tried_at`).
pub(super) fn resumed(
    run: &RunId,
    resumed: &RunResumed,
    tried_at: Option<OffsetDateTime>,
    clock: &WallClock,
    cx: &mut Cx<'_>,
) {
    let mut details = Vec::new();
    if resumed.settings_changed {
        details.push("Settings changed since it stopped".to_owned());
    }
    if !resumed.cli_versions_changed.is_empty() {
        details.push(format!(
            "New versions of {}",
            resumed.cli_versions_changed.join(", ")
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
            "{} fresh with a fallback note",
            plural(fresh, "session started", "sessions started")
        ));
    }
    if !sessions.is_empty() {
        details.push(sessions.join(" · "));
    }
    let tried = tried_at.map(|at| format!("Tried again at {}", clock.clock(at)));
    let cause = match resumed.cause {
        _ if tried.is_some() => tried.as_deref(),
        ResumeCause::User => None,
        ResumeCause::Answer if details.is_empty() => return,
        ResumeCause::Answer => Some("After your answer"),
        ResumeCause::AgentBack => Some("An agent it waited for came back"),
        ResumeCause::Timer => Some("Tried again on its own, as its wait ended"),
        ResumeCause::AskTimeout => {
            Some("The fallback question waited too long, so it fell back to the first choice")
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

/// `⏸ You paused the run · /resume carries it on`, worded by who asked for the pause, then which
/// agents stopped and that their work is kept. Nothing wakes it but you.
pub(super) fn paused(
    run: &RunId,
    now: bool,
    by: Option<&Party>,
    sessions_stopped: u32,
    committed: &[TaskId],
    cx: &mut Cx<'_>,
) {
    let stopped = usize::try_from(sessions_stopped).unwrap_or(usize::MAX);
    let labels = stopped_labels(run, stopped, cx);
    let at = if now { "" } else { " at a safe point" };
    let kept = if committed.is_empty() && stopped == 0 {
        ""
    } else {
        ", their work so far kept"
    };
    let who = match labels.as_slice() {
        [] if stopped == 0 => None,
        [] => Some(plural(stopped, "agent", "agents")),
        labels => Some(argonauts_core::text::and_list(labels)),
    };
    let mut notice = Notice::new(Tone::Normal, paused_line(by))
        .with_glyph(PAUSED)
        .with_after(CARRIES_ON);
    if let Some(who) = who {
        notice = notice.with_detail(format!("{who} stopped{at}{kept}"));
    }
    emit(cx, notice);
}

/// Who paused the run, as its line says it.
fn paused_line(by: Option<&Party>) -> String {
    match by {
        Some(Party::User) | None => "You paused the run · /resume".to_owned(),
        Some(Party::Network) => "The run paused while the network was down · /resume".to_owned(),
        Some(Party::Rules) => "The run paused at your limit · /resume".to_owned(),
        // The name is the client's own, so it's drawn as text, never as escapes.
        Some(Party::OutsideAgent { client }) => format!(
            "{} paused the run · /resume",
            argonauts_core::sanitize::sanitize_for_terminal(client)
        ),
        Some(_) => "The run paused · /resume".to_owned(),
    }
}

/// The labels of the `count` agents of `run` that stopped last, in order.
fn stopped_labels(run: &RunId, count: usize, cx: &Cx<'_>) -> Vec<String> {
    let Some(state) = cx.state.runs.get(run) else {
        return Vec::new();
    };
    let mut stopped: Vec<(&OffsetDateTime, &str)> = state
        .sessions
        .values()
        .filter(|session| session.status == crate::state::SessionStatus::Stopped)
        .filter_map(|session| Some((session.ended.as_ref()?, session.label.as_deref()?)))
        .collect();
    stopped.sort();
    let mut labels: Vec<String> = stopped
        .into_iter()
        .rev()
        .take(count)
        .map(|(_, label)| label.to_owned())
        .collect();
    labels.sort();
    if labels.len() < count {
        return Vec::new();
    }
    labels
}

/// A run that stopped fully, with why, what was tried and how to carry it on, or one that
/// waits out an outage you chose to wait out, with when it tries again. Pauses and approvals
/// have their own lines.
pub(super) fn waiting(reason: &WaitReason, clock: &WallClock, cx: &mut Cx<'_>) {
    let again = |at: &Option<OffsetDateTime>| match at {
        Some(at) => format!(" · tries again at {}", clock.clock_rounded_up(*at)),
        None => String::new(),
    };
    let notice = match reason {
        WaitReason::AllAgentsOut { until } => Notice::new(
            Tone::Normal,
            format!("Waiting for an agent{}", again(until)),
        )
        .with_glyph(WAITING)
        .with_detail("/resume tries now"),
        WaitReason::WaitingForAgent { agent_id, until } => Notice::new(
            Tone::Normal,
            format!(
                "Waiting for {}{}",
                cx.state.names.cli(&agent_id.0),
                again(until)
            ),
        )
        .with_glyph(WAITING)
        .with_detail("/resume tries now"),
        WaitReason::NoFallback { detail, stop, .. } => {
            let why = stop
                .as_ref()
                .map_or(detail.as_str(), |stop| stop.why.as_str());
            let mut notice = Notice::new(Tone::Error, format!("The run stopped · {why}"))
                .with_glyph(STOPPED)
                .with_dim_details();
            let lines = match stop {
                Some(stop) => stop.lines(),
                None => vec!["/resume carries the run on".to_owned()],
            };
            for line in lines {
                notice = notice.with_detail(line);
            }
            notice
        }
        WaitReason::Paused | WaitReason::Approval => return,
    };
    emit(cx, notice);
}

/// `● Merged the run into main (4 tasks, 7 files, +120 -18)`, then `⎿  Run finished · 42s ·
/// $0.06 · run review saved` with who paid for its tokens under it, then the turn end.
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
    let target = story.run_merged.as_deref().unwrap_or("main");
    // A folder without git has no branch the run merged into: its work was written in.
    let merged_into = MergedInto::of(target);
    let written_into_folder = merged_into == MergedInto::YourFolder;
    let (tone, glyph, title, verb) = match outcome {
        RunOutcome::Merged if written_into_folder => {
            (Tone::Success, None, merged_into.heading(), "finished")
        }
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
            "Abandoned the run · its work is removed".to_owned(),
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
    if outcome == RunOutcome::Failed {
        let tasks = state.map(|run| run.tasks_done());
        failed_run(
            story,
            detail,
            elapsed,
            tasks,
            state.map(|run| run.spend()),
            cx,
        );
    } else {
        let mut notice = if outcome == RunOutcome::Merged && written_into_folder {
            Notice::plain_with_after(tone, title, written_after(state))
        } else {
            Notice::new(tone, title)
        };
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
                notice =
                    notice.with_detail(format!("Open it with: {}", shell_words::join(command)));
            }
        }
        let mut finished = format!("Run {verb}");
        if let Some(elapsed) = elapsed {
            finished.push_str(&format!(" · {}", format::duration(elapsed)));
        }
        if let Some(run) = state {
            finished.push_str(&format!(" · {}", format::spend(&run.spend())));
        }
        if let Some(removed) = &story.copies_removed {
            finished.push_str(&format!(" · {removed}"));
        }
        if story.run_review {
            finished.push_str(" · run review saved");
        }
        notice = notice.with_detail(finished);
        if let Some(paid) = state.and_then(|run| who_paid_line(run, cx.settings)) {
            notice = notice.with_detail(paid);
        }
        emit(cx, notice);
        if let Some(answer) = &story.closing {
            emit_closing(answer, cx);
        }
    }
    if let Some(elapsed) = elapsed
        && cx.settings.turn_duration
    {
        let style = match cx.settings.verbs {
            VerbTheme::Voyage => VerbStyle::Voyage,
            VerbTheme::Plain => VerbStyle::Plain,
        };
        let label = format!("{} for", cx.model.verb_picker.done(style));
        let mut end = TurnEnd::new(label, elapsed, Some(clock.clock(event.ts)));
        if WorkflowRowState::ended(outcome) != WorkflowRowState::Done {
            end = end.unfinished();
        }
        emit(cx, end);
    }
}

/// Who paid for a run's tokens, for the dim line under its end line where `ui.paid_by` puts
/// it: `1.2M plan · 232k free · 58k paid $0.31`, `all free`, or `all paid $2.14`. A run all on
/// plans has nothing to say, and nor does one with a session that never said who pays. The
/// split goes by output tokens, which every CLI counts the same way and the run's row shows.
fn who_paid_line(run: &RunState, settings: &UiSettings) -> Option<String> {
    if !settings.paid_by.contains(&PaidByPlace::EndLine) {
        return None;
    }
    let mut paid = WhoPaid::default();
    for session in run
        .sessions
        .values()
        .filter(|session| session.tokens_out > 0)
    {
        let estimated = session
            .end
            .as_ref()
            .is_some_and(|end| end.spend.cost_estimated);
        paid.count(
            session.billing?,
            session.tokens_out,
            session.cost_usd,
            estimated,
        );
    }
    // Paid tokens with no cost at all say no amount rather than a made-up one.
    let cost = paid.paid_cost();
    let cost = if cost.cost_usd > 0.0 || !cost.cost_partial {
        format!(" {}", format::spend(&cost))
    } else {
        String::new()
    };
    match paid.all_by() {
        Some(Billing::Plan) => None,
        Some(Billing::Free) => Some("all free".to_owned()),
        Some(Billing::Paid) => Some(format!("all paid{cost}")),
        // Nothing counted, or a mix: each part that has tokens.
        None => {
            let parts = [
                (Billing::Plan, "plan", ""),
                (Billing::Free, "free", ""),
                (Billing::Paid, "paid", cost.as_str()),
            ];
            let words: Vec<String> = parts
                .into_iter()
                .filter(|(billing, ..)| paid.tokens(*billing) > 0)
                .map(|(billing, word, cost)| {
                    format!("{} {word}{cost}", format::tokens(paid.tokens(billing)))
                })
                .collect();
            (!words.is_empty()).then(|| words.join(" · "))
        }
    }
}

/// The words that open the line of a run that ended without merging.
const RUN_ENDED: &str = "The run ended";
/// And of one whose end recorded no reason.
const RUN_FAILED: &str = "The run failed";

/// A failed run is one block. With a reason recorded, `The run ended · <why>`, and under it
/// what held the run, that its work is kept and how to carry it on. With none, `The run failed
/// · 0 of 1 task done · 0s · $0.00`. The run's last word follows only when it says more.
fn failed_run(
    story: &RunStory,
    detail: Option<&str>,
    elapsed: Option<Duration>,
    tasks: Option<(usize, usize)>,
    spend: Option<Spend>,
    cx: &mut Cx<'_>,
) {
    let reason = detail.map(Reason::of).unwrap_or_default();
    let title = if reason.why.is_empty() {
        RUN_FAILED
    } else {
        RUN_ENDED
    };
    // A replayed block is only kept for the overlay, where no key opens what it hints at.
    let raw = detail
        .filter(|_| reason.held.is_none())
        .and_then(|detail| failure::RawFailure::behind(&reason.why, title, detail))
        .filter(|_| !cx.replaying());
    let notice = if reason.why.is_empty() {
        unexplained(elapsed, tasks, spend, raw.is_some())
    } else {
        let mut under: Vec<&str> = reason.held.iter().map(String::as_str).collect();
        under.push(if story.folder_without_git {
            "the work is kept"
        } else {
            "the branch is kept"
        });
        under.push("/resume carries it on");
        if raw.is_some() {
            under.push(failure::RAW_ALL_HINT);
        }
        Notice::headline(Tone::Error, RUN_ENDED, Some(reason.why.clone()))
            .with_detail(under.join(" · "))
    };
    emit(cx, notice);
    if raw.is_some() {
        cx.model.raw_failure = raw;
    }
    if let Some(answer) = &story.closing {
        let text = answer.text.trim();
        let reads_back = text.starts_with(RUN_FAILED)
            || text.starts_with(RUN_ENDED)
            || (!reason.why.is_empty() && text.contains(&reason.why));
        if !reads_back {
            emit_closing(answer, cx);
        }
    }
}

/// `The run failed · 0 of 1 task done · 0s · $0.00`, for a run whose end recorded no reason.
fn unexplained(
    elapsed: Option<Duration>,
    tasks: Option<(usize, usize)>,
    spend: Option<Spend>,
    raw: bool,
) -> Notice {
    let mut title = RUN_FAILED.to_owned();
    if let Some((done, total)) = tasks.filter(|(_, total)| *total > 0) {
        let noun = if total == 1 { "task" } else { "tasks" };
        title.push_str(&format!(" · {done} of {total} {noun} done"));
    }
    if let Some(elapsed) = elapsed {
        title.push_str(&format!(" · {}", format::duration(elapsed)));
    }
    if let Some(spend) = &spend {
        title.push_str(&format!(" · {}", format::spend(spend)));
    }
    let notice = Notice::new(Tone::Error, title);
    if raw {
        notice.with_detail(failure::RAW_ALL_HINT)
    } else {
        notice
    }
}

/// Why a run ended, from the detail its end recorded.
#[derive(Debug, Default, PartialEq, Eq)]
struct Reason {
    /// In one line, such as `Nothing was merged: build:fix stopped before its checks passed`.
    why: String,
    /// What held the run, such as `it was blocked on a question`: the detail's second line,
    /// when the detail is two plain lines and says no more.
    held: Option<String>,
}

impl Reason {
    fn of(detail: &str) -> Self {
        let lines: Vec<&str> = detail
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .collect();
        let plain = |line: &str| failure::one_line(line) == line;
        let (why, held) = match lines.as_slice() {
            [why, held] if plain(why) && plain(held) => ((*why).to_owned(), Some(*held)),
            _ => (failure::one_line(detail), None),
        };
        // Each reads as a clause before a ` · `, so neither ends with a period or a colon.
        let clause = |text: &str| text.trim().trim_end_matches(['.', ':']).to_owned();
        Self {
            why: clause(&why),
            held: held.map(clause),
        }
    }
}

/// The run's last word, and under it what the run decided in your place, each with whose call
/// it was. Quiet keeps the words alone.
fn emit_closing(answer: &ClosingAnswer, cx: &mut Cx<'_>) {
    let verbosity = cx.verbosity();
    emit(
        cx,
        AssistantMessage::new(closing_markdown(answer, verbosity)),
    );
    if answer.decided.is_empty() || verbosity == Verbosity::Quiet {
        return;
    }
    let named = match verbosity {
        Verbosity::Full => answer.decided.len(),
        _ => DECIDED_NAMED,
    };
    let decided = DecidedOnAutopilot::new(&answer.decided, named);
    emit(cx, Cell::Custom(Box::new(decided)));
}

/// The run's last word, as a message in the main agent's voice: its own words, then what
/// changed, how it was checked, and what's left. Quiet keeps the words alone, and normal
/// keeps the lists short.
fn closing_markdown(answer: &ClosingAnswer, verbosity: Verbosity) -> String {
    let mut markdown = answer.text.trim().to_owned();
    if verbosity == Verbosity::Quiet {
        return markdown;
    }
    let full = verbosity == Verbosity::Full;
    let mut items = Vec::new();
    if !answer.files.is_empty() {
        let named = if full {
            answer.files.len()
        } else {
            FILES_NAMED
        };
        let mut files: Vec<String> = answer
            .files
            .iter()
            .take(named)
            .map(|file| format!("`{}` +{} -{}", file.path, file.insertions, file.deletions))
            .collect();
        let more = answer.files.len().saturating_sub(named);
        if more > 0 {
            files.push(format!("and {more} more"));
        }
        items.push(format!("**Changed** {}", files.join(", ")));
    }
    if !answer.checks.is_empty() {
        let checks: Vec<String> = answer
            .checks
            .iter()
            .map(|check| match (check.passed, check.result.as_str()) {
                (true, "") => format!("{} passed", check.what),
                (true, result) => format!("{}: {result}", check.what),
                (false, "") => format!("{} failed", check.what),
                (false, result) => format!("{} failed: {result}", check.what),
            })
            .collect();
        items.push(format!("**Checked** {}", checks.join(" · ")));
    }
    if full && !answer.tests.is_empty() {
        items.push(format!("**Tests run** {}", answer.tests.join(" · ")));
    }
    if !answer.left.is_empty() {
        let named = if full { answer.left.len() } else { LEFT_NAMED };
        let left: Vec<&str> = answer
            .left
            .iter()
            .take(named)
            .map(|line| line.trim().trim_end_matches('.'))
            .collect();
        items.push(format!("**Left** {}", left.join("; ")));
    }
    if !items.is_empty() {
        markdown.push_str("\n\n");
        for item in items {
            markdown.push_str(&format!("- {item}\n"));
        }
    }
    markdown.trim_end().to_owned()
}

/// `4 files +52 −6 · history kept by Argonauts`, after "Written into your folder".
fn written_after(run: Option<&RunState>) -> String {
    match run.and_then(|run| run.merge_totals.as_ref()) {
        Some(merge) => {
            let stat = &merge.combined;
            let files = usize::try_from(stat.files_changed).unwrap_or(usize::MAX);
            format!(
                "{} +{} \u{2212}{} · {HISTORY_KEPT}",
                plural(files, "file", "files"),
                stat.insertions,
                stat.deletions
            )
        }
        None => HISTORY_KEPT.to_owned(),
    }
}

/// ` (4 tasks, 7 files, +120 -18)`: what the merge question put up, or without one, the tasks'
/// diffs.
fn totals(run: &RunState) -> Option<String> {
    if let Some(merge) = &run.merge_totals {
        let combined = &merge.combined;
        return Some(format!(
            " ({}, {}, +{} -{})",
            plural(
                usize::try_from(merge.tasks).unwrap_or(usize::MAX),
                "task",
                "tasks"
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
    if run.tasks.is_empty() {
        return None;
    }
    let mut parts = vec![plural(run.tasks.len(), "task", "tasks")];
    let diffs: Vec<_> = run.tasks.values().filter_map(|task| task.diff).collect();
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

#[cfg(test)]
#[path = "who_paid_tests.rs"]
mod who_paid_tests;
