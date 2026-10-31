//! The app's views. The workflow and agent views dock in the bottom region under the
//! conversation, as Claude Code's do; the rest take the alternate screen: the workflow
//! library, `/status`, `/context`, `/rewind`, the Learning pickers, the agent panel, a session's
//! view, the resume picker, the pagers, help, `/config`, and `/approvals`. Esc goes back one view,
//! and from the last one to the conversation exactly where it was.
//!
//! Views lay out with ratatui into a `Buffer`, so borders and columns line up at any width
//! ([`crate::base::stability`] checks it).

pub mod agent;
pub mod artifacts;
pub mod asks;
pub mod crew;
pub mod follow;
pub mod goal;
pub mod help;
pub(crate) mod issues;
pub mod learning;
pub mod library;
mod line_edit;
pub mod prompts;
pub mod resume;
pub mod rewind;
pub mod session;
pub mod settings;
pub mod setup_sheet;
mod structured;
#[cfg(test)]
mod tests;
pub mod waiting;
pub mod workflow;

use argonauts_core::RunId;
use argonauts_core::agents::agent_sessions::AgentSessionRef;
use argonauts_core::app::{AppEvent, UiCommand, ViewData};
use argonauts_core::base::names::{RoleName, SessionKey};
use argonauts_core::input::{ResumeTarget, SlashCommand};
use argonauts_core::runs::events::run::{RunOutcome, WaitReason};
use argonauts_core::workflows::steps::StepName;

use crate::base::text::sanitize_line;
use crate::bridge::Cx;
use crate::frame::state::AppState;
use crate::frame::view::{ViewCtx, ViewOutcome};

/// Whether `run` can still be paused or have agents stopped: not finished and not waiting, as far
/// as the screen knows it.
pub(crate) fn run_going(state: &AppState, run: &RunId) -> bool {
    state.runs.get(run).is_none_or(|run| run.is_active())
}

/// Why a key that acts on an agent's session can't act on `row`, which is one of a workflow
/// its CLI runs itself: it has no session of ours, and its CLI alone runs it.
pub(crate) fn inner_why(state: &AppState, row: &argonauts_core::app::views::AgentRow) -> String {
    let label = sanitize_line(&row.label).trim().to_owned();
    let cli = sanitize_line(&state.names.cli(&row.worker.agent))
        .trim()
        .to_owned();
    format!(
        "{label} runs inside {cli}'s own workflow, so only {cli} can message, stop or restart it"
    )
}

/// A restart `r` asked for, waiting for a second `r`.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Restart {
    /// The agent's label, for the line that asks.
    pub label: String,
    /// What the second `r` sends.
    pub command: UiCommand,
}

impl Restart {
    /// The keys' line while it waits.
    pub(crate) fn ask(&self) -> String {
        format!("r again to restart {}", self.label)
    }
}

/// `r` on an agent, where Claude Code restarts an agent of a workflow: the agent's task
/// starts over with the worker it has, in a session of its own, and the work so far is kept.
/// The run does that when a running task is given to the worker that has it. An agent that
/// a script's own step called has no task, and is stopped and asked again. `Err` says why
/// there's nothing to restart, in plain words.
pub(crate) fn restart(
    state: &AppState,
    run: &RunId,
    agent: &argonauts_core::app::views::AgentRow,
) -> Result<Restart, String> {
    if let Some(why) = ended_why(state, run) {
        return Err(why.to_owned());
    }
    if agent.inner.is_some() {
        return Err(inner_why(state, agent));
    }
    let label = sanitize_line(&agent.label).trim().to_owned();
    if agent.status != argonauts_core::app::views::RowStatus::Running {
        return Err(format!(
            "{label} isn't running, so there's nothing to restart"
        ));
    }
    let session = state.session(run, &agent.session);
    // An agent a script's own step called works no task: it is stopped and asked again, and
    // the script waits for the new answer.
    let called_by_a_script = session.is_some_and(|session| {
        session.task.is_none() && session.step.is_none() && session.workflow_step.is_some()
    });
    if called_by_a_script {
        return Ok(Restart {
            label,
            command: UiCommand::RestartAgent {
                run: run.clone(),
                session: agent.session.clone(),
            },
        });
    }
    // Only the session that builds a task starts over with it. A reviewer's or a planner's
    // session is asked again by its step.
    let task = session
        .filter(|session| session.role == RoleName::Worker)
        .filter(|session| matches!(session.step, None | Some(StepName::Build)))
        .and_then(|session| session.task.clone());
    let Some(task) = task else {
        return Err(format!("{label} has no task of its own to start again"));
    };
    let worker = state
        .runs
        .get(run)
        .and_then(|run| run.tasks.get(&task)?.worker.clone())
        .unwrap_or_else(|| agent.worker.clone());
    Ok(Restart {
        label,
        command: UiCommand::Reassign {
            run: run.clone(),
            task,
            to: worker,
        },
    })
}

/// The run has finished, as far as this screen knows, so its sessions are free of it.
pub(crate) fn run_ended(state: &AppState, run: &RunId) -> bool {
    state
        .runs
        .get(run)
        .is_some_and(|run| run.finished.is_some())
}

/// `c` on a finished agent: its session carries on in the conversation as a direct conversation,
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
    ViewOutcome::SendToConversation(UiCommand::Slash {
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

/// `t` on an agent: the take-over confirm while its run goes on and it holds a question, else why
/// not, in plain words.
pub(crate) fn take_over(state: &AppState, run: &RunId, session: &SessionKey) -> ViewOutcome {
    if let Some(why) = ended_why(state, run) {
        return ViewOutcome::Notice(why.to_owned());
    }
    match state
        .session(run, session)
        .and_then(|session| session.task.clone())
    {
        Some(task) => ViewOutcome::ConfirmTakeOver {
            run: run.clone(),
            task,
        },
        None => ViewOutcome::Notice("This agent has no task to take over".to_owned()),
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
            "{} has finished, so it can't take a message. Type in the conversation to guide the run",
            sanitize_line(&state.names.worker(&session.worker)).trim()
        )
    })
}

/// `x x` on a run: the one on screen through `Stop`, any other by name, as `/stop <run>` does.
pub(crate) fn stop(run: &RunId, state: &AppState) -> UiCommand {
    if state.attached.as_ref() == Some(run) {
        UiCommand::Stop
    } else {
        UiCommand::Slash {
            command: SlashCommand::Stop {
                run: Some(run.clone()),
            },
        }
    }
}

/// `p`'s word in a run's views, the workflow's and an open agent's alike: `p resume` while
/// the run is pausing or paused, `p pause` otherwise.
pub(crate) fn pause_hint(state: &AppState, run: &RunId) -> &'static str {
    match pause_state(state, run) {
        Some(Held::Pausing | Held::Paused) => "p resume",
        None => "p pause",
    }
}

/// What `p` does in a run's views: a pause still under way is taken back, a paused run goes
/// on, and any other run on screen pauses at its next safe point.
pub(crate) fn pause_key(run: &RunId, ctx: &ViewCtx<'_>) -> ViewOutcome {
    match pause_state(ctx.state, run) {
        Some(Held::Paused) => ViewOutcome::Send(UiCommand::Resume {
            run: Some(run.clone()),
        }),
        // Nothing waits yet, so there is no wait to resume from: the pause is called off.
        Some(Held::Pausing) if ctx.state.attached.as_ref() == Some(run) => {
            ViewOutcome::Send(UiCommand::CancelPause)
        }
        _ => pause(run, ctx),
    }
}

/// How far a pause has come.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Held {
    /// Asked for; the agents finish their tool calls first.
    Pausing,
    /// The run waits for you.
    Paused,
}

fn pause_state(state: &AppState, run: &RunId) -> Option<Held> {
    let Some(run) = state.runs.get(run) else {
        // A run from an earlier session opens paused from its listing.
        let listed = state.background.iter().find(|listed| listed.run == *run)?;
        return crate::bridge::panel::paused_by_you(listed).then_some(Held::Paused);
    };
    if matches!(run.waiting, Some(WaitReason::Paused)) {
        Some(Held::Paused)
    } else if run.pausing {
        Some(Held::Pausing)
    } else {
        None
    }
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
        ViewData::Workflow { view } => {
            let mut opened = workflow::WorkflowView::new((**view).clone());
            // Opened from a row inside the run in the bottom list, it opens on that workflow.
            let inside = cx.model.bottom.workflows.opening.take();
            if let Some((run, id)) = inside.as_deref().and_then(|key| key.split_once('#'))
                && run == view.run.as_str()
                && let Some(id) = argonauts_core::base::names::WorkflowId::parse(id)
            {
                opened = opened.on_inside(&id);
            }
            // Opened from a council's task, its head says whose task it is in which council.
            let council = &mut cx.model.bottom.council;
            if let Some(task) = council.opening.take()
                && inside.is_some()
            {
                opened = opened.for_council_task(task, council.faces.clone());
            }
            cx.out.open(opened.following(refresh.follower()));
        }
        // A refresh is for a sheet that closed before it came.
        ViewData::SetupSheet { sheet } if sheet.refresh => {}
        ViewData::SetupSheet { sheet } => {
            cx.out
                .open(setup_sheet::SetupSheetView::new((**sheet).clone()));
        }
        ViewData::Agent { view } => cx
            .out
            .open(agent::AgentView::new((**view).clone()).following(refresh.follower())),
        // For the Context tab that asked, which has closed.
        ViewData::SessionContext { .. } => {}
        // Under its tab of the usage panel, beside Status and Usage.
        ViewData::Context { view } => cx.out.open(
            crate::panels::OverviewView::new(
                crate::panels::Tab::Context,
                cx.model.bottom.band.watch.clone(),
                crate::config::panel(),
            )
            .with_context(view.clone()),
        ),
        ViewData::ResumeList {
            conversations,
            sessions,
            in_cli,
        } => cx.out.open(
            resume::ResumePicker::listing(conversations.clone(), sessions.clone())
                .with_in_cli(in_cli.clone()),
        ),
        ViewData::Diff { diff } => cx
            .out
            .open(super::frame::view::pager::Pager::diff(diff.clone())),
        // A refresh is for a /mods that closed before it came.
        ViewData::Mods { view } if view.refresh => {}
        ViewData::Mods { view } => cx.out.open(crate::mods::ModsScreen::new((**view).clone())),
        ViewData::Goal { view } => cx.out.open(goal::GoalView::new((**view).clone())),
        ViewData::Raw { raw } => cx
            .out
            .open(super::frame::view::pager::Pager::raw(raw.clone())),
        // The agent panel lists every repo's runs itself now.
        ViewData::AllRepos { .. } => cx.out.open(crew::AgentPanelView::new(cx.state)),
        ViewData::Settings { .. }
        | ViewData::Models { .. }
        | ViewData::Effort { .. }
        | ViewData::Config { .. }
        | ViewData::Approvals { .. } => settings::open_for(data, cx),
        // The state keeps it for the agent panel.
        ViewData::Crew { .. } => {}
        // A refresh is for a Library tab that closed before it came.
        ViewData::WorkflowLibrary { refresh: true, .. } => {}
        ViewData::WorkflowLibrary { workflows, .. } => {
            cx.out.open(library::LibraryView::new(workflows.clone()));
        }
        // A refresh is for a workflow's page that closed before it came.
        ViewData::WorkflowPage { page } if page.refresh => {}
        ViewData::WorkflowPage { page } => {
            cx.out
                .open(workflow::page::WorkflowPageView::new((**page).clone()));
        }
        ViewData::Checkpoints {
            run,
            task,
            checkpoints,
        } => cx.out.open(rewind::RewindPicker::new(
            run.clone(),
            task.clone(),
            checkpoints.clone(),
        )),
        // A refresh is for a list of prompts that closed before it came.
        ViewData::Prompts { view } if view.refresh => {}
        ViewData::Prompts { view } => {
            cx.out.open(prompts::PromptsView::new((**view).clone()));
        }
        // A refresh is for a picker that closed before it came.
        ViewData::Learning { view } if view.refresh => {}
        ViewData::Learning { view } => {
            cx.out.open(learning::LearningPicker::new((**view).clone()));
        }
        // The state keeps it for the Usage tab and the cards above the input.
        ViewData::Status { .. }
        | ViewData::Usage { .. }
        | ViewData::Overview { .. }
        | ViewData::Behaviors { .. } => {}
        // The conversation prints it.
        ViewData::ContextPrint { .. } => {}
        // Only the usage panel's Status tab draws it, and one that closed takes nothing.
        ViewData::StatusTab { .. } => {}
        // Only the Behaviors tab asks it, and draws its questions in the tab.
        ViewData::DeepOffer { .. } => {}
        // Only a seat's own session view asks it, and one that closed takes nothing.
        ViewData::SeatKey { .. } => {}
        ViewData::Issues { .. } => issues::open(data, cx),
    }
}
