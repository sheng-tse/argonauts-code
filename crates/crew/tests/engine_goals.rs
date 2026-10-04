//! The two ways a crew run works, end to end over fake workers with the real
//! approval policy: a workflow that ends with its final check, a goal met on its second
//! attempt, a stuck goal, a workflow that keeps going as a goal, and Understand's questions in
//! supervised and in auto mode.

mod support;

use std::collections::VecDeque;
use std::path::Path;
use std::sync::{Arc, Mutex, PoisonError};

use anyhow::Context;
use serde_json::{Value, json};

use argonauts_core::engine::{EngineCommand, EngineExit};
use argonauts_core::events::card::{CardEvent, IntegrationEvent};
use argonauts_core::events::checkpoint::{
    AnswerAction, CheckpointEvent, CheckpointKind, CheckpointOpened, FreeText,
};
use argonauts_core::events::goal::{
    Decider, GoalAction, GoalEvent, GoalStatus, GoalVerdict, LET_THE_LEAD_ANSWER,
    LET_THE_LEAD_DECIDE,
};
use argonauts_core::events::run::{ParkReason, RunLifecycle, RunMode};
use argonauts_core::events::session::SessionEvent;
use argonauts_core::events::workflow::WorkflowEvent;
use argonauts_core::events::{RunEvent, RunEventKind};
use argonauts_core::names::WorkerRef;
use argonauts_core::settings::approvals::Answerer;
use argonauts_core::settings::protocol::{GateCommand, UnderstandWhen};
use argonauts_core::settings::workflows::StyleSetting;
use argonauts_core::steps::StepName;
use argonauts_core::worker::SessionSpec;
use argonauts_crew::engine::shared::with_git_env;
use argonauts_crew::goal::pursue::{
    self, DELIVER, KEEP, KEEP_GOING, ONE_MORE, STUCK, TURN_INTO_GOAL,
};
use argonauts_testkit::{FakeSession, FakeWorker};
use tokio::sync::oneshot;

use support::{Crew, Outcome};

const FILE: &str = "verbose.txt";

/// The CheckGoal answers, in order, whoever checks.
type Checks = Arc<Mutex<VecDeque<Value>>>;

/// What the crew answers beyond the defaults: Understand's question, tagged `tag` (a detail
/// when unset), and the lead's brief.
#[derive(Clone, Default)]
struct Own {
    question: Option<&'static str>,
    tag: Option<&'static str>,
    brief: Option<Value>,
}

fn met() -> Value {
    verdict("met", "test_verbose passes", "")
}

fn not_yet() -> Value {
    verdict("not_yet", "", "the flag isn't parsed")
}

fn verdict(state: &str, evidence: &str, missing: &str) -> Value {
    json!({
        "verdicts": [{"id": "G1", "verdict": state, "evidence": evidence, "missing": missing}],
        "overall": if state == "met" { "met" } else { "not_yet" },
        "reason": if state == "met" { "" } else { "the flag isn't parsed yet" },
    })
}

/// Two companies' agents in the crew and a third company's installed, gates that pass, and
/// the check's answers from `checks`.
fn crew(checks: Vec<Value>) -> anyhow::Result<(Crew, Checks)> {
    crew_with(checks, Own::default())
}

/// [`crew`], whose readings differ on a detail that `question` asks about.
fn crew_asking(
    checks: Vec<Value>,
    question: Option<&'static str>,
) -> anyhow::Result<(Crew, Checks)> {
    crew_with(
        checks,
        Own {
            question,
            ..Own::default()
        },
    )
}

fn crew_with(checks: Vec<Value>, own: Own) -> anyhow::Result<(Crew, Checks)> {
    crew_of(checks, own, true)
}

/// [`crew_with`], with a third company's agent installed or not.
fn crew_of(checks: Vec<Value>, own: Own, third_company: bool) -> anyhow::Result<(Crew, Checks)> {
    let mut crew = Crew::new()?;
    crew.settings.divide.contracts_first = false;
    let passing = |name: &str| GateCommand {
        name: name.to_owned(),
        command: vec!["true".to_owned()],
    };
    crew.settings.gates.run = vec![passing("tests")];
    crew.settings.gates.full_suite = vec![passing("suite")];
    crew.settings.workflows.final_check = true;
    let checks: Checks = Arc::new(Mutex::new(checks.into()));
    enlist(
        &mut crew,
        "claude-code/opus",
        "claude",
        &checks,
        own.clone(),
    );
    let others = Own { brief: None, ..own };
    enlist(&mut crew, "codex/gpt-x", "gpt", &checks, others);
    if !third_company {
        return Ok((crew, checks));
    }
    let shared = checks.clone();
    crew.services.workers.add(
        FakeWorker::harness("antigravity-cli", "gemini")
            .with_cheapest_model("gemini-3.6-flash-low")
            .respond_with(move |spec| respond(spec, &shared, &Own::default())),
    );
    Ok((crew, checks))
}

fn enlist(crew: &mut Crew, worker: &str, family: &str, checks: &Checks, own: Own) {
    let checks = checks.clone();
    crew.services.workers.add(
        FakeWorker::harness(worker, family).respond_with(move |spec| respond(spec, &checks, &own)),
    );
    let reference: WorkerRef = worker.parse().expect("a worker reference");
    if crew.settings.crew.is_empty() {
        crew.settings.default_agent = reference.clone();
    }
    crew.settings.crew.push(reference);
}

/// One card for `FILE`, written when it builds; the scripted checks; an attempt the lead names
/// `Parse Flag`; approvals that cite the rules; and the smallest valid answer otherwise.
fn respond(spec: &SessionSpec, checks: &Checks, own: &Own) -> FakeSession {
    let question = own.question;
    match spec.step {
        Some(StepName::DesignWorkflow) if own.brief.is_some() => {
            FakeSession::structured(own.brief.clone().unwrap_or_default())
        }
        Some(StepName::ModelApprover) => FakeSession::structured(json!({
            "verdict": "approve",
            "reasons": ["the evidence meets the rules"],
            "rules_cited": ["R7"]
        })),
        Some(StepName::CheckGoal) => {
            let next = checks
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .pop_front()
                .unwrap_or_else(met);
            FakeSession::structured(next)
        }
        Some(StepName::AnswerQuestions) => FakeSession::structured(json!({
            "answers": [{"n": 1, "option": 0, "why": "the README logs at debug", "needs_user": null}]
        })),
        Some(StepName::PlanAttempt) => FakeSession::structured(json!({
            "title": "Parse Flag",
            "detail": "one owner parses the flag, another company reviews it",
            "steps": "",
        })),
        Some(StepName::Build) => {
            std::fs::write(spec.workdir.join(FILE), "--verbose\n")
                .expect("the build session writes its file");
            smallest(spec)
        }
        Some(StepName::PlanToCards) => FakeSession::structured(json!({
            "cards": [{
                "key": "flag",
                "title": "Add the --verbose flag",
                "task": "Add a --verbose flag.",
                "kind": "build",
                "owned_paths": [FILE],
                "read_only_paths": [],
                "depends_on": [],
                "criteria": [],
            }]
        })),
        Some(StepName::Differ) if question.is_some() => FakeSession::structured(json!({
            "agreements": [],
            "differences": [{
                "topic": "the log level",
                "readings": [
                    {"label": "A", "reading": "debug"},
                    {"label": "B", "reading": "trace"}
                ],
                "tag": own.tag.unwrap_or("detail"),
                "question": {
                    "text": question,
                    "options": ["debug", "trace"],
                    "recommended": 0,
                    "basis": "A reads it as debug"
                }
            }]
        })),
        _ => smallest(spec),
    }
}

fn smallest(spec: &SessionSpec) -> FakeSession {
    match &spec.json_schema {
        Some(schema) => FakeSession::structured(support::fill(schema)),
        None => FakeSession::completed("done"),
    }
}

/// Runs to where it parks or finishes with the real policy, which keeps its records in the
/// repo's state folder.
async fn run(crew: &Crew) -> anyhow::Result<Outcome> {
    let repo = crew.repo_context()?;
    let state = repo
        .run_dir
        .parent()
        .and_then(Path::parent)
        .context("the run folder is under the repo's state folder")?;
    std::fs::create_dir_all(state)?;
    let started = crew.run_started(RunMode::Crew, "add a --verbose flag")?;
    let (mut context, sink, _commands) = crew.context(vec![started])?;
    let services = &crew.services;
    context.services.policy = argonauts_policy::approval_policy_with_models(
        context.settings.clone(),
        Arc::new(|| None),
        state,
        services.workers.clone(),
        services.registry.clone(),
        services.steps.clone(),
    );
    let engine = argonauts_crew::engine_for(&crew.settings, RunMode::Crew).run(context);
    let exit = with_git_env(crew.git_env(), engine).await;
    Ok(Outcome {
        exit,
        events: sink.events(),
    })
}

fn goal_events(events: &[RunEvent]) -> Vec<&GoalEvent> {
    events
        .iter()
        .filter_map(|event| match &event.kind {
            RunEventKind::Goal(goal) => Some(goal),
            _ => None,
        })
        .collect()
}

fn checked(events: &[RunEvent]) -> Vec<(u32, GoalVerdict)> {
    goal_events(events)
        .into_iter()
        .filter_map(|event| match event {
            GoalEvent::Checked(checked) => Some((checked.attempt, checked.overall)),
            _ => None,
        })
        .collect()
}

fn answered(events: &[RunEvent]) -> Vec<(String, String, Decider)> {
    goal_events(events)
        .into_iter()
        .filter_map(|event| match event {
            GoalEvent::Answered {
                question,
                answer,
                by,
            } => Some((question.clone(), answer.clone(), *by)),
            _ => None,
        })
        .collect()
}

/// `(label, worker)` of every session that started.
fn sessions(events: &[RunEvent]) -> Vec<(String, WorkerRef)> {
    events
        .iter()
        .filter_map(|event| match &event.kind {
            RunEventKind::Session(SessionEvent::Started(started)) => Some((
                started.label.clone().unwrap_or_default(),
                started.worker.clone(),
            )),
            _ => None,
        })
        .collect()
}

/// The phase titles of the newest declaration.
fn declared(events: &[RunEvent]) -> Vec<String> {
    events
        .iter()
        .rev()
        .find_map(|event| match &event.kind {
            RunEventKind::Workflow(WorkflowEvent::Declared(declared)) => Some(
                declared
                    .phases
                    .iter()
                    .map(|phase| phase.title.clone())
                    .collect(),
            ),
            _ => None,
        })
        .unwrap_or_default()
}

fn landed(events: &[RunEvent]) -> bool {
    events.iter().any(|event| {
        matches!(
            event.kind,
            RunEventKind::Integration(IntegrationEvent::Landed { .. })
        )
    })
}

fn cards(events: &[RunEvent]) -> Vec<(String, Option<WorkerRef>)> {
    events
        .iter()
        .filter_map(|event| match &event.kind {
            RunEventKind::Card(CardEvent::Created(spec)) => {
                Some((spec.id.to_string(), spec.owner.clone()))
            }
            _ => None,
        })
        .collect()
}

fn finished(outcome: &Outcome) {
    assert!(
        matches!(outcome.exit, EngineExit::Finished(_)),
        "{:?}: {:?}",
        outcome.exit,
        support::names(&outcome.events)
    );
}

#[tokio::test]
async fn a_workflow_ends_with_its_final_check_from_another_company() -> anyhow::Result<()> {
    let (mut crew, _) = crew(vec![met()])?;
    // A third company reviews, so one that wrote nothing is there to check whoever builds.
    crew.settings.roles.insert(
        argonauts_core::names::RoleName::Reviewer,
        argonauts_core::settings::roles::RoleSettings {
            workers: vec!["antigravity-cli".parse()?],
            ..Default::default()
        },
    );
    let outcome = run(&crew).await?;
    finished(&outcome);
    let events = &outcome.events;
    assert!(landed(events));

    // The Check phase comes right before Merge; nothing else was added.
    let phases = declared(events);
    let at = phases.iter().position(|title| title == "Check");
    assert_eq!(
        at.map(|at| phases[at + 1].as_str()),
        Some("Merge"),
        "{phases:?}"
    );
    assert!(!phases.iter().any(|title| title == "Goal"), "{phases:?}");

    assert_eq!(checked(events), [(1, GoalVerdict::Met)]);
    let checker = sessions(events)
        .into_iter()
        .find(|(label, _)| label == "check:final")
        .map(|(_, worker)| worker)
        .context("a check:final session")?;
    assert_ne!(
        checker.worker, "claude-code",
        "the lead's company checks only when no other company wrote nothing"
    );
    assert!(
        cards(events)
            .iter()
            .all(|(_, owner)| owner.as_ref() != Some(&checker)),
        "a card's author checked it"
    );
    // Nobody was asked whether to keep going.
    assert!(
        answered(events)
            .iter()
            .all(|(question, ..)| question != KEEP_GOING)
    );
    Ok(())
}

#[tokio::test]
async fn a_goal_is_met_on_its_second_attempt() -> anyhow::Result<()> {
    let (mut crew, checks) = crew(vec![not_yet(), met()])?;
    crew.settings.workflows.style = StyleSetting::Goal;
    let outcome = run(&crew).await?;
    finished(&outcome);
    let events = &outcome.events;
    assert!(landed(events));

    // Understand, the lead's two attempts, then Merge: no Plan, no Check phase.
    assert_eq!(
        declared(events),
        ["Understand", "Attempt 1", "Parse Flag", "Merge"]
    );
    let goal = goal_events(events);
    assert!(goal.iter().any(|event| matches!(event, GoalEvent::Set(_))));
    let attempts: Vec<(u32, String)> = goal
        .iter()
        .filter_map(|event| match event {
            GoalEvent::AttemptStarted { n, title, .. } => Some((*n, title.clone())),
            _ => None,
        })
        .collect();
    assert_eq!(
        attempts,
        [(1, "Attempt 1".to_owned()), (2, "Parse Flag".to_owned())]
    );
    assert_eq!(
        checked(events),
        [(1, GoalVerdict::NotYet), (2, GoalVerdict::Met)]
    );
    assert!(goal.iter().any(|event| matches!(
        event,
        GoalEvent::StatusChanged {
            status: GoalStatus::Met,
            ..
        }
    )));
    let labels: Vec<String> = sessions(events)
        .into_iter()
        .map(|(label, _)| label)
        .collect();
    for label in ["check:1", "check:2", "lead:attempt"] {
        assert!(
            labels.iter().any(|seen| seen == label),
            "{label}: {labels:?}"
        );
    }
    // Each attempt builds its own card, on what the last one integrated.
    let built: Vec<String> = cards(events).into_iter().map(|(id, _)| id).collect();
    assert_eq!(built, ["attempt-1", "attempt-2"]);
    // A `›` note says why the plan moved.
    assert!(events.iter().any(|event| matches!(
        &event.kind,
        RunEventKind::Workflow(WorkflowEvent::Note(note)) if note.text.contains("attempt 2")
    )));
    assert!(
        checks
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .is_empty()
    );
    Ok(())
}

#[tokio::test]
async fn a_stuck_goal_takes_one_more_attempt_then_delivers_what_holds() -> anyhow::Result<()> {
    let (mut crew, _) = crew(vec![not_yet(), not_yet(), not_yet()])?;
    crew.settings.workflows.style = StyleSetting::Goal;
    crew.settings.goal.stuck_after = 2;
    let outcome = run(&crew).await?;
    let events = &outcome.events;
    assert!(
        !matches!(outcome.exit, EngineExit::Crashed(_)),
        "{:?}",
        outcome.exit
    );

    // Stuck after the same blocker twice; in auto the lead takes one more attempt, then
    // delivers what holds. Nobody asks you.
    let stuck: Vec<(String, Decider)> = answered(events)
        .into_iter()
        .filter(|(question, ..)| question == STUCK)
        .map(|(_, answer, by)| (answer, by))
        .collect();
    assert_eq!(
        stuck,
        [
            (ONE_MORE.to_owned(), Decider::Lead),
            (DELIVER.to_owned(), Decider::Lead)
        ]
    );
    assert_eq!(checked(events).len(), 3);
    // The extra attempt goes to another company's model than the one that got stuck.
    let built = cards(events);
    let third = built
        .iter()
        .find(|(id, _)| id == "attempt-3")
        .and_then(|(_, owner)| owner.clone())
        .context("attempt 3's card")?;
    assert_ne!(third.worker, "claude-code");
    assert!(
        events
            .iter()
            .all(|event| !matches!(&event.kind, RunEventKind::Checkpoint(CheckpointEvent::Opened(opened)) if opened.title == STUCK))
    );
    Ok(())
}

#[tokio::test]
async fn a_workflow_with_a_criterion_unmet_keeps_going_as_a_goal() -> anyhow::Result<()> {
    let (crew, _) = crew(vec![not_yet(), met()])?;
    let outcome = run(&crew).await?;
    finished(&outcome);
    let events = &outcome.events;
    assert!(landed(events));
    assert_eq!(
        answered(events)
            .into_iter()
            .filter(|(question, ..)| question == KEEP_GOING)
            .map(|(_, answer, by)| (answer, by))
            .collect::<Vec<_>>(),
        [(KEEP.to_owned(), Decider::Lead)]
    );
    // The workflow is attempt 1, so the goal's first attempt of its own is attempt 2, added
    // after the Check and before Merge.
    let phases = declared(events);
    let check = phases.iter().position(|title| title == "Check");
    assert_eq!(
        check.map(|at| phases[at + 1..].to_vec()),
        Some(vec!["Parse Flag".to_owned(), "Merge".to_owned()]),
        "{phases:?}"
    );
    assert_eq!(
        checked(events),
        [(1, GoalVerdict::NotYet), (2, GoalVerdict::Met)]
    );
    let set = goal_events(events)
        .into_iter()
        .find_map(|event| match event {
            GoalEvent::Set(goal) => Some(goal.clone()),
            _ => None,
        });
    let set = set.context("the goal was set")?;
    assert_eq!(set.attempts.len(), 1);
    assert_eq!(set.criteria.len(), 1);
    Ok(())
}

/// The newest open checkpoint.
fn open(events: &[RunEvent]) -> Option<CheckpointOpened> {
    let mut open = Vec::new();
    for event in events {
        match &event.kind {
            RunEventKind::Checkpoint(CheckpointEvent::Opened(opened)) => {
                open.push((**opened).clone());
            }
            RunEventKind::Checkpoint(CheckpointEvent::Decided(decided)) => {
                open.retain(|opened| opened.id != decided.id);
            }
            RunEventKind::Checkpoint(CheckpointEvent::Withdrawn { id, .. }) => {
                open.retain(|opened| &opened.id != id);
            }
            _ => {}
        }
    }
    open.pop()
}

#[tokio::test]
async fn in_supervised_mode_understand_asks_choose_only_and_you_can_hand_it_over()
-> anyhow::Result<()> {
    let (mut crew, _) = crew_asking(Vec::new(), Some("Which log level?"))?;
    crew.settings.approvals.answered_by = Some(Answerer::You);
    let outcome = crew.start(RunMode::Crew, "add a --verbose flag").await?;
    let card = open(&outcome.events).context("Understand's question")?;
    assert_eq!(card.kind, CheckpointKind::Goal);
    // Choose-only: the answers, then the lead, and nothing typed.
    let labels: Vec<&str> = card
        .options
        .iter()
        .map(|option| option.label.as_str())
        .collect();
    assert_eq!(labels, ["debug", "trace", LET_THE_LEAD_DECIDE]);
    assert!(
        card.options
            .iter()
            .all(|option| option.action != AnswerAction::Answer)
    );
    assert_eq!(card.free_text, FreeText::None);

    // Handing it to the lead: it answers, and the go-ahead comes to you.
    let (command, reply) = support::answer(
        &outcome.events,
        CheckpointKind::Goal,
        AnswerAction::Choose {
            value: LET_THE_LEAD_DECIDE.to_owned(),
        },
        None,
    )?;
    let next = crew.resume(outcome.events.clone(), vec![command]).await?;
    assert_eq!(reply.await?, Ok(()));
    let go_ahead = open(&next.events).context("the go-ahead")?;
    assert_eq!(go_ahead.title, "Run this workflow?");
    assert_eq!(go_ahead.free_text, FreeText::None);
    assert!(
        go_ahead
            .options
            .iter()
            .all(|option| option.label != "Approve with edits")
    );
    let from_lead: Vec<(String, Decider)> = answered(&next.events)
        .into_iter()
        .map(|(question, _, by)| (question, by))
        .collect();
    assert_eq!(from_lead, [("Which log level?".to_owned(), Decider::Lead)]);

    // "Yes, and let the lead answer" approves the goal and says so in the record.
    let (command, reply) = support::answer(
        &next.events,
        CheckpointKind::Goal,
        AnswerAction::Choose {
            value: LET_THE_LEAD_ANSWER.to_owned(),
        },
        None,
    )?;
    let after = crew.resume(next.events.clone(), vec![command]).await?;
    assert_eq!(reply.await?, Ok(()));
    assert!(
        goal_events(&after.events)
            .iter()
            .any(|event| matches!(event, GoalEvent::Approved { .. }))
    );
    assert!(
        answered(&after.events)
            .iter()
            .any(|(_, answer, by)| answer == LET_THE_LEAD_ANSWER && *by == Decider::You)
    );
    Ok(())
}

#[tokio::test]
async fn in_auto_mode_the_lead_answers_understands_question_and_it_is_recorded()
-> anyhow::Result<()> {
    let (crew, _) = crew_asking(vec![met()], Some("Which log level?"))?;
    let outcome = run(&crew).await?;
    finished(&outcome);
    let events = &outcome.events;
    let questions: Vec<(String, Decider)> = answered(events)
        .into_iter()
        .filter(|(question, ..)| question == "Which log level?")
        .map(|(question, _, by)| (question, by))
        .collect();
    assert_eq!(questions, [("Which log level?".to_owned(), Decider::Lead)]);
    // No question card opened: the lead answered it, and the approver checked the goal.
    assert!(events.iter().all(|event| !matches!(
        &event.kind,
        RunEventKind::Checkpoint(CheckpointEvent::Opened(opened))
            if opened.kind == CheckpointKind::Goal && opened.title != "Run this workflow?"
    )));
    Ok(())
}

#[tokio::test]
async fn keep_going_asks_you_when_you_answer_the_crews_questions() -> anyhow::Result<()> {
    let (mut crew, _) = crew(vec![not_yet()])?;
    crew.settings.approvals.answered_by = Some(Answerer::You);
    let mut outcome = crew.start(RunMode::Crew, "add a --verbose flag").await?;
    // Answer each card with its recommended answer until the keep-going card shows.
    for _ in 0..8 {
        let card = open(&outcome.events).context("a card waits")?;
        if card.title == KEEP_GOING {
            assert_eq!(card.kind, CheckpointKind::Goal);
            assert_eq!(card.options[0].label, KEEP);
            assert!(card.options[0].recommended);
            assert_eq!(card.free_text, FreeText::None);
            return Ok(());
        }
        let (command, reply) = support::recommended(&outcome.events).context("an answer")?;
        outcome = crew.resume(outcome.events.clone(), vec![command]).await?;
        assert_eq!(reply.await?, Ok(()));
    }
    anyhow::bail!(
        "the keep-going card never opened: {:?}",
        support::names(&outcome.events)
    )
}

/// Answers each card that waits with its recommended answer, `commands` going in with the
/// first, until the run finishes or `rounds` run out.
async fn drive(
    crew: &Crew,
    mut outcome: Outcome,
    mut commands: Vec<EngineCommand>,
    rounds: usize,
) -> anyhow::Result<Outcome> {
    for _ in 0..rounds {
        if !matches!(outcome.exit, EngineExit::Parked(_)) {
            break;
        }
        let Some((command, _reply)) = support::recommended(&outcome.events) else {
            break;
        };
        commands.push(command);
        outcome = crew
            .resume(outcome.events.clone(), std::mem::take(&mut commands))
            .await?;
    }
    Ok(outcome)
}

fn goal_command(action: GoalAction) -> (EngineCommand, oneshot::Receiver<Result<(), String>>) {
    let (reply, replied) = oneshot::channel();
    (EngineCommand::Goal { action, reply }, replied)
}

#[tokio::test]
async fn the_leads_goal_brief_plans_its_first_attempt() -> anyhow::Result<()> {
    let mut brief = support::brief(Some("peer-crew"), json!([]), None, "one card is enough");
    brief["first_attempt"] = json!({
        "title": "Add Flag",
        "detail": "one owner adds the flag, another company reviews it",
        "steps": "- { kind: build, input: goal, label: \"add:{item}\", different_family_from: author }\n",
    });
    let (mut crew, _) = crew_with(
        vec![met()],
        Own {
            brief: Some(brief),
            ..Own::default()
        },
    )?;
    crew.settings.workflows.style = StyleSetting::Goal;
    crew.settings.workflows.lead_designs = true;
    crew.settings.workflows.adapt_at_phase_ends = false;
    let outcome = run(&crew).await?;
    finished(&outcome);
    let events = &outcome.events;
    assert_eq!(declared(events), ["Understand", "Add Flag", "Merge"]);
    // The brief names the run and its done-when list, which the goal takes.
    let named = events.iter().rev().find_map(|event| match &event.kind {
        RunEventKind::Run(RunLifecycle::Named(named)) => Some(named.clone()),
        _ => None,
    });
    let named = named.context("the run was named")?;
    assert_eq!(named.name, "look-at-it");
    assert_eq!(named.done_when.len(), 1);
    assert_eq!(checked(events), [(1, GoalVerdict::Met)]);
    let labels: Vec<String> = sessions(events)
        .into_iter()
        .map(|(label, _)| label)
        .collect();
    assert!(
        labels.iter().any(|label| label.starts_with("add:")),
        "{labels:?}"
    );
    Ok(())
}

#[tokio::test]
async fn you_can_turn_a_workflow_into_a_goal_while_it_runs() -> anyhow::Result<()> {
    let (mut crew, _) = crew(vec![not_yet(), met()])?;
    // No final check of its own: turning it into a goal adds one.
    crew.settings.workflows.final_check = false;
    let outcome = crew.start(RunMode::Crew, "add a --verbose flag").await?;
    assert!(matches!(outcome.exit, EngineExit::Parked(_)));
    assert!(
        !declared(&outcome.events)
            .iter()
            .any(|title| title == "Check")
    );
    let (turn, replied) = goal_command(GoalAction::TurnIntoGoal);
    let done = drive(&crew, outcome, vec![turn], 8).await?;
    assert_eq!(replied.await?, Ok(()));
    let events = &done.events;
    assert!(
        answered(events)
            .iter()
            .any(|(question, _, by)| question == TURN_INTO_GOAL && *by == Decider::You)
    );
    // Nobody is asked whether to keep going: you already said so.
    assert!(
        answered(events)
            .iter()
            .all(|(question, ..)| question != KEEP_GOING)
    );
    let phases = declared(events);
    let check = phases.iter().position(|title| title == "Check");
    assert_eq!(
        check.map(|at| phases[at + 1..].to_vec()),
        Some(vec!["Parse Flag".to_owned(), "Merge".to_owned()]),
        "{phases:?}"
    );
    assert_eq!(
        checked(events),
        [(1, GoalVerdict::NotYet), (2, GoalVerdict::Met)]
    );
    Ok(())
}

#[tokio::test]
async fn an_assumption_you_change_is_recorded_and_told_to_the_crew() -> anyhow::Result<()> {
    let (crew, _) = crew_asking(Vec::new(), Some("Which log level?"))?;
    // In auto the lead answers the question, which becomes assumption 1 once you approve.
    let outcome = crew.start(RunMode::Crew, "add a --verbose flag").await?;
    let outcome = drive(&crew, outcome, Vec::new(), 1).await?;
    let approved = goal_events(&outcome.events)
        .into_iter()
        .find_map(|event| match event {
            GoalEvent::Approved { file, .. } => Some(file.clone()),
            _ => None,
        })
        .context("the goal was approved")?;
    assert!(!approved.assumptions.is_empty(), "{approved:?}");
    let (change, replied) = goal_command(GoalAction::ChangeAssumption {
        id: 1,
        to: "Which log level: trace".to_owned(),
    });
    let next = crew.resume(outcome.events.clone(), vec![change]).await?;
    assert_eq!(replied.await?, Ok(()));
    assert!(goal_events(&next.events).iter().any(|event| matches!(
        event,
        GoalEvent::AssumptionChanged {
            id: 1,
            by: Decider::You,
            ..
        }
    )));
    // The crew hears it as your guidance.
    assert!(
        next.events
            .iter()
            .any(|event| matches!(&event.kind, RunEventKind::Blackboard(_)))
    );
    let (bad, replied) = goal_command(GoalAction::ChangeAssumption {
        id: 9,
        to: "anything".to_owned(),
    });
    crew.resume(next.events.clone(), vec![bad]).await?;
    assert!(replied.await?.is_err());
    Ok(())
}

/// A pause, or a usage limit, pauses the goal along with its run, and it's pursued again when
/// the run goes on.
#[tokio::test]
async fn a_goal_pauses_with_its_run_and_goes_on_with_it() -> anyhow::Result<()> {
    let (crew, _) = crew(Vec::new())?;
    let started = crew.run_started(RunMode::Crew, "add a --verbose flag")?;
    let goal = argonauts_core::events::goal::GoalRun {
        objective: "add a --verbose flag".to_owned(),
        criteria: vec![argonauts_core::events::goal::Criterion::new(
            "G1", "it works",
        )],
        attempts: Vec::new(),
        status: GoalStatus::Pursuing,
        max_attempts: 5,
    };
    let set = RunEvent {
        seq: 1,
        kind: RunEventKind::Goal(GoalEvent::Set(Box::new(goal))),
        ..started.clone()
    };
    let (host, sink) = crew.host_over(vec![started, set])?;
    // Waiting on a card isn't a pause.
    pursue::on_park(&host, &ParkReason::Checkpoint)?;
    pursue::on_park(&host, &ParkReason::Paused)?;
    pursue::on_resume(&host)?;
    let statuses: Vec<GoalStatus> = goal_events(&sink.events())
        .into_iter()
        .filter_map(|event| match event {
            GoalEvent::StatusChanged { status, .. } => Some(*status),
            _ => None,
        })
        .collect();
    assert_eq!(statuses, [GoalStatus::Paused, GoalStatus::Pursuing]);
    Ok(())
}

const FIX_THEN_REVIEW: &str = "name: fix-then-review\ndescription: One fix, reviewed in a phase \
    of its own\nphases:\n  - title: Fix\n    detail: one owner fixes it\n    standard: do\n    \
    steps:\n      - { kind: checkpoint, checkpoint: goal }\n      - { kind: build, input: goal, \
    different_family_from: author, label: \"fix:{item}\", review_in: Review }\n  - title: \
    Review\n    detail: another company reviews the fix\n    standard: check\n    steps: []\n  \
    - title: Merge\n    detail: the merge\n    standard: decide\n    steps:\n      - { kind: \
    merge, label: \"merge:{branch}\" }\n";

/// A build that shows its reviews in a later phase runs as a pipeline: that phase starts with
/// the first review, while the build's own phase is still running.
#[tokio::test]
async fn a_review_phase_starts_with_its_first_review() -> anyhow::Result<()> {
    let (mut crew, _) = crew(Vec::new())?;
    crew.settings.workflow = argonauts_core::workflow::WorkflowRef::new("fix-then-review");
    crew.repo
        .write(".argonauts/workflows/fix-then-review.yaml", FIX_THEN_REVIEW)?;
    let outcome = crew.start(RunMode::Crew, "add a --verbose flag").await?;
    let outcome = drive(&crew, outcome, Vec::new(), 4).await?;
    let marks: Vec<(String, &str)> = outcome
        .events
        .iter()
        .filter_map(|event| match &event.kind {
            RunEventKind::Workflow(WorkflowEvent::PhaseStarted(mark)) => {
                Some((mark.title.clone(), "started"))
            }
            RunEventKind::Workflow(WorkflowEvent::PhaseEnded { phase, .. }) => {
                Some((phase.title.clone(), "ended"))
            }
            _ => None,
        })
        .collect();
    let at = |title: &str, what: &str| {
        marks
            .iter()
            .position(|(seen, mark)| seen == title && *mark == what)
    };
    let review_started = at("Review", "started").with_context(|| {
        format!(
            "Review never started: {:?} {:?}",
            outcome.exit,
            support::names(&outcome.events)
        )
    })?;
    let fix_ended = at("Fix", "ended").context("Fix ended")?;
    assert!(review_started < fix_ended, "{marks:?}");
    assert!(at("Review", "ended").is_some(), "{marks:?}");
    Ok(())
}

/// With `understand.when = "lead"`, a lead that skips Understand brings its own open questions
/// to the card before anything writes.
#[tokio::test]
async fn the_leads_own_questions_come_to_the_card_when_it_skips_understand() -> anyhow::Result<()> {
    let mut brief = support::brief(Some("solo-reviewed"), json!([]), None, "one small fix");
    brief["questions"] = json!([{
        "topic": "Log level",
        "text": "Which log level does --verbose turn on?",
        "options": ["debug", "trace"],
    }]);
    let (mut crew, _) = crew_with(
        Vec::new(),
        Own {
            brief: Some(brief),
            ..Own::default()
        },
    )?;
    crew.settings.workflows.lead_designs = true;
    crew.settings.workflows.adapt_at_phase_ends = false;
    crew.settings.understand.when = UnderstandWhen::Lead;
    crew.settings.approvals.answered_by = Some(Answerer::You);
    let outcome = crew.start(RunMode::Crew, "add a --verbose flag").await?;
    // No readings ran: the solo example's own reading went, and the lead's question waits.
    assert!(
        !declared(&outcome.events)
            .iter()
            .any(|title| title == "Understand")
    );
    let card = open(&outcome.events).context("the lead's question")?;
    assert_eq!(card.kind, CheckpointKind::Goal);
    let labels: Vec<&str> = card
        .options
        .iter()
        .map(|option| option.label.as_str())
        .collect();
    assert_eq!(labels, ["debug", "trace", LET_THE_LEAD_DECIDE]);
    assert!(card.options[0].recommended);
    Ok(())
}

/// A crew run whose lead designs it from the solo-reviewed example and asks nothing, with
/// Understand the lead's to add or off, so nothing reads the request.
fn unread(when: UnderstandWhen, style: StyleSetting, yours: bool) -> anyhow::Result<Crew> {
    let brief = support::brief(Some("solo-reviewed"), json!([]), None, "one small fix");
    let (mut crew, _) = crew_with(
        vec![met()],
        Own {
            brief: Some(brief),
            ..Own::default()
        },
    )?;
    crew.settings.workflows.lead_designs = true;
    crew.settings.workflows.adapt_at_phase_ends = false;
    crew.settings.workflows.style = style;
    crew.settings.understand.when = when;
    if yours {
        crew.settings.approvals.answered_by = Some(Answerer::You);
    }
    Ok(crew)
}

/// Where the first event `matches` is in the log.
fn first(events: &[RunEvent], matches: impl Fn(&RunEventKind) -> bool) -> Option<usize> {
    events.iter().position(|event| matches(&event.kind))
}

fn build_started(kind: &RunEventKind) -> bool {
    matches!(kind, RunEventKind::Session(SessionEvent::Started(started))
        if started.step == Some(StepName::Build))
}

fn go_ahead_title(style: StyleSetting) -> &'static str {
    match style {
        StyleSetting::Goal => "Pursue this goal?",
        _ => "Run this workflow?",
    }
}

const UNREAD: [(UnderstandWhen, StyleSetting); 4] = [
    (UnderstandWhen::Lead, StyleSetting::Workflow),
    (UnderstandWhen::Lead, StyleSetting::Goal),
    (UnderstandWhen::Off, StyleSetting::Workflow),
    (UnderstandWhen::Off, StyleSetting::Goal),
];

#[tokio::test]
async fn with_nothing_read_a_supervised_run_asks_the_go_ahead_before_it_builds()
-> anyhow::Result<()> {
    for (when, style) in UNREAD {
        let crew = unread(when, style, true)?;
        let outcome = crew.start(RunMode::Crew, "add a --verbose flag").await?;
        let card = open(&outcome.events)
            .with_context(|| format!("{when:?} {style:?}: the go-ahead waits"))?;
        assert_eq!(card.kind, CheckpointKind::Goal, "{when:?} {style:?}");
        assert_eq!(card.title, go_ahead_title(style), "{when:?} {style:?}");
        assert!(
            first(&outcome.events, build_started).is_none(),
            "{when:?} {style:?}: a build started before the go-ahead"
        );
        // Yes runs it, through its check and the merge.
        let after = drive(&crew, outcome, Vec::new(), 8).await?;
        finished(&after);
        assert!(landed(&after.events), "{when:?} {style:?}");
    }
    Ok(())
}

#[tokio::test]
async fn with_nothing_read_an_auto_run_gets_the_go_ahead_before_it_builds() -> anyhow::Result<()> {
    for (when, style) in UNREAD {
        let crew = unread(when, style, false)?;
        let outcome = run(&crew).await?;
        finished(&outcome);
        let events = &outcome.events;
        let asked = first(events, |kind| {
            matches!(kind, RunEventKind::Checkpoint(CheckpointEvent::Opened(opened))
                if opened.title == go_ahead_title(style))
        })
        .with_context(|| format!("{when:?} {style:?}: no go-ahead"))?;
        let approved = first(events, |kind| {
            matches!(kind, RunEventKind::Goal(GoalEvent::Approved { .. }))
        })
        .with_context(|| format!("{when:?} {style:?}: no approved goal"))?;
        let built = first(events, build_started)
            .with_context(|| format!("{when:?} {style:?}: nothing built"))?;
        assert!(asked < approved && approved < built, "{when:?} {style:?}");
        assert!(landed(events), "{when:?} {style:?}");
    }
    Ok(())
}

#[tokio::test]
async fn in_auto_mode_the_lead_answers_an_always_ask_question_too() -> anyhow::Result<()> {
    let (crew, _) = crew_with(
        vec![met()],
        Own {
            question: Some("Which flags does the change cover?"),
            tag: Some("scope"),
            ..Own::default()
        },
    )?;
    assert!(
        crew.settings
            .understand
            .always_ask
            .iter()
            .any(|topic| topic == "scope")
    );
    let outcome = run(&crew).await?;
    finished(&outcome);
    let questions: Vec<(String, Decider)> = answered(&outcome.events)
        .into_iter()
        .filter(|(question, ..)| question == "Which flags does the change cover?")
        .map(|(question, _, by)| (question, by))
        .collect();
    assert_eq!(
        questions,
        [(
            "Which flags does the change cover?".to_owned(),
            Decider::Lead
        )]
    );
    assert!(landed(&outcome.events));
    Ok(())
}

#[tokio::test]
async fn ask_you_about_keeps_a_topic_yours_in_auto_mode() -> anyhow::Result<()> {
    let (mut crew, _) = crew_with(
        vec![met()],
        Own {
            question: Some("Who may turn on --verbose?"),
            tag: Some("security"),
            ..Own::default()
        },
    )?;
    crew.settings.understand.always_ask = Vec::new();
    crew.settings.approvals.ask_you_about = vec!["Security".to_owned()];
    let outcome = run(&crew).await?;
    assert!(
        matches!(outcome.exit, EngineExit::Parked(_)),
        "{:?}",
        outcome.exit
    );
    let card = open(&outcome.events).context("the security question waits for you")?;
    assert_eq!(card.kind, CheckpointKind::Goal);
    assert!(
        card.title.starts_with("Who may turn on --verbose?")
            || card.title.starts_with("What should the goal say?"),
        "{}",
        card.title
    );
    assert!(
        answered(&outcome.events)
            .iter()
            .all(|(question, ..)| question != "Who may turn on --verbose?")
    );
    assert!(first(&outcome.events, build_started).is_none());
    Ok(())
}

#[tokio::test]
async fn ask_you_about_the_go_ahead_brings_it_to_you_in_auto_mode() -> anyhow::Result<()> {
    let mut crew = unread(UnderstandWhen::Lead, StyleSetting::Workflow, false)?;
    crew.settings.approvals.ask_you_about = vec!["go-ahead".to_owned()];
    let outcome = run(&crew).await?;
    assert!(
        matches!(outcome.exit, EngineExit::Parked(_)),
        "{:?}",
        outcome.exit
    );
    let card = open(&outcome.events).context("the go-ahead waits for you")?;
    assert_eq!(card.title, "Run this workflow?");
    assert!(first(&outcome.events, build_started).is_none());
    Ok(())
}

/// Where the session labeled `label` sits in the workflow.
fn step_of(events: &[RunEvent], label: &str) -> Option<argonauts_core::workflow::StepRef> {
    events.iter().find_map(|event| match &event.kind {
        RunEventKind::Session(SessionEvent::Started(started))
            if started.label.as_deref() == Some(label) =>
        {
            started.workflow_step.clone()
        }
        _ => None,
    })
}

#[tokio::test]
async fn a_cards_agents_go_by_its_item_and_its_review_sits_with_its_build() -> anyhow::Result<()> {
    let (mut crew, _) = crew(vec![met()])?;
    crew.settings.divide.predictions = true;
    let outcome = run(&crew).await?;
    finished(&outcome);
    let events = &outcome.events;
    let labels: Vec<String> = sessions(events)
        .into_iter()
        .map(|(label, _)| label)
        .collect();
    // The planner keyed its card `flag`, titled "Add the --verbose flag".
    for expected in ["build:flag", "review:flag", "predict:flag"] {
        assert!(
            labels.iter().any(|label| label == expected),
            "{expected}: {labels:?}"
        );
    }
    assert!(
        labels.iter().all(|label| !label.contains("card-")),
        "{labels:?}"
    );
    let build = step_of(events, "build:flag").context("the build's step")?;
    assert_eq!(step_of(events, "review:flag"), Some(build));
    // The view's test draws this run, so it stays what a default crew run records.
    if std::env::var_os("ARGONAUTS_RECORD_FIXTURES").is_some() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../app/tests/fixtures/runs/default-crew-run.jsonl");
        std::fs::create_dir_all(path.parent().context("the fixtures folder")?)?;
        let lines: Vec<String> = events
            .iter()
            .map(serde_json::to_string)
            .collect::<Result<_, _>>()?;
        std::fs::write(&path, lines.join("\n") + "\n")?;
    }
    Ok(())
}

#[tokio::test]
async fn a_name_one_of_the_repos_recent_runs_has_takes_a_number() -> anyhow::Result<()> {
    let crew = unread(UnderstandWhen::Lead, StyleSetting::Workflow, true)?;
    // An earlier run of this repo already goes by the brief's name.
    let runs = crew.repo_context()?.run_dir;
    let earlier = runs
        .parent()
        .context("the runs folder")?
        .join("run-20260101-000000-0001");
    std::fs::create_dir_all(&earlier)?;
    let mut named = crew.run_started(RunMode::Crew, "an earlier run")?;
    named.seq = 1;
    named.kind = RunEventKind::Run(RunLifecycle::Named(argonauts_core::events::run::RunNamed {
        name: "look-at-it".to_owned(),
        description: "Look at the repository".to_owned(),
        style: argonauts_core::settings::workflows::RunStyle::Workflow,
        based_on: None,
        done_when: Vec::new(),
    }));
    std::fs::write(
        earlier.join("events.jsonl"),
        serde_json::to_string(&named)? + "\n",
    )?;
    let outcome = crew.start(RunMode::Crew, "add a --verbose flag").await?;
    let names: Vec<String> = outcome
        .events
        .iter()
        .filter_map(|event| match &event.kind {
            RunEventKind::Run(RunLifecycle::Named(named)) => Some(named.name.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(
        names.last().map(String::as_str),
        Some("look-at-it-2"),
        "{names:?}"
    );
    Ok(())
}

#[tokio::test]
async fn no_attempt_is_checked_by_a_model_that_wrote_it() -> anyhow::Result<()> {
    // Claude Code leads and Codex is the only other company, the common setup.
    let (mut crew, _) = crew_of(vec![not_yet(), not_yet(), met()], Own::default(), false)?;
    crew.settings.workflows.style = StyleSetting::Goal;
    crew.settings.goal.stuck_after = 2;
    // With both companies reading the request, no approver is left to approve the goal in your
    // place; this is about the checks.
    crew.settings.understand.when = UnderstandWhen::Off;
    let outcome = run(&crew).await?;
    let events = &outcome.events;
    let owners = cards(events);
    let checkers = sessions(events);
    let mut seen = 0;
    for n in 1..=3 {
        let Some(owner) = owners
            .iter()
            .find(|(id, _)| *id == format!("attempt-{n}"))
            .and_then(|(_, owner)| owner.clone())
        else {
            continue;
        };
        let checker = checkers
            .iter()
            .find(|(label, _)| *label == format!("check:{n}"))
            .map(|(_, worker)| worker.clone())
            .with_context(|| {
                format!(
                    "check:{n}: {checkers:?} {:?} {:?} {:?}",
                    checked(events),
                    outcome.exit,
                    support::names(events)
                )
            })?;
        assert_ne!(checker, owner, "attempt {n} was checked by its own author");
        seen += 1;
    }
    // The stuck goal's extra attempt went to Codex, and so was checked by the lead.
    assert_eq!(
        seen,
        3,
        "{owners:?} {:?} {:?}",
        outcome.exit,
        support::names(events)
            .iter()
            .rev()
            .take(12)
            .collect::<Vec<_>>()
    );
    Ok(())
}
