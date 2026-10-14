//! A resumed run prints a condensed replay: the header, the last conversation turns word for word,
//! one line per milestone, a `※ recap`, then the live state. Every replayed item still joins the
//! history, so Ctrl+O shows everything. A resumed conversation prints the same way: a header, its
//! older turns folded into the history, and its last turns word for word.

use argonauts_core::RunId;
use argonauts_core::api::{RunState, RunSummary};
use argonauts_core::app::{AppEvent, ReplayTurn};
use argonauts_core::cli_sessions::{CliCall, CliStep};
use argonauts_core::events::run::Party;
use argonauts_core::names::{ConversationId, ModelRef};

use crate::bridge::tools;
use crate::cell::{
    AssistantMessage, Cell, CollapsedGroup, Notice, Recap, Thought, Tone, ToolCall, ToolItem,
    UserMessage,
};
use crate::format;

use super::conversation::folds;
use super::{Cx, quiet};

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

/// `● conversation-003 · Fix the flaky login test`, how many turns it had and with whom, then its
/// older turns kept for Ctrl+O and its last ones printed.
pub(super) fn conversation_resumed(
    conversation: &ConversationId,
    title: &str,
    worker: Option<&ModelRef>,
    earlier: &[ReplayTurn],
    turns: &[ReplayTurn],
    cx: &mut Cx<'_>,
) {
    // The conversation takes the screen from the picker that chose it.
    cx.out.close_views = true;
    let title = title.trim();
    let text = if title.is_empty() {
        format!("Resumed {conversation}")
    } else {
        format!("Resumed {conversation} · {title}")
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
        for reprint in turn_cells(turn, cx) {
            cx.model.history.keep(reprint.cell);
        }
    }
    for turn in turns {
        print_turn(turn, cx);
    }
}

/// Your words as you typed them, and the agents' answers as they wrote them.
fn print_turn(turn: &ReplayTurn, cx: &mut Cx<'_>) {
    for reprint in turn_cells(turn, cx) {
        if reprint.prints {
            cx.model.history.push(reprint.cell);
        } else {
            cx.model.history.keep(reprint.cell);
        }
    }
}

/// A cell of a reprinted turn. One that doesn't print joins the Ctrl+O history alone.
struct Reprint {
    cell: Cell,
    prints: bool,
}

/// A turn as it printed when it happened: the words alone, or for a CLI session's turn each
/// thing the agent did, in order. Its calls follow the verbosity as live ones do, and what it
/// thought is kept for Ctrl+O, as Claude Code keeps it.
fn turn_cells(turn: &ReplayTurn, cx: &Cx<'_>) -> Vec<Reprint> {
    if turn.steps.is_empty() {
        let cell = turn_cell(turn);
        return cell
            .map(|cell| Reprint { cell, prints: true })
            .into_iter()
            .collect();
    }
    let agent_id = match &turn.from {
        Party::Agent { worker, .. } => worker.agent.as_str(),
        _ => "",
    };
    let places = tools::Places::of_conversation(cx.state);
    let calls_print = !quiet(cx);
    let mut cells: Vec<Reprint> = Vec::new();
    for step in &turn.steps {
        match step {
            CliStep::Wrote { text } => cells.push(Reprint {
                cell: AssistantMessage::new(text.trim_end()).into(),
                prints: true,
            }),
            CliStep::Thought { text } if text.trim().is_empty() => {}
            CliStep::Thought { text } => cells.push(Reprint {
                cell: Cell::Custom(Box::new(Thought::new(text.as_str()))),
                prints: false,
            }),
            CliStep::Ran(call) => {
                let row = call_row(agent_id, call, &places);
                let group = match cells.last_mut() {
                    Some(Reprint {
                        cell: Cell::Group(group),
                        ..
                    }) => Some(group),
                    _ => None,
                };
                let cell = match (folds(&row, cx), group) {
                    (true, Some(group)) => {
                        group.push(row);
                        continue;
                    }
                    (true, None) => {
                        let mut group = CollapsedGroup::new();
                        group.push(row);
                        group.into()
                    }
                    (false, _) => row.into(),
                };
                cells.push(Reprint {
                    cell,
                    prints: calls_print,
                });
            }
        }
    }
    cells
}

/// A call's row as it stood once the call ended: its patches in place of the preview, then its
/// result. A call the transcript never saw end reads as stopped. What a subagent it started
/// ran folds into its result, as on Claude Code's screen, and Ctrl+O shows each call.
fn call_row(agent_id: &str, call: &CliCall, places: &tools::Places) -> ToolCall {
    let whole = whole_row(agent_id, call, places);
    let is_nested = |item: &ToolItem| matches!(item, ToolItem::Call(_) | ToolItem::Group(_));
    if !whole.body.iter().any(is_nested) {
        return whole;
    }
    let mut folded = whole.clone();
    folded.body.retain(|item| !is_nested(item));
    folded.full = Some(Box::new(whole));
    folded
}

/// The row with every call of its subagent under it, at every depth.
fn whole_row(agent_id: &str, call: &CliCall, places: &tools::Places) -> ToolCall {
    let mut row = tools::start_call_in(agent_id, &call.name, &call.input, places);
    row.kind = tools::kind_of(agent_id, &call.name);
    for step in &call.steps {
        if let CliStep::Ran(inner) = step {
            row.push_call(whole_row(agent_id, inner, places));
        }
    }
    for patch in &call.patches {
        if patch.numbered {
            tools::take_patch_in(&mut row, &patch.path, &patch.hunks, places);
        } else {
            tools::take_unnumbered_patch_in(&mut row, &patch.path, &patch.hunks, places);
        }
    }
    match call.ok {
        Some(ok) => tools::finish_call(&mut row, ok, call.output.as_deref()),
        None => row.stop(),
    }
    row
}

fn turn_cell(turn: &ReplayTurn) -> Option<Cell> {
    let text = turn.text.trim_end();
    if text.trim().is_empty() {
        return None;
    }
    Some(match &turn.from {
        Party::User => UserMessage::new(text).into(),
        Party::OutsideAgent { client } => UserMessage::new(format!("{client}: {text}")).into(),
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
        RunState::Waiting => "Waiting",
        RunState::Crashed => "Stopped when its run host exited · /resume carries it on",
        RunState::NeverStarted => "Never started",
        RunState::FromAnotherBuild => "From an older Argonauts · /runs archive moves it away",
        RunState::Merged => "Merged",
        RunState::PullRequest => "Ready for a pull request",
        RunState::BranchLeft => "Left on its branches",
        RunState::Stopped => "Stopped",
        RunState::Failed => "Failed",
        RunState::Abandoned => "Abandoned",
    }
}
