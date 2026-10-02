//! `subscribe` and `reattach`: the socket while a supervisor is live, else `journal::tail`,
//! deduplicated by `seq` when one hands over to the other.

use std::path::{Path, PathBuf};
use std::time::Duration;

use argonauts_core::RunId;
use argonauts_core::api::{ApiError, EventStream};
use argonauts_core::control::ClientKind;
use argonauts_core::events::run::RunLifecycle;
use argonauts_core::events::{RunEvent, RunEventKind};
use argonauts_core::journal::{self, RunLock};
use argonauts_core::layout::run_files as names;
use tokio::sync::{mpsc, watch};

use super::Client;
use super::journal_ends;
use super::socket::Connection;

const STREAM_BUFFER: usize = 1024;
/// How often a tailed stream looks for a supervisor that came back.
const SUPERVISOR_CHECK: Duration = Duration::from_millis(250);

/// `from_snapshot` starts at the latest `Run::Snapshot` instead of `from_seq`. The stream's
/// `live` turns false once the supervisor has let go of the run and everything it wrote was
/// passed on.
pub(crate) async fn subscribe(
    client: &Client,
    run: &RunId,
    from_seq: u64,
    from_snapshot: bool,
) -> Result<EventStream, ApiError> {
    let run_dir = client.env.run_dir(run);
    if !run_dir.is_dir() {
        return Err(ApiError::NoRun(run.clone()));
    }
    let events_path = run_dir.join(names::EVENTS);
    let replay_until = journal_ends::last_seq(&events_path);
    // Reading the journal needs no supervisor, so a socket that fails only costs liveness.
    let connection = match Connection::connect(&run_dir, client.kind).await {
        Ok(connection) => connection,
        Err(err) => {
            tracing::warn!("following the run's journal instead of its socket: {err:#}");
            None
        }
    };
    let (sender, events) = mpsc::channel(STREAM_BUFFER);
    let (live_sender, live) = watch::channel(connection.is_some());
    let follower = Follower {
        run_dir,
        events_path,
        kind: client.kind,
        next: (!from_snapshot).then_some(from_seq),
        sender,
        live: live_sender,
    };
    tokio::spawn(follower.run(connection));
    Ok(EventStream {
        events,
        replay_until,
        live,
    })
}

struct Follower {
    run_dir: PathBuf,
    events_path: PathBuf,
    kind: ClientKind,
    /// The next `seq` to pass on. `None` until the first event of a stream that starts at the
    /// latest snapshot.
    next: Option<u64>,
    sender: mpsc::Sender<RunEvent>,
    live: watch::Sender<bool>,
}

#[derive(PartialEq)]
enum Flow {
    Continue,
    /// The run finished or nobody reads the stream any more.
    Done,
}

enum Handover {
    Done,
    SupervisorGone,
    SupervisorBack(Connection),
}

impl Follower {
    async fn run(mut self, mut connection: Option<Connection>) {
        loop {
            if let Some(live) = connection.take() {
                self.live.send_replace(true);
                if let Handover::Done = self.over_socket(live).await {
                    break;
                }
            }
            match self.over_journal().await {
                Handover::SupervisorBack(live) => connection = Some(live),
                Handover::Done | Handover::SupervisorGone => break,
            }
        }
        self.live.send_replace(false);
    }

    async fn over_socket(&mut self, connection: Connection) -> Handover {
        let (from_seq, from_snapshot) = match self.next {
            Some(next) => (next, false),
            None => (0, true),
        };
        let mut subscription = match connection.subscribe(from_seq, from_snapshot).await {
            Ok(subscription) => subscription,
            Err(err) => {
                tracing::warn!("following the run's journal instead of its socket: {err:#}");
                return Handover::SupervisorGone;
            }
        };
        loop {
            let next = tokio::select! {
                () = self.sender.closed() => return Handover::Done,
                next = subscription.next() => next,
            };
            match next {
                Ok(Some(event)) => {
                    if self.pass(event).await == Flow::Done {
                        return Handover::Done;
                    }
                }
                Ok(None) => return Handover::SupervisorGone,
                Err(err) => {
                    tracing::warn!("following the run's journal instead of its socket: {err:#}");
                    return Handover::SupervisorGone;
                }
            }
        }
    }

    async fn over_journal(&mut self) -> Handover {
        let from = match self.next {
            Some(next) => next,
            None => snapshot_seq(&self.events_path),
        };
        let mut tail = journal::tail(&self.events_path, from);
        let mut check = tokio::time::interval(SUPERVISOR_CHECK);
        check.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            // `None` is time to look for a supervisor.
            let step = tokio::select! {
                () = self.sender.closed() => return Handover::Done,
                event = tail.recv() => Some(event),
                _ = check.tick() => None,
            };
            match step {
                Some(Some(event)) => {
                    if self.pass(event).await == Flow::Done {
                        return Handover::Done;
                    }
                }
                // The tail only ends after the last `Finished`, which `pass` has already seen.
                Some(None) => return Handover::Done,
                None => {
                    if RunLock::is_held(&self.run_dir) {
                        if let Ok(Some(live)) = Connection::connect(&self.run_dir, self.kind).await
                        {
                            return Handover::SupervisorBack(live);
                        }
                    } else if *self.live.borrow() && self.caught_up() {
                        // A socket can close before the supervisor's last events go out, so
                        // they come from the journal before its exit does.
                        self.live.send_replace(false);
                    }
                }
            }
        }
    }

    /// Every event in the journal so far has been passed on.
    fn caught_up(&self) -> bool {
        match journal_ends::last_event(&self.events_path) {
            Some(last) => self.next.is_some_and(|next| last.seq < next),
            None => true,
        }
    }

    /// Passes `event` on unless an earlier source already did.
    async fn pass(&mut self, event: RunEvent) -> Flow {
        if self.next.is_some_and(|next| event.seq < next) {
            return Flow::Continue;
        }
        self.next = Some(event.seq + 1);
        let finished = matches!(event.kind, RunEventKind::Run(RunLifecycle::Finished { .. }))
            && !journal_ends::resumed_after(&self.events_path, event.seq);
        if self.sender.send(event).await.is_err() || finished {
            return Flow::Done;
        }
        Flow::Continue
    }
}

/// Where the latest `Run::Snapshot` sits, or 0 when there's none.
fn snapshot_seq(events: &Path) -> u64 {
    journal::read_since_snapshot(events)
        .ok()
        .and_then(|back| back.events.first().map(|event| event.seq))
        .unwrap_or(0)
}

/// The run's supervisor died: nothing holds the run, and its log ends neither parked nor
/// finished.
pub(crate) fn supervisor_crashed(run_dir: &Path) -> bool {
    !RunLock::is_held(run_dir) && !journal_ends::ends_settled(&run_dir.join(names::EVENTS))
}
