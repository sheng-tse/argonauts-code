//! The workflow and agent views draw the answer they were given. While one is on top, its run's
//! events ask for that answer again, about once a second, so states, counts, and times move.
//!
//! Only the top view is drawn, so each frame it says what it shows through its [`Follower`],
//! and the bridge's [`Refresh`] reads that when events arrive.

use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use argonauts_core::AgentEvent;
use argonauts_core::RunId;
use argonauts_core::app::views::{AgentRow, InnerAgent, conversation_run};
use argonauts_core::app::{AppEvent, ConversationEvent, UiCommand, ViewData, ViewRequest};
use argonauts_core::base::names::{SessionKey, WorkflowId};
use argonauts_core::runs::events::RunEventKind;
use argonauts_core::runs::events::session::SessionEvent;

/// Events that stream in, such as agent output, ask again at most this often.
const EVERY: Duration = Duration::from_secs(1);

/// A refresh answered later than this is taken for an answer someone asked for.
const ANSWER_WAIT: Duration = Duration::from_secs(30);

/// What a view shows, so it can be asked for again.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Shown {
    Workflow {
        run: RunId,
        id: WorkflowId,
        /// The run's own workflow, which is asked for with no id.
        root: bool,
    },
    Agent {
        run: RunId,
        session: SessionKey,
    },
    /// One agent of a workflow the CLI of `session` runs itself.
    InnerAgent {
        run: RunId,
        session: SessionKey,
        agent: InnerAgent,
    },
}

impl Shown {
    /// The agent `row` shows in `run`.
    pub fn agent(run: RunId, row: &AgentRow) -> Self {
        let session = row.session.clone();
        match row.inner.clone() {
            Some(agent) => Self::InnerAgent {
                run,
                session,
                agent,
            },
            None => Self::Agent { run, session },
        }
    }

    fn run(&self) -> &RunId {
        match self {
            Self::Workflow { run, .. } | Self::Agent { run, .. } | Self::InnerAgent { run, .. } => {
                run
            }
        }
    }

    fn request(&self) -> ViewRequest {
        match self {
            Self::Workflow { run, id, root } => ViewRequest::Workflow {
                run: run.clone(),
                workflow: (!root).then(|| id.clone()),
            },
            Self::Agent { run, session } => ViewRequest::Agent {
                run: run.clone(),
                session: session.clone(),
            },
            Self::InnerAgent {
                run,
                session,
                agent,
            } => ViewRequest::InnerAgent {
                run: run.clone(),
                session: session.clone(),
                agent: agent.clone(),
            },
        }
    }

    pub(super) fn answered_by(&self, data: &ViewData) -> bool {
        match (self, data) {
            (Self::Workflow { run, id, .. }, ViewData::Workflow { view }) => {
                view.run == *run && view.id == *id
            }
            (Self::Agent { run, session }, ViewData::Agent { view }) => {
                view.run == *run && view.row.session == *session && view.row.inner.is_none()
            }
            (Self::InnerAgent { run, agent, .. }, ViewData::Agent { view }) => {
                view.run == *run && view.row.inner.as_ref() == Some(agent)
            }
            _ => false,
        }
    }
}

type Slot = Arc<Mutex<Option<Shown>>>;

fn lock(slot: &Slot) -> MutexGuard<'_, Option<Shown>> {
    // The slot holds plain data, so a panic elsewhere can't leave it half written.
    slot.lock().unwrap_or_else(PoisonError::into_inner)
}

/// A view's end. Dropping it, as closing the view does, says nothing is shown.
#[derive(Debug)]
pub struct Follower(Slot);

impl Follower {
    /// Another end on the same slot, for a view this one opens over itself. Whichever of the
    /// two is drawn says what's shown.
    pub fn another(&self) -> Self {
        Self(self.0.clone())
    }

    /// Called each time the view is drawn, which only the top view is.
    pub fn show(&self, shown: Shown) {
        let mut slot = lock(&self.0);
        if slot.as_ref() != Some(&shown) {
            *slot = Some(shown);
        }
    }
}

impl Drop for Follower {
    fn drop(&mut self) {
        lock(&self.0).take();
    }
}

/// The bridge's end: asks again for what the top view shows as its run's events arrive.
#[derive(Debug, Default)]
pub struct Refresh {
    shown: Slot,
    last: Option<Instant>,
    /// The last refresh, until its answer is seen or you ask for the same view. Its answer
    /// updates the view that asked and never opens a new one.
    sent: Option<(Shown, Instant)>,
    /// The run changed since the last refresh.
    owed: bool,
}

impl Refresh {
    /// The end to give a view opened from an answer.
    pub fn follower(&self) -> Follower {
        Follower(self.shown.clone())
    }

    /// True when `event` answers a refresh no view took, because the view closed or another
    /// view covers it. Refreshing then waits until the view is drawn again.
    pub fn unclaimed(&mut self, event: &AppEvent, now: Instant) -> bool {
        let AppEvent::View { data } = event else {
            return false;
        };
        let Some((shown, at)) = self.sent.take_if(|(shown, _)| shown.answered_by(data)) else {
            return false;
        };
        let mut slot = lock(&self.shown);
        if slot.as_ref() == Some(&shown) {
            slot.take();
        }
        now.saturating_duration_since(at) < ANSWER_WAIT
    }

    /// The refresh to send after `event`, if one is due.
    pub fn on_event(&mut self, event: &AppEvent, now: Instant) -> Option<UiCommand> {
        let shown = lock(&self.shown).clone();
        let Some(shown) = shown else {
            self.owed = false;
            return None;
        };
        let mut settles = false;
        if let AppEvent::Run { event } = event
            && event.run_id == *shown.run()
        {
            self.owed = true;
            settles = settles_rows(&event.kind);
        }
        // The conversation's own CLI reports its workflow as it goes.
        if let AppEvent::Conversation {
            event:
                ConversationEvent::Agent {
                    event: AgentEvent::Workflow { .. },
                    ..
                },
        } = event
            && *shown.run() == conversation_run()
        {
            self.owed = true;
        }
        self.due(shown, settles, now)
    }

    /// The refresh still owed after a key, if one is due. Asking for the same view yourself
    /// means its answer opens a view again.
    pub fn on_commands(&mut self, commands: &[UiCommand], now: Instant) -> Option<UiCommand> {
        for command in commands {
            if let UiCommand::Request { view } = command
                && self
                    .sent
                    .as_ref()
                    .is_some_and(|(shown, _)| shown.request() == *view)
            {
                self.sent = None;
            }
        }
        let shown = lock(&self.shown).clone()?;
        self.due(shown, false, now)
    }

    fn due(&mut self, shown: Shown, at_once: bool, now: Instant) -> Option<UiCommand> {
        let waited = self
            .last
            .is_none_or(|last| now.saturating_duration_since(last) >= EVERY);
        if !self.owed || !(at_once || waited) {
            return None;
        }
        self.owed = false;
        self.last = Some(now);
        let request = shown.request();
        self.sent = Some((shown, now));
        Some(UiCommand::Request { view: request })
    }
}

/// Events that change a row's state go out at once, so the last of them, such as the run's
/// end, never waits on an event that may not come.
fn settles_rows(kind: &RunEventKind) -> bool {
    matches!(
        kind,
        RunEventKind::Run(_)
            | RunEventKind::Phase(_)
            | RunEventKind::Task(_)
            | RunEventKind::Workflow(_)
            | RunEventKind::Session(SessionEvent::Started(_) | SessionEvent::Ended(_))
    )
}

#[cfg(test)]
mod tests {
    use argonauts_core::AgentEvent;
    use argonauts_core::app::views::WorkflowView as WorkflowData;
    use argonauts_core::runs::events::RunEvent;
    use argonauts_core::runs::events::run::{Party, RunLifecycle};
    use time::OffsetDateTime;

    use super::*;

    fn run() -> RunId {
        RunId::new("run-01").unwrap()
    }

    fn shown() -> Shown {
        Shown::Workflow {
            run: run(),
            id: WorkflowId::numbered(1),
            root: true,
        }
    }

    fn run_event(run: RunId, kind: RunEventKind) -> AppEvent {
        AppEvent::Run {
            event: Box::new(RunEvent {
                seq: 0,
                ts: OffsetDateTime::UNIX_EPOCH,
                run_id: run,
                task_id: None,
                session: None,
                source: "argonauts-code".into(),
                kind,
            }),
        }
    }

    fn output() -> AppEvent {
        run_event(
            run(),
            RunEventKind::Agent(AgentEvent::TextDelta {
                text: "more".into(),
                parent: None,
            }),
        )
    }

    fn paused() -> AppEvent {
        run_event(
            run(),
            RunEventKind::Run(RunLifecycle::PauseRequested {
                by: Party::User,
                now: false,
            }),
        )
    }

    fn answer() -> AppEvent {
        AppEvent::View {
            data: Box::new(ViewData::Workflow {
                view: Box::new(WorkflowData {
                    run: run(),
                    id: WorkflowId::numbered(1),
                    name: "ui-kit".into(),
                    description: String::new(),
                    agents_done: 0,
                    agents_total: 0,
                    elapsed_ms: 0,
                    spent: Default::default(),
                    phases: Vec::new(),
                    style: None,
                    status_lines: Vec::new(),
                    notes: Vec::new(),
                    required: Vec::new(),
                    goals: Vec::new(),
                    worker: None,
                    state: Default::default(),
                }),
            }),
        }
    }

    fn root_request() -> Option<UiCommand> {
        Some(UiCommand::Request {
            view: ViewRequest::Workflow {
                run: run(),
                workflow: None,
            },
        })
    }

    #[test]
    fn nothing_shown_asks_nothing() {
        let mut refresh = Refresh::default();
        assert_eq!(refresh.on_event(&output(), Instant::now()), None);
        let follower = refresh.follower();
        follower.show(shown());
        drop(follower);
        assert_eq!(refresh.on_event(&output(), Instant::now()), None);
    }

    #[test]
    fn output_asks_again_about_once_a_second() {
        let mut refresh = Refresh::default();
        let follower = refresh.follower();
        follower.show(shown());
        let t0 = Instant::now();
        assert_eq!(refresh.on_event(&output(), t0), root_request());
        let soon = t0 + Duration::from_millis(300);
        assert_eq!(refresh.on_event(&output(), soon), None);
        // Another run's events, or none at all, pay what's owed once the second is up.
        let other = run_event(
            RunId::new("run-02").unwrap(),
            RunEventKind::Agent(AgentEvent::TextDelta {
                text: "elsewhere".into(),
                parent: None,
            }),
        );
        let later = t0 + EVERY;
        assert_eq!(refresh.on_event(&other, later), root_request());
        assert_eq!(refresh.on_commands(&[], later + EVERY), None);
    }

    #[test]
    fn a_state_change_asks_at_once() {
        let mut refresh = Refresh::default();
        let follower = refresh.follower();
        follower.show(shown());
        let t0 = Instant::now();
        assert_eq!(refresh.on_event(&output(), t0), root_request());
        assert_eq!(
            refresh.on_event(&paused(), t0 + Duration::from_millis(10)),
            root_request()
        );
    }

    #[test]
    fn an_answer_no_view_took_opens_nothing() {
        let mut refresh = Refresh::default();
        let follower = refresh.follower();
        follower.show(shown());
        let t0 = Instant::now();
        refresh.on_event(&output(), t0);
        // The view went under another one, so its refresh came back unclaimed.
        assert!(refresh.unclaimed(&answer(), t0));
        assert_eq!(refresh.on_event(&paused(), t0 + EVERY), None);
        // Drawn again, it follows again.
        follower.show(shown());
        assert_eq!(refresh.on_event(&paused(), t0 + EVERY), root_request());
    }

    #[test]
    fn asking_yourself_opens_the_answer() {
        let mut refresh = Refresh::default();
        let follower = refresh.follower();
        follower.show(shown());
        let t0 = Instant::now();
        refresh.on_event(&output(), t0);
        drop(follower);
        let asked = root_request().into_iter().collect::<Vec<_>>();
        assert_eq!(refresh.on_commands(&asked, t0), None);
        assert!(!refresh.unclaimed(&answer(), t0));
    }
}
