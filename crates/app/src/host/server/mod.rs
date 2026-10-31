//! The control socket's server end, `runs/<run>/control.sock` with mode 0600. Requests that
//! decide or raise autonomy need the run's token, and are refused when the peer process
//! descends from an agent.
//!
//! `admit` checks the token and the peer, `commands` turns a request into an engine command,
//! and `subscribe` streams the run's events to a client.

mod admit;
mod commands;
mod subscribe;
#[cfg(test)]
pub(crate) mod tests;

use std::fs::Permissions;
use std::io;
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context as _;
use argonauts_core::runs::api::DiffScope;
use argonauts_core::runs::events::RunEvent;
use argonauts_core::{Paths, RunId};
use tokio::io::BufReader;
use tokio::net::unix::OwnedWriteHalf;
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::{broadcast, mpsc};
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;

use self::admit::admit;
pub(crate) use self::commands::{EngineReply, command};
use self::subscribe::subscription;
use super::door::{Door, Turned};
use super::log_views;
use super::relay::{Entry, Receipt};
use crate::control::wire::{read_frame, write_frame};
use crate::control::{ControlReply, ControlRequest, Frame};
use crate::host::run_files::{socket_address, socket_path};
use crate::startup::ancestry::AgentMarkers;

/// Replies and events waiting to go out on one connection.
const OUTGOING: usize = 256;
/// How long an exiting run host lets its connections send what they still hold, so a client
/// that stopped reading can't keep it running.
const FLUSH_TIMEOUT: Duration = Duration::from_secs(2);

pub(crate) struct ServerState {
    pub run: RunId,
    pub pid: u32,
    pub token: String,
    pub home: Paths,
    pub markers: AgentMarkers,
    pub run_dir: PathBuf,
    /// The journal, re-read for a subscriber that lagged behind the broadcast.
    pub journal: PathBuf,
    pub door: Door,
    pub events: broadcast::Sender<RunEvent>,
    /// `agent_ancestor`, or a scripted table in tests.
    pub ancestry: AncestryCheck,
    /// Off only in tests that connect from the test runner's own process.
    pub check_peers: bool,
}

pub(crate) type AncestryCheck =
    Arc<dyn Fn(u32, &Paths, &AgentMarkers) -> anyhow::Result<Option<String>> + Send + Sync>;

/// Where a request came from, as far as the approval path cares.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Origin {
    /// A connection; its peer process is checked for agent ancestry.
    Peer { pid: Option<u32> },
    /// `pending-answers.jsonl`, written by a client that ran the same check on itself. No peer
    /// is left to check, so the token, kept in the run's private folder, is the guard.
    Pending,
}

/// Binds the socket for user-only access, replacing a stale file left by a crash.
pub(crate) fn bind(run_dir: &Path) -> anyhow::Result<UnixListener> {
    let path = socket_path(run_dir);
    match std::fs::remove_file(&path) {
        Ok(()) => {}
        Err(err) if err.kind() == io::ErrorKind::NotFound => {}
        Err(err) => {
            return Err(err).with_context(|| format!("removing the old {}", path.display()));
        }
    }
    let address = socket_address(run_dir).with_context(|| {
        format!(
            "finding an address for the control socket {}",
            path.display()
        )
    })?;
    let listener = UnixListener::bind(&address)
        .with_context(|| format!("opening the control socket {}", address.display()))?;
    std::fs::set_permissions(&path, Permissions::from_mode(0o600))
        .with_context(|| format!("making {} private", path.display()))?;
    Ok(listener)
}

/// Accepts connections until `shutdown`, each on its own task, then lets them send what they
/// still hold, such as a subscriber's last events.
pub(crate) async fn serve(
    listener: UnixListener,
    state: Arc<ServerState>,
    shutdown: CancellationToken,
) -> anyhow::Result<()> {
    let mut connections = JoinSet::new();
    loop {
        let accepted = tokio::select! {
            () = shutdown.cancelled() => break,
            accepted = listener.accept() => accepted,
            Some(_) = connections.join_next() => continue,
        };
        match accepted {
            Ok((stream, _)) => {
                let state = state.clone();
                let closing = shutdown.child_token();
                connections.spawn(async move {
                    if let Err(err) = connection(stream, state, closing).await {
                        tracing::debug!("control connection ended: {err}");
                    }
                });
            }
            Err(err) => {
                // Out of file descriptors, say. Back off rather than spin.
                tracing::warn!("accepting a control connection: {err}");
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        }
    }
    // A client connecting now is refused at once instead of waiting for a greeting.
    drop(listener);
    let flushed = tokio::time::timeout(FLUSH_TIMEOUT, async {
        while connections.join_next().await.is_some() {}
    })
    .await;
    if flushed.is_err() {
        tracing::warn!("a control connection was still sending as the run host exited");
    }
    Ok(())
}

async fn connection(
    stream: UnixStream,
    state: Arc<ServerState>,
    shutdown: CancellationToken,
) -> io::Result<()> {
    let peer = stream
        .peer_cred()
        .ok()
        .and_then(|cred| cred.pid())
        .and_then(|pid| u32::try_from(pid).ok());
    let (read, write) = stream.into_split();
    let mut reader = BufReader::new(read);
    let (out, outgoing) = mpsc::channel(OUTGOING);
    let writer = tokio::spawn(write_frames(write, outgoing));
    let closing = shutdown.child_token();
    let result = requests(&mut reader, &state, peer, &out, &closing).await;
    closing.cancel();
    drop(out);
    let _ = writer.await;
    result
}

async fn requests(
    reader: &mut BufReader<tokio::net::unix::OwnedReadHalf>,
    state: &Arc<ServerState>,
    peer: Option<u32>,
    out: &mpsc::Sender<Frame<ControlReply>>,
    closing: &CancellationToken,
) -> io::Result<()> {
    let mut greeted = false;
    loop {
        let frame: Option<Frame<ControlRequest>> = tokio::select! {
            () = closing.cancelled() => return Ok(()),
            frame = read_frame(reader) => match frame {
                Ok(frame) => frame,
                Err(err) => {
                    let _ = out.send(Frame { id: 0, body: error(format!("unreadable request: {err}")) }).await;
                    return Err(err);
                }
            },
        };
        let Some(Frame { id, body }) = frame else {
            return Ok(());
        };
        let reply = match body {
            ControlRequest::Hello { .. } => {
                greeted = true;
                ControlReply::Welcome {
                    run: state.run.clone(),
                    pid: state.pid,
                    version: env!("CARGO_PKG_VERSION").to_owned(),
                }
            }
            _ if !greeted => {
                let _ = out
                    .send(Frame {
                        id,
                        body: error("the first request must be hello"),
                    })
                    .await;
                return Ok(());
            }
            ControlRequest::Subscribe {
                from_seq,
                from_snapshot,
            } => {
                tokio::spawn(subscription(
                    state.clone(),
                    id,
                    from_seq,
                    from_snapshot,
                    out.clone(),
                    closing.clone(),
                ));
                continue;
            }
            request => apply(state, request, Origin::Peer { pid: peer }).await,
        };
        if out.send(Frame { id, body: reply }).await.is_err() {
            return Ok(());
        }
    }
}

async fn write_frames(
    mut write: OwnedWriteHalf,
    mut outgoing: mpsc::Receiver<Frame<ControlReply>>,
) {
    while let Some(frame) = outgoing.recv().await {
        if write_frame(&mut write, &frame).await.is_err() {
            return;
        }
    }
}

/// Checks the token and the origin, queues the request for the engine, and waits for its reply.
/// `Hello` and `Subscribe` are the connection's own business and never get here.
pub(crate) async fn apply(
    state: &ServerState,
    request: ControlRequest,
    origin: Origin,
) -> ControlReply {
    match submit(state, request, origin).await {
        Ok(receipt) => receipt.reply().await,
        Err(reply) => reply,
    }
}

/// Everything `apply` does short of waiting. While the run is waiting within its grace, a request
/// that needs the engine wakes the host, which starts a new one, and the rest are answered here.
pub(crate) async fn submit(
    state: &ServerState,
    request: ControlRequest,
    origin: Origin,
) -> Result<Receipt, ControlReply> {
    if matches!(
        request,
        ControlRequest::Hello { .. } | ControlRequest::Subscribe { .. }
    ) {
        return Err(error("hello and subscribe belong to a connection"));
    }
    admit(state, &request, origin).await.map_err(error)?;
    let wakes = while_waiting(&request).is_none();
    let (entry, receipt) = Entry::new(request);
    match state.door.submit(entry, wakes) {
        Ok(()) => Ok(receipt),
        Err(Turned::Waiting(entry)) => Err(waiting_reply(state, &entry.request).await),
        Err(Turned::Closed) => Err(error(RUN_HOST_EXITING)),
    }
}

/// A request from `pending-answers.jsonl` as a queue entry, once its token checks out. The host
/// queues these ahead of the socket's requests.
pub(crate) async fn admit_pending(
    state: &ServerState,
    request: ControlRequest,
) -> Result<(Entry, Receipt), String> {
    admit(state, &request, Origin::Pending).await?;
    Ok(Entry::new(request))
}

/// The reply to a request that met a run host on its way out, before any engine took it.
/// Clients match it to hand the request to the next run host instead.
pub(crate) const RUN_HOST_EXITING: &str = "the run's run host is exiting; send it again";

/// Why a request doesn't wake a waiting run, or `None` when it does.
pub(super) fn while_waiting(request: &ControlRequest) -> Option<&'static str> {
    match request {
        // Only an engine can diff the last turn's worktree.
        ControlRequest::Diff {
            scope: DiffScope::LastTurn,
            ..
        } => None,
        ControlRequest::Status | ControlRequest::Diff { .. } | ControlRequest::Raw { .. } => {
            Some("the run is waiting, so its log is complete")
        }
        ControlRequest::Steer { .. } | ControlRequest::StopSession { .. } => {
            Some("the run is waiting, so no agent is running")
        }
        ControlRequest::Pause { .. } => Some("the run is already waiting"),
        _ => None,
    }
}

/// The reply to a request that doesn't wake a waiting run. The views come from the run's files,
/// which are complete while no engine runs.
pub(crate) async fn waiting_reply(state: &ServerState, request: &ControlRequest) -> ControlReply {
    let run_dir = state.run_dir.clone();
    let run = state.run.clone();
    let view = match request.clone() {
        ControlRequest::Status => tokio::task::spawn_blocking(move || {
            log_views::status(&run_dir, &run).map(|summary| ControlReply::Status {
                summary: Box::new(summary),
            })
        }),
        ControlRequest::Diff { task, scope } => tokio::task::spawn_blocking(move || {
            log_views::diff(&run_dir, &task, scope, |path| path.display().to_string()).map(|diff| {
                ControlReply::Diff {
                    diff: Box::new(diff),
                }
            })
        }),
        ControlRequest::Raw { session } => tokio::task::spawn_blocking(move || {
            log_views::raw(&run_dir, &run, &session)
                .map(|raw| ControlReply::Raw { raw: Box::new(raw) })
        }),
        other => {
            return error(while_waiting(&other).unwrap_or("the run is waiting"));
        }
    };
    match view.await {
        Ok(Ok(reply)) => reply,
        Ok(Err(err)) => error(err.to_string()),
        Err(err) => error(format!("reading the run's log failed: {err}")),
    }
}

pub(super) fn error(message: impl Into<String>) -> ControlReply {
    ControlReply::Error {
        message: message.into(),
    }
}
