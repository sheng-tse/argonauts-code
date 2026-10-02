//! A resumed run prints a condensed replay: the header, the last chat turns word for word, one
//! line per milestone, a `※ recap`, then the live state. Every replayed item still joins the
//! history, so Ctrl+O shows everything. A resumed conversation prints the same way: a header,
//! its older turns folded into the history, and its last turns word for word.

use argonauts_core::RunId;
use argonauts_core::api::{RunState, RunSummary};
use argonauts_core::app::{AppEvent, ReplayTurn};
use argonauts_core::events::run::Party;
use argonauts_core::names::{ChatId, WorkerRef};

use crate::cell::{AssistantMessage, Notice, Recap, Tone, UserMessage};
use crate::format;

use super::Cx;

#[derive(Debug, Default)]
pub struct ReplayPrinter {
    /// The run being replayed, from `Attached`.
    summary: Option<RunSummary>,
    /// The condensed replay printed, so `ReplayDone` adds nothing.
    printed: bool,
    /// A run this app just started, which needs no header over the prompt that started it.
    started_here: bool,
}

impl ReplayPrinter {
    pub fn on_event(&mut self, event: &AppEvent, cx: &mut Cx<'_>) {
        match event {
            AppEvent::Attached {
                summary,
                started_here,
            } => {
                self.summary = Some(summary.clone());
                self.printed = false;
                self.started_here = *started_here;
            }
            AppEvent::CondensedReplay {
                run,
                turns,
                milestones,
                recap,
                ..
            } => {
                self.header(run, cx);
                for turn in turns {
                    print_turn(turn, cx);
                }
                print_milestones(milestones, cx);
                if cx.settings.recaps && !recap.trim().is_empty() {
                    cx.model.history.push(Recap::new(recap.trim()));
                }
                self.printed = true;
            }
            AppEvent::ReplayDone { run, .. } => {
                // A resumed run with no history sends no condensed replay, and still gets its
                // header.
                if !self.printed && !self.started_here {
                    self.header(run, cx);
                }
                self.summary = None;
                self.printed = false;
                self.started_here = false;
            }
            _ => {}
        }
    }

    /// `● Add a --json flag to export` and where it stands.
    fn header(&self, run: &RunId, cx: &mut Cx<'_>) {
        let summary = self.summary.as_ref().filter(|summary| summary.run == *run);
        let Some(summary) = summary else {
            let name = super::run_name(run, cx);
            cx.model.history.push(Notice::new(Tone::Normal, name));
            return;
        };
        let title = summary.title.trim();
        let text = if title.is_empty() {
            run.to_string()
        } else {
            title.to_owned()
        };
        let mut parts = vec![state_words(summary.state).to_owned()];
        let stopped_at = summary.stopped_at.trim();
        if !stopped_at.is_empty() {
            parts.push(stopped_at.to_owned());
        }
        parts.push(format!("{} spent", format::spend(&summary.spent)));
        if summary.sessions_to_resume > 0 {
            let sessions = usize::try_from(summary.sessions_to_resume).unwrap_or(usize::MAX);
            parts.push(format!(
                "{} to resume",
                super::steps::plural(sessions, "session", "sessions")
            ));
        }
        let notice = Notice::new(Tone::Normal, text)
            .with_detail(parts.join(" · "))
            .with_detail("ctrl+o shows the whole history");
        cx.model.history.push(notice);
    }
}

/// `● chat-003 · Fix the flaky login test`, how many turns it had and with whom, then its
/// older turns kept for Ctrl+O and its last ones printed.
pub(super) fn chat_resumed(
    chat: &ChatId,
    title: &str,
    worker: Option<&WorkerRef>,
    earlier: &[ReplayTurn],
    turns: &[ReplayTurn],
    cx: &mut Cx<'_>,
) {
    // The conversation takes the screen from the picker that chose it.
    cx.out.close_views = true;
    let title = title.trim();
    let text = if title.is_empty() {
        format!("Resumed {chat}")
    } else {
        format!("Resumed {chat} · {title}")
    };
    let mut parts = Vec::new();
    if let Some(worker) = worker {
        parts.push(format!("with {}", cx.state.names.worker(worker)));
    }
    let folded = earlier
        .iter()
        .filter(|turn| matches!(turn.from, Party::User))
        .count();
    if folded > 0 {
        parts.push(format!(
            "{} folded",
            super::steps::plural(folded, "earlier turn", "earlier turns")
        ));
    }
    let mut notice = Notice::new(Tone::Normal, text);
    if !parts.is_empty() {
        notice = notice.with_detail(parts.join(" · "));
    }
    if folded > 0 {
        notice = notice.with_detail("ctrl+o shows the whole history");
    }
    cx.model.history.push(notice);
    for turn in earlier {
        if let Some(cell) = turn_cell(turn) {
            cx.model.history.keep(cell);
        }
    }
    for turn in turns {
        print_turn(turn, cx);
    }
}

/// Your words as you typed them, and the agents' answers as they wrote them.
fn print_turn(turn: &ReplayTurn, cx: &mut Cx<'_>) {
    if let Some(cell) = turn_cell(turn) {
        cx.model.history.push(cell);
    }
}

fn turn_cell(turn: &ReplayTurn) -> Option<crate::cell::Cell> {
    let text = turn.text.trim_end();
    if text.trim().is_empty() {
        return None;
    }
    Some(match &turn.from {
        Party::User => UserMessage::new(text).into(),
        Party::FrontEnd { client } => UserMessage::new(format!("{client}: {text}")).into(),
        _ => AssistantMessage::new(text).into(),
    })
}

/// One line per milestone under one item. The run's title heads the replay just above.
fn print_milestones(milestones: &[String], cx: &mut Cx<'_>) {
    let lines: Vec<&str> = milestones
        .iter()
        .map(|line| line.trim())
        .filter(|line| !line.is_empty())
        .collect();
    if lines.is_empty() {
        return;
    }
    let mut notice = Notice::new(Tone::Dim, "So far in this run");
    for line in lines {
        notice = notice.with_detail(line);
    }
    cx.model.history.push(notice);
}

fn state_words(state: RunState) -> &'static str {
    match state {
        RunState::Running => "Running",
        RunState::Pausing => "Pausing",
        RunState::Parked => "Parked",
        RunState::Crashed => "Stopped by a crash · /resume recovers it",
        RunState::Merged => "Merged",
        RunState::PullRequest => "Ready for a pull request",
        RunState::BranchLeft => "Left on its branches",
        RunState::Stopped => "Stopped",
        RunState::Failed => "Failed",
        RunState::Abandoned => "Abandoned",
    }
}
