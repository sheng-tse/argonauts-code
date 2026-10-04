//! Everything that goes into scrollback other than agent sessions and cards: the startup
//! header, chat turns, milestones, settings changes, notices, recaps, narrator lines, and the
//! condensed replay of a resumed run. Items print once, whole; only the chat answer streams.

mod chat;
mod gates;
mod handoff;
mod items;
mod lifecycle;
mod milestones;
mod replay;
#[cfg(test)]
mod resumed_tests;
mod shell;
mod status;
mod steps;
#[cfg(test)]
mod tests;
mod workflows;

use argonauts_core::app::{AppEvent, ViewData};
use argonauts_core::{RunId, Verbosity};

use crate::cell::Cell;
use crate::format::WallClock;

use super::Cx;

pub use chat::ChatTurns;
pub use replay::ReplayPrinter;
pub use status::{NOTHING_RUNNING, status_cells, status_sections};

use items::Items;
use milestones::Milestones;
use shell::ShellBlocks;

#[derive(Debug, Default)]
pub struct Transcript {
    pub chat: ChatTurns,
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
        // These print nothing, so the header can go on waiting for the status line.
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
            AppEvent::Chat { event } => self.chat.on_chat(event, clock, cx),
            AppEvent::ChatResumed {
                chat,
                title,
                worker,
                earlier,
                turns,
            } => replay::chat_resumed(chat, title, worker.as_ref(), earlier, turns, cx),
            AppEvent::Report { title, lines } => items::report(title, lines, cx),
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
            AppEvent::View { data } => {
                if let ViewData::Status { summary } = data.as_ref() {
                    for cell in status_cells(summary, &cx.state.names) {
                        emit(cx, cell);
                    }
                }
            }
            _ => {}
        }
    }

    /// Anything you do prints the startup header first, if it still waits for the status line.
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
