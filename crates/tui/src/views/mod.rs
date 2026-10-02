//! Full-screen views on the alternate screen: the workflow and agent views, the workflow
//! library, `/status`, `/context`, `/rewind`, the Learning pickers, the crew view, a session's
//! view, the resume picker, the pagers, help, `/config`, and `/approvals`. Esc goes back one view, and from the last one to the chat exactly where it was.
//!
//! Views lay out with ratatui into a `Buffer`, so borders and columns line up at any width
//! ([`crate::stability`] checks it).

pub mod agent;
pub mod context;
pub mod crew;
pub mod follow;
pub mod help;
pub mod layout;
pub mod learning;
pub mod library;
pub mod list;
pub mod pager;
pub mod panes;
pub mod resume;
pub mod rewind;
pub mod session;
pub mod settings;
pub mod status;
#[cfg(test)]
mod vim_tests;
pub mod workflow;

use std::fmt;
use std::time::Instant;

use argonauts_core::app::{AnswerTarget, AppEvent, UiCommand, ViewData, ViewRequest};
use argonauts_core::{CardId, RunId};
use crossterm::event::KeyEvent;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;

use crate::bridge::Cx;
use crate::state::AppState;
use crate::theme::Theme;

pub trait View: fmt::Debug + Send {
    /// Draws the whole screen.
    fn render(&mut self, area: Rect, buf: &mut Buffer, ctx: &ViewCtx<'_>);

    fn handle_key(&mut self, key: KeyEvent, ctx: &ViewCtx<'_>) -> ViewOutcome;

    /// A bracketed paste. Most views have no line to type into, and the prompt behind them
    /// never gets it either.
    fn handle_paste(&mut self, text: &str, ctx: &ViewCtx<'_>) -> ViewOutcome {
        let _ = (text, ctx);
        ViewOutcome::Ignored
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
    /// Back one view, or to the chat.
    Close,
    /// Back to the chat.
    CloseAll,
    Open(Box<dyn View>),
    Replace(Box<dyn View>),
    Send(UiCommand),
    /// Ask the controller; the answer comes back through [`View::accept`].
    Request(ViewRequest),
    /// Back to the chat with this card's keys, such as Enter on a ◆ row.
    AnswerCard(AnswerTarget),
    /// The local confirm before a take-over.
    ConfirmTakeOver {
        run: RunId,
        card: CardId,
    },
    /// A short line in the notice slot, such as why a key did nothing.
    Notice(String),
    /// Back to the chat, where the answer shows, sending this on the way.
    SendToChat(UiCommand),
}

/// `p` in a run's view. A pause acts on the run on screen, so another run's view says so.
/// Whether `run` can still be paused or have agents stopped: not finished and not parked, as far
/// as the screen knows it.
pub(crate) fn run_going(state: &AppState, run: &RunId) -> bool {
    state.runs.get(run).is_none_or(|run| run.is_active())
}

pub(crate) fn pause(run: &RunId, ctx: &ViewCtx<'_>) -> ViewOutcome {
    if ctx.state.attached.as_ref() == Some(run) {
        ViewOutcome::Send(UiCommand::Pause { now: false })
    } else {
        ViewOutcome::Notice(format!(
            "Only the run on screen can be paused. Resume {run} first, then pause it"
        ))
    }
}

/// Opens what a controller answer calls for when no open view took it. `Status` prints into
/// the transcript instead, and settings answers go to [`settings::open_for`]. The workflow
/// and agent views keep up with their run through `refresh`.
pub fn on_event(event: &AppEvent, cx: &mut Cx<'_>, refresh: &follow::Refresh) {
    let AppEvent::View { data } = event else {
        return;
    };
    match data.as_ref() {
        ViewData::Workflow { view } => cx
            .out
            .open(workflow::WorkflowView::new((**view).clone()).following(refresh.follower())),
        ViewData::Agent { view } => cx
            .out
            .open(agent::AgentView::new((**view).clone()).following(refresh.follower())),
        ViewData::Context { view } => cx.out.open(context::ContextView::new(view.clone())),
        ViewData::Runs {
            runs,
            chats,
            sessions,
        } => cx.out.open(resume::ResumePicker::listing(
            runs.clone(),
            chats.clone(),
            sessions.clone(),
        )),
        ViewData::Diff { diff } => cx.out.open(pager::Pager::diff(diff.clone())),
        ViewData::Raw { raw } => cx.out.open(pager::Pager::raw(raw.clone())),
        // The crew panel lists every repo's runs itself now.
        ViewData::AllRepos { .. } => cx.out.open(crew::CrewView::new(cx.state)),
        ViewData::Settings { .. }
        | ViewData::Models { .. }
        | ViewData::Effort { .. }
        | ViewData::Config { .. }
        | ViewData::Approvals { .. } => settings::open_for(data, cx),
        // The state keeps it for the crew panel.
        ViewData::Crew { .. } => {}
        // A refresh is for a Library tab that closed before it came.
        ViewData::WorkflowLibrary { refresh: true, .. } => {}
        ViewData::WorkflowLibrary { workflows, .. } => {
            cx.out.open(library::LibraryView::new(workflows.clone()));
        }
        ViewData::Checkpoints {
            run,
            card,
            checkpoints,
        } => cx.out.open(rewind::RewindPicker::new(
            run.clone(),
            card.clone(),
            checkpoints.clone(),
        )),
        // A refresh is for a picker that closed before it came.
        ViewData::Learning { view } if view.refresh => {}
        ViewData::Learning { view } => {
            cx.out.open(learning::LearningPicker::new((**view).clone()));
        }
        ViewData::Status { .. } => {}
    }
}
