//! A client's subscription to the run's events.

use std::sync::Arc;

use argonauts_core::runs::events::RunEvent;
use argonauts_core::runs::journal::{read_all, read_since_snapshot};
use tokio::sync::broadcast::error::RecvError;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use super::{ServerState, error};
use crate::control::{ControlReply, Frame};

/// Replays the journal from `from_seq` (or its latest snapshot), then streams live events. A
/// subscriber that lagged behind the broadcast is caught up from the journal.
pub(super) async fn subscription(
    state: Arc<ServerState>,
    id: u64,
    from_seq: u64,
    from_snapshot: bool,
    out: mpsc::Sender<Frame<ControlReply>>,
    closing: CancellationToken,
) {
    // Subscribe before reading, so nothing written in between is missed.
    let mut live = state.events.subscribe();
    let backlog = if from_snapshot {
        read_since_snapshot(&state.journal)
    } else {
        read_all(&state.journal)
    };
    let mut next = if from_snapshot { 0 } else { from_seq };
    let backlog = match backlog {
        Ok(back) => back.events,
        Err(err) => {
            let _ = out
                .send(Frame {
                    id,
                    body: error(err.to_string()),
                })
                .await;
            return;
        }
    };
    if !send_events(&out, id, backlog, &mut next).await {
        return;
    }
    loop {
        let received = tokio::select! {
            // Events already recorded go out before the connection closes, so the subscriber
            // sees the last ones, such as `Finished`.
            biased;
            received = live.recv() => received,
            () = closing.cancelled() => return,
        };
        let sent = match received {
            // A gap means an event went by unseen, so the journal fills it in, just as it does
            // after a lag.
            Ok(event) if event.seq <= next => send_events(&out, id, vec![event], &mut next).await,
            Ok(_) | Err(RecvError::Lagged(_)) => match read_all(&state.journal) {
                Ok(back) => send_events(&out, id, back.events, &mut next).await,
                Err(err) => {
                    let _ = out
                        .send(Frame {
                            id,
                            body: error(err.to_string()),
                        })
                        .await;
                    false
                }
            },
            Err(RecvError::Closed) => false,
        };
        if !sent {
            return;
        }
    }
}

/// Sends the events at or after `next`, in order. False once the connection is gone.
async fn send_events(
    out: &mpsc::Sender<Frame<ControlReply>>,
    id: u64,
    events: Vec<RunEvent>,
    next: &mut u64,
) -> bool {
    for event in events {
        if event.seq < *next {
            continue;
        }
        *next = event.seq + 1;
        let frame = Frame {
            id,
            body: ControlReply::Event {
                event: Box::new(event),
            },
        };
        if out.send(frame).await.is_err() {
            return false;
        }
    }
    true
}
