//! What you do to one task: reassigning it, remembering a fallback pick as a rule, and its
//! fallback checkpoints (`/rewind`).

use argonauts_core::api::ApiError;
use argonauts_core::app::{AppEvent, ChoiceKind, ViewData};
use argonauts_core::events::approval::{AnswerAction, ApprovalPacket};
use argonauts_core::events::task::{TaskEvent, TaskState};
use argonauts_core::events::{ApprovalEvent, RunEvent, RunEventKind, SessionEvent};
use argonauts_core::names::{AgentId, ApprovalId, ModelRef, RoleName};
use argonauts_core::settings::crew::CrewRole;
use argonauts_core::{NoteLevel, RunId, TaskId};

use super::views::run_events;
use super::{Handled, NO_APPROVALS, on_screen};
use crate::choices;
use crate::controller::runs::{RunChoice, RunsState};
use crate::controller::{Ctx, Effect};
use crate::views;

/// From a picker: within the role's rules, else a confirm and `Approvals::reassign_overriding`.
/// Giving a task to the worker it already has starts it over, which breaks no rule.
pub(crate) async fn reassign(
    ctx: &mut Ctx,
    runs: &mut RunsState,
    run: RunId,
    task: TaskId,
    to: ModelRef,
) -> Handled {
    let starts_over = worker_of(&run_events(ctx, runs, &run), &task).as_ref() == Some(&to);
    match ctx
        .services
        .conductor
        .reassign(&run, &task, to.clone())
        .await
    {
        Ok(()) => {
            let name = ctx.names.worker(&to);
            let said = if starts_over {
                format!("Starting {task} over with {name}, with its work saved and a note of it.")
            } else {
                format!("Moving {task} to {name}, with its work saved and a fallback note.")
            };
            ctx.notice(NoteLevel::Info, said);
            Ok(Vec::new())
        }
        // Nothing of the task is at work to start over.
        Err(ApiError::Refused(why)) if starts_over => {
            ctx.notice(NoteLevel::Warn, why);
            Ok(Vec::new())
        }
        // The worker breaks one of the role's rules, which only you can override.
        Err(ApiError::Refused(rule)) => {
            confirm_override(ctx, runs, run, task, to, rule);
            Ok(Vec::new())
        }
        Err(err) => super::refused(ctx, err, || format!("Couldn't reassign {task}")),
    }
}

/// Who works `task` now, as the run's events say.
fn worker_of(events: &[RunEvent], task: &TaskId) -> Option<ModelRef> {
    events
        .iter()
        .rev()
        .filter(|event| event.task_id.as_ref() == Some(task))
        .find_map(|event| match &event.kind {
            RunEventKind::Task(TaskEvent::Try { worker, .. }) => Some(worker.clone()),
            RunEventKind::Task(TaskEvent::Reassigned { to, .. }) => Some(to.clone()),
            _ => None,
        })
}

fn confirm_override(
    ctx: &mut Ctx,
    runs: &mut RunsState,
    run: RunId,
    task: TaskId,
    to: ModelRef,
    rule: String,
) {
    let name = ctx.names.worker(&to);
    if ctx.services.approvals.is_none() {
        ctx.notice(
            NoteLevel::Warn,
            format!("{name} can't take {task}: {rule}. {NO_APPROVALS}"),
        );
        return;
    }
    let id = ctx.next_choice_id();
    let choice = choices::app_choice(
        id.clone(),
        ChoiceKind::ConfirmSafetySetting,
        format!("Give {task} to {name} anyway?"),
        vec![
            format!("{name} breaks a rule of the task's role: {rule}."),
            "Overriding it applies to this task only, and the run's log records it.".to_owned(),
        ],
        vec![
            choices::option('y', "Override the rule", AnswerAction::Approve, false),
            choices::option(
                'n',
                "Keep the task where it is",
                AnswerAction::Cancel,
                false,
            ),
        ],
    );
    runs.choices.insert(
        id,
        RunChoice::OverrideRule {
            run,
            task,
            to,
            rule,
        },
    );
    ctx.send(AppEvent::Choice { choice });
}

/// `/reassign <task> <worker>` or `/reassign --from <agent> <worker>`.
pub(crate) async fn slash_reassign(
    ctx: &mut Ctx,
    runs: &mut RunsState,
    task: Option<TaskId>,
    from: Option<AgentId>,
    to: ModelRef,
) -> Handled {
    let Some(run) = on_screen(ctx, runs) else {
        return Ok(Vec::new());
    };
    let tasks = match (task, from) {
        (Some(task), _) => vec![task],
        (None, Some(agent_id)) => {
            let summary = match ctx.services.conductor.status(&run).await {
                Ok(summary) => summary,
                Err(err) => {
                    return super::refused(ctx, err, || "Couldn't read the run's tasks".to_owned());
                }
            };
            let tasks: Vec<TaskId> = summary
                .tasks
                .into_iter()
                .filter(|brief| {
                    // One already with the new worker stays as it is: giving it to that
                    // worker again would start it over.
                    let moves = brief
                        .worker
                        .as_ref()
                        .is_some_and(|worker| worker.agent_id() == agent_id && *worker != to);
                    moves && !matches!(brief.state, TaskState::Merged | TaskState::Cancelled)
                })
                .map(|brief| brief.task)
                .collect();
            if tasks.is_empty() {
                let name = ctx.names.cli(&agent_id.0);
                ctx.notice(
                    NoteLevel::Info,
                    format!("{run} has no open task to move from {name}."),
                );
            }
            tasks
        }
        (None, None) => {
            ctx.notice(
                NoteLevel::Info,
                "Name a task or an agent: /reassign <task> <worker>, or /reassign --from <agent> \
                 <worker>.",
            );
            return Ok(Vec::new());
        }
    };
    let mut effects = Vec::new();
    for task in tasks {
        effects.extend(reassign(ctx, runs, run.clone(), task, to.clone()).await?);
    }
    Ok(effects)
}

/// Saves an answered fallback question's pick as a fallback rule, after the scope question.
pub(crate) async fn remember_pick(
    ctx: &mut Ctx,
    runs: &mut RunsState,
    run: RunId,
    id: ApprovalId,
) -> Handled {
    remember(ctx, runs, &run, &id, None)
}

/// `pick` is the answer just sent; without it, the pick the run's log recorded.
pub(super) fn remember(
    ctx: &mut Ctx,
    runs: &mut RunsState,
    run: &RunId,
    id: &ApprovalId,
    pick: Option<ModelRef>,
) -> Handled {
    let events = run_events(ctx, runs, run);
    let Some(rule) = fallback_rule(&events, id, pick) else {
        ctx.notice(
            NoteLevel::Info,
            format!("{id} has no fallback pick to remember."),
        );
        return Ok(Vec::new());
    };
    Ok(vec![Effect::SaveChanges { pairs: vec![rule] }])
}

/// `roles.<role>.fallback.replace.<agent>.with = [pick]` for the stopped worker's role and
/// agent, as the key and its TOML value. Only the `with` list changes, so the table's strategy
/// and `on_return` stay.
pub(super) fn fallback_rule(
    events: &[RunEvent],
    id: &ApprovalId,
    pick: Option<ModelRef>,
) -> Option<(String, String)> {
    let plan = events.iter().find_map(|event| match &event.kind {
        RunEventKind::Approval(ApprovalEvent::Opened(opened)) if &opened.id == id => {
            match &opened.packet {
                ApprovalPacket::Fallback(plan) => Some(plan),
                _ => None,
            }
        }
        _ => None,
    })?;
    let pick = pick.or_else(|| recorded_pick(events, id))?;
    let role = stopped_role(events, &plan.task, &plan.stopped);
    let key = format!(
        "fallback.{}.replace.{}.with",
        CrewRole::fallback_of(role).as_str(),
        plan.stopped.agent_id()
    );
    let value = toml::Value::Array(vec![toml::Value::String(pick.to_string())]).to_string();
    Some((key, value))
}

fn recorded_pick(events: &[RunEvent], id: &ApprovalId) -> Option<ModelRef> {
    events.iter().rev().find_map(|event| match &event.kind {
        RunEventKind::Approval(ApprovalEvent::Decided(decided)) if &decided.id == id => {
            match &decided.action {
                AnswerAction::Candidate { worker } => Some(worker.clone()),
                _ => None,
            }
        }
        _ => None,
    })
}

/// The role the stopped worker had on the task; a worker when the log doesn't say.
fn stopped_role(events: &[RunEvent], task: &TaskId, stopped: &ModelRef) -> RoleName {
    let sessions = events.iter().rev().filter_map(|event| match &event.kind {
        RunEventKind::Session(SessionEvent::Started(started))
            if event.task_id.as_ref() == Some(task) =>
        {
            Some(started)
        }
        _ => None,
    });
    let mut any = None;
    for started in sessions {
        if &started.worker == stopped {
            return started.role;
        }
        any.get_or_insert(started.role);
    }
    any.unwrap_or(RoleName::Worker)
}

/// `/rewind [task]`: a picker of the task's fallback checkpoints, or every task's. Picking one
/// sends `UiCommand::Rewind`, which asks before it rewinds.
pub(crate) async fn checkpoints(
    ctx: &mut Ctx,
    runs: &mut RunsState,
    task: Option<TaskId>,
) -> Handled {
    let Some(run) = on_screen(ctx, runs) else {
        return Ok(Vec::new());
    };
    let events = run_events(ctx, runs, &run);
    let checkpoints = views::checkpoint_rows(&events, task.as_ref());
    if checkpoints.is_empty() {
        let text = match &task {
            Some(task) => format!("{task} has no fallback checkpoints to go back to."),
            None => format!("No task of {run} has a fallback checkpoint to go back to."),
        };
        ctx.notice(NoteLevel::Info, text);
        return Ok(Vec::new());
    }
    ctx.send(AppEvent::View {
        data: Box::new(ViewData::Checkpoints {
            run,
            task,
            checkpoints,
        }),
    });
    Ok(Vec::new())
}

/// The checkpoint picked in `/rewind`: a `ConfirmRewind` question first.
pub(crate) async fn rewind(
    ctx: &mut Ctx,
    runs: &mut RunsState,
    task: TaskId,
    checkpoint: u32,
) -> Handled {
    let Some(run) = on_screen(ctx, runs) else {
        return Ok(Vec::new());
    };
    let id = ctx.next_choice_id();
    let choice = choices::app_choice(
        id.clone(),
        ChoiceKind::ConfirmRewind,
        format!("Rewind {task} to fallback-{checkpoint}?"),
        vec![
            format!("Its branch goes back to argonauts/{run}/{task}/fallback-{checkpoint}."),
            format!(
                "The work after it stays on argonauts/{run}/{task}-replaced-{checkpoint}, so \
                 nothing is lost."
            ),
        ],
        choices::options(ChoiceKind::ConfirmRewind),
    );
    runs.choices.insert(
        id,
        RunChoice::Rewind {
            run,
            task,
            checkpoint,
        },
    );
    ctx.send(AppEvent::Choice { choice });
    Ok(Vec::new())
}
