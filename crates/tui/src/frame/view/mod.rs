//! The views every screen is built from: lists, docks, maps and pagers.

pub mod dock;
pub mod list;
pub mod map;
pub mod pager;
pub mod raw_log;
pub(crate) mod raw_rows;
pub mod states;

use std::fmt;
use std::time::Instant;

use argonauts_core::app::{AnswerTarget, UiCommand, ViewData, ViewRequest};
use argonauts_core::{RunId, TaskId};
use crossterm::event::KeyEvent;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;

use crate::base::theme::Theme;
use crate::frame::state::AppState;

pub trait View: fmt::Debug + Send {
    /// Draws the whole screen, or for a docked view the bottom region it was given.
    fn render(&mut self, area: Rect, buf: &mut Buffer, ctx: &ViewCtx<'_>);

    /// Whether it draws in the bottom region under the conversation, where the input, the mode
    /// line, and the panel were, rather than on a screen of its own.
    fn docked(&self) -> bool {
        false
    }

    /// The rows a docked view takes on a screen of this size, when it knows better than the
    /// usual half: a panel of cards takes only what they need. The caller keeps it on screen.
    fn dock_height(&mut self, screen: ratatui::layout::Size, ctx: &ViewCtx<'_>) -> Option<u16> {
        let _ = (screen, ctx);
        None
    }

    /// Whether a docked view stays up while a question holds the keys. The question then draws
    /// under it, where its keys were, and takes every key until it is answered or passed on.
    /// Any other view closes as a question takes the keys.
    fn stays_over_question(&self) -> bool {
        false
    }

    fn handle_key(&mut self, key: KeyEvent, ctx: &ViewCtx<'_>) -> ViewOutcome;

    /// A bracketed paste. Most views have no line to type into, and the prompt behind them
    /// never gets it either.
    fn handle_paste(&mut self, text: &str, ctx: &ViewCtx<'_>) -> ViewOutcome {
        let _ = (text, ctx);
        ViewOutcome::Ignored
    }

    /// What the view asks the controller for each time it opens, however it was opened, such
    /// as the agent panel's rows: it shows the last ones sent until the new ones arrive.
    fn asks_on_open(&self) -> Option<ViewRequest> {
        None
    }

    /// A controller answer to a request this view made. True when the view took it.
    fn accept(&mut self, data: &ViewData) -> bool {
        let _ = data;
        false
    }

    /// When the view next changes on its own, such as an elapsed counter. `None` when idle.
    fn next_change(&self, now: Instant) -> Option<Instant> {
        let _ = now;
        None
    }

    /// Asked after every event: what shows in its place once the run it was opened for has
    /// moved on, such as the main agent's session once it starts. `None` keeps it.
    fn follow(&mut self, ctx: &ViewCtx<'_>) -> Option<Follow> {
        let _ = ctx;
        None
    }

    /// The map it draws, which can take a picture and the mouse, when it draws one.
    fn map(&mut self) -> Option<&mut dyn map::MapView> {
        None
    }
}

/// Where a view that follows its run goes next.
#[derive(Debug)]
pub enum Follow {
    Replace(Box<dyn View>),
    /// Closes it and asks the controller for the view that follows, such as the workflow view.
    Request(ViewRequest),
}

pub struct ViewCtx<'a> {
    pub theme: &'a Theme,
    pub state: &'a AppState,
    pub now: Instant,
}

#[derive(Debug)]
pub enum ViewOutcome {
    Consumed,
    /// Not a key this view uses.
    Ignored,
    /// Back one view, or to the conversation.
    Close,
    /// Back to the conversation.
    CloseAll,
    Open(Box<dyn View>),
    Replace(Box<dyn View>),
    Send(UiCommand),
    /// Ask the controller; the answer comes back through [`View::accept`].
    Request(ViewRequest),
    /// Back to the conversation with this question's keys, such as Enter on a ◆ row.
    AnswerQuestion(AnswerTarget),
    /// The local confirm before a take-over.
    ConfirmTakeOver {
        run: RunId,
        task: TaskId,
    },
    /// A short line in the notice slot, such as why a key did nothing.
    Notice(String),
    /// Text for the system clipboard, such as an artifact's link.
    Copy(String),
    /// A claude.ai page to open in the browser, by the name the notice says it by.
    OpenLink {
        url: String,
        name: String,
    },
    /// Back to the conversation, where the answer shows, sending this on the way.
    SendToConversation(UiCommand),
    /// Back to the conversation, leaving `line` under an echo of `command`, as Config's Esc
    /// leaves `⎿  Set Editor mode to vim` under `/config`.
    CloseSaying {
        command: String,
        line: String,
    },
    /// [`ViewOutcome::CloseSaying`], then `then` sent on to the conversation, as a Config row
    /// that opens a picker leaves what the tab changed under `/config` before it.
    CloseSayingThen {
        command: String,
        line: String,
        then: UiCommand,
    },
    /// A setting saved now, which the view sums up itself when it closes, so the conversation
    /// doesn't print it as it happens.
    SaveQuietly(UiCommand),
}
