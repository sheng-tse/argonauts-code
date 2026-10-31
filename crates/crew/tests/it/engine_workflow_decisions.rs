//! How the run's workflow is chosen and how its decisions hold: the main agent's design at the
//! start, decisions that replay after the engine stopped, loops that run out of rounds, workflow
//! files that replace built-ins, and nested workflows that keep their id across a change.

use crate::support;
use std::collections::VecDeque;
use std::sync::{Arc, Mutex, PoisonError};

use serde_json::{Value, json};

use argonauts_core::TaskId;
use argonauts_core::agents::session_runner::SessionRunner;
use argonauts_core::base::log::NoteLevel;
use argonauts_core::base::names::{ApprovalId, ModelRef, WorkflowId};
use argonauts_core::runs::engine::{EngineCommand, EngineExit};
use argonauts_core::runs::events::approval::{
    AnswerAction, ApprovalDecided, ApprovalEvent, ApprovalKind, ApprovalOpened, ApprovalPacket,
    FreeText, QuestionPacket,
};
use argonauts_core::runs::events::run::{Party, RunLifecycle, RunMode, RunOutcome};
use argonauts_core::runs::events::session::{EndReason, SessionEvent, SessionStarted};
use argonauts_core::runs::events::task::MergeEvent;
use argonauts_core::runs::events::workflow::{
    StepEnd, StepFinished, StepStarted, WorkflowEvent, WorkflowStarted,
};
use argonauts_core::runs::events::{RunEvent, RunEventKind};
use argonauts_core::settings::approvals::{ApprovalMode, DecideWith};
use argonauts_core::settings::asking::WhenDeclined;
use argonauts_core::settings::protocol::UnderstandWhen;
use argonauts_core::settings::workflows::OrchestratorDesigns;
use argonauts_core::workflows::steps::StepName;
use argonauts_core::workflows::workflow::{
    StandardPhase, StepKind, StepRef, Workflow, WorkflowRef,
};
use argonauts_crew::workflow::yaml;
use argonauts_testkit::engine::runner::{FakeRunner, FakeSession};

use support::{Crew, Outcome};

fn parse(text: &str) -> Workflow {
    yaml::parse(text).unwrap_or_else(|err| panic!("{err}\n{text}"))
}

const ONE_LOOK: &str = "name: one-look\ndescription: One look, when a condition holds\nparams:\n  check: understand_first\nphases:\n  - name: Look\n    standard: check\n    steps:\n      - kind: checks\n        when: \"{{check}}\"\n";

const LOOK_THEN_LOOK: &str = "name: look-then-look\ndescription: Two looks\nphases:\n  - name: First look\n    standard: check\n    steps:\n      - kind: checks\n  - name: Second look\n    standard: check\n    steps:\n      - kind: checks\n";

fn workflow_events(events: &[RunEvent]) -> Vec<&WorkflowEvent> {
    events
        .iter()
        .filter_map(|event| match &event.kind {
            RunEventKind::Workflow(workflow) => Some(workflow),
            _ => None,
        })
        .collect()
}

fn workflows_started(events: &[RunEvent]) -> Vec<&WorkflowStarted> {
    workflow_events(events)
        .into_iter()
        .filter_map(|event| match event {
            WorkflowEvent::Started(started) => Some(&**started),
            _ => None,
        })
        .collect()
}

fn steps_started(events: &[RunEvent]) -> Vec<&StepStarted> {
    workflow_events(events)
        .into_iter()
        .filter_map(|event| match event {
            WorkflowEvent::StepStarted(started) => Some(started),
            _ => None,
        })
        .collect()
}

fn designs(worker: &FakeRunner) -> usize {
    worker
        .specs()
        .iter()
        .filter(|spec| spec.step == Some(StepName::DesignWorkflow))
        .count()
}

fn notes(events: &[RunEvent], level: NoteLevel) -> Vec<String> {
    events
        .iter()
        .filter_map(|event| match &event.kind {
            RunEventKind::Note(note) if note.level == level => Some(note.text.clone()),
            _ => None,
        })
        .collect()
}

fn opened(events: &[RunEvent], kind: ApprovalKind) -> Vec<&ApprovalOpened> {
    events
        .iter()
        .filter_map(|event| match &event.kind {
            RunEventKind::Approval(ApprovalEvent::Opened(opened)) if opened.kind == kind => {
                Some(&**opened)
            }
            _ => None,
        })
        .collect()
}

fn finished_with(events: &[RunEvent]) -> Option<(RunOutcome, Option<String>)> {
    events.iter().find_map(|event| match &event.kind {
        RunEventKind::Run(RunLifecycle::Finished { outcome, detail }) => {
            Some((*outcome, detail.clone()))
        }
        _ => None,
    })
}

/// The run finished with `outcome`, else the failure says why.
fn ended(outcome: &Outcome, expected: RunOutcome) {
    assert_eq!(
        outcome.exit,
        EngineExit::Finished(expected),
        "{:?}",
        finished_with(&outcome.events)
    );
}

/// The main agent's brief: an example by name with its params, or a workflow of its own.
fn design(template: Option<&str>, params: Value, composed: Option<&str>) -> Value {
    support::brief(template, params, composed, "it fits")
}

/// A worker that gives `designs` to the main agent's `DesignWorkflow` asks in order, `answers` to
/// the steps they name, and the smallest valid answer to everything else, such as the run review.
fn responder(
    crew: &mut Crew,
    worker: &str,
    provider: &str,
    designs: Vec<Value>,
    answers: Vec<(StepName, Value)>,
) -> Arc<FakeRunner> {
    let designs = designs.into_iter().map(FakeSession::structured).collect();
    session_responder(crew, worker, provider, designs, answers)
}

/// The same worker, whose design sessions end as `designs` say, such as in a crash.
fn session_responder(
    crew: &mut Crew,
    worker: &str,
    provider: &str,
    designs: Vec<FakeSession>,
    answers: Vec<(StepName, Value)>,
) -> Arc<FakeRunner> {
    let designs = Mutex::new(VecDeque::from(designs));
    let fake = FakeRunner::cli(worker, provider).respond_with(move |spec| {
        if spec.step == Some(StepName::DesignWorkflow) {
            let next = designs
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .pop_front();
            if let Some(design) = next {
                return design;
            }
        }
        if let Some((_, answer)) = answers.iter().find(|(step, _)| spec.step == Some(*step)) {
            return FakeSession::structured(answer.clone());
        }
        match &spec.json_schema {
            Some(schema) => FakeSession::structured(support::fill(schema)),
            None => FakeSession::completed("done"),
        }
    });
    let fake = crew.services.workers.add(fake);
    let reference = fake.model_ref().clone();
    if crew.settings.crew.is_empty() {
        crew.settings.crew.main = reference.clone();
    }
    crew.settings.crew.add(reference);
    fake
}

/// A crew run whose main agent is the one worker, designing the workflow at the start. Understand
/// is the main agent's to add, so the workflows here run as the main agent wrote them.
fn designing_crew(answers: Vec<Value>) -> anyhow::Result<(Crew, Arc<FakeRunner>)> {
    designing_crew_with(answers.into_iter().map(FakeSession::structured).collect())
}

/// The same crew, whose main agent's design sessions end as `sessions` say, such as in a crash.
fn designing_crew_with(sessions: Vec<FakeSession>) -> anyhow::Result<(Crew, Arc<FakeRunner>)> {
    let mut crew = Crew::new()?;
    let main_agent = session_responder(
        &mut crew,
        "claude-code/opus",
        "anthropic",
        sessions,
        Vec::new(),
    );
    crew.settings.workflow = "look-then-look".to_owned();
    crew.settings.workflows.orchestrator_designs = OrchestratorDesigns::ANY;
    crew.settings.workflows.adapt_at_phase_ends = false;
    crew.settings.understand.when = UnderstandWhen::Main;
    crew.repo
        .write(".argonauts/workflows/one-look.yaml", ONE_LOOK)?;
    crew.repo
        .write(".argonauts/workflows/look-then-look.yaml", LOOK_THEN_LOOK)?;
    Ok((crew, main_agent))
}

#[tokio::test]
async fn main_agent_picks_a_template_with_params() -> anyhow::Result<()> {
    let params = json!([{"name": "check", "value": "understood"}]);
    let (crew, main_agent) = designing_crew(vec![design(Some("one-look"), params, None)])?;
    let outcome = crew.start(RunMode::Crew, "look once").await?;
    let started = workflows_started(&outcome.events);
    assert_eq!(started.len(), 1, "{started:?}");
    assert_eq!(started[0].from, Some(WorkflowRef::new("one-look")));
    let step = &started[0].workflow.phases[0].steps[0];
    assert_eq!(step.when.as_deref(), Some("understood"));
    assert_eq!(designs(&main_agent), 1);
    // Your own files are among the examples the main agent picks from.
    let specs = main_agent.specs();
    let asked = specs
        .iter()
        .find(|spec| spec.step == Some(StepName::DesignWorkflow))
        .unwrap_or_else(|| panic!("no DesignWorkflow session"));
    assert!(
        asked
            .prompt
            .iter()
            .any(|part| part.text.contains("one-look"))
    );
    assert!(
        asked
            .prompt
            .iter()
            .any(|part| part.text.contains("council"))
    );
    Ok(())
}

#[tokio::test]
async fn main_agent_composes_a_workflow_that_runs() -> anyhow::Result<()> {
    let composed = "name: composed-look\ndescription: A look\nphases:\n  - name: Look\n    detail: the checks, once\n    standard: check\n    steps:\n      - kind: checks\n        label: \"look:checks\"\n";
    // Without a detail on its phase, the brief goes back to the main agent with the error.
    let bare = composed.replace("    detail: the checks, once\n", "");
    let (crew, main_agent) = designing_crew(vec![
        design(None, json!([]), Some(&bare)),
        design(None, json!([]), Some(composed)),
    ])?;
    let outcome = crew.start(RunMode::Crew, "look once").await?;
    let started = workflows_started(&outcome.events);
    assert_eq!(started.len(), 1);
    assert_eq!(started[0].from, None);
    // The brief names the run; the main agent's file gives the phases.
    assert_eq!(started[0].workflow.phases, parse(composed).phases);
    assert_eq!(started[0].workflow.name, "look-at-it");
    assert_eq!(started[0].workflow.description, "Look at the repository");
    assert_eq!(designs(&main_agent), 2);
    let retried = main_agent
        .specs()
        .into_iter()
        .filter(|spec| spec.step == Some(StepName::DesignWorkflow))
        .nth(1)
        .unwrap_or_else(|| panic!("no second design"));
    assert!(
        retried
            .prompt
            .iter()
            .any(|part| part.text.contains("phase 1 (Look) has no `detail`")),
        "the error goes back to the main agent"
    );
    assert!(
        workflow_events(&outcome.events)
            .iter()
            .any(|event| matches!(
                event,
                WorkflowEvent::StepFinished(finished) if finished.kind == StepKind::Checks
            ))
    );
    ended(&outcome, RunOutcome::BranchLeft);
    Ok(())
}

#[tokio::test]
async fn two_invalid_designs_fall_back_with_a_note() -> anyhow::Result<()> {
    let wrong = design(Some("no-such-workflow"), json!([]), None);
    let (crew, main_agent) = designing_crew(vec![wrong.clone(), wrong])?;
    let outcome = crew.start(RunMode::Crew, "look twice").await?;
    assert_eq!(designs(&main_agent), 2);
    let started = workflows_started(&outcome.events);
    assert_eq!(started[0].from, Some(WorkflowRef::new("look-then-look")));
    let warned = notes(&outcome.events, NoteLevel::Warn);
    assert!(
        warned
            .iter()
            .any(|text| text.contains("broke the rules twice") && text.contains("no-such-workflow")),
        "{warned:?}"
    );
    Ok(())
}

/// Haiku's first brief for a council run on 10-04, as its session's result held it. Code
/// refused it for its name, `rate-limiter-per-api-key`, in five words, with no note, and a new
/// session started over and read the repository again, so the run's workflow began at 2m06s.
const LOGGED_BRIEF: &str = r#"{"based_on":"council","description":"Make rate limiter per API key with Retry-After headers and tests.","done_when":[{"id":"G1","short":"Per-API-key buckets","text":"Rate limiter creates separate buckets for each X-Api-Key value.","check":"review"},{"id":"G2","short":"Shared bucket for no header","text":"Requests without the X-Api-Key header share a single rate limit bucket.","check":"review"},{"id":"G3","short":"Capped refills","text":"Refills are capped at capacity, making the previously failing test pass.","check":{"test":"test passes"}},{"id":"G4","short":"Retry-After header","text":"429 responses include a Retry-After header with whole seconds remaining.","check":"review"},{"id":"G5","short":"New tests","text":"Tests cover per-API-key buckets, shared bucket behavior, and Retry-After headers.","check":{"test":"test passes"}}],"first_try":{"name":"Rate Limiter","detail":"Worker implements per-API-key rate limiting with Retry-After headers and tests","steps":"- kind: build\n  label: build:rate-limiter\n  role: worker\n  input: plan"},"name":"rate-limiter-per-api-key","params":[],"plan":[],"questions":[],"why":"Standard council without modifications; multiple perspectives help with implementation decisions.","workflow":null}"#;

fn design_sessions(
    spec_of: &FakeRunner,
) -> Vec<argonauts_core::agents::session_runner::SessionSpec> {
    spec_of
        .specs()
        .into_iter()
        .filter(|spec| spec.step == Some(StepName::DesignWorkflow))
        .collect()
}

fn design_starts(events: &[RunEvent]) -> Vec<&SessionStarted> {
    events
        .iter()
        .filter_map(|event| match &event.kind {
            RunEventKind::Session(SessionEvent::Started(started))
                if started.step == Some(StepName::DesignWorkflow) =>
            {
                Some(&**started)
            }
            _ => None,
        })
        .collect()
}

/// The brief from the log, its name read without its filler, goes back once to the session that
/// wrote it, since G3 and G5 named their tests `test passes`. That session names the checks,
/// and the run checks what it named.
#[tokio::test]
async fn the_logged_brief_runs_once_its_session_names_its_checks() -> anyhow::Result<()> {
    let logged: Value = serde_json::from_str(LOGGED_BRIEF)?;
    let mut fixed = logged.clone();
    fixed["done_when"][2]["check"] = json!({"test": "test_never_holds_more_than_capacity"});
    fixed["done_when"][4]["check"] = json!({"command": "python -m pytest tests/"});
    let (crew, main_agent) = designing_crew(vec![logged, fixed])?;
    // council as a file of yours, so the run stays short.
    crew.repo.write(
        ".argonauts/workflows/council.yaml",
        &ONE_LOOK.replace("one-look", "council"),
    )?;
    let outcome = crew.start(RunMode::Crew, "rate limit per API key").await?;
    let asked = design_sessions(&main_agent);
    assert_eq!(asked.len(), 2);
    assert_eq!(asked[1].resume.as_deref(), Some("fake-session"));
    let told: String = asked[1]
        .prompt
        .iter()
        .map(|part| part.text.as_str())
        .collect();
    assert!(
        told.contains("criterion G3: `test: test passes` isn't a test's name"),
        "{told}"
    );
    assert!(told.contains("criterion G5:"), "{told}");
    assert!(!told.contains("words; use two to four"), "{told}");
    let said = notes(&outcome.events, NoteLevel::Info);
    let why = "The main agent's brief gave a check a test name that isn't one; it's fixing that.";
    assert!(
        said.iter().any(|text| text.lines().next() == Some(why)),
        "{said:?}"
    );
    let started = workflows_started(&outcome.events);
    assert_eq!(
        started.len(),
        1,
        "{:?}",
        notes(&outcome.events, NoteLevel::Warn)
    );
    assert_eq!(started[0].from, Some(WorkflowRef::new("council")));
    assert_eq!(started[0].workflow.name, "rate-limiter-api-key");
    let checks: Vec<String> = outcome
        .events
        .iter()
        .rev()
        .find_map(|event| match &event.kind {
            RunEventKind::Run(RunLifecycle::Named(named)) => Some(&named.done_when),
            _ => None,
        })
        .into_iter()
        .flatten()
        .map(|criterion| format!("{}: {:?}", criterion.id, criterion.check))
        .collect();
    assert_eq!(
        checks,
        [
            "G1: Review",
            "G2: Review",
            "G3: Test(\"test_never_holds_more_than_capacity\")",
            "G4: Review",
            "G5: Command(\"python -m pytest tests/\")",
        ]
    );
    Ok(())
}

/// Your own workflow files go by their file names, so a main agent that names `quick_fix` or
/// `Hotfix` exactly runs it from its one session, and another spelling of only that name reads
/// as it.
#[tokio::test]
async fn your_workflow_files_run_by_their_own_names() -> anyhow::Result<()> {
    for (file, written) in [
        ("quick_fix", "quick_fix"),
        ("Hotfix", "Hotfix"),
        ("quick_fix", "Quick Fix"),
    ] {
        let (crew, main_agent) = designing_crew(vec![design(Some(written), json!([]), None)])?;
        crew.repo
            .write(&format!(".argonauts/workflows/{file}.yaml"), ONE_LOOK)?;
        let outcome = crew.start(RunMode::Crew, "look once").await?;
        assert_eq!(designs(&main_agent), 1, "{written}");
        let started = workflows_started(&outcome.events);
        assert_eq!(
            started.first().and_then(|started| started.from.clone()),
            Some(WorkflowRef::new(file)),
            "{written}: {:?}",
            notes(&outcome.events, NoteLevel::Warn)
        );
    }
    Ok(())
}

/// A brief code refuses goes back to the session that wrote it, resumed with only what was
/// refused, and a `›` note says why in plain words, the errors behind it for Ctrl+O.
#[tokio::test]
async fn a_refused_brief_goes_back_to_the_same_session() -> anyhow::Result<()> {
    let mut long = design(Some("one-look"), json!([]), None);
    long["name"] = json!("split-parse-render-cache-layers");
    let (crew, main_agent) = designing_crew(vec![long, design(Some("one-look"), json!([]), None)])?;
    let outcome = crew.start(RunMode::Crew, "look once").await?;
    let asked = design_sessions(&main_agent);
    assert_eq!(asked.len(), 2);
    assert_eq!(asked[0].resume, None);
    assert_eq!(asked[1].resume.as_deref(), Some("fake-session"));
    let told: String = asked[1]
        .prompt
        .iter()
        .map(|part| part.text.as_str())
        .collect();
    assert!(told.contains("has 5 words; use two to four"), "{told}");
    assert!(
        told.contains("Code checked the brief you just wrote"),
        "{told}"
    );
    assert!(!told.contains("## Example workflows"), "{told}");
    // The same main agent, by its row's name, carries on.
    let starts = design_starts(&outcome.events);
    assert_eq!(starts.len(), 2);
    assert_eq!(starts[1].resumed_from.as_deref(), Some("fake-session"));
    assert_eq!(starts[0].label, starts[1].label);
    let said = notes(&outcome.events, NoteLevel::Info);
    let note = said
        .iter()
        .find(|text| text.starts_with("The main agent's brief"))
        .unwrap_or_else(|| panic!("{said:?}"));
    let mut lines = note.lines();
    assert_eq!(
        lines.next(),
        Some("The main agent's brief named the run in 5 words; it's fixing that.")
    );
    assert!(lines.any(|line| line.contains("has 5 words")), "{note}");
    let started = workflows_started(&outcome.events);
    assert_eq!(started[0].workflow.name, "look-at-it");
    assert_eq!(started[0].from, Some(WorkflowRef::new("one-look")));
    Ok(())
}

/// A run stopped while the main agent fixes its brief says why once, however often it replays: one
/// stopped right after the note, before the fix started, and one stopped during the fix.
#[tokio::test]
async fn a_replayed_fix_says_why_once() -> anyhow::Result<()> {
    let mut long = design(Some("one-look"), json!([]), None);
    long["name"] = json!("split-parse-render-cache-layers");
    let valid = design(Some("one-look"), json!([]), None);
    let (crew, _main_agent) = designing_crew(vec![long, valid.clone(), valid.clone(), valid])?;
    let outcome = crew.start(RunMode::Crew, "look once").await?;
    let why = "The main agent's brief named the run in 5 words";
    let noted = cut_after(
        &outcome.events,
        |event| matches!(event, RunEventKind::Note(note) if note.text.starts_with(why)),
    );
    let fixing = cut_after(&outcome.events, |event| {
        matches!(event, RunEventKind::Session(SessionEvent::Started(started))
            if started.step == Some(StepName::DesignWorkflow) && started.resumed_from.is_some())
    });
    for cut in [noted, fixing] {
        let resumed = crew.resume(cut, Vec::new()).await?;
        let said = notes(&resumed.events, NoteLevel::Info);
        let told = said.iter().filter(|text| text.starts_with(why)).count();
        assert_eq!(told, 1, "{said:?}");
        assert_eq!(workflows_started(&resumed.events).len(), 1);
    }
    Ok(())
}

/// A main agent whose session crashed before its brief failed for an agent reason, so its own
/// session goes again with the whole prompt, saying nothing new.
#[tokio::test]
async fn a_crashed_main_agent_designs_again_on_its_own_session() -> anyhow::Result<()> {
    let crashed = FakeSession::ended(EndReason::Crashed {
        message: "claude exited with code 1".to_owned(),
    });
    let valid = FakeSession::structured(design(Some("one-look"), json!([]), None));
    let (crew, main_agent) = designing_crew_with(vec![crashed, valid])?;
    let outcome = crew.start(RunMode::Crew, "look once").await?;
    let asked = design_sessions(&main_agent);
    assert_eq!(asked.len(), 2);
    assert_eq!(asked[0].key, asked[1].key, "its own session again");
    assert_eq!(asked[1].resume, None);
    let told: String = asked[1]
        .prompt
        .iter()
        .map(|part| part.text.as_str())
        .collect();
    assert!(
        told.contains("## Example workflows"),
        "the whole prompt again"
    );
    let said = notes(&outcome.events, NoteLevel::Info);
    assert!(
        !said
            .iter()
            .any(|text| text.starts_with("The main agent's session stopped")),
        "{said:?}"
    );
    let started = workflows_started(&outcome.events);
    assert_eq!(started[0].workflow.name, "look-at-it");
    Ok(())
}

/// A main agent whose context filled up before its brief has nothing to fix, so a new session
/// writes it, and a `›` note says so.
#[tokio::test]
async fn a_main_agent_whose_context_filled_gets_a_new_session_with_a_note() -> anyhow::Result<()> {
    let full = FakeSession::ended(EndReason::ContextFull);
    let valid = FakeSession::structured(design(Some("one-look"), json!([]), None));
    let (crew, main_agent) = designing_crew_with(vec![full, valid])?;
    let outcome = crew.start(RunMode::Crew, "look once").await?;
    let asked = design_sessions(&main_agent);
    assert_eq!(asked.len(), 2);
    assert_ne!(asked[0].key, asked[1].key, "a new session");
    assert_eq!(asked[1].resume, None);
    let told: String = asked[1]
        .prompt
        .iter()
        .map(|part| part.text.as_str())
        .collect();
    assert!(
        told.contains("## Example workflows"),
        "the whole prompt again"
    );
    let said = notes(&outcome.events, NoteLevel::Info);
    let note = said
        .iter()
        .find(|text| text.starts_with("The main agent's session stopped"))
        .unwrap_or_else(|| panic!("{said:?}"));
    assert_eq!(
        note.lines().next(),
        Some(
            "The main agent's session stopped before its brief was done, so a new session is \
             writing it."
        )
    );
    let started = workflows_started(&outcome.events);
    assert_eq!(started[0].workflow.name, "look-at-it");
    Ok(())
}

#[tokio::test]
async fn orchestrator_designs_off_asks_nothing() -> anyhow::Result<()> {
    let (mut crew, main_agent) = designing_crew(Vec::new())?;
    crew.settings.workflows.orchestrator_designs = false.into();
    let outcome = crew.start(RunMode::Crew, "look twice").await?;
    assert_eq!(designs(&main_agent), 0);
    let started = workflows_started(&outcome.events);
    assert_eq!(started[0].from, Some(WorkflowRef::new("look-then-look")));
    Ok(())
}

#[tokio::test]
async fn resumed_run_does_not_design_again() -> anyhow::Result<()> {
    let params = json!([{"name": "check", "value": "understood"}]);
    let (crew, main_agent) = designing_crew(vec![design(Some("one-look"), params, None)])?;
    let outcome = crew.start(RunMode::Crew, "look once").await?;
    let cut = cut_after(&outcome.events, |event| {
        matches!(event, RunEventKind::Workflow(WorkflowEvent::Started(_)))
    });
    let resumed = crew.resume(cut, Vec::new()).await?;
    assert_eq!(designs(&main_agent), 1);
    assert_eq!(workflows_started(&resumed.events).len(), 1);
    ended(&resumed, RunOutcome::BranchLeft);
    Ok(())
}

/// The log up to and including the last event that matches.
fn cut_after(events: &[RunEvent], matches: impl Fn(&RunEventKind) -> bool) -> Vec<RunEvent> {
    let end = events
        .iter()
        .rposition(|event| matches(&event.kind))
        .unwrap_or_else(|| panic!("nothing to cut after"));
    events[..=end].to_vec()
}

#[tokio::test]
async fn captain_run_never_adapts() -> anyhow::Result<()> {
    let mut crew = Crew::new()?;
    let main_agent = crew.answering_worker("claude-code/opus", "anthropic");
    crew.settings.workflows.adapt_at_phase_ends = true;
    crew.settings.workflows.orchestrator_designs = true.into();
    let outcome = crew.start(RunMode::Captain, "fix the typo").await?;
    // It got past a phase end, where a crew run would have asked.
    assert!(
        steps_started(&outcome.events)
            .iter()
            .any(|started| started.step.phase > 0)
    );
    assert_eq!(designs(&main_agent), 0);
    assert!(opened(&outcome.events, ApprovalKind::ChangeRequest).is_empty());
    Ok(())
}

/// A crew run following a workflow that builds the Captain task is Captain work too, so it doesn't
/// adapt at phase ends either.
#[tokio::test]
async fn crew_run_in_a_captain_workflow_never_adapts() -> anyhow::Result<()> {
    let mut crew = Crew::new()?;
    let main_agent = crew.answering_worker("claude-code/opus", "anthropic");
    crew.settings.workflow = "captain".to_owned();
    crew.settings.workflows.adapt_at_phase_ends = true;
    crew.settings.workflows.orchestrator_designs = false.into();
    let outcome = crew.start(RunMode::Crew, "fix the typo").await?;
    // A crew run asks the go-ahead before it builds, here with nothing read.
    let (go_ahead, _reply) = support::recommended(&outcome.events)
        .ok_or_else(|| anyhow::anyhow!("the go-ahead waits"))?;
    let outcome = crew.resume(outcome.events.clone(), vec![go_ahead]).await?;
    assert!(
        steps_started(&outcome.events)
            .iter()
            .any(|started| started.step.phase > 0)
    );
    assert_eq!(designs(&main_agent), 0);
    assert!(opened(&outcome.events, ApprovalKind::ChangeRequest).is_empty());
    Ok(())
}

#[tokio::test]
async fn project_file_replaces_a_builtin() -> anyhow::Result<()> {
    let mut crew = Crew::new()?;
    responder(
        &mut crew,
        "claude-code/opus",
        "anthropic",
        Vec::new(),
        Vec::new(),
    );
    let mine = LOOK_THEN_LOOK.replace("look-then-look", "council");
    crew.repo
        .write(".argonauts/workflows/council.yaml", &mine)?;
    crew.settings.workflow = "council".to_owned();
    crew.settings.workflows.adapt_at_phase_ends = false;
    let outcome = crew.start(RunMode::Crew, "look twice").await?;
    let started = workflows_started(&outcome.events);
    assert_eq!(started[0].workflow.phases, parse(&mine).phases);
    assert_eq!(started[0].workflow.based_on.as_deref(), Some("council"));
    Ok(())
}

/// A workflow's own file holds its phases beside its settings, and they come before a file of
/// phases alone. One that leaves its phases out leaves the built-in's in place.
#[tokio::test]
async fn a_workflows_own_file_gives_its_phases() -> anyhow::Result<()> {
    const OWN: &str = "about = \"Two looks\"\n\n[review]\nper_task = 1\n\n[[phases]]\nname = \"First look\"\nstandard = \"check\"\n\n  [[phases.steps]]\n  kind = \"checks\"\n\n[[phases]]\nname = \"Second look\"\nstandard = \"check\"\n\n  [[phases.steps]]\n  kind = \"checks\"\n";
    let run = async |own_file: &str| -> anyhow::Result<Workflow> {
        let mut crew = Crew::new()?;
        responder(
            &mut crew,
            "claude-code/opus",
            "anthropic",
            Vec::new(),
            Vec::new(),
        );
        crew.repo
            .write(".argonauts/workflows/council.toml", own_file)?;
        crew.repo.write(
            ".argonauts/workflows/council.yaml",
            &ONE_LOOK.replace("one-look", "council"),
        )?;
        crew.settings.workflow = "council".to_owned();
        crew.settings.workflows.adapt_at_phase_ends = false;
        let outcome = crew.start(RunMode::Crew, "look twice").await?;
        Ok(workflows_started(&outcome.events)[0].workflow.clone())
    };

    let with_phases = run(OWN).await?;
    assert_eq!(with_phases.phases, parse(LOOK_THEN_LOOK).phases);

    let settings_alone = run("[review]\nper_task = 1\n").await?;
    let names: Vec<&str> = settings_alone
        .phases
        .iter()
        .map(|phase| phase.name.as_str())
        .collect();
    assert_eq!(names, ["Look"]);
    Ok(())
}

/// A loop that runs out of rounds without its condition fails the run, and nothing after it
/// runs.
#[tokio::test]
async fn loop_out_of_rounds_stops_the_run() -> anyhow::Result<()> {
    let text = "name: waits-for-a-plan\ndescription: Waits\nphases:\n  - name: Plan\n    standard: plan\n    steps:\n      - kind: loop_until\n        until: plan_decided\n        max: 2\n        steps:\n          - kind: checks\n  - name: Look\n    standard: check\n    steps:\n      - kind: checks\n";
    let mut crew = Crew::new()?;
    responder(
        &mut crew,
        "claude-code/opus",
        "anthropic",
        Vec::new(),
        Vec::new(),
    );
    crew.repo
        .write(".argonauts/workflows/waits-for-a-plan.yaml", text)?;
    crew.settings.workflow = "waits-for-a-plan".to_owned();
    crew.settings.workflows.adapt_at_phase_ends = false;
    let outcome = crew.start(RunMode::Crew, "plan it").await?;
    ended(&outcome, RunOutcome::Failed);
    let (_, detail) = finished_with(&outcome.events).unwrap_or_else(|| panic!("not finished"));
    assert!(
        detail
            .as_deref()
            .is_some_and(|detail| detail.contains("didn't hold")),
        "{detail:?}"
    );
    assert!(
        steps_started(&outcome.events)
            .iter()
            .all(|started| started.step.phase == 0)
    );
    Ok(())
}

const UNDERSTAND_UNASKED: &str = "name: understand-unasked\ndescription: An Understand loop that never asks\nphases:\n  - name: Understand\n    standard: understand\n    steps:\n      - kind: loop_until\n        until: understood\n        max: 1\n        steps:\n          - kind: checks\n  - name: Look\n    standard: check\n    steps:\n      - kind: checks\n";

/// An Understand loop that runs out of rounds stops the run, since nothing after it can go on. A
/// run that crashed between that step's end and the run's end stops the same way when it resumes.
#[tokio::test]
async fn understand_loop_out_of_rounds_stops_after_a_crash() -> anyhow::Result<()> {
    let mut crew = Crew::new()?;
    responder(
        &mut crew,
        "claude-code/opus",
        "anthropic",
        Vec::new(),
        Vec::new(),
    );
    crew.repo.write(
        ".argonauts/workflows/understand-unasked.yaml",
        UNDERSTAND_UNASKED,
    )?;
    crew.settings.workflow = "understand-unasked".to_owned();
    crew.settings.workflows.adapt_at_phase_ends = false;
    let outcome = crew.start(RunMode::Crew, "rate limit the API").await?;
    ended(&outcome, RunOutcome::Stopped);

    let cut = cut_after(
        &outcome.events,
        |event| matches!(event, RunEventKind::Workflow(WorkflowEvent::StepFinished(finished)) if finished.kind == StepKind::LoopUntil),
    );
    assert!(finished_with(&cut).is_none());
    let resumed = crew.resume(cut, Vec::new()).await?;
    ended(&resumed, RunOutcome::Stopped);
    assert!(
        steps_started(&resumed.events)
            .iter()
            .all(|started| started.step.phase == 0)
    );
    Ok(())
}

const UNDERSTAND_THEN_LOOK: &str = "name: understand-then-look\ndescription: Understand, then a look\nphases:\n  - name: Understand\n    standard: understand\n    steps:\n      - kind: loop_until\n        until: understood\n        max: 2\n        steps:\n          - kind: read\n          - kind: compare\n          - kind: approval\n            approval: understand\n  - name: Look\n    standard: check\n    steps:\n      - kind: checks\n";

fn reading() -> Value {
    json!({
        "summary": "Add a rate limit to the public API.",
        "success_criteria": ["Requests over the limit get a 429 response"],
        "in_scope": ["the public HTTP API"],
        "out_of_scope": [],
        "assumptions": [],
        "open_questions": []
    })
}

/// Three workers from three providers, each reading the request back the same way and listing no
/// differences.
fn understand_crew() -> anyhow::Result<Crew> {
    let mut crew = Crew::new()?;
    crew.repo.write(
        ".argonauts/workflows/understand-then-look.yaml",
        UNDERSTAND_THEN_LOOK,
    )?;
    crew.repo.commit_all("Add the workflow")?;
    crew.settings.workflow = "understand-then-look".to_owned();
    crew.settings.workflows.adapt_at_phase_ends = false;
    crew.settings.understand.comparers = 3;
    let listing = json!({"agreements": ["The public API gets a rate limit."], "differences": []});
    for (worker, provider) in [
        ("claude-code/opus", "anthropic"),
        ("codex", "gpt"),
        ("agy", "gemini"),
    ] {
        responder(
            &mut crew,
            worker,
            provider,
            Vec::new(),
            vec![
                (StepName::Read, reading()),
                (StepName::Compare, listing.clone()),
            ],
        );
    }
    Ok(crew)
}

async fn decide(
    crew: &Crew,
    outcome: &Outcome,
    kind: ApprovalKind,
    action: AnswerAction,
) -> anyhow::Result<(Outcome, Result<(), String>)> {
    let (command, reply) = support::answer(&outcome.events, kind, action, Some("once more"))?;
    let next = crew.resume(outcome.events.clone(), vec![command]).await?;
    Ok((next, reply.await?))
}

/// You abandon at the Understand approval. Cut right after the deciding step finished (the
/// approval, then the loop around it), the resumed run still ends abandoned and plans
/// nothing.
#[tokio::test]
async fn goal_abandon_replays() -> anyhow::Result<()> {
    let crew = understand_crew()?;
    let outcome = crew.start(RunMode::Crew, "rate limit the API").await?;
    let (abandoned, reply) = decide(
        &crew,
        &outcome,
        ApprovalKind::Understand,
        AnswerAction::Abandon,
    )
    .await?;
    assert_eq!(reply, Ok(()));
    ended(&abandoned, RunOutcome::Abandoned);
    for kind in [StepKind::Approval, StepKind::LoopUntil] {
        let cut = cut_after(
            &abandoned.events,
            |event| matches!(event, RunEventKind::Workflow(WorkflowEvent::StepFinished(finished)) if finished.kind == kind),
        );
        let resumed = crew.resume(cut, Vec::new()).await?;
        assert_eq!(
            resumed.exit,
            EngineExit::Finished(RunOutcome::Abandoned),
            "cut after {kind:?}"
        );
        assert!(
            steps_started(&resumed.events)
                .iter()
                .all(|started| started.step.phase == 0),
            "cut after {kind:?}"
        );
    }
    Ok(())
}

/// Sent back once, the goal's second round is the loop's last: its question offers no send back,
/// so the run can't fall through to planning on a goal you didn't approve.
#[tokio::test]
async fn goal_last_round_offers_no_send_back() -> anyhow::Result<()> {
    let crew = understand_crew()?;
    let outcome = crew.start(RunMode::Crew, "rate limit the API").await?;
    let first = opened(&outcome.events, ApprovalKind::Understand);
    assert!(
        first[0]
            .options
            .iter()
            .any(|option| option.action == AnswerAction::SendBack)
    );
    let (again, reply) = decide(
        &crew,
        &outcome,
        ApprovalKind::Understand,
        AnswerAction::SendBack,
    )
    .await?;
    assert_eq!(reply, Ok(()));
    assert!(matches!(again.exit, EngineExit::Waiting(_)));
    let questions = opened(&again.events, ApprovalKind::Understand);
    assert_eq!(questions.len(), 2);
    assert!(
        questions[1]
            .options
            .iter()
            .all(|option| option.action != AnswerAction::SendBack)
    );
    let (still, reply) = decide(
        &crew,
        &again,
        ApprovalKind::Understand,
        AnswerAction::SendBack,
    )
    .await?;
    assert!(reply.is_err(), "{reply:?}");
    assert!(matches!(still.exit, EngineExit::Waiting(_)));
    assert!(
        steps_started(&still.events)
            .iter()
            .all(|started| started.step.phase == 0)
    );
    Ok(())
}

const PLAN_THEN_LOOK: &str = "name: plan-then-look\ndescription: The plan approval, then a look\nphases:\n  - name: Plan\n    standard: plan\n    steps:\n      - kind: approval\n        approval: plan\n  - name: Look\n    standard: check\n    steps:\n      - kind: checks\n";

/// A run's log that ends right after its first step finished, with `middle` logged while the
/// step ran.
fn step_log(
    crew: &Crew,
    text: &str,
    kind: StepKind,
    standard: StandardPhase,
    middle: Vec<RunEventKind>,
) -> anyhow::Result<Vec<RunEvent>> {
    let started = crew.run_started(RunMode::Crew, "a logged run")?;
    let workflow = parse(text);
    let id = WorkflowId::numbered(1);
    let step = StepRef {
        workflow: id.clone(),
        phase: 0,
        path: vec![0],
    };
    let mut kinds = vec![
        RunEventKind::Workflow(WorkflowEvent::Started(Box::new(WorkflowStarted {
            id: id.clone(),
            from: Some(WorkflowRef::new(&workflow.name)),
            version: "logged".to_owned(),
            parent: None,
            workflow,
        }))),
        RunEventKind::Workflow(WorkflowEvent::StepStarted(StepStarted {
            step: step.clone(),
            kind,
            standard,
            round: 1,
        })),
    ];
    kinds.extend(middle);
    kinds.push(RunEventKind::Workflow(WorkflowEvent::StepFinished(
        StepFinished {
            step,
            kind,
            standard,
            round: 1,
            end: StepEnd::Done,
            summary: Some("decided".to_owned()),
        },
    )));
    let mut events = vec![started.clone()];
    for (seq, kind) in kinds.into_iter().enumerate() {
        events.push(RunEvent {
            seq: seq as u64 + 1,
            source: "argonauts-code".to_owned(),
            kind,
            ..started.clone()
        });
    }
    Ok(events)
}

/// The plan approval, opened and answered with `action`.
fn plan_answered(action: AnswerAction) -> Vec<RunEventKind> {
    let id = ApprovalId::numbered(1);
    let opened = ApprovalOpened {
        id: id.clone(),
        kind: ApprovalKind::Plan,
        title: "Approve this plan?".to_owned(),
        task: None,
        packet: ApprovalPacket::Question(QuestionPacket {
            from: ModelRef::new("claude-code", None),
            task: TaskId::new("plan").unwrap_or_else(|err| panic!("{err}")),
            text: "The plan".to_owned(),
            options: Vec::new(),
        }),
        evidence_sha256: "e".repeat(64),
        mode: ApprovalMode::Supervised,
        decide_with: DecideWith::Rules,
        when_declined: WhenDeclined::Decide,
        options: Vec::new(),
        free_text: FreeText::Guidance,
        needs_you: true,
    };
    let decided = ApprovalDecided {
        id,
        action,
        text: None,
        by: Party::User,
        evidence_sha256: "e".repeat(64),
        auto_preview: None,
        tasks: Vec::new(),
    };
    vec![
        RunEventKind::Approval(ApprovalEvent::Opened(Box::new(opened))),
        RunEventKind::Approval(ApprovalEvent::Decided(decided)),
    ]
}

#[tokio::test]
async fn plan_abandon_replays() -> anyhow::Result<()> {
    let mut crew = Crew::new()?;
    responder(
        &mut crew,
        "claude-code/opus",
        "anthropic",
        Vec::new(),
        Vec::new(),
    );
    crew.settings.workflows.adapt_at_phase_ends = false;
    let log = step_log(
        &crew,
        PLAN_THEN_LOOK,
        StepKind::Approval,
        StandardPhase::Plan,
        plan_answered(AnswerAction::Abandon),
    )?;
    let resumed = crew.resume(log, Vec::new()).await?;
    ended(&resumed, RunOutcome::Abandoned);
    assert!(
        steps_started(&resumed.events)
            .iter()
            .all(|started| started.step.phase == 0)
    );
    Ok(())
}

/// "Run it as Captain" replays as the switch, and the switch finds your own `captain` file first.
#[tokio::test]
async fn plan_switch_to_captain_replays_into_your_file() -> anyhow::Result<()> {
    let mut crew = Crew::new()?;
    responder(
        &mut crew,
        "claude-code/opus",
        "anthropic",
        Vec::new(),
        Vec::new(),
    );
    crew.settings.workflows.adapt_at_phase_ends = false;
    let mine = LOOK_THEN_LOOK.replace("look-then-look", "captain");
    crew.repo
        .write(".argonauts/workflows/captain.yaml", &mine)?;
    let log = step_log(
        &crew,
        PLAN_THEN_LOOK,
        StepKind::Approval,
        StandardPhase::Plan,
        plan_answered(AnswerAction::SwitchToCaptain),
    )?;
    let resumed = crew.resume(log, Vec::new()).await?;
    let started = workflows_started(&resumed.events);
    assert_eq!(started.len(), 2, "{started:?}");
    assert_eq!(started[1].from, Some(WorkflowRef::new("captain")));
    assert_eq!(started[1].workflow, parse(&mine));
    let first = &started[0].id;
    assert!(
        steps_started(&resumed.events)
            .iter()
            .all(|started| started.step.workflow != *first || started.step.phase == 0)
    );
    Ok(())
}

#[tokio::test]
async fn landing_replays_as_merged() -> anyhow::Result<()> {
    let mut crew = Crew::new()?;
    responder(
        &mut crew,
        "claude-code/opus",
        "anthropic",
        Vec::new(),
        Vec::new(),
    );
    crew.settings.workflows.adapt_at_phase_ends = false;
    let text = "name: just-merge\ndescription: Merge\nphases:\n  - name: Merge\n    standard: decide\n    steps:\n      - kind: merge\n";
    let merged = RunEventKind::Merge(MergeEvent::RunMerged {
        target: "main".to_owned(),
        sha: "a".repeat(40),
        fast_forward: true,
    });
    let log = step_log(
        &crew,
        text,
        StepKind::Merge,
        StandardPhase::Decide,
        vec![merged],
    )?;
    let resumed = crew.resume(log, Vec::new()).await?;
    ended(&resumed, RunOutcome::Merged);
    Ok(())
}

const OUTER: &str = "name: outer\ndescription: Runs the inner one\nphases:\n  - name: Nest\n    standard: check\n    steps:\n      - kind: workflow\n        workflow: inner\n";

const INNER: &str = "name: inner\ndescription: Two looks one level down\nphases:\n  - name: First look\n    standard: check\n    steps:\n      - kind: checks\n  - name: Second look\n    standard: check\n    steps:\n      - kind: checks\n";

/// The main agent changes the nested workflow at its first phase end and you accept. Resumed right
/// after the change, the `workflow` step runs the same child on, and starts no other.
#[tokio::test]
async fn nested_workflow_keeps_its_id_after_a_change() -> anyhow::Result<()> {
    let mut crew = Crew::new()?;
    crew.settings.workflow = "outer".to_owned();
    crew.settings.workflows.adapt_at_phase_ends = true;
    crew.repo.write(".argonauts/workflows/outer.yaml", OUTER)?;
    crew.repo.write(".argonauts/workflows/inner.yaml", INNER)?;
    let changed = format!(
        "{INNER}  - name: Extra look\n    standard: check\n    steps:\n      - kind: checks\n"
    );
    let change = design(None, json!([]), Some(&changed));
    responder(
        &mut crew,
        "claude-code/opus",
        "anthropic",
        vec![change],
        Vec::new(),
    );

    let outcome = crew.start(RunMode::Crew, "look inside").await?;
    assert!(matches!(outcome.exit, EngineExit::Waiting(_)));
    let (command, reply) = support::answer(
        &outcome.events,
        ApprovalKind::ChangeRequest,
        AnswerAction::Approve,
        None,
    )?;
    let accepted = crew.resume(outcome.events.clone(), vec![command]).await?;
    assert_eq!(reply.await?, Ok(()));
    let cut = cut_after(&accepted.events, |event| {
        matches!(event, RunEventKind::Workflow(WorkflowEvent::Changed(_)))
    });
    let resumed = crew.resume(cut, Vec::new()).await?;
    ended(&resumed, RunOutcome::BranchLeft);
    let nested: Vec<_> = workflows_started(&resumed.events)
        .into_iter()
        .filter(|started| started.parent.is_some())
        .collect();
    assert_eq!(nested.len(), 1, "{nested:?}");
    let inner = &nested[0].id;
    assert!(
        steps_started(&resumed.events)
            .iter()
            .any(|started| { started.step.workflow == *inner && started.step.phase == 2 })
    );
    Ok(())
}

/// The main agent's change may only change phases that haven't started. One that rewrites the phase
/// that just ended is refused twice, noted, and the workflow goes on unchanged. A run stopped
/// right after the note that says why says it once when it carries on.
#[tokio::test]
async fn change_to_a_phase_that_ran_is_refused() -> anyhow::Result<()> {
    let mut crew = Crew::new()?;
    crew.settings.workflow = "look-then-look".to_owned();
    crew.settings.workflows.adapt_at_phase_ends = true;
    crew.repo
        .write(".argonauts/workflows/look-then-look.yaml", LOOK_THEN_LOOK)?;
    let rewritten = LOOK_THEN_LOOK.replacen("name: First look", "name: Another first look", 1);
    let rewrite = design(None, json!([]), Some(&rewritten));
    let main_agent = responder(
        &mut crew,
        "claude-code/opus",
        "anthropic",
        vec![rewrite.clone(), rewrite.clone(), rewrite],
        Vec::new(),
    );
    let outcome = crew.start(RunMode::Crew, "look twice").await?;
    ended(&outcome, RunOutcome::BranchLeft);
    assert!(opened(&outcome.events, ApprovalKind::ChangeRequest).is_empty());
    // The first refusal goes back to the session that proposed it, and a note says why.
    let asked = design_sessions(&main_agent);
    assert_eq!(
        asked.last().and_then(|spec| spec.resume.as_deref()),
        Some("fake-session")
    );
    let said = notes(&outcome.events, NoteLevel::Info);
    let why = "The main agent's change after phase 1 wrote a workflow that breaks its rules; it's \
               fixing that.";
    let told = said
        .iter()
        .filter(|text| text.lines().next() == Some(why))
        .count();
    assert_eq!(told, 1, "{said:?}");
    let warned = notes(&outcome.events, NoteLevel::Warn);
    assert!(
        warned
            .iter()
            .any(|text| text.contains("phase 1 (First look)")),
        "{warned:?}"
    );
    assert!(
        workflow_events(&outcome.events)
            .iter()
            .all(|event| !matches!(event, WorkflowEvent::Changed(_)))
    );
    let noted = cut_after(
        &outcome.events,
        |event| matches!(event, RunEventKind::Note(note) if note.text.starts_with(why)),
    );
    let resumed = crew.resume(noted, Vec::new()).await?;
    let said = notes(&resumed.events, NoteLevel::Info);
    let told = said
        .iter()
        .filter(|text| text.lines().next() == Some(why))
        .count();
    assert_eq!(told, 1, "{said:?}");
    Ok(())
}

/// Your own proposal and the main agent's phase-end change are separate approvals; neither
/// withdraws the other. Yours, accepted while the run waited, is made from the log.
#[tokio::test]
async fn your_proposal_and_the_main_agents_change_both_stay_open() -> anyhow::Result<()> {
    let mut crew = Crew::new()?;
    crew.settings.workflow = "look-then-look".to_owned();
    crew.settings.workflows.adapt_at_phase_ends = true;
    crew.repo
        .write(".argonauts/workflows/look-then-look.yaml", LOOK_THEN_LOOK)?;
    let extra = format!(
        "{LOOK_THEN_LOOK}  - name: Extra look\n    standard: check\n    steps:\n      - kind: checks\n"
    );
    let change = design(None, json!([]), Some(&extra));
    responder(
        &mut crew,
        "claude-code/opus",
        "anthropic",
        vec![change],
        Vec::new(),
    );
    let outcome = crew.start(RunMode::Crew, "look twice").await?;
    assert!(matches!(outcome.exit, EngineExit::Waiting(_)));
    let yours = format!(
        "{LOOK_THEN_LOOK}  - name: Your look\n    standard: check\n    steps:\n      - kind: checks\n"
    );
    let (reply, replied) = tokio::sync::oneshot::channel();
    let propose = EngineCommand::ProposeWorkflowChange {
        target: None,
        workflow: Box::new(parse(&yours)),
        reason: "one more look of mine".to_owned(),
        reply,
    };
    let resumed = crew.resume(outcome.events.clone(), vec![propose]).await?;
    assert!(replied.await?.is_ok());
    let changes = opened(&resumed.events, ApprovalKind::ChangeRequest);
    assert_eq!(changes.len(), 2, "{changes:?}");
    assert!(!resumed.events.iter().any(|event| matches!(
        event.kind,
        RunEventKind::Approval(ApprovalEvent::Withdrawn { .. })
    )));
    let yours = changes[1].id.clone();
    let (command, reply) = support::answer(
        &resumed.events,
        ApprovalKind::ChangeRequest,
        AnswerAction::Approve,
        None,
    )?;
    let accepted = crew.resume(resumed.events.clone(), vec![command]).await?;
    assert_eq!(reply.await?, Ok(()));
    let made: Vec<ApprovalId> = workflow_events(&accepted.events)
        .into_iter()
        .filter_map(|event| match event {
            WorkflowEvent::Changed(changed) => changed.approval.clone(),
            _ => None,
        })
        .collect();
    assert_eq!(made, vec![yours]);
    Ok(())
}

/// The main agent designs before anything runs, and code opens its pick with Understand: no nested
/// goal workflow, no extra Understand phase, and no Plan phase holding only an approval.
#[tokio::test]
async fn the_main_agent_designs_first_and_understand_opens_the_run() -> anyhow::Result<()> {
    let mut crew = Crew::new()?;
    let goal = || {
        let listing =
            json!({"agreements": ["The public API gets a rate limit."], "differences": []});
        vec![(StepName::Read, reading()), (StepName::Compare, listing)]
    };
    let params = json!([{"name": "check", "value": "understood"}]);
    // The second design is the look at Understand's end, which keeps the workflow.
    let picks = vec![
        design(Some("one-look"), params.clone(), None),
        design(Some("one-look"), params, None),
    ];
    let main_agent = responder(&mut crew, "claude-code/opus", "anthropic", picks, goal());
    responder(&mut crew, "codex", "openai", Vec::new(), goal());
    crew.settings.workflow = "understand-then-look".to_owned();
    crew.settings.workflows.orchestrator_designs = OrchestratorDesigns::ANY;
    crew.settings.workflows.adapt_at_phase_ends = true;
    crew.repo
        .write(".argonauts/workflows/one-look.yaml", ONE_LOOK)?;
    crew.repo.write(
        ".argonauts/workflows/understand-then-look.yaml",
        UNDERSTAND_THEN_LOOK,
    )?;

    let outcome = crew.start(RunMode::Crew, "rate limit the API").await?;
    assert_eq!(designs(&main_agent), 1);
    let started = workflows_started(&outcome.events);
    assert_eq!(started.len(), 1, "{started:?}");
    let run = started[0];
    assert_eq!(run.parent, None);
    assert_eq!(run.from, Some(WorkflowRef::new("one-look")));
    let phases: Vec<&str> = run
        .workflow
        .phases
        .iter()
        .map(|phase| phase.name.as_str())
        .collect();
    assert_eq!(phases, ["Understand", "Look"]);
    let asked = main_agent
        .specs()
        .into_iter()
        .find(|spec| spec.step == Some(StepName::DesignWorkflow))
        .expect("the main agent designed");
    assert!(
        asked
            .prompt
            .iter()
            .any(|part| part.text.contains("### one-look"))
    );
    // Understand ends with its one question.
    assert_eq!(opened(&outcome.events, ApprovalKind::Understand).len(), 1);
    assert!(opened(&outcome.events, ApprovalKind::Plan).is_empty());

    // Approved, it runs on; Understand's end asked the main agent again, and nothing else asks.
    let (done, reply) = decide(
        &crew,
        &outcome,
        ApprovalKind::Understand,
        AnswerAction::Approve,
    )
    .await?;
    assert_eq!(reply, Ok(()));
    assert_eq!(designs(&main_agent), 2);
    assert!(opened(&done.events, ApprovalKind::ChangeRequest).is_empty());
    assert!(opened(&done.events, ApprovalKind::Plan).is_empty());
    assert!(workflow_events(&done.events).iter().any(|event| matches!(
        event,
        WorkflowEvent::StepFinished(finished) if finished.kind == StepKind::Checks
    )));
    ended(&done, RunOutcome::BranchLeft);
    Ok(())
}
