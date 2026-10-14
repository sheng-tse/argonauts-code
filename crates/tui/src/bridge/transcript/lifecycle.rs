//! Where a run stands: resumed, paused, waiting, and finished, with the turn end after it.

use std::time::Duration;

use argonauts_core::events::RunEvent;
use argonauts_core::events::run::{ClosingAnswer, ResumeCause, RunOutcome, RunResumed, WaitReason};
use argonauts_core::hidden_git::{HISTORY_KEPT, MergedInto};
use argonauts_core::settings::ui::VerbTheme;
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

/// The glyph of a run that stopped moving, paused or waiting.
const STOPPED: &str = "■";
/// At normal verbosity the closing answer names this many files, then counts the rest.
const FILES_NAMED: usize = 4;
/// And keeps this many of what's left.
const LEFT_NAMED: usize = 2;
/// And this many of what autopilot decided, then counts the rest.
const DECIDED_NAMED: usize = 4;

/// `● Resume(Add a --json flag)` and what changed while it waited. A run that waited on a question
/// prints nothing when it stops, so it prints nothing when your answer moves it on either,
/// unless something changed meanwhile.
pub(super) fn resumed(run: &RunId, resumed: &RunResumed, cx: &mut Cx<'_>) {
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
    let cause = match resumed.cause {
        ResumeCause::User => None,
        ResumeCause::Answer if details.is_empty() => return,
        ResumeCause::Answer => Some("After your answer"),
        ResumeCause::AgentBack => Some("An agent it waited for is tried again"),
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

/// `■ Paused the run at a safe point · 2 sessions stopped after their tool calls · work
/// committed`, and how to go on.
pub(super) fn paused(now: bool, sessions_stopped: u32, committed: &[TaskId], cx: &mut Cx<'_>) {
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

/// A run waiting because the agents it needs are out, or a job has nobody to fall back to.
/// Pauses and approvals have their own lines. It says it carries on by itself only when it will.
pub(super) fn waiting(reason: &WaitReason, clock: &WallClock, cx: &mut Cx<'_>) {
    let until = |at: &Option<OffsetDateTime>| {
        at.map(|at| format!(" until {}", clock.clock(at)))
            .unwrap_or_default()
    };
    let back = "It resumes when one is back · /resume tries now".to_owned();
    let (text, detail) = match reason {
        WaitReason::AllAgentsOut { until: at } => (
            format!("The run waits · every agent is out{}", until(at)),
            back,
        ),
        WaitReason::WaitingForAgent {
            agent_id,
            until: at,
        } => (
            format!(
                "The run waits · its tasks wait for {}{}",
                cx.state.names.cli(&agent_id.0),
                until(at)
            ),
            back,
        ),
        WaitReason::NoFallback { detail, until: at } => (
            format!("The run waits · {detail}"),
            match at {
                Some(at) => format!("It tries again at {} · /resume tries now", clock.clock(*at)),
                None => "/resume tries again".to_owned(),
            },
        ),
        WaitReason::Paused | WaitReason::Approval => return,
    };
    let notice = Notice::new(Tone::Normal, text)
        .with_glyph(STOPPED)
        .with_detail(detail);
    emit(cx, notice);
}

/// `● Merged the run into main (4 tasks, 7 files, +120 -18)`, then `⎿  Run finished · 42s ·
/// $0.06 · run review saved`, then the turn end.
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
        emit(cx, notice.with_detail(finished));
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

/// A failed run is one block: `The run failed · 0s · $0.00`, what failed in one line beneath,
/// and the run's last word only when it says more than that it failed.
fn failed_run(
    story: &RunStory,
    detail: Option<&str>,
    elapsed: Option<Duration>,
    tasks: Option<(usize, usize)>,
    spend: Option<Spend>,
    cx: &mut Cx<'_>,
) {
    let mut title = "The run failed".to_owned();
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
    let mut notice = Notice::new(Tone::Error, title);
    let said = detail.map(failure::one_line).unwrap_or_default();
    // A replayed block is only kept for the overlay, where no key opens what it hints at.
    let raw = detail
        .and_then(|detail| failure::RawFailure::behind(&said, "The run failed", detail))
        .filter(|_| !cx.replaying());
    let shown = match &raw {
        Some(_) if said.is_empty() => failure::RAW_ALL_HINT.to_owned(),
        Some(_) => format!("{said} · {}", failure::RAW_ALL_HINT),
        None => said.clone(),
    };
    if !shown.is_empty() {
        notice = notice.with_detail(shown);
    }
    emit(cx, notice);
    if raw.is_some() {
        cx.model.raw_failure = raw;
    }
    if let Some(answer) = &story.closing {
        let text = answer.text.trim();
        let reads_back =
            text.starts_with("The run failed") || (!said.is_empty() && text.contains(&said));
        if !reads_back {
            emit_closing(answer, cx);
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
