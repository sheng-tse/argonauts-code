//! A crew run that follows a workflow file's own steps: the runner runs them, each agent goes to
//! the model its role names, workers build tasks in worktrees of their own, and what the
//! workflow's reviews accepted is merged before the merge approval.

mod support;

use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

use serde_json::{Value, json};

use argonauts_core::engine::{EngineCommand, EngineExit};
use argonauts_core::events::card::{CardEvent, CardState, ReviewEvent};
use argonauts_core::events::checkpoint::{
    AnswerAction, CheckpointEvent, CheckpointKind, CheckpointOpened, CheckpointPacket,
};
use argonauts_core::events::run::{RunMode, RunOutcome};
use argonauts_core::events::session::{SessionEvent, SubagentStatus, VendorAgentState};
use argonauts_core::events::workflow::{WorkflowEvent, WorkflowReport};
use argonauts_core::events::{RunEvent, RunEventKind};
use argonauts_core::names::RoleName;
use argonauts_core::settings::approvals::ApprovalMode;
use argonauts_core::settings::crew::{CrewSettings, Orchestrator};
use argonauts_core::settings::sessions::{FanoutSwitch, NativeFanout};
use argonauts_core::settings::workflows::AdaptWith;
use argonauts_core::steps::StepName;
use argonauts_core::steps::judgment::ReviewVerdict;
use argonauts_core::worker::SessionSpec;
use argonauts_testkit::worker::{FakeSession, FakeWorker};

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
role     = "workers"
for_each = "task"

[[step]]
name         = "Review"
role         = "reviewers"
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

/// The main agent plans, Codex builds each task as a file of its own, and Antigravity CLI
/// reviews and signs off. Its first review of the first task asks for a change.
fn crew(workflow: &str) -> anyhow::Result<(Crew, Arc<FakeWorker>)> {
    let mut crew = Crew::new()?;
    crew.settings.evolution.propose_every = 0;
    let main = FakeWorker::harness(MAIN, "claude").respond_with(|_| {
        FakeSession::structured(json!({
            "summary": "Two tasks",
            "tasks": [
                {"title": "Due field", "done_when": ["a task has a due date"]},
                {"title": "Due docs", "done_when": ["the README says how"]},
            ],
        }))
    });
    crew.services.workers.add(main);
    let worker = FakeWorker::harness(WORKER, "gpt").respond_with(|spec| {
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
    let reviewer = FakeWorker::harness(REVIEWER, "gemini").respond_with(move |spec| {
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
    crew.settings.main_agent = MAIN.parse()?;
    crew.settings.crew = CrewSettings {
        orchestrator: Orchestrator::Main,
        workers: vec![WORKER.parse()?],
        reviewers: vec![REVIEWER.parse()?],
        judge: vec![REVIEWER.parse()?],
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

fn card_states(events: &[RunEvent]) -> Vec<(String, CardState)> {
    events
        .iter()
        .filter_map(|event| match &event.kind {
            RunEventKind::Card(CardEvent::State { to, .. }) => {
                Some((event.card_id.as_ref()?.to_string(), *to))
            }
            _ => None,
        })
        .collect()
}

fn opened(events: &[RunEvent], kind: CheckpointKind) -> bool {
    events.iter().any(|event| {
        matches!(&event.kind, RunEventKind::Checkpoint(CheckpointEvent::Opened(opened))
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
    let states = card_states(events);
    for card in ["task-due-field", "task-due-docs"] {
        let last = states
            .iter()
            .rev()
            .find(|(id, _)| id == card)
            .map(|(_, state)| *state);
        assert_eq!(last, Some(CardState::Integrated), "{card}: {states:?}");
    }
    assert!(
        opened(events, CheckpointKind::Merge),
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
                event.card_id.as_ref()?.to_string(),
                reviewer.to_string(),
                *round,
                review.verdict,
            )),
            _ => None,
        })
        .collect();
    let of = |card: &str| -> Vec<(u32, ReviewVerdict)> {
        reviewed
            .iter()
            .filter(|(reviewed, reviewer, ..)| reviewed == card && reviewer == REVIEWER)
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
    let merge = approval(events, CheckpointKind::Merge).expect("the merge approval");
    let CheckpointPacket::Merge(packet) = &merge.packet else {
        panic!("a merge approval: {:?}", merge.packet);
    };
    assert!(
        packet
            .rows
            .iter()
            .filter(|row| row.owner.is_some())
            .all(|row| row.review == Some(ReviewVerdict::Approve)),
        "every task shows as approved: {:?}",
        packet.rows
    );
    assert!(
        !opened(events, CheckpointKind::Plan),
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
            .all(|agent| agent.state == VendorAgentState::Done)
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
    // The run parks at the merge approval, with every step of the script behind it.
    let first = crew.start(RunMode::Crew, REQUEST).await?;
    assert!(
        matches!(first.exit, EngineExit::Parked(_)),
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
    let judge = FakeWorker::harness("claude-code/judge", "claude")
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
    let exit = tokio::time::timeout(std::time::Duration::from_secs(30), run.exit).await??;
    assert_eq!(exit, EngineExit::Finished(RunOutcome::Stopped));
    let stopped = run.sink.events();
    let built = worker.specs().len();
    let sessions = started(&stopped).len();
    assert!(
        card_states(&stopped)
            .iter()
            .all(|(_, state)| *state != CardState::Integrated),
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
    let states = card_states(&reopened.events);
    for card in ["task-due-field", "task-due-docs"] {
        let last = states
            .iter()
            .rev()
            .find(|(id, _)| id == card)
            .map(|(_, state)| *state);
        assert_eq!(last, Some(CardState::Integrated), "{card}: {states:?}");
    }
    assert!(opened(&reopened.events, CheckpointKind::Merge));
    Ok(())
}

#[tokio::test]
async fn a_built_in_workflow_runs_the_script_its_orchestrator_writes() -> anyhow::Result<()> {
    let (mut crew, _worker) = crew("maker-checker")?;
    // The orchestrator answers the ask for a script with one of its own shape.
    let script = r#"export const meta = {
  name: 'due-dates',
  description: 'Add due dates',
  phases: [{ title: 'Build', detail: 'one worker' }, { title: 'Check' }],
}
const built = await agent('Add a due date to tasks: ' + args.request, {
  role: 'workers', label: 'build:due-dates', phase: 'Build', task: 'due-dates',
})
const verdict = await agent('Review the change.', {
  role: 'reviewers', label: 'check:due-dates', phase: 'Check', task: 'due-dates',
  schema: { type: 'object', properties: { accept: { type: 'boolean' } }, required: ['accept'] },
})
return { built, accepted: Boolean(verdict && verdict.accept) }
"#;
    let orchestrator = FakeWorker::harness("claude-code/fable", "claude")
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
        ["plan:orchestrator", "build:due-dates", "check:due-dates"]
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

/// A request that says how big a workflow to use: the orchestrator is told to keep to it,
/// and the workflow's row and a note say what was asked.
#[tokio::test]
async fn the_orchestrator_keeps_to_the_size_the_request_asked_for() -> anyhow::Result<()> {
    let (mut crew, _worker) = crew("maker-checker")?;
    let script = r#"export const meta = { name: 'due-dates', description: 'Add due dates',
  phases: [{ title: 'Build' }] }
const built = await agent('Add a due date to tasks: ' + args.request, {
  role: 'workers', label: 'build:due-dates', phase: 'Build', task: 'due-dates',
})
return { built }
"#;
    let orchestrator = FakeWorker::harness("claude-code/fable", "claude")
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
fn approval(events: &[RunEvent], kind: CheckpointKind) -> Option<&CheckpointOpened> {
    events.iter().rev().find_map(|event| match &event.kind {
        RunEventKind::Checkpoint(CheckpointEvent::Opened(opened)) if opened.kind == kind => {
            let opened: &CheckpointOpened = opened;
            Some(opened)
        }
        _ => None,
    })
}

fn keys(opened: &CheckpointOpened) -> String {
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
        matches!(first.exit, EngineExit::Parked(_)),
        "{:?}",
        first.exit
    );
    assert!(started(&first.events).is_empty(), "nothing ran yet");
    let plan = approval(&first.events, CheckpointKind::Plan).expect("the plan approval");
    // Steps written in a file can't be written again, so there is nothing to send back.
    assert_eq!(keys(plan), "ysxd");
    let CheckpointPacket::Plan(packet) = &plan.packet else {
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
        CheckpointKind::Plan,
        AnswerAction::Approve,
        None,
    )?;
    let after = crew.resume(first.events.clone(), vec![approve]).await?;
    assert!(!worker.specs().is_empty(), "the tasks were built");
    assert!(opened(&after.events, CheckpointKind::Merge));
    let plans = after
        .events
        .iter()
        .filter(|event| {
            matches!(&event.kind, RunEventKind::Checkpoint(CheckpointEvent::Opened(opened))
                if opened.kind == CheckpointKind::Plan)
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
             phases: [{{ title: 'Build' }}] }}\n\
             return await agent('Add a due date: ' + args.request, \
             {{ role: 'workers', label: 'build:due', phase: 'Build', task: 'due' }})\n"
        )
    };
    let (first_try, second_try) = (script("due-dates"), script("due-dates-with-tests"));
    let again = second_try.clone();
    let orchestrator =
        FakeWorker::harness("claude-code/fable", "claude").respond_with(move |spec| {
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
    let plan = approval(&first.events, CheckpointKind::Plan).expect("the plan approval");
    assert_eq!(keys(plan), "ybsxd", "the orchestrator can write them again");
    let CheckpointPacket::Plan(packet) = &plan.packet else {
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
        CheckpointKind::Plan,
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
    let plan = approval(&second.events, CheckpointKind::Plan).expect("the plan approval");
    let CheckpointPacket::Plan(packet) = &plan.packet else {
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
            .all(|(label, ..)| label.starts_with("plan:orchestrator")),
        "no step ran before you approved: {ran:?}"
    );

    // Approved, a run that picks up again runs the steps you approved and asks nobody again.
    let (approve, _reply) = answer(
        &second.events,
        CheckpointKind::Plan,
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
    assert!(opened(&third.events, CheckpointKind::Merge));
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
        CheckpointKind::Plan,
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
         phases: [{{ title: 'Build' }}, {{ title: 'Check' }}] }}\n\
         await agent('Add a due date: ' + args.request, \
         {{ role: 'workers', label: 'build:due', phase: 'Build', task: 'due' }})\n\
         return await agent('Is it done?', {{ role: '{role}', label: 'check:due', \
         phase: 'Check', task: 'due' }})\n"
    )
}

#[tokio::test]
async fn a_script_that_throws_when_rehearsed_is_sent_back_with_what_it_threw() -> anyhow::Result<()>
{
    let (mut crew, worker) = crew("maker-checker")?;
    // Maker-checker has workers and reviewers. The first script asks a judge to sign off.
    let (wrong, right) = (
        build_then_sign_off("with-a-judge", "judge"),
        build_then_sign_off("with-a-reviewer", "reviewers"),
    );
    let orchestrator =
        FakeWorker::harness("claude-code/fable", "claude").respond_with(move |spec| {
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
            "Your last script couldn't be used: rehearsed with stand-in answers, it threw: \
             Error: agent(): this workflow's file has no judge step; its roles are workers, \
             reviewers"
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
    assert!(opened(events, CheckpointKind::Merge));
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
        "[[step]]\nname = \"Benchmark\"\nrole = \"workers\"\ndoes = \"Run the benchmarks.\"\n",
    )?;
    let without = build_then_sign_off("without-a-benchmark", "reviewers");
    let with = "export const meta = { name: 'with-a-benchmark', description: 'Add due dates', \
         phases: [{ title: 'Build' }, { title: 'Benchmark' }, { title: 'Check' }] }\n\
         await agent('Add a due date: ' + args.request, \
         { role: 'workers', label: 'build:due', phase: 'Build', task: 'due' })\n\
         await agent('Run the benchmarks.', \
         { role: 'workers', label: 'benchmark:due', phase: 'Benchmark', task: 'due' })\n\
         return await agent('Is it done?', { role: 'reviewers', label: 'check:due', \
         phase: 'Check', task: 'due' })\n"
        .to_owned();
    let orchestrator =
        FakeWorker::harness("claude-code/fable", "claude").respond_with(move |spec| {
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
    let orchestrator = FakeWorker::harness("claude-code/fable", "claude")
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
  phases: [{ title: 'Build' }],
}
await agent('Build the due field: ' + args.request, {
  role: 'workers', label: 'build:due-field', phase: 'Build', task: 'due-field',
})
return await workflow('docs', { request: args.request, crew: args.crew })
"#;

const DOCS: &str = r#"
name  = "docs"
about = "Write the docs for a change."
adapt = false

[[step]]
name = "Write"
role = "workers"
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
        matches!(first.exit, EngineExit::Parked(_)),
        "{:?}",
        first.exit
    );
    assert!(opened(&first.events, CheckpointKind::Merge));
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
    crew.settings.workflows.max_depth = 1;
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
        "role     = \"workers\"\nfor_each = \"task\"\n",
        "role     = \"workers\"\nfor_each = \"task\"\nown_workflow = true\n",
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
