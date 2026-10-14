//! Everything that goes into scrollback other than agent sessions and tasks: the startup
//! header, conversation turns, milestones, settings changes, notices, recaps, narrator lines, and
//! the condensed replay of a resumed run. Items print once, whole; only the conversation answer
//! streams.

#[cfg(test)]
mod autopilot_picture_tests;
#[cfg(test)]
mod awake_picture_tests;
mod checks;
mod conversation;
#[cfg(test)]
mod crew_start_tests;
mod fallback;
mod items;
mod lifecycle;
#[cfg(test)]
mod live_pictures_tests;
#[cfg(test)]
mod live_rows_tests;
mod milestones;
pub(crate) mod own_calls;
#[cfg(test)]
pub(crate) mod picture_check;
#[cfg(test)]
mod picture_tests;
mod replay;
#[cfg(test)]
mod resumed_tests;
mod shell;
mod status;
mod steps;
#[cfg(test)]
mod templates_picture_tests;
#[cfg(test)]
mod tests;
mod workflows;
#[cfg(test)]
mod working_line_picture_tests;

use argonauts_core::app::{AppEvent, ViewData};
use argonauts_core::{RunId, Verbosity};

use crate::cell::{Cell, UserMessage};
use crate::format::WallClock;
use crate::overview::ContextPrinted;

use super::Cx;

pub use conversation::ConversationTurns;
pub use replay::ReplayPrinter;
pub use status::{NOTHING_RUNNING, status_cells, status_sections};

use items::Items;
use milestones::Milestones;
use shell::ShellBlocks;

#[derive(Debug, Default)]
pub struct Transcript {
    pub conversation: ConversationTurns,
    pub replay: ReplayPrinter,
    items: Items,
    milestones: Milestones,
    shell: ShellBlocks,
    clock: WallClock,
}

impl Transcript {
    /// Clock times such as `done 10:58 AM` read from `clock`, so tests can pin them.
    pub fn with_clock(clock: WallClock) -> Self {
        Self {
            clock,
            ..Self::default()
        }
    }

    pub fn on_event(&mut self, event: &AppEvent, cx: &mut Cx<'_>) {
        let clock = &self.clock;
        // These print nothing, so the header can go on waiting for the bottom line.
        match event {
            AppEvent::Welcome { .. }
            | AppEvent::Status { .. }
            | AppEvent::Mode { .. }
            | AppEvent::UiSettings { .. }
            | AppEvent::NarratorStatus { .. } => {}
            _ => self.items.print_header(cx),
        }
        match event {
            AppEvent::Welcome { .. } => self.items.welcome(event, cx),
            AppEvent::Status { status } => self.items.status(status, cx),
            AppEvent::Attached { .. }
            | AppEvent::CondensedReplay { .. }
            | AppEvent::ReplayDone { .. } => self.replay.on_event(event, cx),
            AppEvent::Run { event } => {
                self.milestones
                    .on_run_event(event, &mut self.items, clock, cx)
            }
            AppEvent::Conversation { event } => self.conversation.on_conversation(event, clock, cx),
            AppEvent::ConversationResumed {
                conversation,
                title,
                worker,
                earlier,
                turns,
                ..
            } => replay::conversation_resumed(
                conversation,
                title,
                worker.as_ref(),
                earlier,
                turns,
                cx,
            ),
            AppEvent::Report { title, lines } => items::report(title, lines, cx),
            AppEvent::Switched { text, undo, caveat } => {
                items::switched(text, undo.as_deref(), caveat.as_deref(), cx);
            }
            AppEvent::Templates { list } => {
                let folder = crate::cell::TemplateFolder::new(list.folder.clone());
                // The rows hang under the command that printed them, as Claude Code's do.
                emit(cx, UserMessage::command("/templates"));
                emit(
                    cx,
                    Cell::Custom(Box::new(crate::cell::TemplateRows::new(list.clone()))),
                );
                emit(cx, Cell::Custom(Box::new(folder)));
            }
            AppEvent::Folded { title, lines, more } => items::folded(title, lines, more, cx),
            AppEvent::Record {
                title,
                tail,
                lines,
                notes,
            } => emit(
                cx,
                Cell::Custom(Box::new(crate::cell::Record {
                    title: title.clone(),
                    tail: tail.clone(),
                    lines: lines.clone(),
                    notes: notes.clone(),
                })),
            ),
            AppEvent::Continued { text } => items::continued(text, cx),
            AppEvent::Recap { text, .. } => items::recap(text, cx),
            AppEvent::SettingChanged {
                key,
                value,
                scope,
                previous,
                reset,
            } => items::setting_changed(key, value, *scope, previous.as_deref(), *reset, cx),
            AppEvent::Notice { level, text } => self.items.notice(*level, text, cx),
            AppEvent::Narrator { line } => items::narrator(line, cx),
            AppEvent::Shell { event } => self.shell.on_event(event, cx),
            AppEvent::View { data } => match data.as_ref() {
                ViewData::Status { summary } => {
                    for cell in status_cells(summary, &cx.state.names) {
                        emit(cx, cell);
                    }
                }
                ViewData::ContextPrint { view } => {
                    // The rows hang under the command that printed them, as Claude Code's do.
                    emit(cx, UserMessage::command("/context print"));
                    let now = crate::bottom::wall_now();
                    let printed = ContextPrinted::of(view, cx.state, now);
                    emit(cx, Cell::Custom(Box::new(printed)));
                }
                _ => {}
            },
            _ => {}
        }
    }

    /// Anything you do prints the startup header first, if it still waits for the bottom line.
    pub fn print_header(&mut self, cx: &mut Cx<'_>) {
        self.items.print_header(cx);
    }
}

/// Finished items go to scrollback. While a resumed run replays its history they only join
/// the history, where Ctrl+O shows them, and the condensed replay prints in their place.
fn emit(cx: &mut Cx<'_>, cell: impl Into<Cell>) {
    if cx.replaying() {
        cx.model.history.keep(cell);
    } else {
        cx.model.history.push(cell);
    }
}

/// The run as people know it, by its title, or by its id before the title is known.
fn run_name(run: &RunId, cx: &Cx<'_>) -> String {
    cx.state
        .runs
        .get(run)
        .map(|state| state.title.trim())
        .filter(|title| !title.is_empty())
        .map_or_else(|| run.to_string(), str::to_owned)
}

/// At `quiet` only milestones show: no info notes, narration, recaps, or folded tool lines.
fn quiet(cx: &Cx<'_>) -> bool {
    cx.verbosity() == Verbosity::Quiet
}
