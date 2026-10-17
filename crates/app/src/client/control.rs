//! Control calls: over the socket when a run host is live, else appended to
//! `pending-answers.jsonl` under the run lock, and a run host started to take them.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use argonauts_core::api::{ApiError, DiffScope, DiffView, RawView, StatusSummary, SteerAck};
use argonauts_core::control::{ControlReply, ControlRequest};
use argonauts_core::events::approval::ApprovalEvent;
use argonauts_core::events::run::{RunLifecycle, RunOutcome, WaitReason};
use argonauts_core::events::session::QueuedMessage;
use argonauts_core::events::{RunEvent, RunEventKind};
use argonauts_core::journal::{RunLock, read_all};
use argonauts_core::layout::run_files as names;
use argonauts_core::names::{ApprovalId, MessageId, SessionKey};
use argonauts_core::policy::DecisionPoint;
use argonauts_core::{RunId, TaskId};
use tokio::time::Instant;

use super::Client;
use super::approvals::approving_settings;
use super::journal_ends;
use super::socket::Connection;
use super::start::{self, WELCOME_TIMEOUT, run_host_error};
use crate::ancestry::{self, AgentMarkers};
use crate::commands;
use crate::host::{RUN_HOST_EXITING, log_views};
use crate::run_files;

/// How often `resume` looks at a woken run's log and lock.
const WAKE_POLL: Duration = Duration::from_millis(50);

/// A run host's reply, or `Deferred` when the request went to `pending-answers.jsonl`.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Reply {
    Now(Box<ControlReply>),
    Deferred,
}

impl Reply {
    pub(crate) fn ok(self) -> Result<(), ApiError> {
        match self {
            Self::Deferred => Ok(()),
            Self::Now(reply) => match *reply {
                ControlReply::Ok => Ok(()),
                other => Err(unexpected(other)),
            },
        }
    }

    /// A deferred message gets its id when the next run host delivers it, so there is none to
    /// take back until then.
    pub(crate) fn queued(self) -> Result<MessageId, ApiError> {
        match self {
            Self::Deferred => Err(ApiError::NotNow(
                "the run is waiting; the message is delivered when it resumes".to_owned(),
            )),
            Self::Now(reply) => match *reply {
                ControlReply::Queued { id } => Ok(id),
                other => Err(unexpected(other)),
            },
        }
    }

    pub(crate) fn steered(self) -> Result<SteerAck, ApiError> {
        match self.now()? {
            ControlReply::Steered { ack } => Ok(ack),
            other => Err(unexpected(other)),
        }
    }

    /// Only a live run host can say which messages it still held, so a deferred take-back is
    /// an error rather than an empty answer, which would read as all of them delivered.
    pub(crate) fn taken_back(self) -> Result<Vec<QueuedMessage>, ApiError> {
        match self.now()? {
            ControlReply::TakenBack { messages } => Ok(messages),
            other => Err(unexpected(other)),
        }
    }

    pub(crate) fn diff(self) -> Result<DiffView, ApiError> {
        match self.now()? {
            ControlReply::Diff { diff } => Ok(*diff),
            other => Err(unexpected(other)),
        }
    }

    pub(crate) fn take_over(self) -> Result<argonauts_core::api::TakeOverTicket, ApiError> {
        match self.now()? {
            ControlReply::TakeOver { ticket } => Ok(ticket),
            other => Err(unexpected(other)),
        }
    }

    /// The run host's reply; a deferred request has none.
    fn now(self) -> Result<ControlReply, ApiError> {
        match self {
            Self::Now(reply) => Ok(*reply),
            Self::Deferred => Err(ApiError::NotNow(
                "the run's run host isn't running".to_owned(),
            )),
        }
    }
}

fn unexpected(reply: ControlReply) -> ApiError {
    match reply {
        ControlReply::Error { message } => ApiError::Refused(message),
        reply => ApiError::RunHost(format!("unexpected reply: {reply:?}")),
    }
}

/// What a request does when no run host is live.
enum Offline {
    /// Into `pending-answers.jsonl` for the next run host, which is started.
    Defer,
    /// It needs a live reply: start a run host, then ask it.
    StartThenAsk,
    Refuse(&'static str),
}

fn offline(request: &ControlRequest) -> Offline {
    match request {
        ControlRequest::Answer { .. }
        | ControlRequest::Guidance { .. }
        | ControlRequest::Mention { .. }
        | ControlRequest::Reassign { .. }
        | ControlRequest::Stop
        | ControlRequest::StopSession { .. }
        | ControlRequest::ApprovalMode { .. }
        | ControlRequest::TakeOverEnded { .. }
        | ControlRequest::SpotCheck { .. }
        | ControlRequest::Goal { .. } => Offline::Defer,
        ControlRequest::TakeOver { .. }
        | ControlRequest::TakeBack { .. }
        | ControlRequest::Rewind { .. } => Offline::StartThenAsk,
        ControlRequest::Steer { .. } => Offline::Refuse("no agent is running in this run"),
        ControlRequest::Pause { .. } | ControlRequest::CancelPause => {
            Offline::Refuse("the run isn't running")
        }
        ControlRequest::Hello { .. }
        | ControlRequest::Subscribe { .. }
        | ControlRequest::Status
        | ControlRequest::Diff { .. }
        | ControlRequest::Raw { .. } => Offline::Refuse("the run's run host isn't running"),
    }
}

/// Requests only a person may send: those that carry the token, and a spot check, whose
/// verdict can raise autonomy.
fn needs_a_person(request: &ControlRequest) -> bool {
    match request {
        ControlRequest::Answer { .. }
        | ControlRequest::ApprovalMode { .. }
        | ControlRequest::TakeOver { .. }
        | ControlRequest::TakeOverEnded { .. }
        | ControlRequest::SpotCheck { .. }
        | ControlRequest::Goal { .. } => true,
        ControlRequest::Reassign { token, .. } => token.is_some(),
        _ => false,
    }
}

/// Tries before giving up on a run whose run hosts keep exiting as requests arrive.
const TRIES: usize = 3;

/// Sends `request` to the live run host, or defers it for the next one and starts it.
pub(crate) async fn call(
    client: &Client,
    run: &RunId,
    request: ControlRequest,
) -> Result<Reply, ApiError> {
    let run_dir = existing_run_dir(client, run)?;
    for _ in 0..TRIES {
        if let Some(reply) = try_once(client, run, &run_dir, &request).await? {
            return Ok(reply);
        }
    }
    Err(kept_exiting())
}

/// One try. `None` when the run host was on its way out, so the next try finds it gone.
async fn try_once(
    client: &Client,
    run: &RunId,
    run_dir: &Path,
    request: &ControlRequest,
) -> Result<Option<Reply>, ApiError> {
    // A run that stopped fully goes on only when you /resume it, so what would change it until
    // then is kept for the run host that /resume starts, and none starts for it now.
    if kept_for_resume(request) && stopped_fully(run_dir) {
        return keep_for_resume(client, run_dir, request).map(Some);
    }
    if let Some(connection) = connect(client, run_dir).await? {
        return live(connection, request.clone()).await;
    }
    let rule = offline(request);
    if let Offline::Refuse(why) = rule {
        return Err(ApiError::NotNow(why.to_owned()));
    }
    match start::outcome(run_dir) {
        Some(RunOutcome::Merged) => {
            return Err(ApiError::Merged(run.clone()));
        }
        Some(_)
            if matches!(
                request,
                ControlRequest::Stop
                    | ControlRequest::StopSession { .. }
                    | ControlRequest::TakeBack { .. }
            ) =>
        {
            return Err(ApiError::NotNow("the run has already finished".to_owned()));
        }
        _ => {}
    }
    if let Offline::StartThenAsk = rule {
        resume(client, run).await?;
        return live(connected(client, run_dir).await?, request.clone()).await;
    }
    if needs_a_person(request) {
        // The run host trusts pending frames, because whoever wrote them passed this check.
        ancestry::check_self(&client.env.paths, &AgentMarkers::builtin())
            .map_err(|err| ApiError::Refused(format!("{err:#}")))?;
    }
    let lock = RunLock::try_acquire(run_dir).map_err(run_host_error)?;
    let Some(lock) = lock else {
        // A run host holds the lock without answering: it's starting, or on its way out.
        if !start::await_holder(client, run_dir).await? {
            return Ok(None);
        }
        return live(connected(client, run_dir).await?, request.clone()).await;
    };
    run_files::append_pending(run_dir, request).map_err(run_host_error)?;
    drop(lock);
    start::launch(client, run, run_dir).await?;
    Ok(Some(Reply::Deferred))
}

/// Requests a run that stopped fully keeps for `/resume`: they change what it does once it goes
/// on, so none of them carries it on.
fn kept_for_resume(request: &ControlRequest) -> bool {
    matches!(
        request,
        ControlRequest::ApprovalMode { .. }
            | ControlRequest::Mention { .. }
            | ControlRequest::Guidance { .. }
            | ControlRequest::Reassign { .. }
            | ControlRequest::Goal { .. }
            | ControlRequest::SpotCheck { .. }
    )
}

/// Nothing could take a job the run needs, so it stopped until you carry it on.
fn stopped_fully(run_dir: &Path) -> bool {
    journal_ends::last_stage_event(&run_dir.join(names::EVENTS)).is_some_and(|event| {
        matches!(
            event.kind,
            RunEventKind::Run(RunLifecycle::Waiting {
                reason: WaitReason::NoFallback { .. },
                ..
            })
        )
    })
}

/// Saves `request` in `pending-answers.jsonl`. A run host that waits out its grace holds the
/// run's lock, and takes what was saved when /resume wakes it.
fn keep_for_resume(
    client: &Client,
    run_dir: &Path,
    request: &ControlRequest,
) -> Result<Reply, ApiError> {
    if needs_a_person(request) {
        ancestry::check_self(&client.env.paths, &AgentMarkers::builtin())
            .map_err(|err| ApiError::Refused(format!("{err:#}")))?;
    }
    let lock = RunLock::try_acquire(run_dir).map_err(run_host_error)?;
    run_files::append_pending(run_dir, request).map_err(run_host_error)?;
    drop(lock);
    Ok(Reply::Deferred)
}

/// Starts a run host unless one is live, and wakes one that is in its wait grace. A
/// merged run refuses. A stopped one is reopened: a `CancelPause` saved for its next run_host
/// asks that engine to go on from the run's last committed state.
pub(crate) async fn resume(client: &Client, run: &RunId) -> Result<(), ApiError> {
    let run_dir = existing_run_dir(client, run)?;
    if let Some(why) = super::list::cant_be_resumed(&run_dir) {
        return Err(ApiError::NotNow(why));
    }
    let events = run_dir.join(names::EVENTS);
    for _ in 0..TRIES {
        let (stopped, merged) = match finished(&events) {
            Some((seq, RunOutcome::Stopped)) => (Some(seq), false),
            Some((_, outcome)) => (None, outcome == RunOutcome::Merged),
            None => (None, false),
        };
        if merged {
            return Err(ApiError::Merged(run.clone()));
        }
        if let Some(connection) = connect(client, &run_dir).await? {
            match (stopped, waiting_at(&events)) {
                // The run host that stopped it is on its way out, unless another reopened it.
                (Some(stopped), _) => {
                    if moved_on_or_released(&run_dir, &events, stopped).await? {
                        return Ok(());
                    }
                }
                (None, None) => return Ok(()),
                (None, Some(waiting)) => {
                    // A waiting run host starts its engine again only for a request that needs
                    // one. `CancelPause` does, and asks for nothing more, since the pause is
                    // over.
                    let woken = ask(connection, ControlRequest::CancelPause)
                        .await
                        .and_then(Reply::ok);
                    match woken {
                        Ok(()) => return moved_on(&events, waiting).await,
                        // It was exiting at the end of its grace, so a new one starts below.
                        Err(_) if released(&run_dir).await => {}
                        Err(err) => return Err(err),
                    }
                }
            }
        }
        match RunLock::try_acquire(&run_dir).map_err(run_host_error)? {
            Some(lock) => {
                if stopped.is_some() {
                    run_files::append_pending(&run_dir, &ControlRequest::CancelPause)
                        .map_err(run_host_error)?;
                }
                drop(lock);
                start::launch(client, run, &run_dir).await?;
                return match stopped {
                    Some(stopped) => moved_on(&events, stopped).await,
                    None => Ok(()),
                };
            }
            None => {
                // One that was starting may not be the one to reopen a stopped run, so that
                // case looks again.
                if start::await_holder(client, &run_dir).await? && stopped.is_none() {
                    return Ok(());
                }
            }
        }
    }
    Err(kept_exiting())
}

/// The `seq` of the `Waiting` the log ends on.
fn waiting_at(events: &Path) -> Option<u64> {
    journal_ends::last_stage_event(events)
        .filter(|event| matches!(event.kind, RunEventKind::Run(RunLifecycle::Waiting { .. })))
        .map(|event| event.seq)
}

/// The `seq` and outcome of the `Finished` the log ends on.
fn finished(events: &Path) -> Option<(u64, RunOutcome)> {
    let event = journal_ends::last_stage_event(events)?;
    match event.kind {
        RunEventKind::Run(RunLifecycle::Finished { outcome, .. }) => Some((event.seq, outcome)),
        _ => None,
    }
}

/// Waits for a live run host of a stopped run to exit, or for the run to move on from the
/// `Finished` at `stopped`. True when it moved on.
async fn moved_on_or_released(
    run_dir: &Path,
    events: &Path,
    stopped: u64,
) -> Result<bool, ApiError> {
    let deadline = Instant::now() + WELCOME_TIMEOUT;
    loop {
        if journal_ends::last_stage_event(events).is_some_and(|event| event.seq > stopped) {
            return Ok(true);
        }
        if !RunLock::is_held(run_dir) {
            return Ok(false);
        }
        if Instant::now() >= deadline {
            return Err(kept_exiting());
        }
        tokio::time::sleep(WAKE_POLL).await;
    }
}

/// Waits for the woken engine to move the run on from the `Waiting` or `Finished` at `settled`,
/// which it does by recording `Resumed` as it rebuilds the run from the log.
async fn moved_on(events: &Path, settled: u64) -> Result<(), ApiError> {
    let deadline = Instant::now() + WELCOME_TIMEOUT;
    loop {
        if journal_ends::last_stage_event(events).is_some_and(|event| event.seq > settled) {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(ApiError::RunHost(format!(
                "the run's run host didn't resume it within {} s",
                WELCOME_TIMEOUT.as_secs()
            )));
        }
        tokio::time::sleep(WAKE_POLL).await;
    }
}

/// Waits for the run's run host to let go of the run. False when it still holds it at the
/// timeout.
async fn released(run_dir: &Path) -> bool {
    let deadline = Instant::now() + WELCOME_TIMEOUT;
    while RunLock::is_held(run_dir) {
        if Instant::now() >= deadline {
            return false;
        }
        tokio::time::sleep(WAKE_POLL).await;
    }
    true
}

/// Your verdict goes to the run's engine, the live one or the next. A finished run has no
/// engine left to take it, so the app records it with the repo's policy, for the approval
/// the run's log queued.
pub(crate) async fn spot_check(
    client: &Client,
    run: &RunId,
    id: &ApprovalId,
    correct: bool,
) -> Result<(), ApiError> {
    let run_dir = existing_run_dir(client, run)?;
    let request = ControlRequest::SpotCheck {
        id: id.clone(),
        correct,
    };
    if let Some(connection) = connect(client, &run_dir).await?
        && let Some(reply) = live(connection, request.clone()).await?
    {
        return reply.ok();
    }
    if start::outcome(&run_dir).is_none() {
        return call(client, run, request).await?.ok();
    }
    let settings = approving_settings(&client.env).map_err(ApiError::Refused)?;
    let events = read_all(&run_dir.join(names::EVENTS))
        .map_err(|err| ApiError::RunHost(format!("reading the run's log: {err}")))?
        .events;
    let point = queued_point(run, id, &events).ok_or_else(|| {
        ApiError::Refused(format!("{id} of {run} isn't waiting for a spot check"))
    })?;
    commands::policy_for(&client.env, Arc::new(settings)).spot_checked(&point, correct);
    Ok(())
}

/// The decision point of approval `id`, when the log put it in the spot-check queue.
fn queued_point(run: &RunId, id: &ApprovalId, events: &[RunEvent]) -> Option<DecisionPoint> {
    let queued = events.iter().any(|event| {
        matches!(
            &event.kind,
            RunEventKind::Approval(ApprovalEvent::SpotCheckQueued { id: queued })
                if queued == id
        )
    });
    let kind = events.iter().find_map(|event| match &event.kind {
        RunEventKind::Approval(ApprovalEvent::Opened(opened)) if opened.id == *id => {
            Some(opened.kind)
        }
        _ => None,
    })?;
    queued.then(|| DecisionPoint {
        run: run.clone(),
        approval: id.clone(),
        kind,
        learned_workflow: None,
        authors: Vec::new(),
        mode_override: None,
        main_agent: None,
    })
}

/// From the live run host, else built from the journal by `status::summary`.
pub(crate) async fn status(client: &Client, run: &RunId) -> Result<StatusSummary, ApiError> {
    let run_dir = existing_run_dir(client, run)?;
    if let Some(connection) = connect(client, &run_dir).await?
        && let Some(reply) = live(connection, ControlRequest::Status).await?
    {
        return match reply {
            Reply::Now(reply) => match *reply {
                ControlReply::Status { summary } => Ok(*summary),
                other => Err(unexpected(other)),
            },
            Reply::Deferred => Err(ApiError::NotNow("no status yet".to_owned())),
        };
    }
    log_views::status(&run_dir, run)
}

/// From the live run host, else the task's last diff from the journal.
pub(crate) async fn diff(
    client: &Client,
    run: &RunId,
    task: &TaskId,
    scope: DiffScope,
) -> Result<DiffView, ApiError> {
    let run_dir = existing_run_dir(client, run)?;
    let request = ControlRequest::Diff {
        task: task.clone(),
        scope,
    };
    if let Some(connection) = connect(client, &run_dir).await?
        && let Some(reply) = live(connection, request).await?
    {
        return reply.diff();
    }
    log_views::diff(&run_dir, task, scope, |path| client.env.display(path))
}

/// From the live run host, else read from the session's raw files.
pub(crate) async fn raw(
    client: &Client,
    run: &RunId,
    session: &SessionKey,
) -> Result<RawView, ApiError> {
    let run_dir = existing_run_dir(client, run)?;
    let request = ControlRequest::Raw {
        session: session.clone(),
    };
    if let Some(connection) = connect(client, &run_dir).await?
        && let Some(reply) = live(connection, request).await?
    {
        return match reply {
            Reply::Now(reply) => match *reply {
                ControlReply::Raw { raw } => Ok(*raw),
                other => Err(unexpected(other)),
            },
            Reply::Deferred => Err(ApiError::NotNow("no raw output yet".to_owned())),
        };
    }
    log_views::raw(&run_dir, run, session)
}

fn kept_exiting() -> ApiError {
    ApiError::RunHost(
        "the run's run host kept exiting as the request arrived; send it again".to_owned(),
    )
}

fn existing_run_dir(client: &Client, run: &RunId) -> Result<PathBuf, ApiError> {
    let run_dir = client.env.run_dir(run);
    if run_dir.is_dir() {
        Ok(run_dir)
    } else {
        Err(ApiError::NoRun(run.clone()))
    }
}

async fn connect(client: &Client, run_dir: &Path) -> Result<Option<Connection>, ApiError> {
    Connection::connect(run_dir, client.kind)
        .await
        .map_err(run_host_error)
}

/// A connection to a run host that just greeted.
async fn connected(client: &Client, run_dir: &Path) -> Result<Connection, ApiError> {
    connect(client, run_dir)
        .await?
        .ok_or_else(|| ApiError::RunHost("the run's run host went away as it started".to_owned()))
}

async fn ask(mut connection: Connection, request: ControlRequest) -> Result<Reply, ApiError> {
    let reply = connection.request(request).await.map_err(run_host_error)?;
    Ok(Reply::Now(Box::new(reply)))
}

/// The live run host's reply, or `None` when it refused the request unseen on its way out.
async fn live(connection: Connection, request: ControlRequest) -> Result<Option<Reply>, ApiError> {
    let reply = ask(connection, request).await?;
    let exiting = matches!(
        &reply,
        Reply::Now(now) if matches!(&**now, ControlReply::Error { message } if message == RUN_HOST_EXITING)
    );
    Ok((!exiting).then_some(reply))
}
