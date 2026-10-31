//! Messages reach a session once: one posted while the session starts reaches it as soon as it
//! is live, two posters handing over one message at once deliver it once, and one a run
//! recorded queued but never delivered reaches the session once the run is rebuilt.

use crate::support;
use std::path::Path;
use std::sync::{Arc, Barrier};
use std::time::Duration;

use tokio::sync::Notify;

use argonauts_core::CommandSpec;
use argonauts_core::agents::session_runner::{
    Access, RunnerCapabilities, RunnerError, RunnerKind, SessionControl, SessionHandle,
    SessionRunner, SessionSpec,
};
use argonauts_core::base::future::BoxFuture;
use argonauts_core::base::names::{AgentId, MessageId, ModelRef, RoleName, SessionKey};
use argonauts_core::runs::events::run::{Party, RunMode};
use argonauts_core::runs::events::session::{Delivery, SessionEvent};
use argonauts_core::runs::events::shared::{BlackboardEvent, BlackboardKind, Recipient};
use argonauts_core::runs::events::{RunEvent, RunEventKind};
use argonauts_crew::blackboard::{self, Post, commands, delivery};
use argonauts_crew::engine::host::{Host, SessionPlan};
use argonauts_testkit::engine::runner::{FakeRunner, FakeSession};
use argonauts_testkit::pty::deadline;

const WORKER: &str = "codex/gpt-x";

/// A fake worker whose start waits until the test lets it go on, as a CLI that takes a while
/// to come up does.
struct SlowToStart {
    inner: Arc<FakeRunner>,
    /// Told when a start has begun.
    starting: Arc<Notify>,
    /// Waited on before the start goes on.
    go_on: Arc<Notify>,
}

impl SessionRunner for SlowToStart {
    fn model_ref(&self) -> &ModelRef {
        self.inner.model_ref()
    }

    fn model_run(&self) -> ModelRef {
        self.inner.model_run()
    }

    fn kind(&self) -> RunnerKind {
        self.inner.kind()
    }

    fn agent_id(&self) -> AgentId {
        self.inner.agent_id()
    }

    fn provider(&self) -> String {
        self.inner.provider()
    }

    fn capabilities(&self) -> RunnerCapabilities {
        self.inner.capabilities()
    }

    fn start(&self, spec: SessionSpec) -> BoxFuture<'_, Result<SessionHandle, RunnerError>> {
        Box::pin(async move {
            self.starting.notify_one();
            self.go_on.notified().await;
            self.inner.start(spec).await
        })
    }

    fn take_over_command(&self, cli_session: &str, workdir: &Path) -> Option<CommandSpec> {
        self.inner.take_over_command(cli_session, workdir)
    }
}

fn plan(host: &Host) -> anyhow::Result<SessionPlan> {
    Ok(SessionPlan {
        without_status: false,
        at: None,
        role: RoleName::Worker,
        worker: WORKER.parse()?,
        task: None,
        access: Access::ReadOnly,
        step: None,
        workdir: host.repo().root.clone(),
        parts: Vec::new(),
        json_schema: None,
        resume: None,
        slot: 0,
        label: None,
        reviews: false,
        fresh: false,
        independent_of: Vec::new(),
        in_place_of: None,
        fork_of: None,
    })
}

fn delivered(events: &[RunEvent], message: &MessageId) -> Vec<SessionKey> {
    events
        .iter()
        .filter_map(|event| match &event.kind {
            RunEventKind::Blackboard(BlackboardEvent::Delivered { message: id, to })
                if id == message =>
            {
                Some(to.clone())
            }
            _ => None,
        })
        .collect()
}

fn deliveries_of(controls: &[(SessionKey, SessionControl)], message: &MessageId) -> usize {
    controls
        .iter()
        .filter(|(_, control)| {
            matches!(control, SessionControl::Deliver(queued) if &queued.id == message)
        })
        .count()
}

#[tokio::test]
async fn guidance_posted_while_a_session_starts_reaches_it_once_live() -> anyhow::Result<()> {
    let mut crew = support::Crew::new()?;
    let inner = Arc::new(FakeRunner::cli(WORKER, "openai"));
    inner.push(FakeSession::until_stopped(Vec::new()));
    let starting = Arc::new(Notify::new());
    let go_on = Arc::new(Notify::new());
    crew.services.workers.add_worker(Arc::new(SlowToStart {
        inner: inner.clone(),
        starting: starting.clone(),
        go_on: go_on.clone(),
    }));
    let reference: ModelRef = WORKER.parse()?;
    crew.settings.crew.main = reference.clone();
    crew.settings.crew.add(reference);
    let (host, sink) = crew.host(RunMode::Captain)?;

    let start = tokio::spawn({
        let host = host.clone();
        let plan = plan(&host)?;
        async move { host.start(plan).await }
    });
    starting.notified().await;
    // The session has its first prompt and isn't live yet, so no poster can reach it.
    let guidance = commands::guidance(&host, "Keep the old flag as an alias.".to_owned())
        .await
        .map_err(anyhow::Error::msg)?;
    assert!(delivered(&sink.events(), &guidance).is_empty());
    go_on.notify_one();
    let running = start.await??;

    let deadline = deadline::after(Duration::from_secs(10));
    while deliveries_of(&inner.controls(), &guidance) == 0 {
        assert!(
            std::time::Instant::now() < deadline,
            "the guidance never reached the session: {:?}",
            inner.controls()
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(
        delivered(&sink.events(), &guidance),
        std::slice::from_ref(&running.key)
    );
    let queued = sink
        .events()
        .into_iter()
        .find_map(|event| match event.kind {
            RunEventKind::Session(SessionEvent::Queued(queued)) if queued.id == guidance => {
                Some(queued)
            }
            _ => None,
        });
    assert_eq!(
        queued.map(|queued| queued.delivery),
        Some(Delivery::NextTurn)
    );

    // Catching up again finds nothing left to hand over.
    assert!(delivery::catch_up(&host, &running.key)?.is_empty());
    host.control(&running.key, SessionControl::Stop)
        .map_err(anyhow::Error::msg)?;
    running.ended().await?;
    assert_eq!(deliveries_of(&inner.controls(), &guidance), 1);
    Ok(())
}

fn queued(events: &[RunEvent], message: &MessageId) -> usize {
    events
        .iter()
        .filter(|event| {
            matches!(
                &event.kind,
                RunEventKind::Session(SessionEvent::Queued(queued)) if &queued.id == message
            )
        })
        .count()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn posters_handing_over_one_message_at_once_deliver_it_once() -> anyhow::Result<()> {
    const POSTERS: usize = 8;
    let mut crew = support::Crew::new()?;
    let worker = crew.worker(WORKER, "openai");
    worker.push(FakeSession::until_stopped(Vec::new()));
    let (host, sink) = crew.host(RunMode::Captain)?;
    let running = host.start(plan(&host)?).await?;
    let session = running.key.clone();
    for round in 0..20 {
        let message = blackboard::post(
            &host,
            Post {
                kind: BlackboardKind::Guidance,
                from: Party::User,
                to: Recipient::All,
                text: format!("Note {round}."),
                in_reply_to: None,
                refs: Vec::new(),
                from_task: None,
            },
        )?;
        // Every poster checks and hands over at the same moment. Each hand-over records the
        // message queued before it sends it, so a second send shows as a second record.
        let together = Barrier::new(POSTERS);
        std::thread::scope(|scope| {
            for _ in 0..POSTERS {
                scope.spawn(|| {
                    together.wait();
                    delivery::queue(&host, &message, &session).expect("queuing the message");
                });
            }
        });
        let events = sink.events();
        assert_eq!(queued(&events, &message), 1, "round {round}");
        assert_eq!(
            delivered(&events, &message),
            std::slice::from_ref(&session),
            "round {round}"
        );
    }
    host.control(&session, SessionControl::Stop)
        .map_err(anyhow::Error::msg)?;
    running.ended().await?;
    Ok(())
}

/// How many times `session` got `message`: in a first prompt or as a hand-over to its turn.
fn times_given(
    worker: &FakeRunner,
    message: &MessageId,
    text: &str,
    session: &SessionKey,
) -> usize {
    let in_prompts = worker
        .specs()
        .iter()
        .filter(|spec| &spec.key == session)
        .flat_map(|spec| spec.prompt.iter())
        .filter(|part| part.text.contains(text))
        .count();
    let handed: Vec<_> = worker
        .controls()
        .into_iter()
        .filter(|(to, _)| to == session)
        .collect();
    in_prompts + deliveries_of(&handed, message)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_message_queued_but_never_delivered_reaches_its_session_once_after_a_replay()
-> anyhow::Result<()> {
    const POSTERS: usize = 8;
    const TEXT: &str = "Keep the old flag as an alias.";
    let mut crew = support::Crew::new()?;
    let worker = crew.worker(WORKER, "openai");
    worker.push(FakeSession::until_stopped(Vec::new()));
    worker.push(FakeSession::until_stopped(Vec::new()));
    let (host, sink) = crew.host(RunMode::Captain)?;
    let running = host.start(plan(&host)?).await?;
    let first = running.key.clone();
    let message = blackboard::post(
        &host,
        Post {
            kind: BlackboardKind::Guidance,
            from: Party::User,
            to: Recipient::All,
            text: TEXT.to_owned(),
            in_reply_to: None,
            refs: Vec::new(),
            from_task: None,
        },
    )?;
    delivery::queue(&host, &message, &first)?;
    host.control(&first, SessionControl::Stop)
        .map_err(anyhow::Error::msg)?;
    running.ended().await?;

    // The log of a run that stopped between the two records of a hand-over: the message is
    // queued on the session, which never got it, and the session never ended.
    let mut history = sink.events();
    let at = history
        .iter()
        .position(|event| {
            matches!(
                &event.kind,
                RunEventKind::Blackboard(BlackboardEvent::Delivered { message: id, .. })
                    if *id == message
            )
        })
        .expect("the hand-over's delivered record");
    history.truncate(at);
    assert_eq!(queued(&history, &message), 1);
    assert!(delivered(&history, &message).is_empty());

    // Rebuilt from it, the run starts the session again, which catches up as it goes live,
    // and every poster reaches for the message at once, then catches up again. What the first
    // run handed over counts for nothing here.
    let first_run = times_given(&worker, &message, TEXT, &first);
    let (host, sink) = crew.host_over(history)?;
    let running = host.start(plan(&host)?).await?;
    let session = running.key.clone();
    let together = Barrier::new(POSTERS);
    std::thread::scope(|scope| {
        for _ in 0..POSTERS {
            scope.spawn(|| {
                together.wait();
                delivery::queue(&host, &message, &session).expect("queuing the message");
            });
        }
    });
    assert!(delivery::catch_up(&host, &session)?.is_empty());
    assert_eq!(
        delivered(&sink.events(), &message),
        std::slice::from_ref(&session)
    );
    host.control(&session, SessionControl::Stop)
        .map_err(anyhow::Error::msg)?;
    running.ended().await?;
    assert_eq!(
        times_given(&worker, &message, TEXT, &session) - first_run,
        1
    );
    Ok(())
}
