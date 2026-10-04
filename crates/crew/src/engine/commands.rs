//! Commands from the inbox. Each goes to the module that owns it; the loop never waits on a
//! long one (reassign, take over), which runs as its own task.

use std::collections::BTreeSet;
use std::io::{BufRead, BufReader};

use argonauts_core::CardId;
use argonauts_core::api::{RawView, StatusSummary};
use argonauts_core::engine::EngineCommand;
use argonauts_core::events::RunEventKind;
use argonauts_core::events::run::{Party, RunLifecycle};
use argonauts_core::layout::{self, session_files};
use argonauts_core::names::SessionKey;
use argonauts_core::settings::approvals::ApprovalMode;
use argonauts_core::worker::{Access, SessionControl};

use crate::engine::host::Host;
use crate::engine::shared::{Admission, lock};
use crate::error::EngineResult;
use crate::status::Viewer;
use crate::{blackboard, card, checkpoints, goal, handoff, integrate, workflow};

pub async fn handle(host: &Host, command: EngineCommand) -> EngineResult<()> {
    match command {
        EngineCommand::Answer { answer, reply } => {
            let _ = reply.send(checkpoints::answer(host, answer));
        }
        EngineCommand::Steer {
            session,
            text,
            reply,
        } => {
            let _ = reply.send(blackboard::commands::steer(host, &session, text).await);
        }
        EngineCommand::Mention {
            target,
            text,
            reply,
        } => {
            let _ = reply.send(blackboard::commands::mention(host, target, text).await);
        }
        EngineCommand::Guidance { text, reply } => {
            let _ = reply.send(blackboard::commands::guidance(host, text).await);
        }
        EngineCommand::TakeBack { messages, reply } => {
            let _ = reply.send(blackboard::commands::take_back(host, &messages));
        }
        EngineCommand::Pause { now } => pause(host, now).await?,
        EngineCommand::CancelPause => cancel_pause(host)?,
        EngineCommand::Stop => stop(host).await?,
        EngineCommand::StopSession { session } => {
            card::takeover::stop_session(host, &session).await?;
        }
        EngineCommand::ApprovalMode { mode } => approval_mode(host, mode)?,
        EngineCommand::Reassign {
            card: id,
            to,
            rule_override,
            reply,
        } => {
            let _ = reply.send(card::takeover::reassign(host, &id, to, rule_override).await);
        }
        EngineCommand::TakeOver { card: id, reply } => {
            let _ = reply.send(card::takeover::take_over(host, &id).await);
        }
        EngineCommand::TakeOverEnded { card: id } => {
            card::takeover::take_over_ended(host, &id).await?;
        }
        EngineCommand::Status { reply } => {
            let _ = reply.send(status(host));
        }
        EngineCommand::Diff {
            card: id,
            scope,
            reply,
        } => {
            let _ = reply.send(integrate::diff::card_diff(host, &id, scope).await);
        }
        EngineCommand::Raw { session, reply } => {
            let _ = reply.send(raw(host, &session));
        }
        EngineCommand::Revert {
            card: id,
            checkpoint,
            reply,
        } => {
            let _ = reply.send(handoff::revert::revert(host, &id, checkpoint).await);
        }
        EngineCommand::ProposeWorkflowChange {
            target,
            workflow: proposed,
            reason,
            reply,
        } => {
            let _ = reply.send(workflow::change::propose(host, target, *proposed, reason).await);
        }
        EngineCommand::SpotCheck { id, correct, reply } => {
            let _ = reply.send(checkpoints::spot_check(host, &id, correct));
        }
        EngineCommand::Goal { action, reply } => {
            let _ = reply.send(goal::pursue::act(host, action).await);
        }
    }
    Ok(())
}

/// A command that reached a finished run: questions are answered, and everything else is
/// refused, so no reply is ever dropped.
pub fn after_finish(host: &Host, command: EngineCommand) {
    const FINISHED: &str = "the run has finished";
    let refused = || Err(FINISHED.to_owned());
    match command {
        EngineCommand::Answer { answer, reply } => {
            let decided = host.read(|state| {
                state
                    .checkpoints
                    .by_id
                    .get(&answer.id)
                    .is_some_and(|record| record.decided.is_some())
            });
            // A decided checkpoint gets its usual answer; anything else can't be decided now.
            let answered = if decided {
                checkpoints::answer(host, answer)
            } else {
                refused()
            };
            let _ = reply.send(answered);
        }
        EngineCommand::Status { reply } => {
            let _ = reply.send(status(host));
        }
        EngineCommand::Raw { session, reply } => {
            let _ = reply.send(raw(host, &session));
        }
        EngineCommand::TakeBack { reply, .. } => {
            let _ = reply.send(Vec::new());
        }
        EngineCommand::Steer { reply, .. } => {
            let _ = reply.send(Err(FINISHED.to_owned()));
        }
        EngineCommand::Mention { reply, .. } | EngineCommand::Guidance { reply, .. } => {
            let _ = reply.send(Err(FINISHED.to_owned()));
        }
        EngineCommand::Reassign { reply, .. } | EngineCommand::Revert { reply, .. } => {
            let _ = reply.send(refused());
        }
        EngineCommand::TakeOver { reply, .. } => {
            let _ = reply.send(Err(FINISHED.to_owned()));
        }
        EngineCommand::Diff { reply, .. } => {
            let _ = reply.send(Err(FINISHED.to_owned()));
        }
        EngineCommand::ProposeWorkflowChange { reply, .. } => {
            let _ = reply.send(Err(FINISHED.to_owned()));
        }
        EngineCommand::Goal { reply, .. } => {
            let _ = reply.send(Err(FINISHED.to_owned()));
        }
        // A spot check is about an approval already given, so a finished run still takes one.
        EngineCommand::SpotCheck { id, correct, reply } => {
            let _ = reply.send(checkpoints::spot_check(host, &id, correct));
        }
        EngineCommand::Pause { .. }
        | EngineCommand::CancelPause
        | EngineCommand::Stop
        | EngineCommand::StopSession { .. }
        | EngineCommand::ApprovalMode { .. }
        | EngineCommand::TakeOverEnded { .. } => {}
    }
}

/// How many raw lines `Raw` returns, the newest ones.
const RAW_LINES: usize = 5_000;

/// Whether the loop should run this command as its own task, so it never waits on it.
pub fn is_long(command: &EngineCommand) -> bool {
    !matches!(
        command,
        EngineCommand::Answer { .. }
            | EngineCommand::TakeBack { .. }
            | EngineCommand::CancelPause
            | EngineCommand::ApprovalMode { .. }
            | EngineCommand::Status { .. }
            | EngineCommand::Raw { .. }
            | EngineCommand::SpotCheck { .. }
    )
}

/// `now: false` starts no sessions and sends `StopAfterTool` to each; `now: true` stops them at
/// once and cancels step sessions. Either way: commit, record `Paused`, and park.
pub async fn pause(host: &Host, now: bool) -> EngineResult<()> {
    if begin_pause(host, now)? {
        finish_pause(host).await?;
    }
    Ok(())
}

/// The part of a pause that happens at once: records `PauseRequested`, closes admission, and
/// tells the sessions to stop. False when there's nothing to pause. The loop runs it before
/// the next command, so a `CancelPause` right behind it finds the pause under way.
pub fn begin_pause(host: &Host, now: bool) -> EngineResult<bool> {
    begin_pause_by(host, now, Party::User)
}

/// [`begin_pause`] for whoever asked, such as your rules when the run's budget is spent.
pub fn begin_pause_by(host: &Host, now: bool, by: Party) -> EngineResult<bool> {
    let settled = host.read(|state| state.run.paused || state.run.finished.is_some());
    if settled {
        return Ok(false);
    }
    host.record(
        None,
        None,
        RunEventKind::Run(RunLifecycle::PauseRequested { by, now }),
    )?;
    host.shared().admission.send_replace(Admission::Paused);
    if now {
        host.interrupted().cancel();
    }
    stop_sessions(host, now, true);
    Ok(true)
}

/// Waits for the stopped sessions to end, commits their work, and records `Paused`.
pub async fn finish_pause(host: &Host) -> EngineResult<()> {
    host.shared().live.wait_empty().await;
    let _settling = host.shared().settling_pause.lock().await;
    // Cancelled while the sessions stopped: they end `Paused` and their steps resume them.
    // Paused already: the task of a pause from before a cancel got there first.
    let still_pausing = host.read(|state| state.run.pause_requested.is_some() && !state.run.paused);
    if !still_pausing {
        return Ok(());
    }
    let stopped = take_stopped(host);
    let committed = commit_stopped(host, &stopped).await?;
    host.record(
        None,
        None,
        RunEventKind::Run(RunLifecycle::Paused {
            sessions_stopped: u32::try_from(stopped.len()).unwrap_or(u32::MAX),
            committed,
        }),
    )?;
    Ok(())
}

/// Reopens admission. Waits that start from now on get a fresh interrupt, so a `now` pause
/// cancelled before it settled doesn't cut them short.
pub fn cancel_pause(host: &Host) -> EngineResult<()> {
    let pending = host.read(|state| state.run.pause_requested.is_some() || state.run.paused);
    if !pending || host.shared().admission() == Admission::Closed {
        return Ok(());
    }
    host.record(None, None, RunEventKind::Run(RunLifecycle::PauseCancelled))?;
    host.shared().renew_interrupt();
    host.shared().admission.send_replace(Admission::Open);
    Ok(())
}

/// Wrap up, commit, and end as `Stopped`.
pub async fn stop(host: &Host) -> EngineResult<()> {
    let settled = host.read(|state| state.run.stop_requested || state.run.finished.is_some());
    if settled {
        return Ok(());
    }
    host.record(
        None,
        None,
        RunEventKind::Run(RunLifecycle::StopRequested { by: Party::User }),
    )?;
    host.shared().admission.send_replace(Admission::Closed);
    host.interrupted().cancel();
    stop_sessions(host, false, false);
    host.shared().live.wait_empty().await;
    commit_stopped(host, &take_stopped(host)).await?;
    Ok(())
}

/// Records `ApprovalModeChanged`; later decision points carry it as `mode_override`.
pub fn approval_mode(host: &Host, mode: ApprovalMode) -> EngineResult<()> {
    host.record(
        None,
        None,
        RunEventKind::Run(RunLifecycle::ApprovalModeChanged {
            mode,
            by: Party::User,
        }),
    )?;
    Ok(())
}

pub fn status(host: &Host) -> StatusSummary {
    lock(&host.shared().status).summary(&Viewer::User)
}

pub fn raw(host: &Host, session: &SessionKey) -> Result<RawView, String> {
    let transcript = host
        .read(|state| {
            state
                .sessions
                .get(session)
                .map(|record| record.transcript.clone())
        })
        .ok_or_else(|| format!("no session {session} in this run"))?;
    let path = layout::session_dir(&host.repo().run_dir, session).join(session_files::RAW_STDOUT);
    let mut lines = Vec::new();
    let mut truncated = false;
    match std::fs::File::open(&path) {
        Ok(file) => {
            for line in BufReader::new(file).lines() {
                let line = line.map_err(|err| format!("reading {}: {err}", path.display()))?;
                if lines.len() == RAW_LINES {
                    lines.remove(0);
                    truncated = true;
                }
                lines.push(line);
            }
        }
        // A session that never started a process has no raw log yet.
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
        Err(err) => return Err(format!("reading {}: {err}", path.display())),
    }
    Ok(RawView {
        session: session.clone(),
        lines,
        truncated,
        path,
        vendor_transcript: transcript,
    })
}

/// Tells every live session to stop, after its tool call or now, and notes them with their
/// cards for the commit. A pause marks them, so they end `Paused` and resume later.
fn stop_sessions(host: &Host, now: bool, pausing: bool) {
    let live = host.shared().live.all();
    if pausing {
        lock(&host.shared().pausing).extend(live.iter().map(|(key, _)| key.clone()));
    }
    lock(&host.shared().stopped).extend(
        live.iter()
            .map(|(key, session)| (key.clone(), session.card.clone())),
    );
    let control = if now {
        SessionControl::Stop
    } else {
        SessionControl::StopAfterTool
    };
    for (key, session) in &live {
        if now {
            session.cancel.cancel();
        }
        // A closed channel means it already ended; a full one gets the stop through dropping
        // its sender when it ends.
        if let Err(err) = host.control(key, control.clone()) {
            tracing::debug!(session = %key, %err, "stop not sent");
        }
    }
}

/// The sessions a pause or stop told to stop, including the ones that went live after it
/// began. A cancelled pause leaves its own here, and the next pause or stop commits them.
fn take_stopped(host: &Host) -> Vec<(SessionKey, Option<CardId>)> {
    std::mem::take(&mut *lock(&host.shared().stopped))
        .into_iter()
        .collect()
}

/// Commits the worktrees of stopped building sessions, so no work is lost while parked.
async fn commit_stopped(
    host: &Host,
    stopped: &[(SessionKey, Option<CardId>)],
) -> EngineResult<Vec<CardId>> {
    let mut committed = BTreeSet::new();
    for (key, card) in stopped {
        let Some(card) = card else {
            continue;
        };
        let builds = host.read(|state| {
            state.sessions.get(key).is_some_and(|session| {
                matches!(session.started.access, Access::Build | Access::Spike)
            })
        });
        if builds
            && crate::card::worktree::commit_after_end(host, card, key)
                .await?
                .is_some()
        {
            committed.insert(card.clone());
        }
    }
    Ok(committed.into_iter().collect())
}
