//! The session's side of its handle. Events queue without bound, so reading the agent's pipes
//! never waits on a slow consumer, and a forwarder moves them into the handle's channel. The end
//! goes out only after the last event, once the events sender is dropped.

use argonauts_core::events::VendorEvent;
use argonauts_core::events::session::{EndReason, SessionEnd, SessionEvent};
use argonauts_core::names::MessageId;
use argonauts_core::names::SessionKey;
use argonauts_core::worker::{SessionControl, SessionHandle, WorkerEvent};
use argonauts_core::{AgentEvent, ErrorCategory};
use tokio::sync::{mpsc, oneshot};

/// Events the handle buffers before the forwarder waits for the consumer.
const EVENT_BUFFER: usize = 256;
const CONTROL_BUFFER: usize = 32;

enum Out {
    Event(WorkerEvent),
    End(Box<SessionEnd>),
}

#[derive(Clone)]
pub struct Outbox {
    tx: mpsc::UnboundedSender<Out>,
}

impl Outbox {
    pub fn send(&self, event: WorkerEvent) {
        // Only fails once the forwarder is gone, and then nobody is listening.
        let _ = self.tx.send(Out::Event(event));
    }

    pub fn session(&self, event: SessionEvent) {
        self.send(WorkerEvent::Session(event));
    }

    pub fn agent(&self, event: AgentEvent) {
        self.send(WorkerEvent::Agent(event));
    }

    pub fn vendor(&self, event: VendorEvent) {
        self.send(WorkerEvent::Vendor(event));
    }

    /// Something that went wrong in the worker rather than the agent, such as a raw log it
    /// couldn't write. Sessions have no event of their own for it, so it goes out as a retryable
    /// `Error` that the end never reads.
    pub fn notice(&self, message: impl Into<String>) {
        self.agent(AgentEvent::Error {
            category: ErrorCategory::Unknown,
            message: message.into(),
            retryable: true,
        });
    }

    /// A message the agent never got, so whoever sent it can send it again.
    pub fn undelivered(&self, message: &MessageId, why: &str) {
        self.notice(format!("message {message} was not delivered: {why}"));
    }

    pub fn end(self, end: SessionEnd) {
        let _ = self.tx.send(Out::End(Box::new(end)));
    }
}

/// The handle, the session's outbox, and the controls it reads.
pub fn open(key: SessionKey) -> (SessionHandle, Outbox, mpsc::Receiver<SessionControl>) {
    let (events_tx, events) = mpsc::channel(EVENT_BUFFER);
    let (control, control_rx) = mpsc::channel(CONTROL_BUFFER);
    let (ended_tx, ended) = oneshot::channel();
    let (tx, rx) = mpsc::unbounded_channel();
    tokio::spawn(forward(rx, events_tx, ended_tx));
    let handle = SessionHandle {
        key,
        events,
        control,
        ended,
    };
    (handle, Outbox { tx }, control_rx)
}

async fn forward(
    mut rx: mpsc::UnboundedReceiver<Out>,
    events: mpsc::Sender<WorkerEvent>,
    ended: oneshot::Sender<SessionEnd>,
) {
    let mut end = None;
    while let Some(out) = rx.recv().await {
        match out {
            Out::Event(event) => {
                // A consumer that dropped its receiver still gets the end.
                let _ = events.send(event).await;
            }
            Out::End(last) => {
                end = Some(*last);
                break;
            }
        }
    }
    drop(events);
    let end = end.unwrap_or_else(|| lost_end("the session's supervisor stopped unexpectedly"));
    let _ = ended.send(end);
}

fn lost_end(message: &str) -> SessionEnd {
    SessionEnd {
        vendor_session: None,
        exit_code: None,
        signal: None,
        duration_ms: 0,
        reason: EndReason::Crashed {
            message: message.to_owned(),
        },
        spend: Default::default(),
        tool_calls: 0,
        result_text: None,
        structured: None,
        last_message: None,
        raw_log: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use argonauts_core::events::session::FileDiff;

    fn diff(path: &str) -> SessionEvent {
        SessionEvent::FileDiff(FileDiff {
            path: path.into(),
            added: 1,
            removed: 0,
            hunk: String::new(),
            truncated: false,
        })
    }

    #[tokio::test]
    async fn the_end_follows_every_event() {
        let (mut handle, out, _control) = open(SessionKey::numbered(1));
        for n in 0..(EVENT_BUFFER * 2) {
            out.session(diff(&n.to_string()));
        }
        out.end(lost_end("done"));
        let mut seen = 0;
        while let Some(event) = handle.events.recv().await {
            assert!(matches!(
                event,
                WorkerEvent::Session(SessionEvent::FileDiff(_))
            ));
            seen += 1;
        }
        assert_eq!(seen, EVENT_BUFFER * 2);
        let end = handle.ended.await.unwrap();
        assert!(matches!(end.reason, EndReason::Crashed { .. }));
    }

    #[tokio::test]
    async fn a_lost_session_still_ends() {
        let (handle, out, _control) = open(SessionKey::numbered(2));
        drop(out);
        drop(handle.events);
        let end = handle.ended.await.unwrap();
        assert!(matches!(end.reason, EndReason::Crashed { .. }));
    }
}
