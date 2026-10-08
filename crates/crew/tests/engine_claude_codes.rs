//! A crew whose orchestrator and workers are on Claude Code runs its workflow as Claude Code's
//! own: one session plans and runs it with its own subagents, asks the
//! reviewers on another CLI through Argonauts' tool, and nothing merges unreviewed.

mod support;

use std::sync::Arc;
use std::time::Duration;

use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;

use argonauts_core::asking::{Ask, AskLine, AskReply};
use argonauts_core::engine::EngineExit;
use argonauts_core::event::AgentEvent;
use argonauts_core::events::card::ReviewEvent;
use argonauts_core::events::run::{RunLifecycle, RunMode, RunOutcome};
use argonauts_core::events::session::{
    SubagentStatus, VendorAgent, VendorAgentState, VendorPhase, VendorWorkflow,
};
use argonauts_core::events::workflow::WorkflowEvent;
use argonauts_core::events::{RunEvent, RunEventKind};
use argonauts_core::settings::crew::{CrewRole, CrewSettings, Orchestrator};
use argonauts_core::settings::workflows::AdaptWith;
use argonauts_core::steps::StepName;
use argonauts_core::worker::{Access, SessionSpec, WorkerEvent};
use argonauts_testkit::worker::{FakeSession, FakeWorker};

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
/// orchestrator's build session writes `due.rs`, slowly enough to be asked something.
fn crew(delay: Duration) -> anyhow::Result<(Crew, Arc<FakeWorker>, Arc<FakeWorker>)> {
    let mut crew = Crew::new()?;
    crew.settings.evolution.propose_every = 0;
    let main = FakeWorker::harness(MAIN, "claude").respond_with(|_| {
        FakeSession::structured(json!({
            "summary": "One task",
            "tasks": [{"title": "Due dates", "done_when": ["a task has a due date"]}],
        }))
    });
    crew.services.workers.add(main);
    let orchestrator = FakeWorker::harness(ORCHESTRATOR, "claude").respond_with(move |spec| {
        if spec.access == Access::Build {
            std::fs::write(spec.workdir.join("due.rs"), "pub struct Due;\n").expect("writes");
        }
        FakeSession::structured(report("Added due dates", "due.rs")).with_delay(delay)
    });
    let orchestrator = crew.services.workers.add(orchestrator);
    let reviewer = FakeWorker::harness(REVIEWER, "gemini").respond_with(|_| {
        FakeSession::structured(json!({"accept": true, "summary": "Does what was asked"}))
    });
    let reviewer = crew.services.workers.add(reviewer);
    crew.settings.main_agent = MAIN.parse()?;
    crew.settings.crew = CrewSettings {
        orchestrator: Orchestrator::Models(vec![ORCHESTRATOR.parse()?]),
        workers: vec![MAIN.parse()?],
        reviewers: vec![REVIEWER.parse()?],
        judge: Vec::new(),
    };
    crew.settings.workflow = "orchestrator".to_owned();
    crew.settings.workflows.adapt_with = AdaptWith::Script;
    Ok((crew, orchestrator, reviewer))
}

fn build_spec(worker: &FakeWorker) -> Option<SessionSpec> {
    worker
        .specs()
        .into_iter()
        .find(|spec| spec.access == Access::Build)
}

/// Asks the reviewers as a workflow agent's tool call does, once the build session started.
async fn ask_the_reviewers(orchestrator: Arc<FakeWorker>) -> AskReply {
    let spec = loop {
        if let Some(spec) = build_spec(&orchestrator) {
            break spec;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    };
    let asking = spec.asking.expect("the build session can ask");
    let mut stream = TcpStream::connect(&asking.endpoint).await.unwrap();
    let line = AskLine {
        token: asking.token,
        ask: Ask::Role {
            role: CrewRole::Reviewers,
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
    assert_eq!(spec.asking.as_ref().unwrap().roles, [CrewRole::Reviewers]);
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
    assert_eq!(asked.len(), 1, "Argonauts asks the reviewers itself");
    assert!(asked[0].step != Some(StepName::Build));
    assert_eq!(reviews(events(&outcomes)), 1);
    assert!(merged(&outcomes));
    Ok(())
}

/// Claude Code's workflow as its stream reports it: three phases and the one task's builder.
fn reported() -> VendorWorkflow {
    let phase = |index: u32, title: &str| VendorPhase {
        index,
        title: title.to_owned(),
        detail: String::new(),
    };
    VendorWorkflow {
        id: "wf-1".to_owned(),
        call: None,
        name: "due-dates".to_owned(),
        description: "Add due dates".to_owned(),
        phases: vec![phase(1, "Plan"), phase(2, "Build"), phase(3, "Review")],
        agents: vec![VendorAgent {
            index: 1,
            label: "build:due".to_owned(),
            phase: Some(2),
            model: None,
            state: VendorAgentState::Done,
            tokens: 0,
            tool_calls: 0,
            duration_ms: 0,
            started_at_ms: None,
            attempt: 1,
            prompt: None,
            result: None,
        }],
        status: SubagentStatus::Done,
        tokens: 0,
        tool_calls: 0,
        duration_ms: 0,
        summary: None,
        started_at_ms: None,
    }
}

/// A step `always.toml` requires goes with the request. Argonauts can't change Claude Code's
/// workflow, so what it reported is checked, and a step it left out is said.
#[tokio::test]
async fn a_required_step_claude_codes_workflow_left_out_is_said() -> anyhow::Result<()> {
    let (crew, orchestrator, _reviewer) = crew(Duration::ZERO)?;
    crew.repo.write(
        ".argonauts/workflows/always.toml",
        "[[step]]\nname = \"Benchmark\"\nrole = \"workers\"\ndoes = \"Run the benchmarks.\"\n",
    )?;
    crew.repo
        .commit_all("the steps every workflow must contain")?;
    orchestrator.respond(|spec| {
        if spec.access == Access::Build {
            std::fs::write(spec.workdir.join("due.rs"), "pub struct Due;\n").expect("writes");
        }
        FakeSession::structured(report("Added due dates", "due.rs")).with_event(WorkerEvent::Agent(
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
