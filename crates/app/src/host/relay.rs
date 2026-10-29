//! The host's own queue of control requests, and the relay that feeds it to each engine one
//! command at a time. A request stays in the queue until an engine has taken it, so one an
//! engine never read goes to the next engine, or back to `pending-answers.jsonl` when the host
//! exits.
//!
//! Some commands (`Stop`, `StopSession`, `Pause`, `CancelPause`, `ApprovalMode`,
//! `TakeOverEnded`) carry no reply channel in the engine contract, so for them "taken" is all
//! the host can confirm, never "applied".

use std::collections::VecDeque;

use argonauts_core::control::{ControlReply, ControlRequest};
use argonauts_core::engine::{EngineCommand, EngineExit};
use argonauts_core::events::run::{RunOutcome, WaitReason};
use tokio::sync::{mpsc, oneshot};

use super::server::{EngineReply, RUN_HOST_EXITING, command, error};

/// One request, and its way back to whoever sent it.
pub(crate) struct Entry {
    pub request: ControlRequest,
    reply: oneshot::Sender<Delivery>,
}

enum Delivery {
    /// An engine took the command, and this is what it will answer.
    Taken(EngineReply),
    /// The host answered, without an engine.
    Answered(Box<ControlReply>),
}

/// Waits for the reply to one queued request.
pub(crate) struct Receipt(oneshot::Receiver<Delivery>);

impl Entry {
    pub(crate) fn new(request: ControlRequest) -> (Self, Receipt) {
        let (reply, receipt) = oneshot::channel();
        (Self { request, reply }, Receipt(receipt))
    }

    /// A sender that hung up no longer wants the reply.
    pub(crate) fn answer(self, reply: ControlReply) {
        let _ = self.reply.send(Delivery::Answered(Box::new(reply)));
    }

    fn taken(self, reply: EngineReply) {
        let _ = self.reply.send(Delivery::Taken(reply));
    }
}

impl Receipt {
    pub(crate) async fn reply(self) -> ControlReply {
        match self.0.await {
            Ok(Delivery::Taken(reply)) => reply.wait().await,
            Ok(Delivery::Answered(reply)) => *reply,
            Err(_) => error(RUN_HOST_EXITING),
        }
    }
}

/// The host's end of the queue. `put_back` puts a request ahead of the ones behind it.
pub(crate) struct Queue {
    incoming: mpsc::UnboundedReceiver<Entry>,
    backlog: VecDeque<Entry>,
}

impl Queue {
    pub(crate) fn new(incoming: mpsc::UnboundedReceiver<Entry>) -> Self {
        Self {
            incoming,
            backlog: VecDeque::new(),
        }
    }

    async fn next(&mut self) -> Option<Entry> {
        match self.backlog.pop_front() {
            Some(entry) => Some(entry),
            None => self.incoming.recv().await,
        }
    }

    /// The next request if one is waiting now.
    fn try_next(&mut self) -> Option<Entry> {
        self.backlog
            .pop_front()
            .or_else(|| self.incoming.try_recv().ok())
    }

    fn put_back(&mut self, entry: Entry) {
        self.backlog.push_front(entry);
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.backlog.is_empty() && self.incoming.is_empty()
    }

    /// Everything waiting now, oldest first.
    pub(crate) fn take_all(&mut self) -> Vec<Entry> {
        let mut entries: Vec<Entry> = self.backlog.drain(..).collect();
        while let Ok(entry) = self.incoming.try_recv() {
            entries.push(entry);
        }
        entries
    }

    /// Returns entries taken with `take_all` to the front, in their order.
    pub(crate) fn restore(&mut self, entries: Vec<Entry>) {
        for entry in entries.into_iter().rev() {
            self.put_back(entry);
        }
    }

    /// Stops taking requests and returns every one still waiting.
    pub(crate) fn close(&mut self) -> Vec<Entry> {
        self.incoming.close();
        self.take_all()
    }
}

/// A request sent on to an engine, and the reply its command brings, until the relay sees the
/// engine take it.
pub(crate) type InFlight = (Entry, EngineReply);

/// What one engine's relay did: how many commands it took, and the last one sent when the
/// engine closed its inbox before the relay saw it taken.
pub(crate) struct Forwarded {
    taken: usize,
    unsettled: Option<InFlight>,
}

impl Forwarded {
    /// Settles the last command once the engine's exit is known, and returns how many commands
    /// the engine took.
    ///
    /// An engine that takes a command and returns before the relay runs again looks the same
    /// as one that never took it. A command with a reply shows it by its answer. One without a
    /// reply counts as taken when the exit already does what it asks, such as a `Stop` and a
    /// run that finished stopped, so the next engine never gets it twice. Any other goes back to
    /// the queue: a second copy is safer than a lost one.
    pub(crate) fn settle(self, queue: &mut Queue, exit: Option<&EngineExit>) -> usize {
        let Some((entry, reply)) = self.unsettled else {
            return self.taken;
        };
        if reply.answered() || exit.is_some_and(|exit| done_by(&entry.request, exit)) {
            entry.taken(reply);
            return self.taken + 1;
        }
        queue.put_back(entry);
        self.taken
    }
}

/// Whether `exit` already did what `request` asks, so delivering it again changes nothing.
fn done_by(request: &ControlRequest, exit: &EngineExit) -> bool {
    matches!(
        (request, exit),
        (
            ControlRequest::Stop,
            EngineExit::Finished(RunOutcome::Stopped)
        ) | (
            ControlRequest::Pause { .. },
            EngineExit::Waiting(WaitReason::Paused)
        )
    )
}

/// Puts the first waiting request in a new engine's `inbox` before the engine runs. An engine
/// reads its inbox as it starts to learn what woke the run, such as your answer, and the relay
/// may not get its first turn until later. [`forward`] follows the command from here.
pub(crate) fn prime(queue: &mut Queue, inbox: &mpsc::Sender<EngineCommand>) -> Option<InFlight> {
    while let Some(entry) = queue.try_next() {
        let (command, reply) = match command(entry.request.clone()) {
            Ok(made) => made,
            Err(refused) => {
                entry.answer(error(refused));
                continue;
            }
        };
        // A new inbox has room for it. Were it refused, the relay would send it later.
        if inbox.try_send(command).is_err() {
            queue.put_back(entry);
            return None;
        }
        return Some((entry, reply));
    }
    None
}

/// Feeds `queue` to one engine through `inbox`, which holds one command, after `primed`, the
/// one [`prime`] put there. The next `reserve` succeeding, or a reply, is how the relay knows
/// the engine took the last one. Returns when the engine drops its inbox;
/// [`Forwarded::settle`] then decides the last command.
pub(crate) async fn forward(
    queue: &mut Queue,
    inbox: mpsc::Sender<EngineCommand>,
    primed: Option<InFlight>,
) -> Forwarded {
    let mut taken = 0;
    let mut in_flight = primed;
    loop {
        let Ok(permit) = inbox.reserve().await else {
            break;
        };
        if let Some((entry, reply)) = in_flight.take() {
            taken += 1;
            entry.taken(reply);
        }
        let next = tokio::select! {
            entry = queue.next() => entry,
            () = inbox.closed() => None,
        };
        let Some(entry) = next else {
            break;
        };
        match command(entry.request.clone()) {
            Ok((command, reply)) => {
                permit.send(command);
                in_flight = Some((entry, reply));
            }
            Err(refused) => entry.answer(error(refused)),
        }
    }
    Forwarded {
        taken,
        unsettled: in_flight,
    }
}

/// What a client does with the request when no run host answers: queue it for the next one.
/// A take-back isn't one, since only a live run host knows which messages it still holds.
pub(crate) fn deferrable(request: &ControlRequest) -> bool {
    matches!(
        request,
        ControlRequest::Answer { .. }
            | ControlRequest::Guidance { .. }
            | ControlRequest::Mention { .. }
            | ControlRequest::Reassign { .. }
            | ControlRequest::Stop
            | ControlRequest::StopSession { .. }
            | ControlRequest::ApprovalMode { .. }
            | ControlRequest::TakeOverEnded { .. }
            | ControlRequest::SpotCheck { .. }
            | ControlRequest::Goal { .. }
            | ControlRequest::Council { .. }
    )
}

/// The reply for a request saved to `pending-answers.jsonl`, as a client reports a deferred one.
pub(crate) fn deferred_reply(request: &ControlRequest) -> ControlReply {
    match request {
        ControlRequest::Guidance { .. } | ControlRequest::Mention { .. } => {
            error("the run is waiting; the message is delivered when it resumes")
        }
        _ => ControlReply::Ok,
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use argonauts_core::settings::approvals::ApprovalMode;

    use super::*;
    use crate::host::server::tests::TOKEN;

    fn approval_mode() -> ControlRequest {
        ControlRequest::ApprovalMode {
            mode: ApprovalMode::Auto,
            token: TOKEN.to_owned(),
        }
    }

    #[tokio::test]
    async fn a_command_the_engine_never_took_goes_to_the_next_engine() {
        let (sender, incoming) = mpsc::unbounded_channel();
        let mut queue = Queue::new(incoming);
        let (entry, receipt) = Entry::new(approval_mode());
        sender.send(entry).unwrap();
        let mut replied = tokio::spawn(receipt.reply());

        let (inbox, unread) = mpsc::channel(1);
        let (forwarded, ()) = tokio::join!(forward(&mut queue, inbox, None), async {
            tokio::task::yield_now().await;
            drop(unread);
        });
        assert_eq!(forwarded.settle(&mut queue, None), 0);
        assert!(!queue.is_empty(), "the command is still the host's");
        assert!(
            tokio::time::timeout(Duration::from_millis(50), &mut replied)
                .await
                .is_err(),
            "nothing is acknowledged before an engine takes it"
        );

        let (inbox, mut read) = mpsc::channel(1);
        let (release, released) = oneshot::channel::<()>();
        let engine = tokio::spawn(async move {
            let command = read.recv().await;
            let _ = released.await;
            command
        });
        let (forwarded, ()) = tokio::join!(forward(&mut queue, inbox, None), async {
            let reply = (&mut replied).await.unwrap();
            assert_eq!(reply, ControlReply::Ok, "acknowledged once taken");
            release.send(()).unwrap();
        });
        assert_eq!(forwarded.settle(&mut queue, None), 1);
        assert!(queue.is_empty());
        assert!(matches!(
            engine.await.unwrap(),
            Some(EngineCommand::ApprovalMode { .. })
        ));
    }

    /// What `prime` puts in the inbox is followed like any other command: one engine never
    /// takes it, so the next gets it, and that one's taking it is what acknowledges it.
    #[tokio::test]
    async fn a_primed_command_is_followed_like_any_other() {
        let (sender, incoming) = mpsc::unbounded_channel();
        let mut queue = Queue::new(incoming);
        let subscribe = ControlRequest::Subscribe {
            from_seq: 0,
            from_snapshot: false,
        };
        sender.send(Entry::new(subscribe).0).unwrap();
        let (entry, receipt) = Entry::new(approval_mode());
        sender.send(entry).unwrap();
        let mut replied = tokio::spawn(receipt.reply());

        let (inbox, unread) = mpsc::channel(1);
        let primed = prime(&mut queue, &inbox);
        assert!(
            queue.is_empty(),
            "past the one no engine takes, it waits in the inbox"
        );
        drop(unread);
        let forwarded = forward(&mut queue, inbox, primed).await;
        assert_eq!(forwarded.settle(&mut queue, None), 0);
        assert!(!queue.is_empty(), "the next engine gets it");
        assert!(
            tokio::time::timeout(Duration::from_millis(50), &mut replied)
                .await
                .is_err(),
            "nothing is acknowledged before an engine takes it"
        );

        let (inbox, mut read) = mpsc::channel(1);
        let primed = prime(&mut queue, &inbox);
        assert!(
            matches!(read.try_recv(), Ok(EngineCommand::ApprovalMode { .. })),
            "it's there before the engine first looks"
        );
        let (forwarded, ()) = tokio::join!(forward(&mut queue, inbox, primed), async {
            let reply = (&mut replied).await.unwrap();
            assert_eq!(reply, ControlReply::Ok, "acknowledged once taken");
            drop(read);
        });
        assert_eq!(forwarded.settle(&mut queue, None), 1);
        assert!(queue.is_empty());
    }

    /// The engine reads the command and returns before the relay polls again, which a
    /// single-threaded join makes certain.
    async fn taken_then_returned(request: ControlRequest, exit: EngineExit) -> (usize, Queue) {
        let (sender, incoming) = mpsc::unbounded_channel();
        let mut queue = Queue::new(incoming);
        sender.send(Entry::new(request).0).unwrap();
        let (inbox, mut read) = mpsc::channel(1);
        let (forwarded, read_one) = tokio::join!(forward(&mut queue, inbox, None), async move {
            let command = read.recv().await;
            drop(read);
            command.is_some()
        });
        assert!(read_one, "the engine took the command");
        let taken = forwarded.settle(&mut queue, Some(&exit));
        (taken, queue)
    }

    #[tokio::test]
    async fn a_command_the_exit_already_did_is_never_sent_again() {
        let stop = taken_then_returned(
            ControlRequest::Stop,
            EngineExit::Finished(RunOutcome::Stopped),
        );
        let (taken, queue) = stop.await;
        assert_eq!(taken, 1);
        assert!(
            queue.is_empty(),
            "the next engine doesn't stop the run again"
        );

        let pause = taken_then_returned(
            ControlRequest::Pause {
                now: true,
                outside_agent: None,
            },
            EngineExit::Waiting(WaitReason::Paused),
        );
        let (taken, queue) = pause.await;
        assert_eq!(taken, 1);
        assert!(
            queue.is_empty(),
            "the next engine doesn't pause the run again"
        );
    }

    #[tokio::test]
    async fn a_command_the_exit_doesnt_explain_goes_to_the_next_engine() {
        let (taken, queue) =
            taken_then_returned(approval_mode(), EngineExit::Waiting(WaitReason::Approval)).await;
        assert_eq!(taken, 0);
        assert!(!queue.is_empty(), "a second copy rather than a lost one");

        let (taken, queue) = taken_then_returned(
            ControlRequest::Stop,
            EngineExit::Waiting(WaitReason::Approval),
        )
        .await;
        assert_eq!(taken, 0);
        assert!(!queue.is_empty(), "the run didn't stop, so the stop waits");
    }

    #[tokio::test]
    async fn closing_returns_what_is_left_in_order() {
        let (sender, incoming) = mpsc::unbounded_channel();
        let mut queue = Queue::new(incoming);
        for request in [approval_mode(), ControlRequest::Stop] {
            sender.send(Entry::new(request).0).unwrap();
        }
        let first = queue.take_all();
        sender
            .send(Entry::new(ControlRequest::CancelPause).0)
            .unwrap();
        queue.restore(first);
        let left: Vec<_> = queue
            .close()
            .into_iter()
            .map(|entry| entry.request)
            .collect();
        assert_eq!(
            left,
            [
                approval_mode(),
                ControlRequest::Stop,
                ControlRequest::CancelPause
            ]
        );
        assert!(sender.send(Entry::new(ControlRequest::Stop).0).is_err());
    }
}
