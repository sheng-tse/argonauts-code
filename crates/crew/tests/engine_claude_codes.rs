//! A crew whose orchestrator and workers are on Claude Code runs its workflow as Claude Code's
//! own: one session plans and runs it with its own subagents, asks the
//! reviewers on another CLI through Argonauts Code's tool, and nothing merges unreviewed.

mod support;

use std::sync::Arc;
use std::time::Duration;

use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;

use argonauts_core::asking::{Ask, AskLine, AskReply};
use argonauts_core::engine::EngineExit;
use argonauts_core::event::AgentEvent;
use argonauts_core::events::run::{RunLifecycle, RunMode, RunOutcome, handed_over};
use argonauts_core::events::session::InputPartKind;
use argonauts_core::events::session::{
    CliAgent, CliAgentState, CliPhase, CliWorkflowRun, SessionEvent, SubagentStatus,
};
use argonauts_core::events::task::{ReviewEvent, TaskEvent, TaskState};
use argonauts_core::events::workflow::WorkflowEvent;
use argonauts_core::events::{RunEvent, RunEventKind};
use argonauts_core::session_runner::{Access, RunnerEvent, SessionSpec};
use argonauts_core::settings::crew::{CrewRole, CrewSettings, Orchestrator};
use argonauts_core::settings::workflows::AdaptWith;
use argonauts_core::steps::StepName;
use argonauts_testkit::runner::{FakeRunner, FakeSession};

use support::{Crew, Outcome};

const REQUEST: &str = "Add due dates to tasks";
const MAIN: &str = "claude-code/opus";
const ORCHESTRATOR: &str = "claude-code/fable";
const REVIEWER: &str = "antigravity-cli/gemini";

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

/// Fable orchestrates and Opus works, both on Claude Code; Antigravity CLI reviews. The
/// orchestrator's build session writes `due.rs`, slowly enough to be asked something. Its
/// workflow's agents write in the background, so no edit of theirs reaches the stream.
fn crew(delay: Duration) -> anyhow::Result<(Crew, Arc<FakeRunner>, Arc<FakeRunner>)> {
    let mut crew = Crew::new()?;
    crew.settings.evolution.propose_every = 0;
    let main = FakeRunner::cli(MAIN, "anthropic").respond_with(|_| {
        FakeSession::structured(json!({
            "summary": "One task",
            "tasks": [{"title": "Due dates", "done_when": ["a task has a due date"]}],
        }))
    });
    crew.services.workers.add(main);
    let orchestrator = FakeRunner::cli(ORCHESTRATOR, "anthropic").respond_with(move |spec| {
        if spec.access == Access::Build {
            std::fs::write(spec.workdir.join("due.rs"), "pub struct Due;\n").expect("writes");
        }
        FakeSession::structured(report("Added due dates", "due.rs")).with_delay(delay)
    });
    let orchestrator = crew.services.workers.add(orchestrator);
    let reviewer = FakeRunner::cli(REVIEWER, "google").respond_with(|_| {
        FakeSession::structured(json!({"accept": true, "summary": "Does what was asked"}))
    });
    let reviewer = crew.services.workers.add(reviewer);
    crew.settings.crew = CrewSettings {
        main: MAIN.parse()?,
        orchestrator: Orchestrator::Models(vec![ORCHESTRATOR.parse()?]),
        worker: vec![MAIN.parse()?],
        reviewer: vec![REVIEWER.parse()?],
        judge: Vec::new(),
        worker_workflows: false,
        merger: Vec::new(),
    };
    crew.settings.workflow = "orchestrator".to_owned();
    crew.settings.workflows.adapt_with = AdaptWith::Script;
    Ok((crew, orchestrator, reviewer))
}

fn build_spec(worker: &FakeRunner) -> Option<SessionSpec> {
    worker
        .specs()
        .into_iter()
        .find(|spec| spec.access == Access::Build)
}

/// Asks the reviewers once, as a workflow agent's tool call does once it has written its file.
async fn ask_the_reviewers(orchestrator: Arc<FakeRunner>) -> AskReply {
    let spec = loop {
        if let Some(spec) = build_spec(&orchestrator)
            && spec.workdir.join("due.rs").exists()
        {
            break spec;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    };
    let asking = spec.asking.expect("the build session can ask");
    let mut stream = TcpStream::connect(&asking.endpoint).await.unwrap();
    let line = AskLine {
        token: asking.token.clone(),
        ask: Ask::Role {
            role: CrewRole::Reviewer,
            label: "review:due".into(),
            what: "a due date on each task".into(),
        },
    };
    let text = format!("{}\n", serde_json::to_string(&line).unwrap());
    stream.write_all(text.as_bytes()).await.unwrap();
    let mut answer = String::new();
    BufReader::new(stream).read_line(&mut answer).await.unwrap();
    serde_json::from_str(answer.trim()).unwrap()
}

fn events(outcomes: &[Outcome]) -> &[RunEvent] {
    &outcomes.last().expect("an outcome").events
}

fn merged(outcomes: &[Outcome]) -> bool {
    matches!(
        outcomes.last().map(|outcome| &outcome.exit),
        Some(EngineExit::Finished(RunOutcome::Merged))
    )
}

fn reviews(events: &[RunEvent]) -> usize {
    events
        .iter()
        .filter(|event| {
            matches!(
                &event.kind,
                RunEventKind::Review(ReviewEvent::Reviewed { reviewer, .. })
                    if reviewer.to_string() == REVIEWER
            )
        })
        .count()
}

#[tokio::test]
async fn claude_codes_workflow_asks_the_reviewers_through_argonauts() -> anyhow::Result<()> {
    let (crew, orchestrator, reviewer) = crew(Duration::from_millis(800))?;
    let _env = crew.repo.env_guard();
    let (outcomes, reply) = tokio::join!(
        crew.run_to_end(RunMode::Crew, REQUEST, 4),
        ask_the_reviewers(orchestrator.clone())
    );
    let outcomes = outcomes?;

    assert!(!reply.is_error, "{}", reply.text);
    assert!(reply.text.contains("accepts it"), "{}", reply.text);
    let spec = build_spec(&orchestrator).expect("the orchestrator built it");
    assert!(
        spec.cli_workflow.ultracode,
        "Claude Code's own workflow for this request"
    );
    assert_eq!(spec.asking.as_ref().unwrap().roles, [CrewRole::Reviewer]);
    assert!(
        spec.env
            .iter()
            .any(|(name, value)| name == "CLAUDE_CODE_SUBAGENT_MODEL" && value == "opus")
    );
    let events = events(&outcomes);
    // The reviewers read the final change through the tool, so they aren't asked again.
    assert_eq!(reviewer.specs().len(), 1);
    assert_eq!(reviews(events), 1);
    let said = events.iter().any(|event| {
        matches!(&event.kind, RunEventKind::Workflow(WorkflowEvent::Note(note))
            if note.text.contains("Claude Code's own workflow"))
    });
    assert!(said, "the screen says which engine runs it");
    assert!(merged(&outcomes), "{:?}", outcomes.last().map(|o| &o.exit));
    Ok(())
}

#[tokio::test]
async fn a_change_that_never_went_through_the_tool_is_reviewed_before_it_merges()
-> anyhow::Result<()> {
    let (crew, _orchestrator, reviewer) = crew(Duration::ZERO)?;
    let _env = crew.repo.env_guard();
    let outcomes = crew.run_to_end(RunMode::Crew, REQUEST, 4).await?;
    let asked = reviewer.specs();
    assert_eq!(asked.len(), 1, "Argonauts Code asks the reviewers itself");
    assert!(asked[0].step != Some(StepName::Build));
    assert_eq!(reviews(events(&outcomes)), 1);
    assert!(merged(&outcomes));
    Ok(())
}

/// The checks run on the change before each review is asked, and the reviewers read their
/// results; the label comes from the task's name.
#[tokio::test]
async fn the_reviewers_get_the_checks_run_just_before_they_are_asked() -> anyhow::Result<()> {
    let (mut crew, _orchestrator, reviewer) = crew(Duration::ZERO)?;
    crew.settings.checks.run = vec![argonauts_core::settings::protocol::CheckCommand {
        name: "tests".into(),
        command: vec![
            "python3".into(),
            "-m".into(),
            "unittest".into(),
            "-q".into(),
        ],
    }];
    let _env = crew.repo.env_guard();
    let outcomes = crew.run_to_end(RunMode::Crew, REQUEST, 4).await?;

    let asked = reviewer.specs();
    assert_eq!(asked.len(), 1);
    let checks = asked[0]
        .prompt
        .iter()
        .find(|part| part.label == "the checks")
        .expect("the reviewer reads the checks");
    assert_eq!(checks.kind, InputPartKind::CheckOutput);
    assert_eq!(checks.text.trim(), "tests (python3 -m unittest -q) passed");
    let label = reviewer_labels(events(&outcomes));
    assert_eq!(label, ["review:add-due-dates"]);
    assert!(merged(&outcomes));
    Ok(())
}

/// A request the main session handed over opens with your words, which may say how to run it.
/// The task and its agents' labels are named for the work, from the session's account of it,
/// as the run is.
#[tokio::test]
async fn a_handed_over_requests_task_is_named_for_the_work() -> anyhow::Result<()> {
    let (crew, _orchestrator, reviewer) = crew(Duration::ZERO)?;
    let _env = crew.repo.env_guard();
    let request = handed_over(
        "Use a workflow for this, with agy as the reviewer.",
        REQUEST,
    );
    let outcomes = crew.run_to_end(RunMode::Crew, &request, 4).await?;
    assert_eq!(reviewer.specs().len(), 1);
    let label = reviewer_labels(events(&outcomes));
    assert_eq!(label, ["review:add-due-dates"]);
    assert!(merged(&outcomes));
    Ok(())
}

/// Checks asked for while the workflow's agents still work run on a clean copy of the change,
/// never in the folder the agents are editing, and so does every later run of them.
#[tokio::test]
async fn the_checks_before_an_ask_run_on_a_clean_copy() -> anyhow::Result<()> {
    let (mut crew, orchestrator, _reviewer) = crew(Duration::from_millis(800))?;
    crew.settings.checks.run = vec![argonauts_core::settings::protocol::CheckCommand {
        name: "tests".into(),
        command: vec!["python3".into(), "-m".into(), "unittest".into()],
    }];
    let _env = crew.repo.env_guard();
    let (outcomes, reply) = tokio::join!(
        crew.run_to_end(RunMode::Crew, REQUEST, 4),
        ask_the_reviewers(orchestrator.clone())
    );
    let outcomes = outcomes?;

    assert!(!reply.is_error, "{}", reply.text);
    let workdirs: Vec<String> = crew
        .services
        .checks
        .requests()
        .iter()
        .map(|request| request.workdir.to_string_lossy().into_owned())
        .collect();
    let task_folder = build_spec(&orchestrator).expect("built").workdir;
    assert!(!workdirs.is_empty());
    assert!(
        !workdirs
            .iter()
            .any(|dir| std::path::Path::new(dir) == task_folder),
        "no check ran in the agents' folder: {workdirs:?}"
    );
    let before_the_merge: Vec<&String> = workdirs
        .iter()
        .filter(|dir| !dir.ends_with("merge"))
        .collect();
    assert!(
        before_the_merge.iter().all(|dir| dir.ends_with("-checks")),
        "{workdirs:?}"
    );
    assert!(merged(&outcomes), "{:?}", outcomes.last().map(|o| &o.exit));
    Ok(())
}

/// A final change the reviewers don't accept goes back to the session with their reasons for
/// a round of fixes, and is asked about again, before anything comes to you.
#[tokio::test]
async fn a_rejected_final_change_gets_its_rounds_of_fixes_first() -> anyhow::Result<()> {
    let (crew, orchestrator, reviewer) = crew(Duration::ZERO)?;
    let builds = Arc::new(std::sync::atomic::AtomicU32::new(0));
    let counted = builds.clone();
    orchestrator.respond(move |spec| {
        if spec.access == Access::Build {
            let round = counted.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let text = format!("pub struct Due;\n// round {round}\n");
            std::fs::write(spec.workdir.join("due.rs"), text).expect("writes");
        }
        FakeSession::structured(report("Added due dates", "due.rs"))
    });
    let reviews_given = Arc::new(std::sync::atomic::AtomicU32::new(0));
    let given = reviews_given.clone();
    reviewer.respond(move |_| {
        let first = given.fetch_add(1, std::sync::atomic::Ordering::SeqCst) == 0;
        FakeSession::structured(json!({
            "accept": !first,
            "summary": if first { "Past dates pass" } else { "Does what was asked" },
            "findings": if first { json!(["a past due date is accepted"]) } else { json!([]) },
        }))
    });
    let _env = crew.repo.env_guard();
    let outcomes = crew.run_to_end(RunMode::Crew, REQUEST, 4).await?;

    let resumed = orchestrator
        .specs()
        .into_iter()
        .filter(|spec| spec.access == Access::Build)
        .nth(1)
        .expect("the session went again");
    let told = resumed.prompt.iter().any(|part| {
        part.text.contains("Round 1 of fixes") && part.text.contains("a past due date is accepted")
    });
    assert!(told, "the session reads the reviewers' reasons");
    assert_eq!(
        reviewer_labels(events(&outcomes)),
        ["review:add-due-dates", "review:add-due-dates · round 2"]
    );
    assert!(merged(&outcomes), "{:?}", outcomes.last().map(|o| &o.exit));
    Ok(())
}

/// With no rounds left, the change comes to you with the reviewers' own reasons.
#[tokio::test]
async fn with_no_rounds_left_it_comes_to_you_with_the_reviewers_reasons() -> anyhow::Result<()> {
    let (mut crew, _orchestrator, reviewer) = crew(Duration::ZERO)?;
    crew.settings.review.max_change_rounds = 0;
    reviewer.respond(|_| {
        FakeSession::structured(json!({
            "accept": false,
            "summary": "Past dates pass",
            "findings": ["a past due date is accepted"],
        }))
    });
    let _env = crew.repo.env_guard();
    let outcomes = crew.run_to_end(RunMode::Crew, REQUEST, 4).await?;

    let needs_you = events(&outcomes)
        .iter()
        .find_map(|event| match &event.kind {
            RunEventKind::Task(TaskEvent::State { to, reason, .. })
                if *to == TaskState::NeedsYou =>
            {
                Some(reason.clone())
            }
            _ => None,
        });
    let reason = needs_you.expect("it came to you");
    assert!(
        reason.contains("asks for changes: Past dates pass"),
        "{reason}"
    );
    assert!(!merged(&outcomes));
    Ok(())
}

fn reviewer_labels(events: &[RunEvent]) -> Vec<String> {
    events
        .iter()
        .filter_map(|event| match &event.kind {
            RunEventKind::Session(SessionEvent::Started(started))
                if started.worker.to_string() == REVIEWER =>
            {
                started.label.clone()
            }
            _ => None,
        })
        .collect()
}

/// Claude Code's workflow as its stream reports it: three phases and the one task's builder.
fn reported() -> CliWorkflowRun {
    let phase = |index: u32, title: &str| CliPhase {
        index,
        title: title.to_owned(),
        detail: String::new(),
    };
    CliWorkflowRun {
        id: "wf-1".to_owned(),
        call: None,
        name: "due-dates".to_owned(),
        description: "Add due dates".to_owned(),
        phases: vec![phase(1, "Plan"), phase(2, "Build"), phase(3, "Review")],
        agents: vec![CliAgent {
            index: 1,
            label: "build:due".to_owned(),
            phase: Some(2),
            model: None,
            state: CliAgentState::Done,
            tokens: 0,
            tool_calls: 0,
            duration_ms: 0,
            started_at_ms: None,
            try_number: 1,
            prompt: None,
            result: None,
            ..CliAgent::default()
        }],
        status: SubagentStatus::Done,
        tokens: 0,
        tool_calls: 0,
        duration_ms: 0,
        summary: None,
        started_at_ms: None,
    }
}

/// A step `always.toml` requires goes with the request. Argonauts Code can't change Claude Code's
/// workflow, so what it reported is checked, and a step it left out is said.
#[tokio::test]
async fn a_required_step_claude_codes_workflow_left_out_is_said() -> anyhow::Result<()> {
    let (crew, orchestrator, _reviewer) = crew(Duration::ZERO)?;
    crew.repo.write(
        ".argonauts/workflows/always.toml",
        "[[step]]\nname = \"Benchmark\"\nrole = \"worker\"\ndoes = \"Run the benchmarks.\"\n",
    )?;
    crew.repo
        .commit_all("the steps every workflow must contain")?;
    orchestrator.respond(|spec| {
        if spec.access == Access::Build {
            std::fs::write(spec.workdir.join("due.rs"), "pub struct Due;\n").expect("writes");
        }
        FakeSession::structured(report("Added due dates", "due.rs")).with_event(RunnerEvent::Agent(
            AgentEvent::Workflow {
                workflow: Box::new(reported()),
            },
        ))
    });
    let _env = crew.repo.env_guard();
    let outcomes = crew.run_to_end(RunMode::Crew, REQUEST, 4).await?;

    let spec = build_spec(&orchestrator).expect("the orchestrator built it");
    let told = spec.prompt.iter().any(|part| {
        part.text
            .contains("Your workflow must have a phase for each of these steps")
            && part
                .text
                .contains("- Benchmark, before the sign-off, by the workers")
    });
    assert!(told, "the request carries the step");
    let said = events(&outcomes).iter().any(|event| {
        matches!(&event.kind, RunEventKind::Workflow(WorkflowEvent::Note(note))
            if note.text == "It had no step for \"Benchmark\", which you require.")
    });
    assert!(said, "what it lacked is said");
    assert!(
        merged(&outcomes),
        "it still merges what the reviewers accepted"
    );
    let left = events(&outcomes)
        .iter()
        .find_map(|event| match &event.kind {
            RunEventKind::Run(RunLifecycle::ClosingAnswer(answer)) => Some(answer.left.clone()),
            _ => None,
        });
    assert_eq!(
        left.as_deref()
            .and_then(<[String]>::first)
            .map(String::as_str),
        Some("It had no step for \"Benchmark\", which you require."),
        "the closing answer lists it first among what's left"
    );
    Ok(())
}

/// Every new file the workflow's agents leave is the task's work, listed in the report or not.
#[tokio::test]
async fn a_file_the_workflow_made_lands_though_the_report_leaves_it_out() -> anyhow::Result<()> {
    let (crew, orchestrator, reviewer) = crew(Duration::ZERO)?;
    orchestrator.respond(|spec| {
        if spec.access == Access::Build {
            std::fs::write(spec.workdir.join("due.rs"), "pub struct Due;\n").expect("writes");
            let test = "#[test]\nfn a_task_has_a_due_date() {}\n";
            std::fs::write(spec.workdir.join("due_test.rs"), test).expect("writes");
        }
        FakeSession::structured(report("Added due dates", "due.rs"))
    });
    let _env = crew.repo.env_guard();
    let outcomes = crew.run_to_end(RunMode::Crew, REQUEST, 4).await?;

    let change = reviewer.specs()[0]
        .prompt
        .iter()
        .find(|part| part.label == "the change")
        .map(|part| part.text.clone())
        .expect("the reviewer reads the change");
    assert!(change.contains("due_test.rs"), "{change}");
    assert!(merged(&outcomes), "{:?}", outcomes.last().map(|o| &o.exit));
    let merged = crew.repo.git(&["show", "HEAD:due_test.rs"])?;
    assert!(merged.contains("a_task_has_a_due_date"), "{merged}");
    Ok(())
}

/// The reviewers read the change its commit will hold: bytecode a command left in the task's
/// folder is no part of it.
#[tokio::test]
async fn the_reviewers_read_the_change_without_what_a_command_left_behind() -> anyhow::Result<()> {
    let (crew, orchestrator, reviewer) = crew(Duration::from_millis(400))?;
    let _env = crew.repo.env_guard();
    let leave_bytecode = async {
        let spec = loop {
            if let Some(spec) = build_spec(&orchestrator) {
                break spec;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        };
        let cache = spec.workdir.join("__pycache__");
        std::fs::create_dir_all(&cache).expect("a cache folder");
        std::fs::write(cache.join("due.cpython-312.pyc"), b"\x00\x0dbytecode\n").expect("writes");
    };
    let (outcomes, ()) = tokio::join!(crew.run_to_end(RunMode::Crew, REQUEST, 4), leave_bytecode);
    let outcomes = outcomes?;

    let asked = reviewer.specs();
    assert_eq!(asked.len(), 1);
    let change = asked[0]
        .prompt
        .iter()
        .find(|part| part.label == "the change")
        .expect("the reviewer reads the change");
    assert!(change.text.contains("due.rs"), "{}", change.text);
    assert!(!change.text.contains("__pycache__"), "{}", change.text);
    assert!(merged(&outcomes));
    Ok(())
}
