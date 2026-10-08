//! Control calls: over the socket when a supervisor is live, else appended to
//! `pending-answers.jsonl` under the run lock, and a supervisor started to take them.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use argonauts_core::api::{ApiError, DiffScope, DiffView, RawView, StatusSummary, SteerAck};
use argonauts_core::control::{ControlReply, ControlRequest};
use argonauts_core::events::checkpoint::CheckpointEvent;
use argonauts_core::events::run::{RunLifecycle, RunOutcome};
use argonauts_core::events::session::QueuedMessage;
use argonauts_core::events::{RunEvent, RunEventKind};
use argonauts_core::journal::{RunLock, read_all};
use argonauts_core::layout::run_files as names;
use argonauts_core::names::{CheckpointId, MessageId, SessionKey};
use argonauts_core::policy::DecisionPoint;
use argonauts_core::{CardId, RunId};
use tokio::time::Instant;

use super::Client;
use super::approvals::approving_settings;
use super::journal_ends;
use super::socket::Connection;
use super::start::{self, WELCOME_TIMEOUT, supervisor_error};
use crate::ancestry::{self, AgentMarkers};
use crate::commands;
use crate::host::{SUPERVISOR_EXITING, log_views};
use crate::run_files;

/// How often `resume` looks at a woken run's log and lock.
const WAKE_POLL: Duration = Duration::from_millis(50);

/// A supervisor's reply, or `Deferred` when the request went to `pending-answers.jsonl`.
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

    /// A deferred message gets its id when the next supervisor delivers it, so there is none to
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

    /// Only a live supervisor can say which messages it still held, so a deferred take-back is
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

    /// The supervisor's reply; a deferred request has none.
    fn now(self) -> Result<ControlReply, ApiError> {
        match self {
            Self::Now(reply) => Ok(*reply),
            Self::Deferred => Err(ApiError::NotNow(
                "the run's supervisor isn't running".to_owned(),
            )),
        }
    }
}

fn unexpected(reply: ControlReply) -> ApiError {
    match reply {
        ControlReply::Error { message } => ApiError::Refused(message),
        reply => ApiError::Supervisor(format!("unexpected reply: {reply:?}")),
    }
}

/// What a request does when no supervisor is live.
enum Offline {
    /// Into `pending-answers.jsonl` for the next supervisor, which is started.
    Defer,
    /// It needs a live reply: start a supervisor, then ask it.
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
        | ControlRequest::Revert { .. } => Offline::StartThenAsk,
        ControlRequest::Steer { .. } => Offline::Refuse("no agent is running in this run"),
        ControlRequest::Pause { .. } | ControlRequest::CancelPause => {
            Offline::Refuse("the run isn't running")
        }
        ControlRequest::Hello { .. }
        | ControlRequest::Subscribe { .. }
        | ControlRequest::Status
        | ControlRequest::Diff { .. }
        | ControlRequest::Raw { .. } => Offline::Refuse("the run's supervisor isn't running"),
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

/// Tries before giving up on a run whose supervisors keep exiting as requests arrive.
const ATTEMPTS: usize = 3;

/// Sends `request` to the live supervisor, or defers it for the next one and starts it.
pub(crate) async fn call(
    client: &Client,
    run: &RunId,
    request: ControlRequest,
) -> Result<Reply, ApiError> {
    let run_dir = existing_run_dir(client, run)?;
    for _ in 0..ATTEMPTS {
        if let Some(reply) = attempt(client, run, &run_dir, &request).await? {
            return Ok(reply);
        }
    }
    Err(kept_exiting())
}

/// One try. `None` when the supervisor was on its way out, so the next try finds it gone.
async fn attempt(
    client: &Client,
    run: &RunId,
    run_dir: &Path,
    request: &ControlRequest,
) -> Result<Option<Reply>, ApiError> {
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
        // The supervisor trusts pending frames, because whoever wrote them passed this check.
        ancestry::check_self(&client.env.paths, &AgentMarkers::builtin())
            .map_err(|err| ApiError::Refused(format!("{err:#}")))?;
    }
    let lock = RunLock::try_acquire(run_dir).map_err(supervisor_error)?;
    let Some(lock) = lock else {
        // A supervisor holds the lock without answering: it's starting, or on its way out.
        if !start::await_holder(client, run_dir).await? {
            return Ok(None);
        }
        return live(connected(client, run_dir).await?, request.clone()).await;
    };
    run_files::append_pending(run_dir, request).map_err(supervisor_error)?;
    drop(lock);
    start::launch(client, run, run_dir).await?;
    Ok(Some(Reply::Deferred))
}

/// Starts a supervisor unless one is live, and wakes one that is waiting out its park grace. A
/// merged run refuses. A stopped one is reopened: a `CancelPause` saved for its next supervisor
/// asks that engine to go on from the run's last committed state.
pub(crate) async fn resume(client: &Client, run: &RunId) -> Result<(), ApiError> {
    let run_dir = existing_run_dir(client, run)?;
    let events = run_dir.join(names::EVENTS);
    for _ in 0..ATTEMPTS {
        let (stopped, merged) = match finished(&events) {
            Some((seq, RunOutcome::Stopped)) => (Some(seq), false),
            Some((_, outcome)) => (None, outcome == RunOutcome::Merged),
            None => (None, false),
        };
        if merged {
            return Err(ApiError::Merged(run.clone()));
        }
        if let Some(connection) = connect(client, &run_dir).await? {
            match (stopped, parked_at(&events)) {
                // The supervisor that stopped it is on its way out, unless another reopened it.
                (Some(stopped), _) => {
                    if moved_on_or_released(&run_dir, &events, stopped).await? {
                        return Ok(());
                    }
                }
                (None, None) => return Ok(()),
                (None, Some(parked)) => {
                    // A parked supervisor starts its engine again only for a request that needs
                    // one. `CancelPause` does, and asks for nothing more, since the pause is
                    // over.
                    let woken = ask(connection, ControlRequest::CancelPause)
                        .await
                        .and_then(Reply::ok);
                    match woken {
                        Ok(()) => return moved_on(&events, parked).await,
                        // It was exiting at the end of its grace, so a new one starts below.
                        Err(_) if released(&run_dir).await => {}
                        Err(err) => return Err(err),
                    }
                }
            }
        }
        match RunLock::try_acquire(&run_dir).map_err(supervisor_error)? {
            Some(lock) => {
                if stopped.is_some() {
                    run_files::append_pending(&run_dir, &ControlRequest::CancelPause)
                        .map_err(supervisor_error)?;
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

/// The `seq` of the `Parked` the log ends on.
fn parked_at(events: &Path) -> Option<u64> {
    journal_ends::last_stage_event(events)
        .filter(|event| matches!(event.kind, RunEventKind::Run(RunLifecycle::Parked { .. })))
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

/// Waits for a live supervisor of a stopped run to exit, or for the run to move on from the
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

/// Waits for the woken engine to move the run on from the `Parked` or `Finished` at `settled`,
/// which it does by recording `Resumed` as it rebuilds the run from the log.
async fn moved_on(events: &Path, settled: u64) -> Result<(), ApiError> {
    let deadline = Instant::now() + WELCOME_TIMEOUT;
    loop {
        if journal_ends::last_stage_event(events).is_some_and(|event| event.seq > settled) {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(ApiError::Supervisor(format!(
                "the run's supervisor didn't resume it within {} s",
                WELCOME_TIMEOUT.as_secs()
            )));
        }
        tokio::time::sleep(WAKE_POLL).await;
    }
}

/// Waits for the run's supervisor to let go of the run. False when it still holds it at the
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
/// engine left to take it, so the app records it with the repo's policy, for the checkpoint
/// the run's log queued.
pub(crate) async fn spot_check(
    client: &Client,
    run: &RunId,
    id: &CheckpointId,
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
        .map_err(|err| ApiError::Supervisor(format!("reading the run's log: {err}")))?
        .events;
    let point = queued_point(run, id, &events).ok_or_else(|| {
        ApiError::Refused(format!("{id} of {run} isn't waiting for a spot check"))
    })?;
    commands::policy_for(&client.env, Arc::new(settings)).spot_checked(&point, correct);
    Ok(())
}

/// The decision point of checkpoint `id`, when the log put it in the spot-check queue.
fn queued_point(run: &RunId, id: &CheckpointId, events: &[RunEvent]) -> Option<DecisionPoint> {
    let queued = events.iter().any(|event| {
        matches!(
            &event.kind,
            RunEventKind::Checkpoint(CheckpointEvent::SpotCheckQueued { id: queued })
                if queued == id
        )
    });
    let kind = events.iter().find_map(|event| match &event.kind {
        RunEventKind::Checkpoint(CheckpointEvent::Opened(opened)) if opened.id == *id => {
            Some(opened.kind)
        }
        _ => None,
    })?;
    queued.then(|| DecisionPoint {
        run: run.clone(),
        checkpoint: id.clone(),
        kind,
        recipe: None,
        authors: Vec::new(),
        mode_override: None,
        lead: None,
    })
}

/// From the live supervisor, else built from the journal by `status::summary`.
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

/// From the live supervisor, else the card's last diff from the journal.
pub(crate) async fn diff(
    client: &Client,
    run: &RunId,
    card: &CardId,
    scope: DiffScope,
) -> Result<DiffView, ApiError> {
    let run_dir = existing_run_dir(client, run)?;
    let request = ControlRequest::Diff {
        card: card.clone(),
        scope,
    };
    if let Some(connection) = connect(client, &run_dir).await?
        && let Some(reply) = live(connection, request).await?
    {
        return reply.diff();
    }
    log_views::diff(&run_dir, card, scope, |path| client.env.display(path))
}

/// From the live supervisor, else read from the session's raw files.
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
    ApiError::Supervisor(
        "the run's supervisor kept exiting as the request arrived; send it again".to_owned(),
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
        .map_err(supervisor_error)
}

/// A connection to a supervisor that just greeted.
async fn connected(client: &Client, run_dir: &Path) -> Result<Connection, ApiError> {
    connect(client, run_dir).await?.ok_or_else(|| {
        ApiError::Supervisor("the run's supervisor went away as it started".to_owned())
    })
}

async fn ask(mut connection: Connection, request: ControlRequest) -> Result<Reply, ApiError> {
    let reply = connection
        .request(request)
        .await
        .map_err(supervisor_error)?;
    Ok(Reply::Now(Box::new(reply)))
}

/// The live supervisor's reply, or `None` when it refused the request unseen on its way out.
async fn live(connection: Connection, request: ControlRequest) -> Result<Option<Reply>, ApiError> {
    let reply = ask(connection, request).await?;
    let exiting = matches!(
        &reply,
        Reply::Now(now) if matches!(&**now, ControlReply::Error { message } if message == SUPERVISOR_EXITING)
    );
    Ok((!exiting).then_some(reply))
}
