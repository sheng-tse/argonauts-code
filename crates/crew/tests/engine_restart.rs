//! Restarting a task: giving a running task to the worker it already has stops the session at
//! work and starts one of its own, which carries no earlier session on.

mod support;

use std::sync::Arc;
use std::time::Duration;

use serde_json::{Value, json};
use tokio::sync::oneshot;

use argonauts_core::TaskId;
use argonauts_core::engine::{EngineCommand, EngineExit};
use argonauts_core::events::run::{RunMode, WaitReason};
use argonauts_core::events::session::{EndReason, SessionEvent};
use argonauts_core::events::task::{TaskEvent, TryCause};
use argonauts_core::events::{RunEvent, RunEventKind};
use argonauts_core::names::ModelRef;
use argonauts_core::session_runner::{Access, SessionControl, SessionSpec};
use argonauts_crew::task::take_over;
use argonauts_testkit::runner::{FakeRunner, FakeSession};

use support::{Crew, Spawned, names};

const REQUEST: &str = "Fix the typo in the README's install section";
const WORKER: &str = "claude-code/opus";

fn report() -> Value {
    json!({
        "status": "done",
        "summary": "Fixed the typo",
        "files_changed": [],
        "tests_run": [],
        "concerns": [],
        "questions": [],
        "change_requests": [],
        "memory_proposals": [],
        "answer": "I fixed the misspelled word in the README's install section."
    })
}

/// A Captain crew whose main agent is a fake Claude Code.
fn captain_crew() -> anyhow::Result<(Crew, Arc<FakeRunner>)> {
    let mut crew = Crew::new()?;
    crew.settings.crew.main = WORKER.parse()?;
    crew.settings.workflows.adapt_at_phase_ends = false;
    crew.settings.evolution.propose_every = 0;
    let worker = crew.worker(WORKER, "anthropic");
    Ok((crew, worker))
}

fn builds(worker: &FakeRunner) -> Vec<SessionSpec> {
    worker
        .specs()
        .into_iter()
        .filter(|spec| spec.access == Access::Build)
        .collect()
}

/// The run's one task, once its build session has started.
async fn building(run: &Spawned) -> anyhow::Result<TaskId> {
    for _ in 0..500 {
        let events = run.sink.events();
        if names(&events).contains(&"session/started".to_owned()) {
            let task = events.iter().find_map(|event| match &event.kind {
                RunEventKind::Task(TaskEvent::Try { .. }) => event.task_id.clone(),
                _ => None,
            });
            return task.ok_or_else(|| anyhow::anyhow!("a session started with no try"));
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    anyhow::bail!("no build started: {:?}", names(&run.sink.events()))
}

async fn reassign(run: &Spawned, task: &TaskId, to: &str) -> anyhow::Result<Result<(), String>> {
    let (reply, answered) = oneshot::channel();
    run.commands
        .send(EngineCommand::Reassign {
            task: task.clone(),
            to: to.parse()?,
            rule_override: None,
            reply,
        })
        .await?;
    Ok(answered.await?)
}

fn tries(events: &[RunEvent]) -> Vec<(u32, ModelRef, TryCause)> {
    events
        .iter()
        .filter_map(|event| match &event.kind {
            RunEventKind::Task(TaskEvent::Try {
                try_number,
                worker,
                cause,
            }) => Some((*try_number, worker.clone(), *cause)),
            _ => None,
        })
        .collect()
}

#[tokio::test]
async fn a_running_task_given_to_its_own_worker_starts_over() -> anyhow::Result<()> {
    let (crew, worker) = captain_crew()?;
    // The first build goes on until it is stopped. The fresh one reports.
    worker.push(FakeSession::until_stopped(Vec::new()));
    worker.push(FakeSession::structured(report()));
    let _env = crew.repo.env_guard();
    let run = crew.spawn(RunMode::Captain, REQUEST)?;
    let task = building(&run).await?;

    assert_eq!(reassign(&run, &task, WORKER).await?, Ok(()));
    let exit = tokio::time::timeout(Duration::from_secs(30), run.exit).await??;
    assert_eq!(exit, EngineExit::Waiting(WaitReason::Approval));

    let events = run.sink.events();
    let main: ModelRef = WORKER.parse()?;
    assert_eq!(
        tries(&events),
        [
            (1, main.clone(), TryCause::First),
            (2, main, TryCause::Restarted),
        ]
    );
    // It was stopped at once, with no wrap-up to wait for.
    let controls: Vec<SessionControl> = worker.controls().into_iter().map(|(_, c)| c).collect();
    assert_eq!(controls, [SessionControl::Stop]);
    // Nobody else got the task, so the log says no reassignment.
    assert!(
        !events.iter().any(|event| matches!(
            &event.kind,
            RunEventKind::Task(TaskEvent::Reassigned { .. })
        )),
        "{:?}",
        names(&events)
    );
    let ends: Vec<EndReason> = events
        .iter()
        .filter_map(|event| match &event.kind {
            RunEventKind::Session(SessionEvent::Ended(end)) => Some(end.reason.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(ends, [EndReason::Stopped, EndReason::Completed]);

    // The second build is a session of its own, told why it starts.
    let builds = builds(&worker);
    assert_eq!(builds.len(), 2);
    assert!(builds[1].resume.is_none(), "{:?}", builds[1].resume);
    let told = serde_json::to_string(&builds[1].prompt)?;
    assert!(told.contains("you restarted it"), "{told}");
    Ok(())
}

#[tokio::test]
async fn a_task_with_no_session_at_work_has_nothing_to_restart() -> anyhow::Result<()> {
    let (crew, worker) = captain_crew()?;
    worker.push(FakeSession::structured(report()));
    let _env = crew.repo.env_guard();
    let whole = crew.start(RunMode::Captain, REQUEST).await?;
    assert_eq!(whole.exit, EngineExit::Waiting(WaitReason::Approval));
    // The run as it stood when its task had a try and no session yet.
    let tried = whole
        .events
        .iter()
        .position(|event| matches!(&event.kind, RunEventKind::Task(TaskEvent::Try { .. })))
        .expect("the task's first try");
    let task = whole.events[tried].task_id.clone().expect("the try's task");
    let (host, sink) = crew.host_over(whole.events[..=tried].to_vec())?;
    let before = sink.events().len();
    let controls = worker.controls().len();

    let refused = take_over::reassign(&host, &task, WORKER.parse()?, None).await;
    assert_eq!(
        refused,
        Err(format!(
            "{WORKER} already has {task}, and no session of it is at work to restart"
        ))
    );
    assert_eq!(sink.events().len(), before, "nothing was recorded");
    assert_eq!(
        worker.controls().len(),
        controls,
        "no session was told to stop"
    );

    // Once it is merged into the run, it can't move at all.
    let (host, _) = crew.host_over(whole.events.clone())?;
    let refused = take_over::reassign(&host, &task, WORKER.parse()?, None).await;
    assert_eq!(refused, Err(format!("{task} is Merged, so it can't move")));
    Ok(())
}
