//! A crew run that follows a workflow file's own steps: the runner runs them, each agent goes to
//! the model its role names, workers build tasks in worktrees of their own, and what the
//! workflow's reviews accepted is merged before the merge approval.

use crate::support;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

use anyhow::Context;
use serde_json::{Value, json};

use argonauts_core::engine::{EngineCommand, EngineExit};
use argonauts_core::events::approval::{
    AnswerAction, ApprovalEvent, ApprovalKind, ApprovalOpened, ApprovalPacket,
};
use argonauts_core::events::run::{Party, RunLifecycle, RunMode, RunOutcome, WaitReason};
use argonauts_core::events::session::{CliAgentState, EndReason, SessionEvent, SubagentStatus};
use argonauts_core::events::task::{ReviewEvent, TaskEvent, TaskState};
use argonauts_core::events::workflow::{WorkflowEvent, WorkflowReport};
use argonauts_core::events::{RunEvent, RunEventKind};
use argonauts_core::names::{RoleName, SessionKey};
use argonauts_core::session_runner::SessionSpec;
use argonauts_core::settings::approvals::ApprovalMode;
use argonauts_core::settings::crew::{CrewSettings, Orchestrator};
use argonauts_core::settings::sessions::{FanoutSwitch, NativeFanout};
use argonauts_core::settings::workflows::AdaptWith;
use argonauts_core::steps::StepName;
use argonauts_core::steps::judgment::ReviewVerdict;
use argonauts_crew::engine::shared::with_git_env;
use argonauts_testkit::deadline;
use argonauts_testkit::runner::{FakeRunner, FakeSession};

use support::{Crew, Outcome, answer};

const REQUEST: &str = "Add due dates to tasks";
const MAIN: &str = "claude-code/opus";
const WORKER: &str = "codex/gpt";
const REVIEWER: &str = "antigravity-cli/gemini";

const SHIP: &str = r#"
name  = "ship"
about = "Plan, build each task, review it, sign off."
adapt = false

[[step]]
name = "Plan"
role = "orchestrator"
does = "Split the request into tasks: {request}"

[[step]]
name     = "Build"
role     = "worker"
for_each = "task"

[[step]]
name         = "Review"
role         = "reviewer"
for_each     = "task"
send_back_to = "Build"

[[step]]
name = "Sign off"
role = "judge"
"#;

fn prompt(spec: &SessionSpec) -> String {
    spec.prompt
        .iter()
        .map(|part| part.text.as_str())
        .collect::<Vec<_>>()
        .join("\n")
}

fn report(summary: &str, file: &str) -> Value {
    json!({
        "status": "done",
        "summary": summary,
        "files_changed": [file],
        "tests_run": [],
        "concerns": [],
        "questions": [],
        "change_requests": [],
        "memory_proposals": [],
        "answer": summary,
    })
}

/// The main agent plans, Codex builds each task as a file of its own, and agy
/// reviews and signs off. Its first review of the first task asks for a change.
fn crew(workflow: &str) -> anyhow::Result<(Crew, Arc<FakeRunner>)> {
    let mut crew = Crew::new()?;
    crew.settings.evolution.propose_every = 0;
    let main = FakeRunner::cli(MAIN, "anthropic").respond_with(|_| {
        FakeSession::structured(json!({
            "summary": "Two tasks",
            "tasks": [
                {"title": "Due field", "done_when": ["a task has a due date"]},
                {"title": "Due docs", "done_when": ["the README says how"]},
            ],
        }))
    });
    crew.services.workers.add(main);
    let worker = FakeRunner::cli(WORKER, "openai").respond_with(|spec| {
        assert_eq!(spec.step, Some(StepName::Build), "{}", prompt(spec));
        let asked = prompt(spec);
        let file = if asked.contains("Due docs") {
            "docs.md"
        } else {
            "due.rs"
        };
        let round = if asked.contains("Sent back by Review") {
            "second"
        } else {
            "first"
        };
        std::fs::write(spec.workdir.join(file), format!("{round} try\n")).expect("writes its file");
        FakeSession::structured(report(&format!("Wrote {file}, {round} try"), file))
    });
    let worker = crew.services.workers.add(worker);
    let field_reviews = AtomicU32::new(0);
    let reviewer = FakeRunner::cli(REVIEWER, "google").respond_with(move |spec| {
        if spec.json_schema.is_none() {
            return FakeSession::completed("Both tasks are done as asked.");
        }
        let first = prompt(spec).contains("The task: Due field")
            && field_reviews.fetch_add(1, Ordering::Relaxed) == 0;
        FakeSession::structured(if first {
            json!({"accept": false, "findings": ["a due date in the past is accepted"]})
        } else {
            json!({"accept": true, "findings": []})
        })
    });
    crew.services.workers.add(reviewer);
    crew.settings.crew = CrewSettings {
        main: MAIN.parse()?,
        orchestrator: Orchestrator::Main,
        worker: vec![WORKER.parse()?],
        reviewer: vec![REVIEWER.parse()?],
        judge: vec![REVIEWER.parse()?],
        worker_workflows: false,
        merger: Vec::new(),
        orchestrator_context: Default::default(),
        orchestrator_picks: Default::default(),
        ways: Default::default(),
    };
    crew.settings.workflow = workflow.to_owned();
    crew.settings.workflows.adapt_with = AdaptWith::Script;
    Ok((crew, worker))
}

fn write_workflow(crew: &Crew, name: &str, text: &str) -> anyhow::Result<()> {
    let folder = crew.repo.root().join(".argonauts/workflows");
    std::fs::create_dir_all(&folder)?;
    std::fs::write(folder.join(name), text)?;
    Ok(())
}

/// The workflow's own sessions, in the order they started. The main agent's closing answer
/// comes after the run and isn't one of them.
fn started(events: &[RunEvent]) -> Vec<(String, RoleName, String)> {
    events
        .iter()
        .filter_map(|event| match &event.kind {
            RunEventKind::Session(SessionEvent::Started(started)) => Some((
                started.label.clone().unwrap_or_default(),
                started.role,
                started.worker.to_string(),
            )),
            _ => None,
        })
        .filter(|(_, role, _)| *role != RoleName::Main)
        .collect()
}

fn reports(events: &[RunEvent]) -> Vec<&WorkflowReport> {
    events
        .iter()
        .filter_map(|event| match &event.kind {
            RunEventKind::Workflow(WorkflowEvent::Reported(report)) => Some(report.as_ref()),
            _ => None,
        })
        .collect()
}

fn task_states(events: &[RunEvent]) -> Vec<(String, TaskState)> {
    events
        .iter()
        .filter_map(|event| match &event.kind {
            RunEventKind::Task(TaskEvent::State { to, .. }) => {
                Some((event.task_id.as_ref()?.to_string(), *to))
            }
            _ => None,
        })
        .collect()
}

fn opened(events: &[RunEvent], kind: ApprovalKind) -> bool {
    events.iter().any(|event| {
        matches!(&event.kind, RunEventKind::Approval(ApprovalEvent::Opened(opened))
            if opened.kind == kind)
    })
}

fn all_events(outcomes: &[Outcome]) -> &[RunEvent] {
    &outcomes[outcomes.len() - 1].events
}

#[tokio::test]
async fn a_workflow_files_steps_run_as_written_and_what_they_accept_is_merged() -> anyhow::Result<()>
{
    let (crew, worker) = crew("ship")?;
    write_workflow(&crew, "ship.toml", SHIP)?;
    let _env = crew.repo.env_guard();
    let outcomes = crew.run_to_end(RunMode::Crew, REQUEST, 3).await?;
    let events = all_events(&outcomes);

    // Each agent went to the model its role names, under the label the steps gave it.
    let sessions: Vec<(String, RoleName, String)> = started(events);
    let mut shown: Vec<String> = sessions
        .iter()
        .map(|(label, role, worker)| format!("{label} {} {worker}", role.as_str()))
        .collect();
    assert_eq!(shown[0], format!("plan orchestrator {MAIN}"));
    assert_eq!(shown[7], format!("sign-off judge {REVIEWER}"));
    // The two tasks run side by side, so only what ran is fixed, not its order.
    shown.sort();
    assert_eq!(
        shown,
        [
            format!("build:due-docs worker {WORKER}"),
            format!("build:due-field worker {WORKER}"),
            // The task that was sent back: its worker carries on, and it is reviewed again.
            format!("build:due-field worker {WORKER}"),
            format!("plan orchestrator {MAIN}"),
            format!("review:due-docs reviewer {REVIEWER}"),
            format!("review:due-field reviewer {REVIEWER}"),
            format!("review:due-field-2 reviewer {REVIEWER}"),
            format!("sign-off judge {REVIEWER}"),
        ],
        "{shown:#?}"
    );
    let sent_back = worker
        .specs()
        .iter()
        .map(prompt)
        .find(|asked| asked.contains("Sent back by Review"))
        .expect("the worker was sent back");
    assert!(
        sent_back.contains("- a due date in the past is accepted"),
        "{sent_back}"
    );

    // Every task was built in a worktree of its own, checked, and merged into the run.
    let states = task_states(events);
    for task in ["task-due-field", "task-due-docs"] {
        let last = states
            .iter()
            .rev()
            .find(|(id, _)| id == task)
            .map(|(_, state)| *state);
        assert_eq!(last, Some(TaskState::Merged), "{task}: {states:?}");
    }
    assert!(
        opened(events, ApprovalKind::Merge),
        "the merge approval follows"
    );
    // Each verdict of the workflow's own review is a review of the task, under the name of
    // whoever gave it, for the round of building it read.
    let reviewed: Vec<(String, String, u32, ReviewVerdict)> = events
        .iter()
        .filter_map(|event| match &event.kind {
            RunEventKind::Review(ReviewEvent::Reviewed {
                reviewer,
                round,
                review,
                ..
            }) => Some((
                event.task_id.as_ref()?.to_string(),
                reviewer.to_string(),
                *round,
                review.verdict,
            )),
            _ => None,
        })
        .collect();
    let of = |task: &str| -> Vec<(u32, ReviewVerdict)> {
        reviewed
            .iter()
            .filter(|(reviewed, reviewer, ..)| reviewed == task && reviewer == REVIEWER)
            .map(|(_, _, round, verdict)| (*round, *verdict))
            .collect()
    };
    assert_eq!(
        of("task-due-field"),
        [
            (1, ReviewVerdict::RequestChanges),
            (2, ReviewVerdict::Approve)
        ],
        "{reviewed:?}"
    );
    assert_eq!(of("task-due-docs"), [(1, ReviewVerdict::Approve)]);
    let merge = approval(events, ApprovalKind::Merge).expect("the merge approval");
    let ApprovalPacket::Merge(packet) = &merge.packet else {
        panic!("a merge approval: {:?}", merge.packet);
    };
    assert!(
        packet
            .rows
            .iter()
            .filter(|row| row.worker.is_some())
            .all(|row| row.review == Some(ReviewVerdict::Approve)),
        "every task shows as approved: {:?}",
        packet.rows
    );
    assert!(
        !opened(events, ApprovalKind::Plan),
        "the steps are the plan"
    );
    let last = outcomes.last().expect("an outcome");
    assert!(
        matches!(last.exit, EngineExit::Finished(RunOutcome::Merged)),
        "{:?}",
        last.exit
    );
    assert_eq!(
        std::fs::read_to_string(crew.repo.root().join("due.rs"))?,
        "second try\n"
    );
    assert_eq!(
        std::fs::read_to_string(crew.repo.root().join("docs.md"))?,
        "first try\n"
    );

    // The views get the workflow's own name, phases and agents, and the session behind each.
    let reports = reports(events);
    let last = reports.last().expect("the runner reported");
    assert_eq!(last.workflow.name, "ship");
    assert_eq!(last.workflow.status, SubagentStatus::Done);
    let phases: Vec<&str> = last
        .workflow
        .phases
        .iter()
        .map(|p| p.title.as_str())
        .collect();
    assert_eq!(phases, ["Plan", "Build", "Review", "Sign off"]);
    assert_eq!(last.workflow.agents.len(), 8);
    assert!(
        last.workflow
            .agents
            .iter()
            .all(|agent| agent.state == CliAgentState::Done)
    );
    let build = &last.workflow.agents[1];
    assert_eq!(
        (build.label.as_str(), build.phase),
        ("build:due-field", Some(2))
    );
    assert!(build.model.is_some(), "its row names the model that ran it");
    assert_eq!(last.sessions.len(), 8, "{:?}", last.sessions);
    assert!(
        last.session_of(build.index).is_some(),
        "{:?}",
        last.sessions
    );
    // The run's journal reads every event back from a JSON value, so each has to survive that.
    for event in events {
        let value = serde_json::to_value(&event.kind)?;
        assert_eq!(serde_json::from_value::<RunEventKind>(value)?, event.kind);
    }
    assert_eq!(
        reports[0].workflow.agents.len(),
        0,
        "the first report lists the phases alone"
    );
    Ok(())
}

#[tokio::test]
async fn a_run_that_stops_mid_workflow_picks_its_script_up_without_running_anything_twice()
-> anyhow::Result<()> {
    let (crew, worker) = crew("ship")?;
    write_workflow(&crew, "ship.toml", SHIP)?;
    let _env = crew.repo.env_guard();
    // The run waits at the merge approval, with every step of the script behind it.
    let first = crew.start(RunMode::Crew, REQUEST).await?;
    assert!(
        matches!(first.exit, EngineExit::Waiting(_)),
        "{:?}",
        first.exit
    );
    let built = worker.specs().len();
    let sessions = started(&first.events).len();

    // Reopened from its log, the script answers every call from what it saved.
    let again = crew.reopen(first.events.clone()).await?;
    assert_eq!(worker.specs().len(), built, "no task was built again");
    assert_eq!(
        started(&again.events).len(),
        sessions,
        "no session started again"
    );
    Ok(())
}

#[tokio::test]
async fn a_run_stopped_inside_its_workflow_answers_the_finished_calls_from_what_it_saved()
-> anyhow::Result<()> {
    let (mut crew, worker) = crew("ship")?;
    write_workflow(&crew, "ship.toml", SHIP)?;
    // The judge's sign-off hangs until the run is stopped. Reopened, it answers.
    let judge = FakeRunner::cli("claude-code/judge", "anthropic")
        .then(FakeSession::until_stopped(Vec::new()))
        .then(FakeSession::completed("Signed off."));
    crew.services.workers.add(judge);
    crew.settings.crew.judge = vec!["claude-code/judge".parse()?];
    let _env = crew.repo.env_guard();
    let run = crew.spawn(RunMode::Crew, REQUEST)?;
    let mut signing_off = false;
    for _ in 0..2_000 {
        signing_off = started(&run.sink.events())
            .iter()
            .any(|(label, ..)| label == "sign-off");
        if signing_off {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }
    assert!(signing_off, "{:?}", started(&run.sink.events()));
    run.commands.send(EngineCommand::Stop).await?;
    let exit = tokio::time::timeout(
        deadline::scaled(std::time::Duration::from_secs(30)),
        run.exit,
    )
    .await??;
    assert_eq!(exit, EngineExit::Finished(RunOutcome::Stopped));
    let stopped = run.sink.events();
    let built = worker.specs().len();
    let sessions = started(&stopped).len();
    assert!(
        task_states(&stopped)
            .iter()
            .all(|(_, state)| *state != TaskState::Merged),
        "nothing is merged before the workflow ends"
    );

    // Reopened, the script runs again from its top: every call it finished answers from the
    // run's own record, only the sign-off goes on, and the tasks its reviews accepted before
    // the stop are merged.
    let reopened = crew.reopen(stopped.clone()).await?;
    assert_eq!(worker.specs().len(), built, "no task was built again");
    let after = started(&reopened.events);
    assert_eq!(after.len(), sessions + 1, "{after:#?}");
    assert_eq!(after[sessions].0, "sign-off", "the same row carries on");
    let states = task_states(&reopened.events);
    for task in ["task-due-field", "task-due-docs"] {
        let last = states
            .iter()
            .rev()
            .find(|(id, _)| id == task)
            .map(|(_, state)| *state);
        assert_eq!(last, Some(TaskState::Merged), "{task}: {states:?}");
    }
    assert!(opened(&reopened.events, ApprovalKind::Merge));
    Ok(())
}

/// An agent a script's own step called works no task, so nothing but the script waits for
/// it. Asked to start over, its session is stopped and the same worker is asked again with
/// the prompt it had, and the script takes the new answer, where a stop alone leaves the
/// script without one.
#[tokio::test]
async fn an_agent_a_script_called_starts_over_and_the_script_takes_its_new_answer()
-> anyhow::Result<()> {
    let (mut crew, _worker) = crew("ship")?;
    write_workflow(&crew, "ship.toml", SHIP)?;
    // The judge's first sign-off hangs until it is stopped. Asked again, it answers.
    let judge = FakeRunner::cli("claude-code/judge", "anthropic")
        .then(FakeSession::until_stopped(Vec::new()))
        .then(FakeSession::completed("Signed off."));
    let judge = crew.services.workers.add(judge);
    crew.settings.crew.judge = vec!["claude-code/judge".parse()?];
    let _env = crew.repo.env_guard();
    let run = crew.spawn(RunMode::Crew, REQUEST)?;
    let sign_offs = |events: &[RunEvent]| -> Vec<argonauts_core::names::SessionKey> {
        events
            .iter()
            .filter_map(|event| match &event.kind {
                RunEventKind::Session(SessionEvent::Started(started))
                    if started.label.as_deref() == Some("sign-off") =>
                {
                    event.session.clone()
                }
                _ => None,
            })
            .collect()
    };
    let mut first = None;
    for _ in 0..2_000 {
        first = sign_offs(&run.sink.events()).into_iter().next();
        if first.is_some() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }
    let first = first.expect("the sign-off starts");

    // A builder's session works a task, which starts over another way: it is refused, and
    // goes on untouched.
    let builder = run
        .sink
        .events()
        .iter()
        .find_map(|event| match &event.kind {
            RunEventKind::Session(SessionEvent::Started(started))
                if started.step == Some(StepName::Build) =>
            {
                event.session.clone()
            }
            _ => None,
        })
        .expect("a builder ran");
    let restart = |session: argonauts_core::names::SessionKey| EngineCommand::StopSession {
        session,
        again: true,
    };
    run.commands.send(restart(builder)).await?;
    run.commands.send(restart(first.clone())).await?;

    // The script went on to the merge, which waits for you, with the second answer.
    let exit = tokio::time::timeout(
        deadline::scaled(std::time::Duration::from_secs(30)),
        run.exit,
    )
    .await??;
    assert!(matches!(exit, EngineExit::Waiting(_)), "{exit:?}");
    let events = run.sink.events();
    assert!(opened(&events, ApprovalKind::Merge));
    let asked = sign_offs(&events);
    assert_eq!(asked.len(), 2, "{:?}", started(&events));
    assert_eq!(asked[0], asked[1], "the same row carries on");
    let specs = judge.specs();
    assert_eq!(specs.len(), 2);
    assert_eq!(
        prompt(&specs[0]),
        prompt(&specs[1]),
        "with the prompt it had"
    );
    let ends: Vec<_> = events
        .iter()
        .filter_map(|event| match &event.kind {
            RunEventKind::Session(SessionEvent::Ended(end))
                if event.session.as_ref() == Some(&first) =>
            {
                Some(end.reason.clone())
            }
            _ => None,
        })
        .collect();
    use argonauts_core::events::session::EndReason;
    assert_eq!(ends, [EndReason::Stopped, EndReason::Completed]);
    Ok(())
}

/// The script a forked orchestrator writes for the maker-checker workflow.
const FORKED_SCRIPT: &str = r#"export const meta = {
  name: 'due-dates',
  description: 'Add due dates',
  phases: [{ name: 'Build' }],
}
await agent('Add a due date to tasks: ' + args.request, {
  role: 'worker', label: 'build:due-dates', phase: 'Build', task: 'due-dates',
})
"#;

/// A crew whose orchestrator, `orchestrator`, answers with [`FORKED_SCRIPT`] unless `answers`
/// says otherwise, in a run your words started from a Claude Code conversation.
fn forking(
    orchestrator: &str,
    answers: impl Fn(&SessionSpec) -> FakeSession + Send + Sync + 'static,
) -> anyhow::Result<(Crew, Arc<FakeRunner>)> {
    use argonauts_core::events::run::{ForkFrom, StartsFrom};

    let (mut crew, _worker) = crew("maker-checker")?;
    let fake = crew
        .services
        .workers
        .add(FakeRunner::cli(orchestrator, "anthropic").respond_with(answers));
    crew.settings.crew.orchestrator = Orchestrator::Models(vec![orchestrator.parse()?]);
    crew.starts_from = Some(StartsFrom::Fork(ForkFrom {
        worker: "claude-code/opus".parse()?,
        cli_session: "main-session-1".to_owned(),
        folder: crew.repo.root().to_path_buf(),
        tokens: Some(48_200),
    }));
    Ok((crew, fake))
}

fn the_script(_: &SessionSpec) -> FakeSession {
    FakeSession::structured(json!({ "script": FORKED_SCRIPT }))
}

/// An orchestrator on the main session's CLI starts as a copy of that session, in the folder
/// it ran in, and reads first that it now orchestrates the run.
#[tokio::test]
async fn an_orchestrator_on_the_main_sessions_cli_starts_as_its_fork() -> anyhow::Result<()> {
    let (crew, orchestrator) = forking("claude-code/fable", the_script)?;
    let _env = crew.repo.env_guard();
    crew.run_to_end(RunMode::Crew, REQUEST, 3).await?;

    let specs = orchestrator.specs();
    let plan = specs.first().context("the orchestrator's session")?;
    assert!(plan.fork, "a fork of the main session");
    assert_eq!(plan.resume.as_deref(), Some("main-session-1"));
    assert_eq!(plan.workdir, crew.repo.root());
    assert_eq!(
        plan.access,
        argonauts_core::session_runner::Access::ReadOnly
    );
    let first = plan.prompt.first().context("a prompt")?;
    assert_eq!(first.text.trim_end(), argonauts_crew::orchestrated::FORKED);
    Ok(())
}

/// An orchestrator on another CLI can't read the main session, so it starts from the brief.
#[tokio::test]
async fn an_orchestrator_on_another_cli_starts_from_the_brief() -> anyhow::Result<()> {
    let (crew, orchestrator) = forking("codex/gpt-6-luna", the_script)?;
    let _env = crew.repo.env_guard();
    crew.run_to_end(RunMode::Crew, REQUEST, 3).await?;

    let specs = orchestrator.specs();
    let plan = specs.first().context("the orchestrator's session")?;
    assert!(!plan.fork);
    assert_eq!(plan.resume, None);
    assert!(
        plan.prompt
            .iter()
            .all(|part| part.text.trim_end() != argonauts_crew::orchestrated::FORKED)
    );
    Ok(())
}

/// A fork that ends without its script gives way to the brief: the next try is a session of
/// its own.
#[tokio::test]
async fn a_fork_that_fails_gives_way_to_the_brief() -> anyhow::Result<()> {
    use argonauts_core::ErrorCategory;
    use argonauts_core::events::session::EndReason;

    let (crew, orchestrator) = forking("claude-code/fable", |spec| {
        if spec.fork {
            FakeSession::ended(EndReason::CliError {
                category: ErrorCategory::SessionLost,
                message: "No conversation found with session ID: main-session-1".to_owned(),
            })
        } else {
            the_script(spec)
        }
    })?;
    let _env = crew.repo.env_guard();
    crew.run_to_end(RunMode::Crew, REQUEST, 3).await?;

    let forks: Vec<bool> = orchestrator
        .specs()
        .iter()
        .filter(|spec| spec.role == RoleName::Orchestrator)
        .map(|spec| spec.fork)
        .collect();
    assert_eq!(forks.first(), Some(&true), "{forks:?}");
    assert_eq!(forks.last(), Some(&false), "{forks:?}");
    Ok(())
}

#[tokio::test]
async fn a_built_in_workflow_runs_the_script_its_orchestrator_writes() -> anyhow::Result<()> {
    let (mut crew, _worker) = crew("maker-checker")?;
    // The orchestrator answers the ask for a script with one of its own shape.
    let script = r#"export const meta = {
  name: 'due-dates',
  description: 'Add due dates',
  phases: [{ name: 'Build', detail: 'one worker' }, { name: 'Check' }],
}
const built = await agent('Add a due date to tasks: ' + args.request, {
  role: 'worker', label: 'build:due-dates', phase: 'Build', task: 'due-dates',
})
const verdict = await agent('Review the change.', {
  role: 'reviewer', label: 'check:due-dates', phase: 'Check', task: 'due-dates',
  schema: { type: 'object', properties: { accept: { type: 'boolean' } }, required: ['accept'] },
})
return { built, accepted: Boolean(verdict && verdict.accept) }
"#;
    let orchestrator = FakeRunner::cli("claude-code/fable", "anthropic")
        .respond_with(move |_| FakeSession::structured(json!({"script": script})));
    crew.services.workers.add(orchestrator);
    crew.settings.crew.orchestrator = Orchestrator::Models(vec!["claude-code/fable".parse()?]);
    let _env = crew.repo.env_guard();
    let outcomes = crew.run_to_end(RunMode::Crew, REQUEST, 3).await?;
    let events = all_events(&outcomes);

    let labels: Vec<String> = started(events)
        .into_iter()
        .map(|(label, ..)| label)
        .collect();
    assert_eq!(
        labels,
        ["plan:maker-checker", "build:due-dates", "check:due-dates"]
    );
    let reports = reports(events);
    let last = reports.last().expect("the runner reported");
    assert_eq!(last.workflow.name, "due-dates");
    let phases: Vec<&str> = last
        .workflow
        .phases
        .iter()
        .map(|p| p.title.as_str())
        .collect();
    assert_eq!(phases, ["Build", "Check"]);
    assert!(
        matches!(
            outcomes.last().expect("an outcome").exit,
            EngineExit::Finished(RunOutcome::Merged)
        ),
        "{:?}",
        outcomes.last().map(|outcome| &outcome.exit)
    );
    Ok(())
}

/// A script that keeps sending a task back gets as many rounds of fixes as
/// `review.max_change_rounds` allows, and then the task comes to you with the reviewers' words.
#[tokio::test]
async fn a_script_sends_a_task_back_only_as_often_as_the_settings_allow() -> anyhow::Result<()> {
    let (mut crew, _worker) = crew("maker-checker")?;
    crew.settings.review.max_change_rounds = 2;
    let script = r#"export const meta = {
  name: 'due-dates',
  description: 'Add due dates',
  phases: [{ name: 'Build', detail: 'one worker' }, { name: 'Check' }],
}
let findings = []
for (let round = 0; round < 5; round++) {
  const asked = findings.length ? 'Fix: ' + findings.join('; ') : 'Add due dates: ' + args.request
  const built = await agent(asked, {
    role: 'worker', label: 'build:due-dates', phase: 'Build', task: 'due-dates',
  })
  if (!built) break
  const verdict = await agent('Review the change.', {
    role: 'reviewer', label: 'check:due-dates', phase: 'Check', task: 'due-dates',
    schema: {
      type: 'object',
      properties: { accept: { type: 'boolean' }, findings: { type: 'array', items: { type: 'string' } } },
      required: ['accept'],
    },
  })
  if (!verdict || verdict.accept) break
  findings = Array.isArray(verdict.findings) ? verdict.findings : []
}
return { done: true }
"#;
    let orchestrator = crew.services.workers.add(
        FakeRunner::cli("claude-code/fable", "anthropic")
            .respond_with(move |_| FakeSession::structured(json!({"script": script}))),
    );
    crew.settings.crew.orchestrator = Orchestrator::Models(vec!["claude-code/fable".parse()?]);
    let strict = "antigravity-cli/strict";
    crew.services
        .workers
        .add(FakeRunner::cli(strict, "google").respond_with(|spec| {
            if spec.json_schema.is_none() {
                return FakeSession::completed("Not done.");
            }
            FakeSession::structured(
                json!({"accept": false, "findings": ["a due date in the past is accepted"]}),
            )
        }));
    crew.settings.crew.reviewer = vec![strict.parse()?];
    let _env = crew.repo.env_guard();
    let outcomes = crew.run_to_end(RunMode::Crew, REQUEST, 3).await?;
    let events = all_events(&outcomes);

    let told = orchestrator
        .specs()
        .iter()
        .any(|spec| prompt(spec).contains("Send a task back to its worker at most 2 times"));
    assert!(told, "the script's writer knows the limit");
    let needs_you = events
        .iter()
        .position(|event| {
            matches!(&event.kind, RunEventKind::Task(TaskEvent::State { to, .. })
                if *to == TaskState::NeedsYou)
        })
        .expect("the task came to you");
    let builds = started(&events[..needs_you])
        .iter()
        .filter(|(_, role, _)| *role == RoleName::Worker)
        .count();
    assert_eq!(builds, 3, "the first build and two rounds of fixes");
    let RunEventKind::Task(TaskEvent::State { reason, .. }) = &events[needs_you].kind else {
        unreachable!("found as a task's state");
    };
    assert!(reason.contains("after 2 rounds of fixes"), "{reason}");
    assert!(
        reason.contains("a due date in the past is accepted"),
        "{reason}"
    );
    Ok(())
}

/// The judge who signs off the whole run reads what the checks said about every task, as
/// each task's reviewer does, though the sign-off names no task.
#[tokio::test]
async fn the_sign_off_reads_every_tasks_checks() -> anyhow::Result<()> {
    use argonauts_core::events::session::InputPartKind;
    use argonauts_core::settings::protocol::CheckCommand;

    let (mut crew, _worker) = crew("ship")?;
    write_workflow(&crew, "ship.toml", SHIP)?;
    let judge = FakeRunner::cli("antigravity-cli/judge", "google").respond_with(|spec| {
        if spec.json_schema.is_none() {
            FakeSession::completed("Both tasks are done as asked.")
        } else {
            FakeSession::structured(json!({"accept": true, "findings": []}))
        }
    });
    let judge = crew.services.workers.add(judge);
    crew.settings.crew.judge = vec!["antigravity-cli/judge".parse()?];
    crew.settings.checks.run = vec![CheckCommand {
        name: "tests".into(),
        command: vec!["node".into(), "--test".into()],
    }];
    let _env = crew.repo.env_guard();
    let outcomes = crew.run_to_end(RunMode::Crew, REQUEST, 3).await?;

    let signed = judge.specs();
    let asked = signed
        .last()
        .unwrap_or_else(|| panic!("no sign-off: {:#?}", started(all_events(&outcomes))));
    let checks = asked
        .prompt
        .iter()
        .find(|part| part.kind == InputPartKind::CheckOutput)
        .unwrap_or_else(|| panic!("no checks in the sign-off: {}", prompt(asked)));
    for task in ["due-field", "due-docs"] {
        assert!(
            checks
                .text
                .contains(&format!("{task}: tests (node --test) passed")),
            "{}",
            checks.text
        );
    }
    Ok(())
}

/// The judge who signs off the whole run works in a folder that holds every task's change,
/// so the files it opens and the tests it runs are the ones it judges, and the folder made for
/// it goes when it's done.
#[tokio::test]
async fn the_sign_off_works_in_a_folder_that_holds_the_change() -> anyhow::Result<()> {
    let (mut crew, _worker) = crew("ship")?;
    write_workflow(&crew, "ship.toml", SHIP)?;
    let missing = Arc::new(std::sync::Mutex::new(None));
    let seen = missing.clone();
    let judge = FakeRunner::cli("antigravity-cli/judge", "google").respond_with(move |spec| {
        let unseen: Vec<&str> = ["due.rs", "docs.md"]
            .into_iter()
            .filter(|file| !spec.workdir.join(file).is_file())
            .collect();
        *seen.lock().expect("not poisoned") = Some(unseen.join(", "));
        FakeSession::completed("Both tasks are done as asked.")
    });
    let judge = crew.services.workers.add(judge);
    crew.settings.crew.judge = vec!["antigravity-cli/judge".parse()?];
    let _env = crew.repo.env_guard();
    let outcomes = crew.run_to_end(RunMode::Crew, REQUEST, 3).await?;

    let signed = judge.specs();
    let asked = signed
        .last()
        .unwrap_or_else(|| panic!("no sign-off: {:#?}", started(all_events(&outcomes))));
    let unseen = missing.lock().expect("not poisoned").clone();
    assert_eq!(unseen.as_deref(), Some(""), "files the judge couldn't see");
    assert!(
        !asked.workdir.exists(),
        "the folder made for the sign-off is gone after it: {}",
        asked.workdir.display()
    );
    let merged = task_states(all_events(&outcomes))
        .into_iter()
        .filter(|(_, state)| *state == TaskState::Merged)
        .count();
    assert_eq!(merged, 2, "{:#?}", task_states(all_events(&outcomes)));
    Ok(())
}

/// Two tasks that write one file each their own way can't share one folder, so a sign-off
/// about the whole change fails saying why, rather than judging the base in its place.
#[tokio::test]
async fn a_sign_off_on_changes_that_dont_merge_says_so() -> anyhow::Result<()> {
    let (mut crew, _worker) = crew("maker-checker")?;
    let script = "export const meta = { name: 'clash', description: 'Add due dates', \
         phases: [{ name: 'Build' }, { name: 'Check' }] }\n\
         await agent('Due field: ' + args.request, \
         { role: 'worker', label: 'build:field', phase: 'Build', task: 'field' })\n\
         await agent('Due docs: ' + args.request, \
         { role: 'worker', label: 'build:docs', phase: 'Build', task: 'docs' })\n\
         return await agent('Is the whole change done?', { role: 'reviewer', \
         label: 'check:all', phase: 'Check', schema: { type: 'object', properties: \
         { accept: { type: 'boolean' }, findings: { type: 'array', items: { type: 'string' } } }, \
         required: ['accept', 'findings'] } })\n";
    let orchestrator = FakeRunner::cli("claude-code/fable", "anthropic")
        .respond_with(move |_| FakeSession::structured(json!({"script": script})));
    crew.services.workers.add(orchestrator);
    crew.settings.crew.orchestrator = Orchestrator::Models(vec!["claude-code/fable".parse()?]);
    let clashing = FakeRunner::cli("codex/clashing", "openai").respond_with(|spec| {
        let line = if prompt(spec).contains("Due docs") {
            "docs first\n"
        } else {
            "field first\n"
        };
        std::fs::write(spec.workdir.join("shared.md"), line).expect("writes its file");
        FakeSession::structured(report("Wrote shared.md", "shared.md"))
    });
    crew.services.workers.add(clashing);
    crew.settings.crew.worker = vec!["codex/clashing".parse()?];
    let signer = FakeRunner::cli("antigravity-cli/signer", "google")
        .respond_with(|_| FakeSession::structured(json!({"accept": true, "findings": []})));
    crew.services.workers.add(signer);
    crew.settings.crew.reviewer = vec!["antigravity-cli/signer".parse()?];
    let _env = crew.repo.env_guard();
    let outcomes = crew.run_to_end(RunMode::Crew, REQUEST, 3).await?;

    let said = all_events(&outcomes).iter().any(|event| {
        matches!(&event.kind, RunEventKind::Note(note)
            if note.text == "check:all gave no answer: field and docs don't merge cleanly: shared.md")
    });
    let order: Vec<String> = started(all_events(&outcomes))
        .into_iter()
        .map(|(label, ..)| label)
        .collect();
    assert!(said, "{order:#?}");
    // The run's merge settles the clash afterwards, and only then may the whole change be judged.
    let settled = order.iter().position(|label| label == "settle conflict");
    let signed = order.iter().position(|label| label == "check:all");
    assert!(
        settled.is_some() && (signed.is_none() || signed > settled),
        "no sign-off ran before the changes merged: {order:?}"
    );
    Ok(())
}

/// Builds when asked to, and accepts what it's asked to review.
fn builds_and_reviews(spec: &SessionSpec) -> FakeSession {
    if spec.step == Some(StepName::Build) {
        std::fs::write(spec.workdir.join("due.rs"), "due\n").expect("writes its file");
        FakeSession::structured(report("Wrote due.rs", "due.rs"))
    } else if spec.json_schema.is_some() {
        FakeSession::structured(json!({"accept": true, "findings": []}))
    } else {
        FakeSession::completed("Done.")
    }
}

/// A review step given to the workers, as council's Cross-review is, still reads a change
/// to judge it, so its session may run the checks and nothing else.
#[tokio::test]
async fn a_worker_that_reviews_a_task_may_run_only_the_checks() -> anyhow::Result<()> {
    use argonauts_core::session_runner::SessionShell;
    use argonauts_core::settings::protocol::CheckCommand;

    const CROSS: &str = r#"
name  = "cross"
about = "Build each task, and another worker reviews it."
adapt = false

[[step]]
name = "Plan"
role = "orchestrator"
does = "Split the request into tasks: {request}"

[[step]]
name     = "Build"
role     = "worker"
for_each = "task"

[[step]]
name           = "Cross-review"
role           = "worker"
for_each       = "task"
not_the_author = true
send_back_to   = "Build"
"#;
    let mut crew = Crew::new()?;
    crew.settings.evolution.propose_every = 0;
    let main = FakeRunner::cli(MAIN, "anthropic").respond_with(|_| {
        FakeSession::structured(json!({
            "summary": "One task",
            "tasks": [{"title": "Due field", "done_when": ["a task has a due date"]}],
        }))
    });
    crew.services.workers.add(main);
    let codex = crew
        .services
        .workers
        .add(FakeRunner::cli(WORKER, "openai").respond_with(builds_and_reviews));
    let agy = crew
        .services
        .workers
        .add(FakeRunner::cli(REVIEWER, "google").respond_with(builds_and_reviews));
    crew.settings.crew = CrewSettings {
        main: MAIN.parse()?,
        orchestrator: Orchestrator::Main,
        worker: vec![WORKER.parse()?, REVIEWER.parse()?],
        reviewer: vec![REVIEWER.parse()?],
        judge: vec![REVIEWER.parse()?],
        worker_workflows: false,
        merger: Vec::new(),
        orchestrator_context: Default::default(),
        orchestrator_picks: Default::default(),
        ways: Default::default(),
    };
    crew.settings.workflow = "cross".to_owned();
    crew.settings.workflows.adapt_with = AdaptWith::Script;
    crew.settings.checks.run = vec![CheckCommand {
        name: "tests".into(),
        command: vec!["node".into(), "--test".into()],
    }];
    write_workflow(&crew, "cross.toml", CROSS)?;
    let _env = crew.repo.env_guard();
    let outcomes = crew.run_to_end(RunMode::Crew, REQUEST, 3).await?;

    let specs: Vec<SessionSpec> = codex.specs().into_iter().chain(agy.specs()).collect();
    let reviews: Vec<&SessionSpec> = specs
        .iter()
        .filter(|spec| spec.step != Some(StepName::Build) && spec.json_schema.is_some())
        .collect();
    assert!(
        !reviews.is_empty(),
        "no cross-review: {:#?}",
        started(all_events(&outcomes))
    );
    for review in reviews {
        assert_eq!(
            review.shell,
            SessionShell::Only(vec!["node --test".to_owned()])
        );
    }
    Ok(())
}

/// A request that says how big a workflow to use: the orchestrator is told to keep to it,
/// and the workflow's row and a note say what was asked.
#[tokio::test]
async fn the_orchestrator_keeps_to_the_size_the_request_asked_for() -> anyhow::Result<()> {
    let (mut crew, _worker) = crew("maker-checker")?;
    let script = r#"export const meta = { name: 'due-dates', description: 'Add due dates',
  phases: [{ name: 'Build' }] }
const built = await agent('Add a due date to tasks: ' + args.request, {
  role: 'worker', label: 'build:due-dates', phase: 'Build', task: 'due-dates',
})
return { built }
"#;
    let orchestrator = FakeRunner::cli("claude-code/fable", "anthropic")
        .respond_with(move |_| FakeSession::structured(json!({"script": script})));
    let orchestrator = crew.services.workers.add(orchestrator);
    crew.settings.crew.orchestrator = Orchestrator::Models(vec!["claude-code/fable".parse()?]);
    let _env = crew.repo.env_guard();
    let request = "Add due dates to tasks, use a small workflow, 3 agents max";
    let outcomes = crew.run_to_end(RunMode::Crew, request, 3).await?;
    let events = all_events(&outcomes);

    let asked = prompt(&orchestrator.specs()[0]);
    assert!(
        asked.contains("The request asks for a small workflow, 3 agents at most."),
        "{asked}"
    );
    let last = reports(events)
        .last()
        .copied()
        .expect("the runner reported");
    assert_eq!(
        last.size_asked.as_deref(),
        Some("a small workflow, 3 agents at most")
    );
    let noted = events.iter().any(|event| {
        matches!(&event.kind, RunEventKind::Workflow(WorkflowEvent::Note(note))
            if note.text == "As you asked: a small workflow, 3 agents at most")
    });
    assert!(noted, "a note says what was asked");
    Ok(())
}

/// The newest approval of `kind` the run opened.
fn approval(events: &[RunEvent], kind: ApprovalKind) -> Option<&ApprovalOpened> {
    events.iter().rev().find_map(|event| match &event.kind {
        RunEventKind::Approval(ApprovalEvent::Opened(opened)) if opened.kind == kind => {
            let opened: &ApprovalOpened = opened;
            Some(opened)
        }
        _ => None,
    })
}

fn keys(opened: &ApprovalOpened) -> String {
    opened.options.iter().map(|option| option.key).collect()
}

#[tokio::test]
async fn in_supervised_mode_the_steps_wait_for_you_before_any_agent_starts() -> anyhow::Result<()> {
    let (mut crew, worker) = crew("ship")?;
    crew.settings.approvals.plan.mode = ApprovalMode::Supervised;
    write_workflow(&crew, "ship.toml", SHIP)?;
    let _env = crew.repo.env_guard();
    let first = crew.start(RunMode::Crew, REQUEST).await?;
    assert!(
        matches!(first.exit, EngineExit::Waiting(_)),
        "{:?}",
        first.exit
    );
    assert!(started(&first.events).is_empty(), "nothing ran yet");
    let plan = approval(&first.events, ApprovalKind::Plan).expect("the plan approval");
    // Steps written in a file can't be written again, so there is nothing to send back.
    assert_eq!(keys(plan), "ysxd");
    let ApprovalPacket::Plan(packet) = &plan.packet else {
        panic!("a plan approval: {:?}", plan.packet);
    };
    assert_eq!(packet.summary, "The steps of ship run as written");
    let shown = packet.workflow.as_ref().expect("the steps");
    assert_eq!(shown.workflow.name, "ship");
    let phases: Vec<(&str, &str)> = shown
        .workflow
        .phases
        .iter()
        .map(|phase| (phase.name.as_str(), phase.detail.as_str()))
        .collect();
    assert_eq!(
        phases,
        [
            ("Plan", "orchestrator · 1 agent"),
            ("Build", "workers · one per task"),
            ("Review", "reviewers · one per task · sends back to Build"),
            ("Sign off", "judge · 1 agent"),
        ]
    );

    // Approved, the run picks up the same approval and the steps run to the merge approval.
    let (approve, _reply) = answer(
        &first.events,
        ApprovalKind::Plan,
        AnswerAction::Approve,
        None,
    )?;
    let after = crew.resume(first.events.clone(), vec![approve]).await?;
    assert!(!worker.specs().is_empty(), "the tasks were built");
    assert!(opened(&after.events, ApprovalKind::Merge));
    let plans = after
        .events
        .iter()
        .filter(|event| {
            matches!(&event.kind, RunEventKind::Approval(ApprovalEvent::Opened(opened))
                if opened.kind == ApprovalKind::Plan)
        })
        .count();
    assert_eq!(plans, 1, "the plan was asked about once");
    Ok(())
}

#[tokio::test]
async fn steps_you_send_back_are_written_again_with_your_note() -> anyhow::Result<()> {
    let (mut crew, _worker) = crew("maker-checker")?;
    crew.settings.approvals.plan.mode = ApprovalMode::Supervised;
    let script = |name: &str| {
        format!(
            "export const meta = {{ name: '{name}', description: 'Add due dates', \
             phases: [{{ name: 'Build' }}] }}\n\
             return await agent('Add a due date: ' + args.request, \
             {{ role: 'worker', label: 'build:due', phase: 'Build', task: 'due' }})\n"
        )
    };
    let (first_try, second_try) = (script("due-dates"), script("due-dates-with-tests"));
    let again = second_try.clone();
    let orchestrator =
        FakeRunner::cli("claude-code/fable", "anthropic").respond_with(move |spec| {
            let asked = prompt(spec);
            let source = if asked.contains("sent it back") {
                &again
            } else {
                &first_try
            };
            FakeSession::structured(json!({"script": source}))
        });
    let orchestrator = crew.services.workers.add(orchestrator);
    crew.settings.crew.orchestrator = Orchestrator::Models(vec!["claude-code/fable".parse()?]);
    let _env = crew.repo.env_guard();

    let first = crew.start(RunMode::Crew, REQUEST).await?;
    let plan = approval(&first.events, ApprovalKind::Plan).expect("the plan approval");
    assert_eq!(keys(plan), "ybsxd", "the orchestrator can write them again");
    let ApprovalPacket::Plan(packet) = &plan.packet else {
        panic!("a plan approval: {:?}", plan.packet);
    };
    assert_eq!(
        packet.summary,
        "The orchestrator wrote these steps for your request, with the roles of maker-checker"
    );
    assert_eq!(
        packet
            .workflow
            .as_ref()
            .map(|shown| shown.workflow.name.as_str()),
        Some("due-dates")
    );

    let (send_back, _reply) = answer(
        &first.events,
        ApprovalKind::Plan,
        AnswerAction::SendBack,
        Some("write the tests first"),
    )?;
    let second = crew.resume(first.events.clone(), vec![send_back]).await?;
    let asked: Vec<String> = orchestrator.specs().iter().map(prompt).collect();
    assert_eq!(asked.len(), 2, "asked once more, with the note");
    assert!(asked[1].contains("- write the tests first"), "{}", asked[1]);
    assert!(
        asked[1].contains("name: 'due-dates'"),
        "it reads the script it wrote last: {}",
        asked[1]
    );
    let plan = approval(&second.events, ApprovalKind::Plan).expect("the plan approval");
    let ApprovalPacket::Plan(packet) = &plan.packet else {
        panic!("a plan approval: {:?}", plan.packet);
    };
    assert_eq!(
        packet
            .workflow
            .as_ref()
            .map(|shown| shown.workflow.name.as_str()),
        Some("due-dates-with-tests"),
        "the new steps wait for you in an approval of their own"
    );
    let ran = started(&second.events);
    assert!(
        ran.iter()
            .all(|(label, ..)| label.starts_with("plan:maker-checker")),
        "no step ran before you approved: {ran:?}"
    );

    // Approved, a run that picks up again runs the steps you approved and asks nobody again.
    let (approve, _reply) = answer(
        &second.events,
        ApprovalKind::Plan,
        AnswerAction::Approve,
        None,
    )?;
    let third = crew.resume(second.events.clone(), vec![approve]).await?;
    assert_eq!(
        orchestrator.specs().len(),
        2,
        "nothing was written a third time"
    );
    let reports = reports(&third.events);
    assert_eq!(
        reports.last().map(|report| report.workflow.name.as_str()),
        Some("due-dates-with-tests")
    );
    assert!(opened(&third.events, ApprovalKind::Merge));
    Ok(())
}

#[tokio::test]
async fn abandoning_at_the_plan_approval_ends_the_run_with_nothing_built() -> anyhow::Result<()> {
    let (mut crew, worker) = crew("ship")?;
    crew.settings.approvals.plan.mode = ApprovalMode::Supervised;
    write_workflow(&crew, "ship.toml", SHIP)?;
    let _env = crew.repo.env_guard();
    let first = crew.start(RunMode::Crew, REQUEST).await?;
    let (abandon, _reply) = answer(
        &first.events,
        ApprovalKind::Plan,
        AnswerAction::Abandon,
        None,
    )?;
    let after = crew.resume(first.events.clone(), vec![abandon]).await?;
    assert_eq!(after.exit, EngineExit::Finished(RunOutcome::Abandoned));
    assert!(worker.specs().is_empty(), "nothing was built");
    Ok(())
}

/// One worker builds under `label`, and `role` signs it off.
fn build_then_sign_off(name: &str, role: &str) -> String {
    format!(
        "export const meta = {{ name: '{name}', description: 'Add due dates', \
         phases: [{{ name: 'Build' }}, {{ name: 'Check' }}] }}\n\
         await agent('Add a due date: ' + args.request, \
         {{ role: 'worker', label: 'build:due', phase: 'Build', task: 'due' }})\n\
         return await agent('Is it done?', {{ role: '{role}', label: 'check:due', \
         phase: 'Check', task: 'due', schema: {{ type: 'object', properties: \
         {{ accept: {{ type: 'boolean' }}, findings: {{ type: 'array', items: {{ type: 'string' }} }} }}, \
         required: ['accept', 'findings'] }} }})\n"
    )
}

#[tokio::test]
async fn a_script_that_throws_when_rehearsed_is_sent_back_with_what_it_threw() -> anyhow::Result<()>
{
    let (mut crew, worker) = crew("maker-checker")?;
    // Maker-checker has a worker and a reviewer step. The first script asks a judge to sign off.
    let (wrong, right) = (
        build_then_sign_off("with-a-judge", "judge"),
        build_then_sign_off("with-a-reviewer", "reviewer"),
    );
    let orchestrator =
        FakeRunner::cli("claude-code/fable", "anthropic").respond_with(move |spec| {
            let source = if prompt(spec).contains("couldn't be used") {
                &right
            } else {
                &wrong
            };
            FakeSession::structured(json!({"script": source}))
        });
    let orchestrator = crew.services.workers.add(orchestrator);
    crew.settings.crew.orchestrator = Orchestrator::Models(vec!["claude-code/fable".parse()?]);
    let _env = crew.repo.env_guard();
    let outcomes = crew.run_to_end(RunMode::Crew, REQUEST, 3).await?;
    let events = all_events(&outcomes);

    let asked: Vec<String> = orchestrator.specs().iter().map(prompt).collect();
    assert_eq!(asked.len(), 2, "asked once more");
    assert!(
        asked[1].contains(
            "Your last script couldn't be used: rehearsed with placeholder answers, it threw: \
             Error: agent(): this workflow's file has no judge step; its roles are worker, \
             reviewer"
        ),
        "{}",
        asked[1]
    );
    assert_eq!(worker.specs().len(), 1, "the rehearsal built nothing");
    let reports = reports(events);
    assert_eq!(
        reports.last().map(|report| report.workflow.name.as_str()),
        Some("with-a-reviewer")
    );
    assert!(opened(events, ApprovalKind::Merge));
    Ok(())
}

/// A reviewer whose format leaves `accept` optional may answer with neither yes nor no, so the
/// script goes back to the planner with the format to use.
#[tokio::test]
async fn a_reviewer_whose_accept_is_optional_is_sent_back() -> anyhow::Result<()> {
    let (mut crew, _worker) = crew("maker-checker")?;
    let right = build_then_sign_off("with-a-verdict", "reviewer");
    let wrong = right
        .replace(", required: ['accept', 'findings']", "")
        .replace("with-a-verdict", "without-a-verdict");
    assert_ne!(right, wrong);
    let orchestrator =
        FakeRunner::cli("claude-code/fable", "anthropic").respond_with(move |spec| {
            let source = if prompt(spec).contains("couldn't be used") {
                &right
            } else {
                &wrong
            };
            FakeSession::structured(json!({"script": source}))
        });
    let orchestrator = crew.services.workers.add(orchestrator);
    crew.settings.crew.orchestrator = Orchestrator::Models(vec!["claude-code/fable".parse()?]);
    let _env = crew.repo.env_guard();
    let outcomes = crew.run_to_end(RunMode::Crew, REQUEST, 3).await?;
    let events = all_events(&outcomes);

    let asked: Vec<String> = orchestrator.specs().iter().map(prompt).collect();
    assert_eq!(asked.len(), 2, "asked once more");
    assert!(
        asked[1].contains(
            "Your last script couldn't be used: a reviewer or a judge that signs off answers \
             without a required yes-or-no accept: check:due."
        ),
        "{}",
        asked[1]
    );
    assert!(asked[1].contains(r#""required": ["accept", "findings"]"#));
    let reports = reports(events);
    assert_eq!(
        reports.last().map(|report| report.workflow.name.as_str()),
        Some("with-a-verdict")
    );
    Ok(())
}

/// A step `always.toml` requires is in what the orchestrator reads. A script without it is
/// sent back once, saying what to place and where.
#[tokio::test]
async fn a_script_without_a_step_you_require_is_sent_back_once_to_place_it() -> anyhow::Result<()> {
    let (mut crew, _worker) = crew("maker-checker")?;
    write_workflow(
        &crew,
        "always.toml",
        "[[step]]\nname = \"Benchmark\"\nrole = \"worker\"\ndoes = \"Run the benchmarks.\"\n",
    )?;
    let without = build_then_sign_off("without-a-benchmark", "reviewer");
    let with = "export const meta = { name: 'with-a-benchmark', description: 'Add due dates', \
         phases: [{ name: 'Build' }, { name: 'Benchmark' }, { name: 'Check' }] }\n\
         await agent('Add a due date: ' + args.request, \
         { role: 'worker', label: 'build:due', phase: 'Build', task: 'due' })\n\
         await agent('Run the benchmarks.', \
         { role: 'worker', label: 'benchmark:due', phase: 'Benchmark', task: 'due' })\n\
         return await agent('Is it done?', { role: 'reviewer', label: 'check:due', \
         phase: 'Check', task: 'due', schema: { type: 'object', properties: \
         { accept: { type: 'boolean' }, findings: { type: 'array', items: { type: 'string' } } }, \
         required: ['accept', 'findings'] } })\n"
        .to_owned();
    let orchestrator =
        FakeRunner::cli("claude-code/fable", "anthropic").respond_with(move |spec| {
            let source = if prompt(spec).contains("doesn't have every step the user requires") {
                &with
            } else {
                &without
            };
            FakeSession::structured(json!({"script": source}))
        });
    let orchestrator = crew.services.workers.add(orchestrator);
    crew.settings.crew.orchestrator = Orchestrator::Models(vec!["claude-code/fable".parse()?]);
    let _env = crew.repo.env_guard();
    let outcomes = crew.run_to_end(RunMode::Crew, REQUEST, 3).await?;

    let asked: Vec<String> = orchestrator.specs().iter().map(prompt).collect();
    assert_eq!(asked.len(), 2, "asked once more");
    assert!(
        asked[0].contains(
            "The user requires these steps in every workflow. Give each a step of its own, by \
             this name, where it says:\n- Benchmark, before the sign-off, by the workers: Run \
             the benchmarks."
        ),
        "{}",
        asked[0]
    );
    assert!(
        asked[1].contains(
            "- Benchmark, before the sign-off, by the workers: Run the benchmarks. (missing)"
        ),
        "{}",
        asked[1]
    );
    let reports = reports(all_events(&outcomes));
    assert_eq!(
        reports.last().map(|report| report.workflow.name.as_str()),
        Some("with-a-benchmark")
    );
    Ok(())
}

#[tokio::test]
async fn when_no_script_can_be_used_the_files_own_steps_run_and_nobody_is_asked_again()
-> anyhow::Result<()> {
    let (mut crew, _worker) = crew("maker-checker")?;
    let orchestrator = FakeRunner::cli("claude-code/fable", "anthropic")
        .respond_with(|_| FakeSession::structured(json!({"script": "const steps = []"})));
    let orchestrator = crew.services.workers.add(orchestrator);
    crew.settings.crew.orchestrator = Orchestrator::Models(vec!["claude-code/fable".parse()?]);
    let _env = crew.repo.env_guard();
    let first = crew.start(RunMode::Crew, REQUEST).await?;
    assert_eq!(orchestrator.specs().len(), 2, "asked, and once more");
    let said = first.events.iter().any(|event| {
        matches!(&event.kind, RunEventKind::Workflow(WorkflowEvent::Note(note))
            if note.text.starts_with("The orchestrator's steps couldn't be used")
                && note.text.ends_with("so maker-checker's own steps run as written"))
    });
    assert!(said, "the run says why the file's own steps run");
    let reports = reports(&first.events);
    let last = reports.last().expect("the runner reported");
    assert_eq!(last.workflow.name, "maker-checker");
    let phases: Vec<&str> = last
        .workflow
        .phases
        .iter()
        .map(|phase| phase.title.as_str())
        .collect();
    assert_eq!(phases, ["Build", "Review"]);

    // Reopened, the run carries on with the steps it started with.
    crew.reopen(first.events.clone()).await?;
    assert_eq!(
        orchestrator.specs().len(),
        2,
        "nobody is asked a third time"
    );
    Ok(())
}

const SHIP_WITH_DOCS: &str = r#"export const meta = {
  name: 'ship-with-docs',
  description: 'Build the field, then run the docs workflow',
  phases: [{ name: 'Build' }],
}
await agent('Build the due field: ' + args.request, {
  role: 'worker', label: 'build:due-field', phase: 'Build', task: 'due-field',
})
return await workflow('docs', { request: args.request, crew: args.crew })
"#;

const DOCS: &str = r#"
name  = "docs"
about = "Write the docs for a change."
adapt = false

[[step]]
name = "Write"
role = "worker"
does = "Due docs: document {request}"

[[step]]
name = "Read"
role = "judge"
does = "Read the docs for {request} and say whether they are right."
"#;

#[tokio::test]
async fn a_workflow_run_as_a_step_shows_under_its_own_name_and_is_not_run_twice()
-> anyhow::Result<()> {
    let (crew, worker) = crew("ship-with-docs")?;
    write_workflow(&crew, "ship-with-docs.js", SHIP_WITH_DOCS)?;
    write_workflow(&crew, "docs.toml", DOCS)?;
    let _env = crew.repo.env_guard();
    let first = crew.start(RunMode::Crew, REQUEST).await?;
    assert!(
        matches!(first.exit, EngineExit::Waiting(_)),
        "{:?}",
        first.exit
    );
    assert!(opened(&first.events, ApprovalKind::Merge));
    let labels: Vec<String> = started(&first.events)
        .into_iter()
        .map(|(label, ..)| label)
        .collect();
    // A writer's session goes by its task, which a step without tasks names after itself.
    assert_eq!(labels, ["build:due-field", "build:write", "read"]);

    // The step's agents show grouped under its name, each with the session that ran it.
    let reports = reports(&first.events);
    let last = reports.last().expect("the runner reported");
    assert_eq!(last.workflow.name, "ship-with-docs");
    let phases: Vec<&str> = last
        .workflow
        .phases
        .iter()
        .map(|phase| phase.title.as_str())
        .collect();
    assert_eq!(phases, ["Build", "▸ docs"]);
    let rows: Vec<(&str, Option<u32>)> = last
        .workflow
        .agents
        .iter()
        .map(|agent| (agent.label.as_str(), agent.phase))
        .collect();
    assert_eq!(
        rows,
        [
            ("build:due-field", Some(1)),
            ("write", Some(2)),
            ("read", Some(2))
        ]
    );
    let mut sessions: Vec<String> = last
        .workflow
        .agents
        .iter()
        .map(|agent| {
            last.session_of(agent.index)
                .unwrap_or_else(|| panic!("{} has no session: {:?}", agent.label, last.sessions))
                .to_string()
        })
        .collect();
    sessions.sort();
    sessions.dedup();
    assert_eq!(sessions.len(), 3, "each agent has a session of its own");
    assert_eq!(
        std::fs::read_to_string(crew.repo.root().join("docs.md")).ok(),
        None,
        "nothing reaches your branch before the merge approval"
    );

    // Reopened at the merge approval, neither the script nor its step runs anything again.
    let built = worker.specs().len();
    let ran = started(&first.events).len();
    let again = crew.reopen(first.events.clone()).await?;
    assert_eq!(worker.specs().len(), built, "no task was built again");
    assert_eq!(
        started(&again.events).len(),
        ran,
        "no session started again"
    );
    let after = self::reports(&again.events);
    let last = after.last().expect("the runner reported");
    assert_eq!(last.workflow.agents.len(), 3, "the step's rows are back");
    assert!(
        last.workflow
            .agents
            .iter()
            .all(|agent| last.session_of(agent.index).is_some()),
        "{:?}",
        last.sessions
    );
    Ok(())
}

#[tokio::test]
async fn with_nesting_off_a_script_cannot_run_another_workflow() -> anyhow::Result<()> {
    let (mut crew, _worker) = crew("ship-with-docs")?;
    crew.settings.workflows.max_depth = 0;
    write_workflow(&crew, "ship-with-docs.js", SHIP_WITH_DOCS)?;
    write_workflow(&crew, "docs.toml", DOCS)?;
    let _env = crew.repo.env_guard();
    let outcomes = crew.run_to_end(RunMode::Crew, REQUEST, 3).await?;
    let events = all_events(&outcomes);
    let why = events.iter().find_map(|event| match &event.kind {
        RunEventKind::Workflow(WorkflowEvent::StepFinished(finished)) => finished.summary.clone(),
        _ => None,
    });
    let why = why.expect("the step said how it ended");
    assert!(
        why.contains("this run doesn't let a workflow run another as a step"),
        "{why}"
    );
    assert_eq!(
        outcomes.last().expect("an outcome").exit,
        EngineExit::Finished(RunOutcome::Failed)
    );
    Ok(())
}

#[tokio::test]
async fn a_step_can_let_its_workers_run_a_workflow_of_their_own() -> anyhow::Result<()> {
    let (crew, worker) = crew("ship")?;
    let free = SHIP.replace(
        "role     = \"worker\"\nfor_each = \"task\"\n",
        "role     = \"worker\"\nfor_each = \"task\"\nown_workflow = true\n",
    );
    assert_ne!(free, SHIP);
    write_workflow(&crew, "ship.toml", &free)?;
    let _env = crew.repo.env_guard();
    crew.start(RunMode::Crew, REQUEST).await?;
    // The step's workers fan out as their CLI sees fit, a worker sent back too.
    let fanouts: Vec<NativeFanout> = worker
        .specs()
        .iter()
        .map(|spec| spec.native_fanout)
        .collect();
    assert_eq!(fanouts.len(), 3, "two tasks, one of them sent back");
    assert!(
        fanouts
            .iter()
            .all(|fanout| *fanout == NativeFanout::Switch(FanoutSwitch::On)),
        "{fanouts:?}"
    );
    Ok(())
}

#[tokio::test]
async fn without_the_steps_leave_a_worker_keeps_the_cap_your_settings_give_it() -> anyhow::Result<()>
{
    let (crew, worker) = crew("ship")?;
    write_workflow(&crew, "ship.toml", SHIP)?;
    let _env = crew.repo.env_guard();
    crew.start(RunMode::Crew, REQUEST).await?;
    let capped = crew.settings.native_fanout;
    assert_ne!(capped, NativeFanout::Switch(FanoutSwitch::On));
    assert!(
        worker
            .specs()
            .iter()
            .all(|spec| spec.native_fanout == capped),
        "every worker keeps {capped:?}"
    );
    Ok(())
}

/// A file whose steps run as written gives its agents its own words: a worker reads its
/// frame, then its task's words as the workflow's, never as words the orchestrator wrote.
#[tokio::test]
async fn a_files_own_steps_reach_their_agents_as_the_workflows_words() -> anyhow::Result<()> {
    use argonauts_core::events::session::InputPartKind;
    use argonauts_core::prompt_parts::heading;

    let (crew, worker) = crew("ship")?;
    write_workflow(&crew, "ship.toml", SHIP)?;
    let _env = crew.repo.env_guard();
    crew.run_to_end(RunMode::Crew, REQUEST, 3).await?;
    let first = worker.specs().into_iter().next().expect("a build session");
    assert_eq!(
        first.prompt[0].kind,
        InputPartKind::Frame,
        "{}",
        prompt(&first)
    );
    let words = heading(&first.prompt[1]).unwrap_or_default();
    assert!(words.starts_with("From the workflow"), "{words}");
    Ok(())
}

/// A file's `goal` is checked by the goal's own check, here by the `check` command written
/// beside it, and the check is recorded as every goal's is, so the goal panels show it.
#[tokio::test]
async fn a_files_goal_is_checked_as_every_goal_is() -> anyhow::Result<()> {
    use argonauts_core::events::goal::{CheckedBy, GoalChecked, GoalEvent, GoalVerdict};
    use argonauts_core::settings::goal::CheckWith;

    let (mut crew, _) = crew("ship")?;
    crew.settings.goal.check = CheckWith::Command;
    let file = format!("goal  = \"the build passes\"\ncheck = [\"true\"]\n{SHIP}");
    write_workflow(&crew, "ship.toml", &file)?;
    let _env = crew.repo.env_guard();
    let outcomes = crew.run_to_end(RunMode::Crew, REQUEST, 3).await?;
    let checked: Vec<&GoalChecked> = all_events(&outcomes)
        .iter()
        .filter_map(|event| match &event.kind {
            RunEventKind::Goal(GoalEvent::Checked(checked)) if checked.task.is_none() => {
                Some(&**checked)
            }
            _ => None,
        })
        .collect();
    let first = checked.first().expect("the file's goal was checked");
    assert_eq!(first.overall, GoalVerdict::Met, "{first:?}");
    assert!(
        first
            .checks
            .iter()
            .any(|run| run.name == "true" && run.by == CheckedBy::Command && run.passed),
        "{first:?}"
    );
    Ok(())
}

/// Every worker proposes side by side without writing, then the judge is told how many
/// proposals came in.
const PANEL: &str = r#"export const meta = {
  name: 'panel',
  description: 'Everyone proposes, the judge decides',
  phases: [{ name: 'Propose' }, { name: 'Challenge' }, { name: 'Decide' }],
}
const asked = [0, 1, 2].map((n) => () => agent('Propose how to do it: ' + args.request, {
  role: 'worker', access: 'read', model: n, label: 'propose:w' + n, phase: 'Propose',
}))
const proposals = (await parallel(asked)).filter(Boolean)
const challenges = proposals.map((_, n) => () => agent('Argue against the other proposals.', {
  role: 'worker', access: 'read', model: n, label: 'challenge:w' + n, phase: 'Challenge',
}))
await parallel(challenges)
const picked = await agent('Pick the strongest of ' + proposals.length + ' proposals.', {
  role: 'judge', label: 'decide', phase: 'Decide',
})
return { proposals: proposals.length, picked }
"#;

/// How each session with this label ended, in the order they started. One still running has
/// no end.
fn ends_of(events: &[RunEvent], label: &str) -> Vec<Option<EndReason>> {
    let mut sessions: Vec<(&SessionKey, Option<EndReason>)> = Vec::new();
    for event in events {
        let (RunEventKind::Session(session), Some(key)) = (&event.kind, &event.session) else {
            continue;
        };
        match session {
            SessionEvent::Started(started) if started.label.as_deref() == Some(label) => {
                sessions.push((key, None));
            }
            SessionEvent::Ended(end) => {
                if let Some(last) = sessions.iter_mut().rev().find(|(held, _)| *held == key) {
                    last.1 = Some(end.reason.clone());
                }
            }
            _ => {}
        }
    }
    sessions.into_iter().map(|(_, end)| end).collect()
}

/// Three workers propose side by side and two of them can't sign in, so nobody may take their
/// places and the run waits. The third was still writing: the wait cut its session short,
/// nothing of its own did, so it goes again each time the run wakes, and once the others are
/// back all three proposals are in.
#[tokio::test]
async fn an_agent_cut_short_by_a_wait_for_others_goes_again_when_the_run_wakes()
-> anyhow::Result<()> {
    let mut crew = Crew::new()?;
    crew.settings.evolution.propose_every = 0;
    let writing = Arc::new(AtomicBool::new(true));
    let main = FakeRunner::cli(MAIN, "anthropic").respond_with(move |spec| {
        if spec.role == RoleName::Orchestrator {
            return FakeSession::structured(json!({"script": PANEL}));
        }
        if spec.role == RoleName::Worker {
            if writing.swap(false, Ordering::SeqCst) {
                return FakeSession::until_stopped(Vec::new());
            }
            return FakeSession::completed("Keep the due date beside the title.");
        }
        support::smallest_answer(spec)
    });
    let main = crew.services.workers.add(main);
    let signed_in = Arc::new(AtomicBool::new(false));
    for (worker, provider) in [(WORKER, "openai"), (REVIEWER, "google")] {
        let signed_in = signed_in.clone();
        let fake = FakeRunner::cli(worker, provider).respond_with(move |_| {
            if signed_in.load(Ordering::SeqCst) {
                return FakeSession::completed("Store it as a date of its own.");
            }
            FakeSession::ended(EndReason::Auth {
                message: "authentication required".to_owned(),
            })
        });
        crew.services.workers.add(fake);
    }
    crew.settings.crew = CrewSettings {
        main: MAIN.parse()?,
        orchestrator: Orchestrator::Main,
        worker: vec![MAIN.parse()?, WORKER.parse()?, REVIEWER.parse()?],
        reviewer: vec![MAIN.parse()?],
        judge: vec![MAIN.parse()?],
        worker_workflows: false,
        merger: Vec::new(),
        orchestrator_context: Default::default(),
        orchestrator_picks: Default::default(),
        ways: Default::default(),
    };
    crew.settings.workflow = "council".to_owned();
    crew.settings.workflows.adapt_with = AdaptWith::Script;
    crew.with_council_steps()?;
    let _env = crew.repo.env_guard();

    let first = crew.start(RunMode::Crew, REQUEST).await?;
    let waits = |exit: &EngineExit| {
        matches!(exit, EngineExit::Waiting(WaitReason::NoFallback { detail, .. })
            if detail == "nobody to fall back to as the task's worker")
    };
    assert!(waits(&first.exit), "{:?}", first.exit);
    assert_eq!(
        ends_of(&first.events, "propose:w0"),
        [Some(EndReason::Paused)],
        "the wait cut it short, so it goes on later"
    );

    // The run wakes with the others still signed out: the one that can work is asked again
    // and the run waits once more.
    let again = crew.resume(first.events.clone(), Vec::new()).await?;
    assert!(waits(&again.exit), "{:?}", again.exit);
    assert_eq!(proposals_asked(&main), 2, "it went again");

    // With the others back, their proposals join the one already written.
    signed_in.store(true, Ordering::SeqCst);
    let back = crew.resume(again.events.clone(), Vec::new()).await?;
    let mine = ends_of(&back.events, "propose:w0");
    assert_eq!(mine.last(), Some(&Some(EndReason::Completed)), "{mine:?}");
    assert!(!mine.contains(&Some(EndReason::Stopped)), "{mine:?}");
    let said_nothing = back.events.iter().any(|event| {
        matches!(&event.kind, RunEventKind::Note(note)
            if note.text.starts_with("propose:w0 gave no answer"))
    });
    assert!(!said_nothing, "its stop was never read as its answer");
    let decides: Vec<String> = main
        .specs()
        .iter()
        .filter(|spec| spec.role == RoleName::Judge)
        .map(prompt)
        .collect();
    assert_eq!(decides.len(), 1, "{:?}", started(&back.events));
    assert!(
        decides[0].contains("Pick the strongest of 3 proposals."),
        "{}",
        decides[0]
    );
    Ok(())
}

/// How many times this agent was put to its proposal, a session that carries on counted too.
fn proposals_asked(agent: &FakeRunner) -> usize {
    agent
        .specs()
        .iter()
        .filter(|spec| spec.role == RoleName::Worker)
        .count()
}

/// Runs the request on autopilot with the real approval policy, until the run waits or finishes.
async fn on_autopilot(crew: &Crew) -> anyhow::Result<Outcome> {
    let started = crew.run_started(RunMode::Crew, REQUEST)?;
    let autopilot = RunEvent {
        seq: 1,
        kind: RunEventKind::Run(RunLifecycle::ApprovalModeChanged {
            mode: ApprovalMode::Autopilot,
            by: Party::User,
        }),
        ..started.clone()
    };
    let (mut context, sink, _commands) = crew.context(vec![started, autopilot])?;
    let repo = crew.repo_context()?;
    let state = repo
        .run_dir
        .parent()
        .and_then(std::path::Path::parent)
        .context("the run folder is under the repo's state folder")?;
    std::fs::create_dir_all(state)?;
    context.services.policy = argonauts_policy::approval_policy_with_models(
        context.settings.clone(),
        Arc::new(|| None),
        state,
        crew.services.workers.clone(),
        crew.services.registry.clone(),
        crew.services.steps.clone(),
    );
    let engine = argonauts_crew::engine_for(&crew.settings, RunMode::Crew).run(context);
    let exit = with_git_env(crew.git_env(), engine).await;
    Ok(Outcome {
        exit,
        events: sink.events(),
    })
}

/// A judge's approval that cites the default rules.
fn approves() -> FakeSession {
    FakeSession::structured(json!({
        "verdict": "approve",
        "reasons": ["the evidence meets the rules"],
        "rules_cited": ["R7"]
    }))
}

/// A file of steps runs as written, and its orchestrator's step is still the run's
/// orchestrator: on the main session's CLI it starts as that session's copy.
#[tokio::test]
async fn a_files_orchestrator_step_on_the_main_sessions_cli_starts_as_its_fork()
-> anyhow::Result<()> {
    use argonauts_core::events::run::{ForkFrom, StartsFrom};

    let mut crew = Crew::new()?;
    crew.settings.evolution.propose_every = 0;
    let main = crew
        .services
        .workers
        .add(
            FakeRunner::cli(MAIN, "anthropic").respond_with(|spec| match spec.role {
                RoleName::Orchestrator => FakeSession::structured(json!({
                    "summary": "One task",
                    "tasks": [{"title": "Due field", "done_when": ["a task has a due date"]}],
                })),
                _ => support::smallest_answer(spec),
            }),
        );
    for (model, provider) in [(WORKER, "openai"), (REVIEWER, "google")] {
        crew.services
            .workers
            .add(FakeRunner::cli(model, provider).respond_with(support::smallest_answer));
    }
    crew.settings.crew = CrewSettings {
        main: MAIN.parse()?,
        orchestrator: Orchestrator::Main,
        worker: vec![WORKER.parse()?],
        reviewer: vec![REVIEWER.parse()?],
        judge: vec![REVIEWER.parse()?],
        worker_workflows: false,
        merger: Vec::new(),
        orchestrator_context: Default::default(),
        orchestrator_picks: Default::default(),
        ways: Default::default(),
    };
    crew.settings.workflow = "spec-first".to_owned();
    crew.settings.workflows.adapt_with = AdaptWith::Script;
    let written = argonauts_runner::template("spec-first", ".argonauts/workflows");
    write_workflow(&crew, "spec-first.toml", &written)?;
    crew.starts_from = Some(StartsFrom::Fork(ForkFrom {
        worker: MAIN.parse()?,
        cli_session: "main-session-1".to_owned(),
        folder: crew.repo.root().to_path_buf(),
        tokens: Some(48_200),
    }));
    let _env = crew.repo.env_guard();
    crew.run_to_end(RunMode::Crew, REQUEST, 3).await?;

    let plan = main
        .specs()
        .into_iter()
        .find(|spec| spec.role == RoleName::Orchestrator)
        .context("the plan's session")?;
    assert!(plan.fork, "a copy of the main session");
    assert_eq!(plan.resume.as_deref(), Some("main-session-1"));
    let first = plan.prompt.first().context("a prompt")?;
    assert_eq!(first.text.trim_end(), argonauts_crew::orchestrated::FORKED);
    Ok(())
}

/// A workflow of your own, exactly as `/workflow new` writes it, on autopilot with a crew of
/// three CLIs: the main agent splits the request, another CLI builds the task, and a third
/// reviews it. The run goes from the request to the merge with nothing left to you: every
/// session ends well, and no approval is yours or still open.
#[tokio::test]
async fn on_autopilot_a_new_workflow_file_goes_from_request_to_merge() -> anyhow::Result<()> {
    use argonauts_core::settings::protocol::CheckCommand;

    let mut crew = Crew::new()?;
    crew.settings.evolution.propose_every = 0;
    let main = FakeRunner::cli(MAIN, "anthropic").respond_with(|spec| match spec.step {
        Some(StepName::ModelJudge) => approves(),
        _ if spec.role == RoleName::Orchestrator => FakeSession::structured(json!({
            "summary": "One task",
            "tasks": [{"title": "Due field", "done_when": ["a task has a due date"]}],
        })),
        _ => support::smallest_answer(spec),
    });
    crew.services.workers.add(main);
    let worker = FakeRunner::cli(WORKER, "openai").respond_with(|spec| match spec.step {
        Some(StepName::Build) => {
            std::fs::write(spec.workdir.join("due.rs"), "due\n").expect("writes its file");
            FakeSession::structured(report("Wrote due.rs", "due.rs"))
        }
        Some(StepName::ModelJudge) => approves(),
        _ => support::smallest_answer(spec),
    });
    crew.services.workers.add(worker);
    let reviewer = FakeRunner::cli(REVIEWER, "google").respond_with(|spec| match spec.step {
        Some(StepName::ModelJudge) => approves(),
        _ if spec.json_schema.is_some() => {
            FakeSession::structured(json!({"accept": true, "findings": []}))
        }
        _ => FakeSession::completed("The task is done as asked."),
    });
    crew.services.workers.add(reviewer);
    crew.settings.crew = CrewSettings {
        main: MAIN.parse()?,
        orchestrator: Orchestrator::Main,
        worker: vec![WORKER.parse()?],
        reviewer: vec![REVIEWER.parse()?],
        judge: vec![REVIEWER.parse()?],
        worker_workflows: false,
        merger: Vec::new(),
        orchestrator_context: Default::default(),
        orchestrator_picks: Default::default(),
        ways: Default::default(),
    };
    crew.settings.workflow = "spec-first".to_owned();
    crew.settings.workflows.adapt_with = AdaptWith::Script;
    // The repository has tests, which autopilot wants to see pass before it merges.
    let passing = |name: &str| CheckCommand {
        name: name.to_owned(),
        command: vec!["true".to_owned()],
    };
    crew.settings.checks.run = vec![passing("tests")];
    crew.settings.checks.full_suite = vec![passing("suite")];
    let written = argonauts_runner::template("spec-first", ".argonauts/workflows");
    write_workflow(&crew, "spec-first.toml", &written)?;
    let _env = crew.repo.env_guard();

    let outcome = on_autopilot(&crew).await?;
    let events = &outcome.events;
    let said: Vec<&str> = events
        .iter()
        .filter_map(|event| match &event.kind {
            RunEventKind::Note(note) => Some(note.text.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(
        outcome.exit,
        EngineExit::Finished(RunOutcome::Merged),
        "{said:#?}"
    );
    assert!(crew.repo.root().join("due.rs").exists(), "the work is in");

    // The file's three steps ran as written, each on the CLI its role names. The merger's
    // commit message starts while the merge waits for autopilot, so whether it shows is timing.
    let shown: Vec<String> = started(events)
        .iter()
        .filter(|(label, ..)| label != "commit message")
        .map(|(label, role, worker)| format!("{label} {} {worker}", role.as_str()))
        .collect();
    assert_eq!(
        shown,
        [
            format!("plan orchestrator {MAIN}"),
            format!("build:due-field worker {WORKER}"),
            format!("review:due-field reviewer {REVIEWER}"),
        ]
    );

    // Nothing waited for you, and no row is red.
    let mut open = Vec::new();
    for event in events {
        match &event.kind {
            RunEventKind::Approval(ApprovalEvent::Opened(opened)) => open.push(opened.id.clone()),
            RunEventKind::Approval(ApprovalEvent::Decided(decided)) => {
                assert_ne!(decided.by, Party::User, "{decided:?}");
                open.retain(|id| *id != decided.id);
            }
            RunEventKind::Session(SessionEvent::Ended(end)) => {
                assert_eq!(end.reason, EndReason::Completed, "{:?}", event.session);
            }
            _ => {}
        }
    }
    assert!(open.is_empty(), "left open: {open:?}");
    Ok(())
}

/// A judge of the worker's own provider, on another model than the worker's.
const SAME_PROVIDER_JUDGE: &str = "codex/judge";

/// The verdict a script's readers answer with.
const VERDICT: &str = "{ type: 'object', properties: { accept: { type: 'boolean' }, findings: \
     { type: 'array', items: { type: 'string' } } }, required: ['accept', 'findings'] }";

/// A crew whose orchestrator writes `script`, with the worker building every task and a
/// reviewer of another provider, both answering whatever they're asked to judge.
fn scripted(script: String) -> anyhow::Result<Crew> {
    let mut crew = Crew::new()?;
    crew.settings.evolution.propose_every = 0;
    let orchestrator =
        FakeRunner::cli("claude-code/fable", "anthropic").respond_with(move |spec| {
            match spec.step {
                Some(StepName::ModelJudge) => approves(),
                _ => FakeSession::structured(json!({"script": script})),
            }
        });
    crew.services.workers.add(orchestrator);
    let main = FakeRunner::cli(MAIN, "anthropic").respond_with(|spec| match spec.step {
        Some(StepName::ModelJudge) => approves(),
        _ => support::smallest_answer(spec),
    });
    crew.services.workers.add(main);
    let judges = |spec: &SessionSpec| match spec.step {
        Some(StepName::ModelJudge) => approves(),
        _ if spec.json_schema.is_some() => {
            FakeSession::structured(json!({"accept": true, "findings": []}))
        }
        _ => FakeSession::completed("Done as asked."),
    };
    let worker = FakeRunner::cli(WORKER, "openai").respond_with(move |spec| {
        if spec.access.writes() {
            let file = if prompt(spec).contains("Due docs") {
                "docs.md"
            } else {
                "due.rs"
            };
            std::fs::write(spec.workdir.join(file), "due\n").expect("writes its file");
            return FakeSession::structured(report(&format!("Wrote {file}"), file));
        }
        judges(spec)
    });
    crew.services.workers.add(worker);
    crew.services
        .workers
        .add(FakeRunner::cli(REVIEWER, "google").respond_with(judges));
    crew.settings.crew = CrewSettings {
        main: MAIN.parse()?,
        orchestrator: Orchestrator::Models(vec!["claude-code/fable".parse()?]),
        worker: vec![WORKER.parse()?],
        reviewer: vec![REVIEWER.parse()?],
        judge: vec![REVIEWER.parse()?],
        worker_workflows: false,
        merger: Vec::new(),
        orchestrator_context: Default::default(),
        orchestrator_picks: Default::default(),
        ways: Default::default(),
    };
    crew.settings.workflow = "orchestrator".to_owned();
    crew.settings.workflows.adapt_with = AdaptWith::Script;
    // The repository has tests, which autopilot wants to see pass before it merges.
    let passing = |name: &str| argonauts_core::settings::protocol::CheckCommand {
        name: name.to_owned(),
        command: vec!["true".to_owned()],
    };
    crew.settings.checks.run = vec![passing("tests")];
    crew.settings.checks.full_suite = vec![passing("suite")];
    Ok(crew)
}

/// The worker each labelled session ran on, in the order they started.
fn ran_on(events: &[RunEvent], label: &str) -> Vec<String> {
    started(events)
        .into_iter()
        .filter(|(named, ..)| named == label)
        .map(|(_, _, worker)| worker)
        .collect()
}

/// The script's last report of the agent under `label`.
fn reported(events: &[RunEvent], label: &str) -> Option<argonauts_core::events::session::CliAgent> {
    reports(events)
        .last()?
        .workflow
        .agents
        .iter()
        .find(|agent| agent.label == label)
        .cloned()
}

/// A model that wraps its JSON answer in a Markdown fence still answered, so the call counts
/// with its answer and its row is done.
#[tokio::test]
async fn a_fenced_json_answer_fits_its_schema() -> anyhow::Result<()> {
    fenced_answer_counts("```json\n{\"plan\": \"Add a due field\"}\n```").await
}

/// Words before and after the fence are still around the same answer.
#[tokio::test]
async fn a_fenced_json_answer_inside_words_fits_its_schema() -> anyhow::Result<()> {
    fenced_answer_counts(
        "Here is my plan:\n\n```json\n{\"plan\": \"Add a due field\"}\n```\n\n\
         It keeps every test as it is.",
    )
    .await
}

async fn fenced_answer_counts(fenced: &'static str) -> anyhow::Result<()> {
    let script = "export const meta = { name: 'fenced', description: 'Add due dates', \
         phases: [{ name: 'Propose' }] }\n\
         const plan = await agent('Propose a plan', { role: 'worker', access: 'read', \
         label: 'propose:p0', phase: 'Propose', schema: { type: 'object', properties: { plan: { type: 'string' } }, \
         required: ['plan'] } })\n\
         return plan ? plan.plan : 'no plan'\n";
    let mut crew = scripted(script.to_owned())?;
    crew.services.workers.add(
        FakeRunner::cli("opencode/gemma", "unknown")
            .respond_with(move |_| FakeSession::completed(fenced)),
    );
    crew.settings.crew.worker = vec!["opencode/gemma".parse()?];
    let _env = crew.repo.env_guard();
    let outcomes = crew.run_to_end(RunMode::Crew, REQUEST, 3).await?;
    let events = all_events(&outcomes);

    let row = reported(events, "propose:p0").context("propose:p0 is reported")?;
    assert_eq!(row.state, CliAgentState::Done, "{row:?}");
    assert_eq!(row.error, None);
    let missing = events.iter().any(|event| {
        matches!(&event.kind, RunEventKind::Note(note) if note.text.starts_with("propose:p0 "))
    });
    assert!(!missing, "no note says propose:p0 went without an answer");
    Ok(())
}

/// A reader that ends without its answer fails, and its row keeps why, as the note says it.
#[tokio::test]
async fn a_failed_agents_row_says_why() -> anyhow::Result<()> {
    let script = format!(
        "export const meta = {{ name: 'apology', description: 'Add due dates', \
         phases: [{{ name: 'Challenge' }}] }}\n\
         await agent('Challenge the plan', {{ role: 'reviewer', label: 'challenge:p3', \
         phase: 'Challenge', schema: {VERDICT} }})\n\
         return 'done'\n"
    );
    let mut crew = scripted(script)?;
    crew.services.workers.add(
        FakeRunner::cli("opencode/gemma", "unknown").respond_with(|_| {
            FakeSession::completed("I apologize for the previous execution errors.")
        }),
    );
    crew.settings.crew.reviewer = vec!["opencode/gemma".parse()?];
    let _env = crew.repo.env_guard();
    let outcomes = crew.run_to_end(RunMode::Crew, REQUEST, 3).await?;
    let events = all_events(&outcomes);

    let row = reported(events, "challenge:p3").context("challenge:p3 is reported")?;
    assert_eq!(row.state, CliAgentState::Failed, "{row:?}");
    assert_eq!(row.error.as_deref(), Some("ended without an answer"));
    assert!(
        notes(events).contains(&"challenge:p3 ended without an answer"),
        "{:#?}",
        notes(events)
    );
    Ok(())
}

/// A sign-off about no one task judges every builder's work, so on autopilot it comes from
/// another provider than theirs even when the judges' list puts a builder's first, and the run
/// merges.
#[tokio::test]
async fn on_autopilot_a_sign_off_about_no_one_task_comes_from_another_provider()
-> anyhow::Result<()> {
    let script = format!(
        "export const meta = {{ name: 'signed', description: 'Add due dates', \
         phases: [{{ name: 'Build' }}, {{ name: 'Review' }}, {{ name: 'Sign-off' }}] }}\n\
         await agent('Due field: ' + args.request, {{ role: 'worker', label: 'build:field', \
         phase: 'Build', task: 'field' }})\n\
         await agent('Review the due field', {{ role: 'reviewer', label: 'review:field', \
         phase: 'Review', task: 'field', schema: {VERDICT} }})\n\
         return await agent('Is the whole change done?', {{ role: 'judge', label: 'sign-off', \
         phase: 'Sign-off', schema: {VERDICT} }})\n"
    );
    let mut crew = scripted(script)?;
    crew.settings.crew.judge = vec![WORKER.parse()?, REVIEWER.parse()?];
    let _env = crew.repo.env_guard();
    let outcome = on_autopilot(&crew).await?;
    let events = &outcome.events;

    assert_eq!(ran_on(events, "sign-off"), [REVIEWER]);
    assert_eq!(
        outcome.exit,
        EngineExit::Finished(RunOutcome::Merged),
        "{:#?}",
        notes(events)
    );
    Ok(())
}

/// On autopilot, a review from another provider of the build as it stands counts beside a
/// sign-off from the worker's own provider, so the run merges.
#[tokio::test]
async fn a_cross_review_from_another_provider_counts_beside_a_same_provider_sign_off()
-> anyhow::Result<()> {
    let script = format!(
        "export const meta = {{ name: 'cross', description: 'Add due dates', \
         phases: [{{ name: 'Build' }}, {{ name: 'Review' }}, {{ name: 'Sign-off' }}] }}\n\
         await agent('Due field: ' + args.request, {{ role: 'worker', label: 'build:field', \
         phase: 'Build', task: 'field' }})\n\
         await agent('Review the due field', {{ role: 'reviewer', label: 'review:field', \
         phase: 'Review', task: 'field', schema: {VERDICT} }})\n\
         return await agent('Sign off the due field', {{ role: 'judge', label: 'sign-off', \
         phase: 'Sign-off', task: 'field', schema: {VERDICT} }})\n"
    );
    let mut crew = scripted(script)?;
    crew.services
        .workers
        .add(
            FakeRunner::cli(SAME_PROVIDER_JUDGE, "openai").respond_with(|spec| match spec.step {
                Some(StepName::ModelJudge) => approves(),
                _ => FakeSession::structured(json!({"accept": true, "findings": []})),
            }),
        );
    crew.settings.crew.judge = vec![SAME_PROVIDER_JUDGE.parse()?];
    let _env = crew.repo.env_guard();
    let outcome = on_autopilot(&crew).await?;
    let events = &outcome.events;

    assert_eq!(ran_on(events, "review:field"), [REVIEWER]);
    assert_eq!(ran_on(events, "sign-off"), [SAME_PROVIDER_JUDGE]);
    assert_eq!(
        outcome.exit,
        EngineExit::Finished(RunOutcome::Merged),
        "{:#?}",
        notes(events)
    );
    Ok(())
}

/// A cross-provider approval counts only for the build it read: once the task is built again
/// and only its worker's provider approves the new build, the run keeps its branch.
#[tokio::test]
async fn a_cross_review_of_an_earlier_build_doesnt_count() -> anyhow::Result<()> {
    let script = format!(
        "export const meta = {{ name: 'stale', description: 'Add due dates', \
         phases: [{{ name: 'Build' }}, {{ name: 'Review' }}, {{ name: 'Sign-off' }}] }}\n\
         await agent('Due field: ' + args.request, {{ role: 'worker', label: 'build:field', \
         phase: 'Build', task: 'field' }})\n\
         await agent('Review the due field', {{ role: 'reviewer', label: 'review:field', \
         phase: 'Review', task: 'field', schema: {VERDICT} }})\n\
         await agent('Due field again: ' + args.request, {{ role: 'worker', label: 'build:field', \
         phase: 'Build', task: 'field', tryNumber: 2 }})\n\
         return await agent('Sign off the due field', {{ role: 'judge', label: 'sign-off', \
         phase: 'Sign-off', task: 'field', schema: {VERDICT} }})\n"
    );
    let mut crew = scripted(script)?;
    crew.services
        .workers
        .add(
            FakeRunner::cli(SAME_PROVIDER_JUDGE, "openai").respond_with(|spec| match spec.step {
                Some(StepName::ModelJudge) => approves(),
                _ => FakeSession::structured(json!({"accept": true, "findings": []})),
            }),
        );
    crew.settings.crew.judge = vec![SAME_PROVIDER_JUDGE.parse()?];
    let _env = crew.repo.env_guard();
    let outcome = on_autopilot(&crew).await?;
    let events = &outcome.events;

    assert_eq!(ran_on(events, "sign-off"), [SAME_PROVIDER_JUDGE]);
    assert_eq!(
        outcome.exit,
        EngineExit::Finished(RunOutcome::BranchLeft),
        "{:#?}",
        notes(events)
    );
    assert!(
        notes(events)
            .iter()
            .any(|note| note.contains(", picked from its builder's own provider, reviewed ")),
        "{:#?}",
        notes(events)
    );
    Ok(())
}

/// A review only the task's builder could take is skipped rather than given to the builder,
/// whatever the reviewers' list says. On autopilot the run keeps its work on its branch, and
/// the reason names the task nobody reviewed.
#[tokio::test]
async fn on_autopilot_a_review_only_its_builder_could_take_keeps_the_branch() -> anyhow::Result<()>
{
    let mut crew = scripted(build_and_review(&[("field", "Due field")]))?;
    crew.settings.crew.reviewer = vec![WORKER.parse()?];
    let _env = crew.repo.env_guard();
    let outcome = on_autopilot(&crew).await?;
    let events = &outcome.events;
    let said = notes(events);

    assert_eq!(
        ran_on(events, "review:field"),
        Vec::<String>::new(),
        "{said:#?}"
    );
    assert!(
        said.contains(
            &"review:field skipped, since nobody outside its builder's provider is left to \
              review build:field"
        ),
        "{said:#?}"
    );
    assert_eq!(
        outcome.exit,
        EngineExit::Finished(RunOutcome::BranchLeft),
        "{said:#?}"
    );
    assert!(
        said.contains(
            &"Autopilot answered \"Merge this run into main?\" with \"Keep the branch\" instead \
              of asking you, since nobody outside its builder's provider is left to review \
              build:field."
        ),
        "{said:#?}"
    );
    Ok(())
}

/// A script's review records whether you chose its reviewer. One you named for the role from
/// the builder's own provider lets the run merge on autopilot; the same model taken from the
/// crew is code's pick, so the run keeps its work on its branch.
#[tokio::test]
async fn on_autopilot_a_scripts_reviewer_you_chose_from_the_builders_provider_lets_it_merge()
-> anyhow::Result<()> {
    use argonauts_core::events::task::PickedBy;
    use argonauts_core::settings::roles::RoleSettings;

    for named in [true, false] {
        let mut crew = scripted(build_and_review(&[("field", "Due field")]))?;
        crew.services
            .workers
            .add(FakeRunner::cli(SAME_PROVIDER_JUDGE, "openai").respond_with(
                |spec| match spec.step {
                    Some(StepName::ModelJudge) => approves(),
                    _ => FakeSession::structured(json!({"accept": true, "findings": []})),
                },
            ));
        if named {
            let models = vec![SAME_PROVIDER_JUDGE.parse()?];
            let reviewers = RoleSettings {
                models,
                ..RoleSettings::default()
            };
            crew.settings.roles.insert(RoleName::Reviewer, reviewers);
        } else {
            crew.settings.crew.reviewer = vec![SAME_PROVIDER_JUDGE.parse()?];
        }
        let _env = crew.repo.env_guard();
        let outcome = on_autopilot(&crew).await?;
        let events = &outcome.events;
        let picked: Vec<Option<PickedBy>> = events
            .iter()
            .filter_map(|event| match &event.kind {
                RunEventKind::Review(ReviewEvent::Reviewed { picked_by, .. }) => Some(*picked_by),
                _ => None,
            })
            .collect();

        assert_eq!(ran_on(events, "review:field"), [SAME_PROVIDER_JUDGE]);
        let (by, ended) = if named {
            (PickedBy::You, RunOutcome::Merged)
        } else {
            (PickedBy::Code, RunOutcome::BranchLeft)
        };
        assert_eq!(picked, [Some(by)]);
        assert_eq!(
            outcome.exit,
            EngineExit::Finished(ended),
            "{:#?}",
            notes(events)
        );
    }
    Ok(())
}

fn notes(events: &[RunEvent]) -> Vec<&str> {
    events
        .iter()
        .filter_map(|event| match &event.kind {
            RunEventKind::Note(note) => Some(note.text.as_str()),
            _ => None,
        })
        .collect()
}

/// Qwen on Ollama, as setup offers it for reviews: served without tools, with the window its
/// tag holds it to.
const LOCAL_REVIEWER: &str = "ollama/qwen3-coder:30b-64k";

/// The crew of [`scripted`] with a local model first among the reviewers, held to `window`
/// tokens or sized as it loads, and a CLI reviewer of another provider after it.
fn with_a_local_reviewer(
    script: String,
    window: Option<u64>,
) -> anyhow::Result<(Crew, Arc<FakeRunner>)> {
    let mut crew = scripted(script)?;
    crew.services.registry.add(serde_json::from_value(json!({
        "id": LOCAL_REVIEWER,
        "provider": "alibaba",
        "kind": "endpoint",
        "access": "local",
        "cost": {"unit": "none"},
        "window": window,
        "context_window": 262_144,
        "discovered": true,
    }))?);
    let local = crew.services.workers.add(
        FakeRunner::endpoint(LOCAL_REVIEWER, "alibaba")
            .respond_with(|_| FakeSession::structured(json!({"accept": true, "findings": []}))),
    );
    crew.settings.crew.reviewer = vec![LOCAL_REVIEWER.parse()?, REVIEWER.parse()?];
    // Each task's worker writes its file, the big one far past a small window.
    let worker = FakeRunner::cli("codex/writer", "openai").respond_with(|spec| {
        let asked = prompt(spec);
        let (file, text) = if asked.contains("Big:") {
            ("big.rs", "// a long line of the big change\n".repeat(2_000))
        } else {
            ("small.rs", "small\n".to_owned())
        };
        std::fs::write(spec.workdir.join(file), text).expect("writes its file");
        FakeSession::structured(report(&format!("Wrote {file}"), file))
    });
    crew.services.workers.add(worker);
    crew.settings.crew.worker = vec!["codex/writer".parse()?];
    Ok((crew, local))
}

/// Builds `tasks` one after another, each reviewed once it's built.
fn build_and_review(tasks: &[(&str, &str)]) -> String {
    let mut script = "export const meta = { name: 'reviewed', description: 'Add due dates', \
         phases: [{ name: 'Build' }, { name: 'Review' }] }\n"
        .to_owned();
    for (task, asked) in tasks {
        script.push_str(&format!(
            "await agent('{asked}: ' + args.request, {{ role: 'worker', label: 'build:{task}', \
             phase: 'Build', task: '{task}' }})\n\
             await agent('Review {task}', {{ role: 'reviewer', label: 'review:{task}', \
             phase: 'Review', task: '{task}', schema: {VERDICT} }})\n"
        ));
    }
    script.push_str("return 'done'\n");
    script
}

/// A local model served without tools reviews with the change and the checks in its prompt,
/// and nobody stands in for it.
#[tokio::test]
async fn a_local_reviewer_starts_with_no_tools_and_the_change_in_its_prompt() -> anyhow::Result<()>
{
    use argonauts_core::events::session::InputPartKind;
    use argonauts_core::session_runner::Access;

    let (crew, local) =
        with_a_local_reviewer(build_and_review(&[("small", "Small")]), Some(65_536))?;
    let _env = crew.repo.env_guard();
    let outcomes = crew.run_to_end(RunMode::Crew, REQUEST, 3).await?;
    let events = all_events(&outcomes);

    assert_eq!(ran_on(events, "review:small"), [LOCAL_REVIEWER]);
    let asked = local.specs();
    let review = asked.last().context("the local model reviewed")?;
    assert_eq!(review.access, Access::NoTools);
    let change = review
        .prompt
        .iter()
        .find(|part| part.kind == InputPartKind::Evidence && part.label == "the change")
        .with_context(|| format!("no change in its prompt: {}", prompt(review)))?;
    assert!(change.text.contains("+small"), "{}", change.text);
    assert!(!change.text.contains("folder"), "{}", change.text);
    assert!(
        review
            .prompt
            .iter()
            .any(|part| part.kind == InputPartKind::CheckOutput),
        "the checks are in its prompt: {}",
        prompt(review)
    );
    Ok(())
}

/// A change too big for the local model's window goes to a reviewer with tools, saying why,
/// and a later small review still runs on the local model.
#[tokio::test]
async fn a_change_too_big_for_a_local_window_goes_to_a_reviewer_with_tools() -> anyhow::Result<()> {
    use argonauts_core::session_runner::Access;

    let script = build_and_review(&[("big", "Big"), ("small", "Small")]);
    let (crew, local) = with_a_local_reviewer(script, Some(8_192))?;
    let _env = crew.repo.env_guard();
    let outcomes = crew.run_to_end(RunMode::Crew, REQUEST, 3).await?;
    let events = all_events(&outcomes);

    assert_eq!(ran_on(events, "review:big"), [REVIEWER]);
    assert_eq!(ran_on(events, "review:small"), [LOCAL_REVIEWER]);
    assert!(
        local
            .specs()
            .iter()
            .all(|spec| spec.access == Access::NoTools)
    );
    let said = events.iter().any(|event| {
        matches!(&event.kind, RunEventKind::Workflow(WorkflowEvent::Note(note))
            if note.text.starts_with("review:big goes to ")
                && note.text.ends_with(", whose 8k window can't hold this change"))
    });
    assert!(said, "{:#?}", notes(events));
    // It runs in the local model's place, and says why for its agent view.
    let stood_in = events
        .iter()
        .find_map(|event| match &event.kind {
            RunEventKind::Session(SessionEvent::Started(started))
                if started.label.as_deref() == Some("review:big") =>
            {
                Some(started)
            }
            _ => None,
        })
        .context("review:big started")?;
    let asked = stood_in.asked.as_ref().map(ToString::to_string);
    assert_eq!(asked.as_deref(), Some(LOCAL_REVIEWER));
    assert_eq!(
        stood_in.asked_why.as_deref(),
        Some("whose 8k window can't hold this change")
    );
    Ok(())
}

/// Two reviews of one task, the first on the local model, each by its number.
fn reviewed_twice() -> String {
    format!(
        "export const meta = {{ name: 'reviewed twice', description: 'Add due dates', \
         phases: [{{ name: 'Build' }}, {{ name: 'Review' }}] }}\n\
         await agent('Big: ' + args.request, {{ role: 'worker', label: 'build:big', \
         phase: 'Build', task: 'big' }})\n\
         await agent('Review big', {{ role: 'reviewer', label: 'review:a', phase: 'Review', \
         task: 'big', model: 0, schema: {VERDICT} }})\n\
         await agent('Review big again', {{ role: 'reviewer', label: 'review:b', \
         phase: 'Review', task: 'big', model: 1, schema: {VERDICT} }})\n\
         return 'done'\n"
    )
}

/// A reviewer from the builder's provider, beside the local model and the other reviewer.
const BUILDERS_OWN: &str = "codex/luna";

/// The reviewer that takes a local model's place is one no other review of the same task has
/// in the round, so no model's review counts twice: the next review keeps clear of it.
#[tokio::test]
async fn a_local_reviewers_fallback_repeats_no_other_review() -> anyhow::Result<()> {
    let (mut crew, _local) = with_a_local_reviewer(reviewed_twice(), Some(8_192))?;
    crew.services.workers.add(
        FakeRunner::cli(BUILDERS_OWN, "openai")
            .respond_with(|_| FakeSession::structured(json!({"accept": true, "findings": []}))),
    );
    crew.settings.crew.reviewer.push(BUILDERS_OWN.parse()?);
    let _env = crew.repo.env_guard();
    let outcomes = crew.run_to_end(RunMode::Crew, REQUEST, 3).await?;
    let events = all_events(&outcomes);

    assert_eq!(ran_on(events, "review:a"), [REVIEWER]);
    assert_eq!(ran_on(events, "review:b"), [BUILDERS_OWN]);
    Ok(())
}

/// The reviewer that takes a local model's place comes from another provider than the
/// builder's while one is free, whatever number the call had.
#[tokio::test]
async fn a_local_reviewers_fallback_comes_from_another_provider() -> anyhow::Result<()> {
    let script = format!(
        "export const meta = {{ name: 'reviewed', description: 'Add due dates', \
         phases: [{{ name: 'Build' }}, {{ name: 'Review' }}] }}\n\
         await agent('Big: ' + args.request, {{ role: 'worker', label: 'build:big', \
         phase: 'Build', task: 'big' }})\n\
         await agent('Review big', {{ role: 'reviewer', label: 'review:big', phase: 'Review', \
         task: 'big', model: 1, schema: {VERDICT} }})\n\
         return 'done'\n"
    );
    let (mut crew, _local) = with_a_local_reviewer(script, Some(8_192))?;
    crew.services.workers.add(
        FakeRunner::cli(BUILDERS_OWN, "openai")
            .respond_with(|_| FakeSession::structured(json!({"accept": true, "findings": []}))),
    );
    crew.settings.crew.reviewer = vec![
        REVIEWER.parse()?,
        LOCAL_REVIEWER.parse()?,
        BUILDERS_OWN.parse()?,
    ];
    let _env = crew.repo.env_guard();
    let outcomes = crew.run_to_end(RunMode::Crew, REQUEST, 3).await?;
    let events = all_events(&outcomes);

    assert_eq!(ran_on(events, "review:big"), [REVIEWER]);
    Ok(())
}

/// A local model its server sizes as it loads, and that isn't loaded, is held to the least a
/// local server gives, so a big change goes to a reviewer with tools and says why.
#[tokio::test]
async fn a_local_model_not_yet_loaded_is_held_to_the_least_window() -> anyhow::Result<()> {
    let script = build_and_review(&[("big", "Big"), ("small", "Small")]);
    let (crew, _local) = with_a_local_reviewer(script, None)?;
    let _env = crew.repo.env_guard();
    let outcomes = crew.run_to_end(RunMode::Crew, REQUEST, 3).await?;
    let events = all_events(&outcomes);

    assert_eq!(ran_on(events, "review:big"), [REVIEWER]);
    assert_eq!(ran_on(events, "review:small"), [LOCAL_REVIEWER]);
    let said = events.iter().any(|event| {
        matches!(&event.kind, RunEventKind::Workflow(WorkflowEvent::Note(note))
            if note.text.ends_with(", whose window isn't known until it loads"))
    });
    assert!(said, "{:#?}", notes(events));
    Ok(())
}

/// The script of a task that builds on another: cap first, reviewed, then per-key after it.
fn per_key_after_cap() -> String {
    format!(
        "export const meta = {{ name: 'after', description: 'Add due dates', \
         phases: [{{ name: 'Build' }}, {{ name: 'Review' }}] }}\n\
         await agent('Cap: ' + args.request, {{ role: 'worker', label: 'build:cap', \
         phase: 'Build', task: 'cap' }})\n\
         await agent('Review cap', {{ role: 'reviewer', label: 'review:cap', phase: 'Review', \
         task: 'cap', schema: {VERDICT} }})\n\
         await agent('Per key: ' + args.request, {{ role: 'worker', label: 'build:per-key', \
         phase: 'Build', task: 'per-key', after: ['cap'] }})\n\
         await agent('Review per-key', {{ role: 'reviewer', label: 'review:per-key', \
         phase: 'Review', task: 'per-key', after: ['cap'], schema: {VERDICT} }})\n\
         return 'done'\n"
    )
}

/// Writes cap.rs for the cap task, and for per-key, per_key.rs beside whatever cap.rs its
/// worktree started with, which it says in its report.
fn builds_on_cap() -> FakeRunner {
    FakeRunner::cli("codex/writer", "openai").respond_with(|spec| {
        if !spec.access.writes() {
            return FakeSession::structured(json!({"accept": true, "findings": []}));
        }
        if prompt(spec).contains("Cap:") {
            std::fs::write(spec.workdir.join("cap.rs"), "capped\n").expect("writes its file");
            return FakeSession::structured(report("Wrote cap.rs", "cap.rs"));
        }
        let found = if spec.workdir.join("cap.rs").exists() {
            "cap.rs was there"
        } else {
            "cap.rs was missing"
        };
        std::fs::write(spec.workdir.join("per_key.rs"), "per key\n").expect("writes its file");
        FakeSession::structured(report(found, "per_key.rs"))
    })
}

/// A task that comes after another starts from that one's accepted work: the first merges
/// into the run before the second's worktree is made, and the second's change leaves it out.
#[tokio::test]
async fn a_task_after_another_starts_from_its_merge_and_holds_its_change() -> anyhow::Result<()> {
    use argonauts_core::events::session::InputPartKind;

    let mut crew = scripted(per_key_after_cap())?;
    crew.services.workers.add(builds_on_cap());
    crew.settings.crew.worker = vec!["codex/writer".parse()?];
    let _env = crew.repo.env_guard();
    let outcomes = crew.run_to_end(RunMode::Crew, REQUEST, 3).await?;
    let events = all_events(&outcomes);

    let merged: Vec<String> = events
        .iter()
        .filter_map(|event| match &event.kind {
            RunEventKind::Merge(argonauts_core::events::task::MergeEvent::Merged {
                task, ..
            }) => Some(task.to_string()),
            _ => None,
        })
        .collect();
    let built = started(events)
        .iter()
        .position(|(label, ..)| label == "build:per-key");
    let first_merge = events.iter().position(|event| {
        matches!(&event.kind, RunEventKind::Merge(argonauts_core::events::task::MergeEvent::Merged { task, .. }) if task.as_str().contains("cap"))
    });
    assert!(built.is_some() && first_merge.is_some(), "{merged:?}");
    let per_key_started = events.iter().position(|event| {
        matches!(&event.kind, RunEventKind::Session(SessionEvent::Started(started))
            if started.label.as_deref() == Some("build:per-key"))
    });
    assert!(
        first_merge < per_key_started,
        "cap merges before per-key starts"
    );
    let said = notes(events);
    assert!(
        !said.iter().any(|note| note.contains("skipped")),
        "{said:#?}"
    );
    let reported = task_states(events);
    assert!(
        reported
            .iter()
            .filter(|(_, state)| *state == TaskState::Merged)
            .count()
            >= 2,
        "{reported:#?}"
    );
    // Per-key's worktree held cap's work, and its review reads only its own change.
    let review = crew
        .services
        .workers
        .fake(REVIEWER)
        .context("the reviewer")?
        .specs()
        .into_iter()
        .rev()
        .find(|spec| prompt(spec).contains("Review per-key"))
        .context("per-key was reviewed")?;
    let change = review
        .prompt
        .iter()
        .find(|part| part.kind == InputPartKind::Evidence)
        .context("the change")?;
    assert!(change.text.contains("per_key.rs"), "{}", change.text);
    assert!(!change.text.contains("cap.rs"), "{}", change.text);
    let summaries: Vec<String> = events
        .iter()
        .filter_map(|event| match &event.kind {
            RunEventKind::Task(TaskEvent::Reported(report)) => Some(report.summary.clone()),
            _ => None,
        })
        .collect();
    assert!(
        summaries
            .iter()
            .any(|summary| summary == "cap.rs was there"),
        "{summaries:#?}"
    );
    Ok(())
}

/// A task that comes after one its review never accepted is skipped, saying why, and no
/// worker builds it.
#[tokio::test]
async fn a_task_after_one_never_accepted_is_skipped_and_says_why() -> anyhow::Result<()> {
    let mut crew = scripted(per_key_after_cap())?;
    crew.services.workers.add(builds_on_cap());
    crew.settings.crew.worker = vec!["codex/writer".parse()?];
    crew.services.workers.add(
        FakeRunner::cli("antigravity-cli/refuses", "google").respond_with(|_| {
            FakeSession::structured(json!({"accept": false, "findings": ["it overshoots"]}))
        }),
    );
    crew.settings.crew.reviewer = vec!["antigravity-cli/refuses".parse()?];
    let _env = crew.repo.env_guard();
    let outcomes = crew.run_to_end(RunMode::Crew, REQUEST, 3).await?;
    let events = all_events(&outcomes);

    assert!(ran_on(events, "build:per-key").is_empty());
    let why = "skipped, since it comes after cap, which wasn't accepted";
    let row = reported(events, "build:per-key").context("build:per-key is reported")?;
    assert_eq!(row.error.as_deref(), Some(why));
    assert!(
        notes(events).contains(&format!("build:per-key {why}").as_str()),
        "{:#?}",
        notes(events)
    );
    Ok(())
}
