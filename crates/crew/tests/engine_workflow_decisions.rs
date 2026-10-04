//! How the run's workflow is chosen and how its decisions hold: the lead's design at the start,
//! decisions that replay after the engine stopped, loops that run out of rounds, workflow files
//! that replace built-ins, and nested workflows that keep their id across a change.

mod support;

use std::collections::VecDeque;
use std::sync::{Arc, Mutex, PoisonError};

use serde_json::{Value, json};

use argonauts_core::CardId;
use argonauts_core::engine::{EngineCommand, EngineExit};
use argonauts_core::events::card::IntegrationEvent;
use argonauts_core::events::checkpoint::{
    AnswerAction, CheckpointDecided, CheckpointEvent, CheckpointKind, CheckpointOpened,
    CheckpointPacket, FreeText, QuestionPacket,
};
use argonauts_core::events::run::{Party, RunLifecycle, RunMode, RunOutcome};
use argonauts_core::events::workflow::{
    StepEnd, StepFinished, StepStarted, WorkflowEvent, WorkflowStarted,
};
use argonauts_core::events::{RunEvent, RunEventKind};
use argonauts_core::log::NoteLevel;
use argonauts_core::names::{CheckpointId, WorkerRef, WorkflowId};
use argonauts_core::settings::approvals::{ApprovalMode, DecideWith};
use argonauts_core::settings::protocol::UnderstandWhen;
use argonauts_core::settings::workflows::LeadDesigns;
use argonauts_core::steps::StepName;
use argonauts_core::worker::Worker;
use argonauts_core::workflow::{StandardPhase, StepKind, StepRef, Workflow, WorkflowRef};
use argonauts_crew::workflow::yaml;
use argonauts_testkit::worker::{FakeSession, FakeWorker};

use support::{Crew, Outcome};

fn parse(text: &str) -> Workflow {
    yaml::parse(text).unwrap_or_else(|err| panic!("{err}\n{text}"))
}

const ONE_LOOK: &str = "name: one-look\ndescription: One look, when a condition holds\nparams:\n  check: restate_first\nphases:\n  - name: Look\n    standard: check\n    steps:\n      - kind: gate\n        when: \"{{check}}\"\n";

const LOOK_THEN_LOOK: &str = "name: look-then-look\ndescription: Two looks\nphases:\n  - name: First look\n    standard: check\n    steps:\n      - kind: gate\n  - name: Second look\n    standard: check\n    steps:\n      - kind: gate\n";

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

fn designs(worker: &FakeWorker) -> usize {
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

fn opened(events: &[RunEvent], kind: CheckpointKind) -> Vec<&CheckpointOpened> {
    events
        .iter()
        .filter_map(|event| match &event.kind {
            RunEventKind::Checkpoint(CheckpointEvent::Opened(opened)) if opened.kind == kind => {
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

/// The lead's brief: an example by name with its params, or a workflow of its own.
fn design(template: Option<&str>, params: Value, composed: Option<&str>) -> Value {
    support::brief(template, params, composed, "it fits")
}

/// A worker that gives `designs` to the lead's `DesignWorkflow` asks in order, `answers` to the
/// steps they name, and the smallest valid answer to everything else, such as the retro.
fn responder(
    crew: &mut Crew,
    worker: &str,
    family: &str,
    designs: Vec<Value>,
    answers: Vec<(StepName, Value)>,
) -> Arc<FakeWorker> {
    let designs = Mutex::new(VecDeque::from(designs));
    let fake = FakeWorker::harness(worker, family).respond_with(move |spec| {
        if spec.step == Some(StepName::DesignWorkflow) {
            let next = designs
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .pop_front();
            if let Some(design) = next {
                return FakeSession::structured(design);
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
    let reference = fake.worker_ref().clone();
    if crew.settings.crew.is_empty() {
        crew.settings.default_agent = reference.clone();
    }
    crew.settings.crew.push(reference);
    fake
}

/// A crew run whose lead is the one worker, designing the workflow at the start. Understand is
/// the lead's to add, so the workflows here run as the lead wrote them.
fn designing_crew(answers: Vec<Value>) -> anyhow::Result<(Crew, Arc<FakeWorker>)> {
    let mut crew = Crew::new()?;
    let lead = responder(
        &mut crew,
        "claude-code/opus",
        "anthropic",
        answers,
        Vec::new(),
    );
    crew.settings.workflow = WorkflowRef::new("look-then-look");
    crew.settings.workflows.lead_designs = LeadDesigns::ANY;
    crew.settings.workflows.adapt_at_phase_ends = false;
    crew.settings.understand.when = UnderstandWhen::Lead;
    crew.repo
        .write(".argonauts/workflows/one-look.yaml", ONE_LOOK)?;
    crew.repo
        .write(".argonauts/workflows/look-then-look.yaml", LOOK_THEN_LOOK)?;
    Ok((crew, lead))
}

#[tokio::test]
async fn lead_picks_a_template_with_params() -> anyhow::Result<()> {
    let params = json!([{"name": "check", "value": "goal_approved"}]);
    let (crew, lead) = designing_crew(vec![design(Some("one-look"), params, None)])?;
    let outcome = crew.start(RunMode::Crew, "look once").await?;
    let started = workflows_started(&outcome.events);
    assert_eq!(started.len(), 1, "{started:?}");
    assert_eq!(started[0].from, Some(WorkflowRef::new("one-look")));
    let step = &started[0].workflow.phases[0].steps[0];
    assert_eq!(step.when.as_deref(), Some("goal_approved"));
    assert_eq!(designs(&lead), 1);
    // Your own files are among the examples the lead picks from.
    let specs = lead.specs();
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
            .any(|part| part.text.contains("peer-crew"))
    );
    Ok(())
}

#[tokio::test]
async fn lead_composes_a_workflow_that_runs() -> anyhow::Result<()> {
    let composed = "name: composed-look\ndescription: A look\nphases:\n  - title: Look\n    detail: the gates, once\n    standard: check\n    steps:\n      - kind: gate\n        label: \"look:gates\"\n";
    // Without a detail on its phase, the brief goes back to the lead with the error.
    let bare = composed.replace("    detail: the gates, once\n", "");
    let (crew, lead) = designing_crew(vec![
        design(None, json!([]), Some(&bare)),
        design(None, json!([]), Some(composed)),
    ])?;
    let outcome = crew.start(RunMode::Crew, "look once").await?;
    let started = workflows_started(&outcome.events);
    assert_eq!(started.len(), 1);
    assert_eq!(started[0].from, None);
    // The brief names the run; the lead's file gives the phases.
    assert_eq!(started[0].workflow.phases, parse(composed).phases);
    assert_eq!(started[0].workflow.name, "look-at-it");
    assert_eq!(started[0].workflow.description, "Look at the repository");
    assert_eq!(designs(&lead), 2);
    let retried = lead
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
        "the error goes back to the lead"
    );
    assert!(
        workflow_events(&outcome.events)
            .iter()
            .any(|event| matches!(
                event,
                WorkflowEvent::StepFinished(finished) if finished.kind == StepKind::Gate
            ))
    );
    ended(&outcome, RunOutcome::BranchLeft);
    Ok(())
}

#[tokio::test]
async fn two_invalid_designs_fall_back_with_a_note() -> anyhow::Result<()> {
    let wrong = design(Some("no-such-workflow"), json!([]), None);
    let (crew, lead) = designing_crew(vec![wrong.clone(), wrong])?;
    let outcome = crew.start(RunMode::Crew, "look twice").await?;
    assert_eq!(designs(&lead), 2);
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

#[tokio::test]
async fn lead_designs_off_asks_nothing() -> anyhow::Result<()> {
    let (mut crew, lead) = designing_crew(Vec::new())?;
    crew.settings.workflows.lead_designs = false.into();
    let outcome = crew.start(RunMode::Crew, "look twice").await?;
    assert_eq!(designs(&lead), 0);
    let started = workflows_started(&outcome.events);
    assert_eq!(started[0].from, Some(WorkflowRef::new("look-then-look")));
    Ok(())
}

#[tokio::test]
async fn resumed_run_does_not_design_again() -> anyhow::Result<()> {
    let params = json!([{"name": "check", "value": "goal_approved"}]);
    let (crew, lead) = designing_crew(vec![design(Some("one-look"), params, None)])?;
    let outcome = crew.start(RunMode::Crew, "look once").await?;
    let cut = cut_after(&outcome.events, |event| {
        matches!(event, RunEventKind::Workflow(WorkflowEvent::Started(_)))
    });
    let resumed = crew.resume(cut, Vec::new()).await?;
    assert_eq!(designs(&lead), 1);
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
async fn solo_run_never_adapts() -> anyhow::Result<()> {
    let mut crew = Crew::new()?;
    let lead = crew.answering_worker("claude-code/opus", "anthropic");
    crew.settings.workflows.adapt_at_phase_ends = true;
    crew.settings.workflows.lead_designs = true.into();
    let outcome = crew.start(RunMode::Solo, "fix the typo").await?;
    // It got past a phase end, where a crew run would have asked.
    assert!(
        steps_started(&outcome.events)
            .iter()
            .any(|started| started.step.phase > 0)
    );
    assert_eq!(designs(&lead), 0);
    assert!(opened(&outcome.events, CheckpointKind::ChangeRequest).is_empty());
    Ok(())
}

/// A crew run following a workflow that builds the solo card is solo work too, so it doesn't
/// adapt at phase ends either.
#[tokio::test]
async fn crew_run_in_a_solo_workflow_never_adapts() -> anyhow::Result<()> {
    let mut crew = Crew::new()?;
    let lead = crew.answering_worker("claude-code/opus", "anthropic");
    crew.settings.workflow = WorkflowRef::new("solo");
    crew.settings.workflows.adapt_at_phase_ends = true;
    crew.settings.workflows.lead_designs = false.into();
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
    assert_eq!(designs(&lead), 0);
    assert!(opened(&outcome.events, CheckpointKind::ChangeRequest).is_empty());
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
    let mine = LOOK_THEN_LOOK.replace("look-then-look", "peer-crew");
    crew.repo
        .write(".argonauts/workflows/peer-crew.yaml", &mine)?;
    crew.settings.workflow = WorkflowRef::new("peer-crew");
    crew.settings.workflows.adapt_at_phase_ends = false;
    let outcome = crew.start(RunMode::Crew, "look twice").await?;
    let started = workflows_started(&outcome.events);
    assert_eq!(started[0].workflow.phases, parse(&mine).phases);
    assert_eq!(started[0].workflow.based_on.as_deref(), Some("peer-crew"));
    Ok(())
}

/// A loop that runs out of rounds without its condition fails the run, and nothing after it
/// runs.
#[tokio::test]
async fn loop_out_of_rounds_stops_the_run() -> anyhow::Result<()> {
    let text = "name: waits-for-a-plan\ndescription: Waits\nphases:\n  - name: Plan\n    standard: plan\n    steps:\n      - kind: loop_until\n        until: plan_decided\n        max: 2\n        steps:\n          - kind: gate\n  - name: Look\n    standard: check\n    steps:\n      - kind: gate\n";
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
    crew.settings.workflow = WorkflowRef::new("waits-for-a-plan");
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

const GOAL_UNASKED: &str = "name: goal-unasked\ndescription: A goal loop that never asks\nphases:\n  - name: Goal\n    standard: understand\n    steps:\n      - kind: loop_until\n        until: goal_approved\n        max: 1\n        steps:\n          - kind: gate\n  - name: Look\n    standard: check\n    steps:\n      - kind: gate\n";

/// A goal loop that runs out of rounds stops the run, since nothing after it can go on. A run
/// that crashed between that step's end and the run's end stops the same way when it resumes.
#[tokio::test]
async fn goal_loop_out_of_rounds_stops_after_a_crash() -> anyhow::Result<()> {
    let mut crew = Crew::new()?;
    responder(
        &mut crew,
        "claude-code/opus",
        "anthropic",
        Vec::new(),
        Vec::new(),
    );
    crew.repo
        .write(".argonauts/workflows/goal-unasked.yaml", GOAL_UNASKED)?;
    crew.settings.workflow = WorkflowRef::new("goal-unasked");
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

const GOAL_THEN_LOOK: &str = "name: goal-then-look\ndescription: The goal, then a look\nphases:\n  - name: Goal\n    standard: understand\n    steps:\n      - kind: loop_until\n        until: goal_approved\n        max: 2\n        steps:\n          - kind: restate\n          - kind: differ\n          - kind: checkpoint\n            checkpoint: goal\n  - name: Look\n    standard: check\n    steps:\n      - kind: gate\n";

fn restatement() -> Value {
    json!({
        "summary": "Add a rate limit to the public API.",
        "success_criteria": ["Requests over the limit get a 429 response"],
        "in_scope": ["the public HTTP API"],
        "out_of_scope": [],
        "assumptions": [],
        "open_questions": []
    })
}

/// Three workers from three families, each restating the goal the same way and listing no
/// differences.
fn goal_crew() -> anyhow::Result<Crew> {
    let mut crew = Crew::new()?;
    crew.repo
        .write(".argonauts/workflows/goal-then-look.yaml", GOAL_THEN_LOOK)?;
    crew.repo.commit_all("Add the workflow")?;
    crew.settings.workflow = WorkflowRef::new("goal-then-look");
    crew.settings.workflows.adapt_at_phase_ends = false;
    crew.settings.understand.comparers = 3;
    let listing = json!({"agreements": ["The public API gets a rate limit."], "differences": []});
    for (worker, family) in [
        ("claude-code/opus", "claude"),
        ("codex", "gpt"),
        ("agy", "gemini"),
    ] {
        responder(
            &mut crew,
            worker,
            family,
            Vec::new(),
            vec![
                (StepName::Restate, restatement()),
                (StepName::Differ, listing.clone()),
            ],
        );
    }
    Ok(crew)
}

async fn decide(
    crew: &Crew,
    outcome: &Outcome,
    kind: CheckpointKind,
    action: AnswerAction,
) -> anyhow::Result<(Outcome, Result<(), String>)> {
    let (command, reply) = support::answer(&outcome.events, kind, action, Some("once more"))?;
    let next = crew.resume(outcome.events.clone(), vec![command]).await?;
    Ok((next, reply.await?))
}

/// You abandon at the goal checkpoint. Cut right after the deciding step finished (the
/// checkpoint, then the loop around it), the resumed run still ends abandoned and plans
/// nothing.
#[tokio::test]
async fn goal_abandon_replays() -> anyhow::Result<()> {
    let crew = goal_crew()?;
    let outcome = crew.start(RunMode::Crew, "rate limit the API").await?;
    let (abandoned, reply) =
        decide(&crew, &outcome, CheckpointKind::Goal, AnswerAction::Abandon).await?;
    assert_eq!(reply, Ok(()));
    ended(&abandoned, RunOutcome::Abandoned);
    for kind in [StepKind::Checkpoint, StepKind::LoopUntil] {
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

/// Sent back once, the goal's second round is the loop's last: its card offers no send back,
/// so the run can't fall through to planning on a goal you didn't approve.
#[tokio::test]
async fn goal_last_round_offers_no_send_back() -> anyhow::Result<()> {
    let crew = goal_crew()?;
    let outcome = crew.start(RunMode::Crew, "rate limit the API").await?;
    let first = opened(&outcome.events, CheckpointKind::Goal);
    assert!(
        first[0]
            .options
            .iter()
            .any(|option| option.action == AnswerAction::SendBack)
    );
    let (again, reply) = decide(
        &crew,
        &outcome,
        CheckpointKind::Goal,
        AnswerAction::SendBack,
    )
    .await?;
    assert_eq!(reply, Ok(()));
    assert!(matches!(again.exit, EngineExit::Parked(_)));
    let cards = opened(&again.events, CheckpointKind::Goal);
    assert_eq!(cards.len(), 2);
    assert!(
        cards[1]
            .options
            .iter()
            .all(|option| option.action != AnswerAction::SendBack)
    );
    let (still, reply) =
        decide(&crew, &again, CheckpointKind::Goal, AnswerAction::SendBack).await?;
    assert!(reply.is_err(), "{reply:?}");
    assert!(matches!(still.exit, EngineExit::Parked(_)));
    assert!(
        steps_started(&still.events)
            .iter()
            .all(|started| started.step.phase == 0)
    );
    Ok(())
}

const PLAN_THEN_LOOK: &str = "name: plan-then-look\ndescription: The plan checkpoint, then a look\nphases:\n  - name: Plan\n    standard: plan\n    steps:\n      - kind: checkpoint\n        checkpoint: plan\n  - name: Look\n    standard: check\n    steps:\n      - kind: gate\n";

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
            source: "conductor".to_owned(),
            kind,
            ..started.clone()
        });
    }
    Ok(events)
}

/// The plan checkpoint, opened and answered with `action`.
fn plan_answered(action: AnswerAction) -> Vec<RunEventKind> {
    let id = CheckpointId::numbered(1);
    let opened = CheckpointOpened {
        id: id.clone(),
        kind: CheckpointKind::Plan,
        title: "Approve this plan?".to_owned(),
        card: None,
        packet: CheckpointPacket::Question(QuestionPacket {
            from: WorkerRef::new("claude-code", None),
            card: CardId::new("plan").unwrap_or_else(|err| panic!("{err}")),
            text: "The plan".to_owned(),
            options: Vec::new(),
        }),
        evidence_sha256: "e".repeat(64),
        mode: ApprovalMode::Supervised,
        decide_with: DecideWith::Rules,
        options: Vec::new(),
        free_text: FreeText::Guidance,
        needs_you: true,
    };
    let decided = CheckpointDecided {
        id,
        action,
        text: None,
        by: Party::User,
        evidence_sha256: "e".repeat(64),
        shadow: None,
        cards: Vec::new(),
    };
    vec![
        RunEventKind::Checkpoint(CheckpointEvent::Opened(Box::new(opened))),
        RunEventKind::Checkpoint(CheckpointEvent::Decided(decided)),
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
        StepKind::Checkpoint,
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

/// "Run it solo" replays as the switch, and the switch finds your own `solo` file first.
#[tokio::test]
async fn plan_switch_to_solo_replays_into_your_file() -> anyhow::Result<()> {
    let mut crew = Crew::new()?;
    responder(
        &mut crew,
        "claude-code/opus",
        "anthropic",
        Vec::new(),
        Vec::new(),
    );
    crew.settings.workflows.adapt_at_phase_ends = false;
    let mine = LOOK_THEN_LOOK.replace("look-then-look", "solo");
    crew.repo.write(".argonauts/workflows/solo.yaml", &mine)?;
    let log = step_log(
        &crew,
        PLAN_THEN_LOOK,
        StepKind::Checkpoint,
        StandardPhase::Plan,
        plan_answered(AnswerAction::SwitchToSolo),
    )?;
    let resumed = crew.resume(log, Vec::new()).await?;
    let started = workflows_started(&resumed.events);
    assert_eq!(started.len(), 2, "{started:?}");
    assert_eq!(started[1].from, Some(WorkflowRef::new("solo")));
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
    let landed = RunEventKind::Integration(IntegrationEvent::Landed {
        target: "main".to_owned(),
        sha: "a".repeat(40),
        fast_forward: true,
    });
    let log = step_log(
        &crew,
        text,
        StepKind::Merge,
        StandardPhase::Decide,
        vec![landed],
    )?;
    let resumed = crew.resume(log, Vec::new()).await?;
    ended(&resumed, RunOutcome::Merged);
    Ok(())
}

const OUTER: &str = "name: outer\ndescription: Runs the inner one\nphases:\n  - name: Nest\n    standard: check\n    steps:\n      - kind: workflow\n        workflow: inner\n";

const INNER: &str = "name: inner\ndescription: Two looks one level down\nphases:\n  - name: First look\n    standard: check\n    steps:\n      - kind: gate\n  - name: Second look\n    standard: check\n    steps:\n      - kind: gate\n";

/// The lead changes the nested workflow at its first phase end and you accept. Resumed right
/// after the change, the `workflow` step runs the same child on, and starts no other.
#[tokio::test]
async fn nested_workflow_keeps_its_id_after_a_change() -> anyhow::Result<()> {
    let mut crew = Crew::new()?;
    crew.settings.workflow = WorkflowRef::new("outer");
    crew.settings.workflows.adapt_at_phase_ends = true;
    crew.repo.write(".argonauts/workflows/outer.yaml", OUTER)?;
    crew.repo.write(".argonauts/workflows/inner.yaml", INNER)?;
    let changed = format!(
        "{INNER}  - name: Extra look\n    standard: check\n    steps:\n      - kind: gate\n"
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
    assert!(matches!(outcome.exit, EngineExit::Parked(_)));
    let (command, reply) = support::answer(
        &outcome.events,
        CheckpointKind::ChangeRequest,
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

/// The lead's change may only change phases that haven't started. One that rewrites the phase
/// that just ended is refused twice, noted, and the workflow goes on unchanged.
#[tokio::test]
async fn change_to_a_phase_that_ran_is_refused() -> anyhow::Result<()> {
    let mut crew = Crew::new()?;
    crew.settings.workflow = WorkflowRef::new("look-then-look");
    crew.settings.workflows.adapt_at_phase_ends = true;
    crew.repo
        .write(".argonauts/workflows/look-then-look.yaml", LOOK_THEN_LOOK)?;
    let rewritten = LOOK_THEN_LOOK.replacen("name: First look", "name: Another first look", 1);
    let rewrite = design(None, json!([]), Some(&rewritten));
    responder(
        &mut crew,
        "claude-code/opus",
        "anthropic",
        vec![rewrite.clone(), rewrite],
        Vec::new(),
    );
    let outcome = crew.start(RunMode::Crew, "look twice").await?;
    ended(&outcome, RunOutcome::BranchLeft);
    assert!(opened(&outcome.events, CheckpointKind::ChangeRequest).is_empty());
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
    Ok(())
}

/// Your own proposal and the lead's phase-end change are separate checkpoints; neither
/// withdraws the other. Yours, accepted after the run parked, is made from the log.
#[tokio::test]
async fn your_proposal_and_the_leads_change_both_stay_open() -> anyhow::Result<()> {
    let mut crew = Crew::new()?;
    crew.settings.workflow = WorkflowRef::new("look-then-look");
    crew.settings.workflows.adapt_at_phase_ends = true;
    crew.repo
        .write(".argonauts/workflows/look-then-look.yaml", LOOK_THEN_LOOK)?;
    let extra = format!(
        "{LOOK_THEN_LOOK}  - name: Extra look\n    standard: check\n    steps:\n      - kind: gate\n"
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
    assert!(matches!(outcome.exit, EngineExit::Parked(_)));
    let yours = format!(
        "{LOOK_THEN_LOOK}  - name: Your look\n    standard: check\n    steps:\n      - kind: gate\n"
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
    let changes = opened(&resumed.events, CheckpointKind::ChangeRequest);
    assert_eq!(changes.len(), 2, "{changes:?}");
    assert!(!resumed.events.iter().any(|event| matches!(
        event.kind,
        RunEventKind::Checkpoint(CheckpointEvent::Withdrawn { .. })
    )));
    let yours = changes[1].id.clone();
    let (command, reply) = support::answer(
        &resumed.events,
        CheckpointKind::ChangeRequest,
        AnswerAction::Approve,
        None,
    )?;
    let accepted = crew.resume(resumed.events.clone(), vec![command]).await?;
    assert_eq!(reply.await?, Ok(()));
    let made: Vec<CheckpointId> = workflow_events(&accepted.events)
        .into_iter()
        .filter_map(|event| match event {
            WorkflowEvent::Changed(changed) => changed.checkpoint.clone(),
            _ => None,
        })
        .collect();
    assert_eq!(made, vec![yours]);
    Ok(())
}

/// The lead designs before anything runs, and code opens its pick with Understand: no nested
/// goal workflow, no Goal phase, and no Plan phase holding only a checkpoint.
#[tokio::test]
async fn the_lead_designs_first_and_understand_opens_the_run() -> anyhow::Result<()> {
    let mut crew = Crew::new()?;
    let goal = || {
        let listing =
            json!({"agreements": ["The public API gets a rate limit."], "differences": []});
        vec![
            (StepName::Restate, restatement()),
            (StepName::Differ, listing),
        ]
    };
    let params = json!([{"name": "check", "value": "goal_approved"}]);
    // The second design is the look at Understand's end, which keeps the workflow.
    let picks = vec![
        design(Some("one-look"), params.clone(), None),
        design(Some("one-look"), params, None),
    ];
    let lead = responder(&mut crew, "claude-code/opus", "anthropic", picks, goal());
    responder(&mut crew, "codex", "openai", Vec::new(), goal());
    crew.settings.workflow = WorkflowRef::new("goal-then-look");
    crew.settings.workflows.lead_designs = LeadDesigns::ANY;
    crew.settings.workflows.adapt_at_phase_ends = true;
    crew.repo
        .write(".argonauts/workflows/one-look.yaml", ONE_LOOK)?;
    crew.repo
        .write(".argonauts/workflows/goal-then-look.yaml", GOAL_THEN_LOOK)?;

    let outcome = crew.start(RunMode::Crew, "rate limit the API").await?;
    assert_eq!(designs(&lead), 1);
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
    let asked = lead
        .specs()
        .into_iter()
        .find(|spec| spec.step == Some(StepName::DesignWorkflow))
        .expect("the lead designed");
    assert!(
        asked
            .prompt
            .iter()
            .any(|part| part.text.contains("### one-look"))
    );
    // Understand ends with its one card.
    assert_eq!(opened(&outcome.events, CheckpointKind::Goal).len(), 1);
    assert!(opened(&outcome.events, CheckpointKind::Plan).is_empty());

    // Approved, it runs on; Understand's end asked the lead again, and nothing else asks.
    let (done, reply) =
        decide(&crew, &outcome, CheckpointKind::Goal, AnswerAction::Approve).await?;
    assert_eq!(reply, Ok(()));
    assert_eq!(designs(&lead), 2);
    assert!(opened(&done.events, CheckpointKind::ChangeRequest).is_empty());
    assert!(opened(&done.events, CheckpointKind::Plan).is_empty());
    assert!(workflow_events(&done.events).iter().any(|event| matches!(
        event,
        WorkflowEvent::StepFinished(finished) if finished.kind == StepKind::Gate
    )));
    ended(&done, RunOutcome::BranchLeft);
    Ok(())
}
