//! Reassign, take over, and stopping one session.

use std::path::PathBuf;
use std::time::Duration;

use argonauts_core::agents::session_runner::SessionControl;
use argonauts_core::base::names::{ModelRef, RoleName, SessionKey};
use argonauts_core::runs::api::{ApprovalAnswer, TakeOverTicket};
use argonauts_core::runs::events::approval::{AnswerAction, ApprovalKind, ApprovalOpened};
use argonauts_core::runs::events::run::Party;
use argonauts_core::runs::events::task::{TaskEvent, TaskState};
use argonauts_core::{DiffStat, TaskId};

use crate::approvals;
use crate::engine::error::{EngineError, EngineResult};
use crate::engine::host::Host;
use crate::engine::shared::lock;
use crate::roles;
use crate::state::tasks::TaskEntry;
use crate::task::machine::Trigger;
use crate::task::pipeline::transition;
use crate::task::support::{
    newest_worker_session, progress_of, record_of, record_task, state_of, wait_until, worker_of,
};
use crate::task::worktree;

/// How long `/reassign` waits for the worker's report after the wrap-up before it stops the
/// session: briefly, since you asked for the task to move now.
const WRAP_UP_WAIT: Duration = Duration::from_secs(30);
/// How long a take-over waits for the session to stop after its tool call.
const STOP_WAIT: Duration = Duration::from_secs(30);

/// Stops one session; its task goes to `NeedsDecision` with a stuck-style approval.
pub async fn stop_session(host: &Host, session: &SessionKey) -> EngineResult<()> {
    let task = host.read(|state| state.sessions.get(session).and_then(|s| s.task.clone()));
    // The task moves first, so its pipeline sees your stop when the session ends.
    if let Some(task) = &task
        && state_of(host, task)? == TaskState::Running
    {
        transition(
            host,
            task,
            Trigger::NeedsDecision,
            "you stopped its session",
        )?;
    }
    host.control(session, SessionControl::Stop)
        .map_err(EngineError::Refused)
}

/// Stops an agent that a script's own step called and has it start over with the prompt it
/// had, as a reviewer or a judge there does: it works no task, and the script waits for its
/// answer, which is then the new session's. A task's worker starts over through [`restart`],
/// which keeps its work.
pub fn restart_session(host: &Host, session: &SessionKey) -> EngineResult<()> {
    let called_by_a_script = host.read(|state| {
        state.sessions.get(session).is_some_and(|session| {
            session.task.is_none()
                && session.started.step.is_none()
                && session.started.workflow_step.is_some()
        })
    });
    if !called_by_a_script {
        return Err(EngineError::Refused(format!(
            "{session} isn't an agent a workflow's own step called, so nothing waits to \
             start it over"
        )));
    }
    lock(&host.shared().starting_over).insert(session.clone());
    let stopped = host.control(session, SessionControl::Stop);
    if stopped.is_err() {
        lock(&host.shared().starting_over).remove(session);
    }
    stopped.map_err(EngineError::Refused)
}

/// Sends the wrap-up, waits briefly for the report, stops, commits, builds the note, and
/// starts the new worker. Only candidates the role allows, unless you override one rule for this
/// task (already checked against the token, and logged).
///
/// The worker changes at once; the task's pipeline commits the stopped session's work and
/// starts the new worker with the note.
///
/// Giving a running task to the worker it already has starts it over: see [`restart`].
pub async fn reassign(
    host: &Host,
    task: &TaskId,
    to: ModelRef,
    rule_override: Option<String>,
) -> Result<(), String> {
    reassign_task(host, task, to, rule_override)
        .await
        .map_err(|err| err.to_string())
}

async fn reassign_task(
    host: &Host,
    task: &TaskId,
    to: ModelRef,
    rule_override: Option<String>,
) -> EngineResult<()> {
    let record = record_of(host, task)?;
    if matches!(record.state, TaskState::Merged | TaskState::Cancelled) {
        return Err(EngineError::Refused(format!(
            "{task} is {:?}, so it can't move",
            record.state
        )));
    }
    let from = worker_of(host, &record);
    if from == to {
        return restart(host, task, &record, &to);
    }
    if let Some(rule) = broken_rule(host, &record, &from, &to)
        && rule_override.is_none()
    {
        return Err(EngineError::Refused(rule));
    }
    let event = TaskEvent::Reassigned {
        from: Some(from),
        to: to.clone(),
        by: Party::User,
        rule_override,
    };
    record_task(host, task, event)?;
    let reason = format!("you reassigned it to {to}");
    match record.state {
        TaskState::Running => wrap_up_and_stop(host, task).await?,
        TaskState::Failed => {
            transition(host, task, Trigger::Retry, &reason)?;
        }
        TaskState::NeedsYou => {
            transition(host, task, Trigger::NewWorker, &reason)?;
        }
        TaskState::NeedsDecision => {
            transition(host, task, Trigger::Decided, &reason)?;
        }
        _ => {}
    }
    Ok(())
}

/// Starts a running task over with the worker it has. The session at work is told to stop,
/// and when it ends the task's pipeline commits its work and starts a session of its own
/// with a note of that work. No rule is checked, since who works the task doesn't change.
fn restart(host: &Host, task: &TaskId, record: &TaskEntry, worker: &ModelRef) -> EngineResult<()> {
    let nothing_at_work = || {
        EngineError::Refused(format!(
            "{worker} already has {task}, and no session of it is at work to restart"
        ))
    };
    let live = live_session(host, task).filter(|_| record.state == TaskState::Running);
    let session = live.ok_or_else(nothing_at_work)?;
    // Marked before the stop, so the session's end finds the mark however soon it comes.
    lock(&host.shared().restarting).insert(task.clone());
    if host.control(&session, SessionControl::Stop).is_err() {
        // It ended by itself meanwhile, and its task has moved on.
        lock(&host.shared().restarting).remove(task);
        return Err(nothing_at_work());
    }
    Ok(())
}

/// Why `to` may not own the task: the role doesn't list it, it shares a provider with one of the
/// task's reviewers, or another role rule.
fn broken_rule(host: &Host, record: &TaskEntry, from: &ModelRef, to: &ModelRef) -> Option<String> {
    let allowed = roles::candidates(host.settings(), RoleName::Worker);
    if !allowed.is_empty() && !allowed.contains(to) {
        return Some(format!("roles.worker doesn't list {to}"));
    }
    let provider = roles::provider(host, to);
    if let Some(reviewer) = record
        .spec
        .reviewers
        .iter()
        .find(|reviewer| roles::provider(host, reviewer) == provider)
    {
        return Some(format!(
            "{to} is the same provider as {reviewer}, who reviews {}",
            record.spec.id
        ));
    }
    roles::conflict(RoleName::Worker, to, Some(from), &|w| {
        roles::provider(host, w)
    })
}

async fn wrap_up_and_stop(host: &Host, task: &TaskId) -> EngineResult<()> {
    let Some(session) = live_session(host, task) else {
        return Ok(());
    };
    if host.control(&session, SessionControl::WrapUp).is_ok()
        && ended_within(host, &session, WRAP_UP_WAIT).await?
    {
        return Ok(());
    }
    let _ = host.control(&session, SessionControl::Stop);
    Ok(())
}

/// Pauses the task (`StopAfterTool`), commits, and returns the agent's take-over command in
/// the task's worktree.
pub async fn take_over(host: &Host, task: &TaskId) -> Result<TakeOverTicket, String> {
    take_over_task(host, task)
        .await
        .map_err(|err| err.to_string())
}

async fn take_over_task(host: &Host, task: &TaskId) -> EngineResult<TakeOverTicket> {
    let record = record_of(host, task)?;
    // The fallback question this take-over answers takes the task over too, and finds it yours.
    if held_by_you(host, task)? {
        let tree = worktree::ensure(host, task).await?;
        return Ok(ticket(host, task, worker_of(host, &record), tree.path));
    }
    let open = matches!(
        record.state,
        TaskState::Running | TaskState::NeedsDecision | TaskState::NeedsYou
    );
    if !open {
        return Err(EngineError::Refused(format!(
            "{task} is {:?}; only a running, stopped, or needs-you task can be taken over",
            record.state
        )));
    }
    let worker = worker_of(host, &record);
    let tree = worktree::ensure(host, task).await?;
    let live = live_session(host, task);
    transition(host, task, Trigger::TakeOver, "you took it over")?;
    if let Some(session) = &live {
        let stopped = host.control(session, SessionControl::StopAfterTool).is_ok()
            && ended_within(host, session, STOP_WAIT).await?;
        if !stopped {
            let _ = host.control(session, SessionControl::Stop);
            ended_within(host, session, STOP_WAIT).await?;
        }
    }
    worktree::commit_in(host, task, &tree, live.as_ref(), Some(worker.clone())).await?;
    let ticket = ticket(host, task, worker, tree.path);
    let argv = ticket.command.as_ref().map(|spec| {
        std::iter::once(spec.program.clone())
            .chain(spec.args.iter().cloned())
            .collect()
    });
    record_task(host, task, TaskEvent::TakenOver { command: argv })?;
    answer_fallback_question(host, task);
    Ok(ticket)
}

/// A take-over is the "Take over myself" a stopped task's fallback question offers, so that
/// question closes with it. Its handler then finds the task already yours.
fn answer_fallback_question(host: &Host, task: &TaskId) {
    let open: Vec<ApprovalOpened> = host.read(|state| {
        state
            .approvals
            .by_id
            .values()
            .filter(|record| {
                record.is_open()
                    && record.opened.kind == ApprovalKind::Fallback
                    && record.opened.task.as_ref() == Some(task)
            })
            .map(|record| record.opened.clone())
            .collect()
    });
    for opened in open {
        let answer = ApprovalAnswer {
            id: opened.id,
            action: AnswerAction::TakeOver,
            text: None,
            evidence_sha256: opened.evidence_sha256,
            tasks: Vec::new(),
        };
        if let Err(why) = approvals::answer(host, answer) {
            tracing::warn!(
                "the fallback question of {task} stayed open after the take-over: {why}"
            );
        }
    }
}

/// Taken over and not given back yet.
fn held_by_you(host: &Host, task: &TaskId) -> EngineResult<bool> {
    Ok(state_of(host, task)? == TaskState::TakenOver
        && progress_of(host, task).take_over_end.is_none())
}

/// The worker's agent UI on its newest session, in the task's worktree.
fn ticket(host: &Host, task: &TaskId, worker: ModelRef, worktree: PathBuf) -> TakeOverTicket {
    let cli_session =
        host.read(|state| newest_worker_session(state, task).and_then(|s| s.cli_session.clone()));
    let command = match (cli_session, host.services().workers.get(&worker)) {
        (Some(id), Ok(model)) => model.take_over_command(&id, &worktree),
        _ => None,
    };
    TakeOverTicket {
        task: task.clone(),
        command,
        worktree,
        agent_id: worker.agent,
    }
}

/// Commits your changes and opens the take-over-return approval: let the worker continue
/// (told what changed) or keep it yours (checks and review run when you say done).
///
/// This records the end of your take-over; the task's pipeline opens the approval, so the
/// command never waits on your answer.
pub async fn take_over_ended(host: &Host, task: &TaskId) -> EngineResult<()> {
    let record = record_of(host, task)?;
    if record.state != TaskState::TakenOver || progress_of(host, task).take_over_end.is_some() {
        return Ok(());
    }
    let tree = worktree::ensure(host, task).await?;
    let commit = worktree::commit_in(host, task, &tree, None, None).await?;
    let (changed, sha) = match commit {
        Some(info) => (info.stat, info.sha),
        None => (DiffStat::default(), None),
    };
    let ended = TaskEvent::TakeOverEnded {
        changed,
        commit: sha,
    };
    record_task(host, task, ended)?;
    Ok(())
}

/// The task's worker session that is still running, if any.
fn live_session(host: &Host, task: &TaskId) -> Option<SessionKey> {
    host.read(|state| {
        newest_worker_session(state, task)
            .filter(|session| session.end.is_none())
            .map(|session| session.key.clone())
    })
}

/// True when the session's end is recorded within `limit`.
async fn ended_within(host: &Host, session: &SessionKey, limit: Duration) -> EngineResult<bool> {
    let ended = |state: &crate::state::RunState| {
        state
            .sessions
            .get(session)
            .is_none_or(|record| record.end.is_some())
    };
    match tokio::time::timeout(limit, wait_until(host, ended)).await {
        Ok(result) => result.map(|()| true),
        Err(_) => Ok(false),
    }
}
