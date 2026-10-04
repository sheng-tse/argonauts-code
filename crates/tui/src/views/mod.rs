//! The app's views. The workflow and agent views dock in the bottom region under the
//! conversation, as Claude Code's do; the rest take the alternate screen: the workflow
//! library, `/status`, `/context`, `/rewind`, the Learning pickers, the crew view, a session's
//! view, the resume picker, the pagers, help, `/config`, and `/approvals`. Esc goes back one view, and from the last one to the chat exactly where it was.
//!
//! Views lay out with ratatui into a `Buffer`, so borders and columns line up at any width
//! ([`crate::stability`] checks it).

pub mod agent;
pub mod columns;
pub mod context;
pub mod crew;
pub mod dock;
pub mod follow;
pub mod goal;
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
pub mod waiting;
pub mod workflow;

use std::fmt;
use std::time::Instant;

use argonauts_core::agent_sessions::AgentSessionRef;
use argonauts_core::app::{AnswerTarget, AppEvent, UiCommand, ViewData, ViewRequest};
use argonauts_core::events::run::RunOutcome;
use argonauts_core::input::{ResumeTarget, SlashCommand};
use argonauts_core::names::SessionKey;
use argonauts_core::{CardId, RunId};
use crossterm::event::KeyEvent;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;

use crate::bridge::Cx;
use crate::state::AppState;
use crate::text::sanitize_line;
use crate::theme::Theme;

pub trait View: fmt::Debug + Send {
    /// Draws the whole screen, or for a docked view the bottom region it was given.
    fn render(&mut self, area: Rect, buf: &mut Buffer, ctx: &ViewCtx<'_>);

    /// Whether it draws in the bottom region under the conversation, where the input, the mode
    /// line, and the panel were, rather than on a screen of its own.
    fn docked(&self) -> bool {
        false
    }

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

    /// Asked after every event: what shows in its place once the run it was opened for has
    /// moved on, such as the lead's session once it starts. `None` keeps it.
    fn follow(&mut self, ctx: &ViewCtx<'_>) -> Option<Follow> {
        let _ = ctx;
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

/// Whether `run` can still be paused or have agents stopped: not finished and not parked, as far
/// as the screen knows it.
pub(crate) fn run_going(state: &AppState, run: &RunId) -> bool {
    state.runs.get(run).is_none_or(|run| run.is_active())
}

/// The run has finished, as far as this screen knows, so its sessions are free of it.
pub(crate) fn run_ended(state: &AppState, run: &RunId) -> bool {
    state
        .runs
        .get(run)
        .is_some_and(|run| run.finished.is_some())
}

/// `c` on a finished agent: its session carries on in the chat as a direct conversation,
/// through its CLI's own resume, once its run ended. Until then the run owns the session, and a
/// running agent is taken over instead.
pub(crate) fn carry_on(
    state: &AppState,
    run: &RunId,
    session: &SessionKey,
    running: bool,
) -> ViewOutcome {
    if running {
        return ViewOutcome::Notice(format!(
            "{session} is still working. t in its agent view takes it over"
        ));
    }
    // The app tells of a run this screen hasn't loaded.
    if state.runs.contains_key(run) && !run_ended(state, run) {
        return ViewOutcome::Notice(format!(
            "{run} hasn't ended, so it still owns {session}. t takes it over, or it carries on \
             once the run ends"
        ));
    }
    ViewOutcome::SendToChat(UiCommand::Slash {
        command: SlashCommand::Resume {
            target: Some(ResumeTarget::Agent(AgentSessionRef {
                run: run.clone(),
                session: session.clone(),
            })),
        },
    })
}

/// Why keys that act on `run`'s agents can't any more, in plain words, once the run has
/// ended. `None` while it goes on.
pub(crate) fn ended_why(state: &AppState, run: &RunId) -> Option<&'static str> {
    let outcome = state.runs.get(run)?.finished?;
    Some(match outcome {
        RunOutcome::Merged => {
            "That run already merged, so its agents are done. To build on its result, type \
             /follow-up and what you want next."
        }
        RunOutcome::PullRequest | RunOutcome::BranchLeft => {
            "That run has finished, so its agents are done. To build on its branch, type \
             /follow-up and what you want next."
        }
        RunOutcome::Stopped => {
            "That run stopped, so its agents are done. To go on with it, type /resume and pick \
             it."
        }
        RunOutcome::Abandoned | RunOutcome::Failed => {
            "That run has ended, so its agents are done. To carry its work on, type /follow-up \
             and what you want next."
        }
    })
}

/// `t` on an agent: the take-over confirm while its run goes on and it holds a card, else why
/// not, in plain words.
pub(crate) fn take_over(state: &AppState, run: &RunId, session: &SessionKey) -> ViewOutcome {
    if let Some(why) = ended_why(state, run) {
        return ViewOutcome::Notice(why.to_owned());
    }
    match state
        .session(run, session)
        .and_then(|session| session.card.clone())
    {
        Some(card) => ViewOutcome::ConfirmTakeOver {
            run: run.clone(),
            card,
        },
        None => ViewOutcome::Notice("This agent has no card to take over".to_owned()),
    }
}

/// Why `i` can't steer an agent now, in plain words: steering needs it running. `None` when
/// it can.
pub(crate) fn steer_refusal(state: &AppState, run: &RunId, session: &SessionKey) -> Option<String> {
    if let Some(why) = ended_why(state, run) {
        return Some(why.to_owned());
    }
    let session = state.session(run, session)?;
    (!session.is_running()).then(|| {
        format!(
            "{} has finished, so it can't take a message. Type in the chat to guide the run",
            sanitize_line(&state.names.worker(&session.worker)).trim()
        )
    })
}

/// `p` in a run's view. A pause acts on the run on screen, so another run's view says so.
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
            agents,
            agents_read,
        } => cx.out.open(
            resume::ResumePicker::listing(runs.clone(), chats.clone(), sessions.clone())
                .with_agents(agents.clone(), agents_read.clone()),
        ),
        // For a picker that closed before they came.
        ViewData::AgentSessions { .. } => {}
        ViewData::Diff { diff } => cx.out.open(pager::Pager::diff(diff.clone())),
        ViewData::Goal { view } => cx.out.open(goal::GoalView::new((**view).clone())),
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
