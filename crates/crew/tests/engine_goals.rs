//! A crew run and the goal around it, end to end over fake workers with the real approval
//! policy: a workflow that ends with its final check, a goal you set that goes round until it
//! holds, a stuck goal, a workflow that keeps going for its done-when list, and Understand's
//! questions in supervised and in auto mode.

mod support;

use std::collections::VecDeque;
use std::path::Path;
use std::sync::{Arc, Mutex, PoisonError};

use anyhow::Context;
use serde_json::{Value, json};

use argonauts_core::engine::{EngineCommand, EngineExit};
use argonauts_core::events::approval::{
    AnswerAction, ApprovalEvent, ApprovalKind, ApprovalOpened, FreeText,
};
use argonauts_core::events::goal::{
    Decider, GoalAction, GoalEvent, GoalStatus, GoalVerdict, LET_THE_MAIN_AGENT_ANSWER,
    LET_THE_MAIN_AGENT_DECIDE,
};
use argonauts_core::events::run::{RunLifecycle, RunMode, WaitReason};
use argonauts_core::events::session::SessionEvent;
use argonauts_core::events::task::{MergeEvent, TaskEvent};
use argonauts_core::events::workflow::WorkflowEvent;
use argonauts_core::events::{RunEvent, RunEventKind};
use argonauts_core::goal::GoalDefault;
use argonauts_core::names::ModelRef;
use argonauts_core::session_runner::SessionSpec;
use argonauts_core::settings::approvals::{Answerer, ApprovalMode};
use argonauts_core::settings::protocol::{CheckCommand, UnderstandWhen};
use argonauts_core::settings::workflows::OrchestratorDesigns;
use argonauts_core::steps::StepName;
use argonauts_crew::engine::shared::with_git_env;
use argonauts_crew::goal::pursue::{self, DELIVER, KEEP, KEEP_GOING, STUCK, TURN_INTO_GOAL};
use argonauts_testkit::{FakeRunner, FakeSession};
use tokio::sync::oneshot;

use support::{Crew, Outcome};

const FILE: &str = "verbose.txt";

/// The CheckGoal answers, in order, whoever checks.
type Checks = Arc<Mutex<VecDeque<Value>>>;

/// What the crew answers beyond the defaults: Understand's question, tagged `tag` (a detail
/// when unset), and the main agent's brief.
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

/// Two providers' agents in the crew and a third provider's installed, checks that pass, and
/// the check's answers from `checks`.
fn crew(checks: Vec<Value>) -> anyhow::Result<(Crew, Checks)> {
    crew_with(checks, Own::default())
}

/// [`crew`], whose readings disagree on a detail that `question` asks about.
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

/// [`crew_with`], with a third provider's agent installed or not.
fn crew_of(checks: Vec<Value>, own: Own, third_provider: bool) -> anyhow::Result<(Crew, Checks)> {
    let mut crew = Crew::new()?;
    crew.settings.divide.contracts_first = false;
    let passing = |name: &str| CheckCommand {
        name: name.to_owned(),
        command: vec!["true".to_owned()],
    };
    crew.settings.checks.run = vec![passing("tests")];
    crew.settings.checks.full_suite = vec![passing("suite")];
    crew.settings.workflows.final_check = true;
    let checks: Checks = Arc::new(Mutex::new(checks.into()));
    enlist(
        &mut crew,
        "claude-code/opus",
        "anthropic",
        &checks,
        own.clone(),
    );
    let others = Own { brief: None, ..own };
    enlist(&mut crew, "codex/gpt-x", "openai", &checks, others);
    if !third_provider {
        return Ok((crew, checks));
    }
    let shared = checks.clone();
    crew.services.workers.add(
        FakeRunner::cli("antigravity-cli", "google")
            .with_cheapest_model("gemini-3.8-flash-low")
            .respond_with(move |spec| respond(spec, &shared, &Own::default())),
    );
    Ok((crew, checks))
}

fn enlist(crew: &mut Crew, worker: &str, provider: &str, checks: &Checks, own: Own) {
    let checks = checks.clone();
    crew.services.workers.add(
        FakeRunner::cli(worker, provider).respond_with(move |spec| respond(spec, &checks, &own)),
    );
    let reference: ModelRef = worker.parse().expect("a worker reference");
    if crew.settings.crew.is_empty() {
        crew.settings.crew.main = reference.clone();
    }
    crew.settings.crew.add(reference);
}

/// One task for `FILE`, written when it builds; the scripted checks; a try the main agent names
/// `Parse Flag`; approvals that cite the rules; and the smallest valid answer otherwise.
fn respond(spec: &SessionSpec, checks: &Checks, own: &Own) -> FakeSession {
    let question = own.question;
    match spec.step {
        Some(StepName::DesignWorkflow) if own.brief.is_some() => {
            FakeSession::structured(own.brief.clone().unwrap_or_default())
        }
        Some(StepName::ModelJudge) => FakeSession::structured(json!({
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
        Some(StepName::PlanTry) => FakeSession::structured(json!({
            "name": "Parse Flag",
            "detail": "one worker parses the flag, another provider reviews it",
            "steps": "",
        })),
        Some(StepName::Build) => {
            std::fs::write(spec.workdir.join(FILE), "--verbose\n")
                .expect("the build session writes its file");
            smallest(spec)
        }
        Some(StepName::PlanToTasks) => FakeSession::structured(json!({
            "tasks": [{
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
        Some(StepName::Compare) if question.is_some() => FakeSession::structured(json!({
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

/// Runs to where it waits or finishes with the real policy, which keeps its records in the
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
            GoalEvent::Checked(checked) => Some((checked.try_number, checked.overall)),
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
fn sessions(events: &[RunEvent]) -> Vec<(String, ModelRef)> {
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

/// The phase names of the newest declaration.
fn declared(events: &[RunEvent]) -> Vec<String> {
    events
        .iter()
        .rev()
        .find_map(|event| match &event.kind {
            RunEventKind::Workflow(WorkflowEvent::Declared(declared)) => Some(
                declared
                    .phases
                    .iter()
                    .map(|phase| phase.name.clone())
                    .collect(),
            ),
            _ => None,
        })
        .unwrap_or_default()
}

fn merged(events: &[RunEvent]) -> bool {
    events.iter().any(|event| {
        matches!(
            event.kind,
            RunEventKind::Merge(MergeEvent::RunMerged { .. })
        )
    })
}

fn tasks(events: &[RunEvent]) -> Vec<(String, Option<ModelRef>)> {
    events
        .iter()
        .filter_map(|event| match &event.kind {
            RunEventKind::Task(TaskEvent::Created(spec)) => {
                Some((spec.id.to_string(), spec.worker.clone()))
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
async fn a_workflow_ends_with_its_final_check_from_another_provider() -> anyhow::Result<()> {
    let (mut crew, _) = crew(vec![met()])?;
    // A third provider reviews, so one that wrote nothing is there to check whoever builds.
    crew.settings.roles.insert(
        argonauts_core::names::RoleName::Reviewer,
        argonauts_core::settings::roles::RoleSettings {
            models: vec!["antigravity-cli".parse()?],
            ..Default::default()
        },
    );
    let outcome = run(&crew).await?;
    finished(&outcome);
    let events = &outcome.events;
    assert!(merged(events));

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
        .map(|(_, model)| model)
        .context("a check:final session")?;
    assert_ne!(
        checker.agent, "claude-code",
        "the main agent's provider checks only when no other provider wrote nothing"
    );
    assert!(
        tasks(events)
            .iter()
            .all(|(_, worker)| worker.as_ref() != Some(&checker)),
        "a task's author checked it"
    );
    // Nobody was asked whether to keep going.
    assert!(
        answered(events)
            .iter()
            .all(|(question, ..)| question != KEEP_GOING)
    );
    Ok(())
}

/// The condition the tests set as the goal. The judge reads it, since it names no command.
const UNTIL: &str = "the --verbose flag is parsed";

fn rounds(events: &[RunEvent]) -> Vec<(u32, String)> {
    goal_events(events)
        .into_iter()
        .filter_map(|event| match event {
            GoalEvent::TryStarted {
                n,
                title,
                task: None,
                ..
            } => Some((*n, title.clone())),
            _ => None,
        })
        .collect()
}

fn statuses(events: &[RunEvent]) -> Vec<(GoalStatus, Option<String>)> {
    goal_events(events)
        .into_iter()
        .filter_map(|event| match event {
            GoalEvent::StatusChanged {
                status,
                why,
                task: None,
                ..
            } => Some((*status, why.clone())),
            _ => None,
        })
        .collect()
}

/// A goal you set is a condition around the run: its workflow is round 1, the check decides,
/// and the orchestrator plans round 2 from what failed. Nobody is asked whether to keep going,
/// whichever CLI is the main agent.
#[tokio::test]
async fn a_goal_you_set_goes_round_its_workflow_until_it_holds() -> anyhow::Result<()> {
    let (mut crew, checks) = crew(vec![not_yet(), met()])?;
    crew.settings.goal.until = Some(UNTIL.to_owned());
    // The goal's check runs whether or not workflows end with one.
    crew.settings.workflows.final_check = false;
    let outcome = run(&crew).await?;
    finished(&outcome);
    let events = &outcome.events;
    assert!(merged(events));

    // The workflow stays the workflow, its check comes before Merge, and round 2 after it.
    let phases = declared(events);
    let check = phases.iter().position(|title| title == "Check");
    assert_eq!(
        check.map(|at| phases[at + 1..].to_vec()),
        Some(vec!["Parse Flag".to_owned(), "Merge".to_owned()]),
        "{phases:?}"
    );
    assert!(phases.iter().any(|title| title == "Build"), "{phases:?}");
    let set = goal_events(events)
        .into_iter()
        .find_map(|event| match event {
            GoalEvent::Set(goal) => Some(goal.clone()),
            _ => None,
        })
        .context("the goal was set")?;
    assert_eq!(set.objective, UNTIL);
    assert_eq!(set.criteria.len(), 1);
    assert_eq!(set.criteria[0].text, UNTIL);
    assert_eq!(set.max_rounds, None, "no fixed number of rounds");
    let rounds = rounds(events);
    assert_eq!(rounds.len(), 2, "{rounds:?}");
    assert_eq!(rounds[1], (2, "Parse Flag".to_owned()));
    assert_eq!(
        checked(events),
        [(1, GoalVerdict::NotYet), (2, GoalVerdict::Met)]
    );
    assert_eq!(statuses(events), [(GoalStatus::Met, None)]);
    assert!(
        answered(events)
            .iter()
            .all(|(question, ..)| question != KEEP_GOING)
    );
    // The goal is stated once it's set, and a `›` note says why the plan moved.
    let notes: Vec<String> = events
        .iter()
        .filter_map(|event| match &event.kind {
            RunEventKind::Workflow(WorkflowEvent::Note(note)) => Some(note.text.clone()),
            _ => None,
        })
        .collect();
    assert!(
        notes
            .iter()
            .any(|note| note == "Goal set · goal with 1 line · checked by the judge"),
        "{notes:?}"
    );
    assert!(
        notes.iter().any(|note| note.contains("round 2")),
        "{notes:?}"
    );
    assert!(
        checks
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .is_empty()
    );
    Ok(())
}

/// The same failure `stuck_after` rounds running changes approach once: the next round goes to
/// another provider's model. Stuck again, the goal stops and says why. Nobody is asked.
#[tokio::test]
async fn a_stuck_goal_changes_approach_once_then_stops_and_says_why() -> anyhow::Result<()> {
    let (mut crew, _) = crew(vec![not_yet(), not_yet(), not_yet(), met()])?;
    crew.settings.goal.until = Some(UNTIL.to_owned());
    crew.settings.goal.stuck_after = 2;
    let outcome = run(&crew).await?;
    let events = &outcome.events;
    assert!(
        !matches!(outcome.exit, EngineExit::Crashed(_)),
        "{:?}",
        outcome.exit
    );

    // Three checks, never a fourth: rounds 1 and 2 fail alike, round 3 takes another approach
    // and fails the same way.
    assert_eq!(checked(events).len(), 3);
    let changed: Vec<(u32, String)> = goal_events(events)
        .into_iter()
        .filter_map(|event| match event {
            GoalEvent::ApproachChanged { after, why, .. } => Some((*after, why.clone())),
            _ => None,
        })
        .collect();
    assert_eq!(
        changed,
        [(2, "the same failure 2 rounds running".to_owned())]
    );
    assert_eq!(
        statuses(events),
        [(
            GoalStatus::Stuck,
            Some("the same failure 2 rounds running, after a change of approach".to_owned())
        )]
    );
    // The round after the change goes to another provider's model than the one that got stuck.
    let built = tasks(events);
    let third = built
        .iter()
        .find(|(id, _)| id == "try-3")
        .and_then(|(_, worker)| worker.clone())
        .context("round 3's task")?;
    assert_ne!(third.agent, "claude-code");
    // It delivers what holds, and no question was opened for you.
    assert_eq!(
        answered(events)
            .into_iter()
            .filter(|(question, ..)| question == STUCK)
            .map(|(_, answer, by)| (answer, by))
            .collect::<Vec<_>>(),
        [(DELIVER.to_owned(), Decider::MainAgent)]
    );
    assert!(
        events
            .iter()
            .all(|event| !matches!(&event.kind, RunEventKind::Approval(ApprovalEvent::Opened(opened)) if opened.title == STUCK))
    );
    Ok(())
}

/// A cap on rounds is yours to set, and with one the goal stops at it.
#[tokio::test]
async fn a_goal_stops_at_the_rounds_you_allow() -> anyhow::Result<()> {
    let (mut crew, _) = crew(vec![not_yet(), not_yet(), met()])?;
    crew.settings.goal.until = Some(UNTIL.to_owned());
    crew.settings.goal.max_rounds = Some(2);
    let outcome = run(&crew).await?;
    let events = &outcome.events;
    assert_eq!(checked(events).len(), 2);
    assert_eq!(
        statuses(events),
        [(
            GoalStatus::Stuck,
            Some("it didn't hold after the 2 rounds you allow".to_owned())
        )]
    );
    Ok(())
}

/// `goal.default = "from-request"`: every request's done-when list is its goal, so a line the
/// check finds unmet sends the crew round again without anyone being asked.
#[tokio::test]
async fn from_request_makes_the_done_when_list_the_goal() -> anyhow::Result<()> {
    let (mut crew, _) = crew(vec![not_yet(), met()])?;
    crew.settings.goal.default = GoalDefault::FromRequest;
    crew.settings.workflows.final_check = false;
    let outcome = run(&crew).await?;
    finished(&outcome);
    let events = &outcome.events;
    assert!(merged(events));
    assert_eq!(
        checked(events),
        [(1, GoalVerdict::NotYet), (2, GoalVerdict::Met)]
    );
    assert!(
        answered(events)
            .iter()
            .all(|(question, ..)| question != KEEP_GOING)
    );
    assert_eq!(statuses(events), [(GoalStatus::Met, None)]);
    Ok(())
}

#[tokio::test]
async fn a_workflow_with_a_criterion_unmet_keeps_going_as_a_goal() -> anyhow::Result<()> {
    let (crew, _) = crew(vec![not_yet(), met()])?;
    let outcome = run(&crew).await?;
    finished(&outcome);
    let events = &outcome.events;
    assert!(merged(events));
    assert_eq!(
        answered(events)
            .into_iter()
            .filter(|(question, ..)| question == KEEP_GOING)
            .map(|(_, answer, by)| (answer, by))
            .collect::<Vec<_>>(),
        [(KEEP.to_owned(), Decider::MainAgent)]
    );
    // The workflow is try 1, so the goal's first try of its own is try 2, added
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
    assert_eq!(set.tries.len(), 1);
    assert_eq!(set.criteria.len(), 1);
    Ok(())
}

/// The newest open approval.
fn open(events: &[RunEvent]) -> Option<ApprovalOpened> {
    let mut open = Vec::new();
    for event in events {
        match &event.kind {
            RunEventKind::Approval(ApprovalEvent::Opened(opened)) => {
                open.push((**opened).clone());
            }
            RunEventKind::Approval(ApprovalEvent::Decided(decided)) => {
                open.retain(|opened| opened.id != decided.id);
            }
            RunEventKind::Approval(ApprovalEvent::Withdrawn { id, .. }) => {
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
    crew.settings.approvals.answers = Some(Answerer::You);
    let outcome = crew.start(RunMode::Crew, "add a --verbose flag").await?;
    let asked = open(&outcome.events).context("Understand's question")?;
    assert_eq!(asked.kind, ApprovalKind::Understand);
    // Choose-only: the answers, then the main agent, and nothing typed.
    let labels: Vec<&str> = asked
        .options
        .iter()
        .map(|option| option.label.as_str())
        .collect();
    assert_eq!(labels, ["debug", "trace", LET_THE_MAIN_AGENT_DECIDE]);
    assert!(
        asked
            .options
            .iter()
            .all(|option| option.action != AnswerAction::Answer)
    );
    assert_eq!(asked.free_text, FreeText::None);

    // Handing it to the main agent: it answers, and the go-ahead comes to you.
    let (command, reply) = support::answer(
        &outcome.events,
        ApprovalKind::Understand,
        AnswerAction::Choose {
            value: LET_THE_MAIN_AGENT_DECIDE.to_owned(),
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
    let from_main_agent: Vec<(String, Decider)> = answered(&next.events)
        .into_iter()
        .map(|(question, _, by)| (question, by))
        .collect();
    assert_eq!(
        from_main_agent,
        [("Which log level?".to_owned(), Decider::MainAgent)]
    );

    // "Yes, and let the main agent answer" approves the goal and says so in the record.
    let (command, reply) = support::answer(
        &next.events,
        ApprovalKind::Understand,
        AnswerAction::Choose {
            value: LET_THE_MAIN_AGENT_ANSWER.to_owned(),
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
            .any(|(_, answer, by)| answer == LET_THE_MAIN_AGENT_ANSWER && *by == Decider::You)
    );
    Ok(())
}

#[tokio::test]
async fn in_auto_mode_the_main_agent_answers_understands_question_and_it_is_recorded()
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
    assert_eq!(
        questions,
        [("Which log level?".to_owned(), Decider::MainAgent)]
    );
    // No question opened: the main agent answered it, and the judge checked the goal.
    assert!(events.iter().all(|event| !matches!(
        &event.kind,
        RunEventKind::Approval(ApprovalEvent::Opened(opened))
            if opened.kind == ApprovalKind::Understand && opened.title != "Run this workflow?"
    )));
    Ok(())
}

#[tokio::test]
async fn keep_going_asks_you_when_you_answer_the_crews_questions() -> anyhow::Result<()> {
    let (mut crew, _) = crew(vec![not_yet()])?;
    crew.settings.approvals.answers = Some(Answerer::You);
    let mut outcome = crew.start(RunMode::Crew, "add a --verbose flag").await?;
    // Answer each question with its recommended answer until the keep-going question shows.
    for _ in 0..8 {
        let question = open(&outcome.events).context("a question waits")?;
        if question.title == KEEP_GOING {
            assert_eq!(question.kind, ApprovalKind::Understand);
            assert_eq!(question.options[0].label, KEEP);
            assert!(question.options[0].recommended);
            assert_eq!(question.free_text, FreeText::None);
            return Ok(());
        }
        let (command, reply) = support::recommended(&outcome.events).context("an answer")?;
        outcome = crew.resume(outcome.events.clone(), vec![command]).await?;
        assert_eq!(reply.await?, Ok(()));
    }
    anyhow::bail!(
        "the keep-going question never opened: {:?}",
        support::names(&outcome.events)
    )
}

/// Answers each question that waits with its recommended answer, `commands` going in with the
/// first, until the run finishes or `rounds` run out.
async fn drive(
    crew: &Crew,
    mut outcome: Outcome,
    mut commands: Vec<EngineCommand>,
    rounds: usize,
) -> anyhow::Result<Outcome> {
    for _ in 0..rounds {
        if !matches!(outcome.exit, EngineExit::Waiting(_)) {
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
async fn you_can_turn_a_workflow_into_a_goal_while_it_runs() -> anyhow::Result<()> {
    let (mut crew, _) = crew(vec![not_yet(), met()])?;
    // No final check of its own: turning it into a goal adds one.
    crew.settings.workflows.final_check = false;
    let outcome = crew.start(RunMode::Crew, "add a --verbose flag").await?;
    assert!(matches!(outcome.exit, EngineExit::Waiting(_)));
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

/// Turned into a goal while an approval waits, and answered only later: the phase that waited
/// goes on where it was, and the check goes in when the next phase begins.
#[tokio::test]
async fn a_run_turned_into_a_goal_while_it_waits_goes_on_where_it_was() -> anyhow::Result<()> {
    let (mut crew, _) = crew(vec![not_yet(), met()])?;
    crew.settings.workflows.final_check = false;
    let outcome = crew.start(RunMode::Crew, "add a --verbose flag").await?;
    assert!(matches!(outcome.exit, EngineExit::Waiting(_)));

    // The command alone wakes the run, which waits again at the same approval.
    let (turn, replied) = goal_command(GoalAction::TurnIntoGoal);
    let waiting = crew.resume(outcome.events.clone(), vec![turn]).await?;
    assert_eq!(replied.await?, Ok(()));
    assert!(matches!(waiting.exit, EngineExit::Waiting(_)));
    assert_eq!(
        open(&waiting.events).map(|asked| asked.id),
        open(&outcome.events).map(|asked| asked.id)
    );

    let done = drive(&crew, waiting, Vec::new(), 8).await?;
    finished(&done);
    let events = &done.events;
    assert!(merged(events), "{:?}", support::names(events));
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

/// A goal the workflow's own steps state on the way, as a file's `goal =` is. The workflow had
/// no check before Merge, so it gets the goal's, and when the steps make no round of their own
/// the orchestrator plans the next one from what failed.
#[tokio::test]
async fn a_goal_stated_on_the_way_is_checked_before_the_merge() -> anyhow::Result<()> {
    use argonauts_core::goal::{CheckMeans, CheckWith, GoalFrom, GoalSpec};

    let (mut crew, checks) = crew(vec![not_yet(), met()])?;
    crew.settings.workflows.final_check = false;
    let outcome = crew.start(RunMode::Crew, "add a --verbose flag").await?;
    assert!(matches!(outcome.exit, EngineExit::Waiting(_)));
    assert!(
        !declared(&outcome.events)
            .iter()
            .any(|title| title == "Check")
    );

    let (host, sink) = crew.host_over(outcome.events.clone())?;
    let spec = GoalSpec::from_words(
        UNTIL,
        CheckWith::Auto,
        CheckMeans::default(),
        GoalFrom::WorkflowFile,
    )
    .map_err(anyhow::Error::msg)?;
    assert!(pursue::around(&host, &spec, 0)?);
    let stated = Outcome {
        exit: outcome.exit,
        events: sink.events(),
    };
    let done = drive(&crew, stated, Vec::new(), 8).await?;
    finished(&done);
    let events = &done.events;

    let phases = declared(events);
    let check = phases.iter().position(|title| title == "Check");
    assert_eq!(
        check.map(|at| phases[at + 1..].to_vec()),
        Some(vec!["Parse Flag".to_owned(), "Merge".to_owned()]),
        "{phases:?}"
    );
    assert_eq!(
        phases.iter().filter(|title| *title == "Check").count(),
        1,
        "{phases:?}"
    );
    assert_eq!(
        checked(events),
        [(1, GoalVerdict::NotYet), (2, GoalVerdict::Met)]
    );
    assert_eq!(statuses(events), [(GoalStatus::Met, None)]);
    let said = events.iter().any(|event| match &event.kind {
        RunEventKind::Workflow(WorkflowEvent::Note(note)) => {
            note.text == "The run has a goal, so its check comes before Merge"
        }
        _ => false,
    });
    assert!(said, "{:?}", support::names(events));
    assert!(
        checks
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .is_empty()
    );
    Ok(())
}

#[tokio::test]
async fn an_assumption_you_change_is_recorded_and_told_to_the_crew() -> anyhow::Result<()> {
    let (crew, _) = crew_asking(Vec::new(), Some("Which log level?"))?;
    // In auto the main agent answers the question, which becomes assumption 1 once you approve.
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
    let spec = argonauts_core::goal::GoalSpec::from_lines(
        "add a --verbose flag",
        vec![argonauts_core::events::goal::Criterion::new(
            "G1", "it works",
        )],
        argonauts_core::goal::CheckWith::Auto,
        argonauts_core::goal::GoalFrom::Command,
    );
    let goal = argonauts_core::events::goal::GoalRun::new(
        &spec,
        argonauts_core::goal::GoalScope::Workflow,
        argonauts_core::goal::KeptBy::Argonauts,
    );
    let set = RunEvent {
        seq: 1,
        kind: RunEventKind::Goal(GoalEvent::Set(Box::new(goal))),
        ..started.clone()
    };
    let (host, sink) = crew.host_over(vec![started, set])?;
    // Waiting on a question isn't a pause.
    pursue::on_wait(&host, &WaitReason::Approval)?;
    pursue::on_wait(&host, &WaitReason::Paused)?;
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

/// A run whose goal is set, over a host a test can act on.
fn host_with_a_goal(
    crew: &Crew,
) -> anyhow::Result<(
    argonauts_crew::engine::host::Host,
    Arc<argonauts_testkit::MemorySink>,
)> {
    let started = crew.run_started(RunMode::Crew, "add a --verbose flag")?;
    let spec = argonauts_core::goal::GoalSpec::from_words(
        "the flag is parsed and `make docs` passes",
        argonauts_core::goal::CheckWith::Auto,
        argonauts_core::goal::CheckMeans::default(),
        argonauts_core::goal::GoalFrom::Command,
    )
    .map_err(anyhow::Error::msg)?;
    let goal = argonauts_core::events::goal::GoalRun::new(
        &spec,
        argonauts_core::goal::GoalScope::Workflow,
        argonauts_core::goal::KeptBy::Argonauts,
    );
    let set = RunEvent {
        seq: 1,
        kind: RunEventKind::Goal(GoalEvent::Set(Box::new(goal))),
        ..started.clone()
    };
    crew.host_over(vec![started, set])
}

/// A goal you pause yourself stays paused when its run goes on, until you resume it. A goal a
/// waiting run paused goes on with the run.
#[tokio::test]
async fn a_goal_you_pause_waits_for_you_to_resume_it() -> anyhow::Result<()> {
    let (crew, _) = crew(Vec::new())?;
    let (host, sink) = host_with_a_goal(&crew)?;
    pursue::act(&host, GoalAction::Pause { task: None })
        .await
        .map_err(anyhow::Error::msg)?;
    // The run going on doesn't pick the goal back up.
    pursue::on_resume(&host)?;
    assert_eq!(
        statuses(&sink.events()),
        [(GoalStatus::Paused, Some(pursue::YOU_PAUSED.to_owned()))]
    );
    pursue::act(&host, GoalAction::Resume { task: None })
        .await
        .map_err(anyhow::Error::msg)?;
    let after = statuses(&sink.events());
    assert_eq!(after.last(), Some(&(GoalStatus::Pursuing, None)));
    // There's nothing to resume twice.
    assert!(
        pursue::act(&host, GoalAction::Resume { task: None })
            .await
            .is_err()
    );
    Ok(())
}

/// A workflow file's `goal =` and `check = [...]`: the run takes them as its goal before its
/// first step, whatever the settings say, and keeps a goal it already has.
#[tokio::test]
async fn a_workflow_files_goal_is_the_runs_goal() -> anyhow::Result<()> {
    use argonauts_core::goal::{CheckMeans, CheckWith, GoalFrom, GoalSpec};

    let (crew, _) = crew(Vec::new())?;
    let started = crew.run_started(RunMode::Crew, "add a --verbose flag")?;
    let (host, sink) = crew.host_over(vec![started])?;
    let mut spec = GoalSpec::from_words(
        "every acceptance test passes",
        CheckWith::Auto,
        CheckMeans::default(),
        GoalFrom::WorkflowFile,
    )
    .map_err(anyhow::Error::msg)?;
    spec.commands = vec!["cargo test --test acceptance".to_owned()];
    assert!(pursue::around(&host, &spec, 0)?);
    let goal = host
        .read(|state| state.goal.pursuit.clone())
        .context("the goal")?;
    assert_eq!(goal.objective, "every acceptance test passes");
    assert_eq!(goal.commands, ["cargo test --test acceptance"]);
    assert_eq!(goal.tries.len(), 1, "the workflow is round 1");
    assert!(host.read(|state| pursue::pursuing(state, host.settings())));
    let said = sink.events().iter().any(|event| match &event.kind {
        RunEventKind::Workflow(WorkflowEvent::Note(note)) => {
            note.text == "Goal set · goal with 1 line · checked by cargo test --test acceptance"
        }
        _ => false,
    });
    assert!(said, "{:?}", sink.events());
    // A second file's goal doesn't replace it.
    let other = GoalSpec::from_words(
        "the README names the flag",
        CheckWith::Auto,
        CheckMeans::default(),
        GoalFrom::WorkflowFile,
    )
    .map_err(anyhow::Error::msg)?;
    assert!(!pursue::around(&host, &other, 0)?);
    assert_eq!(
        host.read(|state| state
            .goal
            .pursuit
            .as_ref()
            .map(|goal| goal.objective.clone())),
        Some("every acceptance test passes".to_owned())
    );
    Ok(())
}

/// `e` on a goal rewrites its lines, each with the check its words give it.
#[tokio::test]
async fn a_goals_lines_can_be_rewritten() -> anyhow::Result<()> {
    use argonauts_core::events::goal::CriterionCheck;

    let (crew, _) = crew(Vec::new())?;
    let (host, _) = host_with_a_goal(&crew)?;
    let lines = vec![
        "the flag is parsed".to_owned(),
        "`make docs` passes".to_owned(),
        "the README names the flag".to_owned(),
    ];
    pursue::act(&host, GoalAction::EditLines { task: None, lines })
        .await
        .map_err(anyhow::Error::msg)?;
    let goal = host
        .read(|state| state.goal.pursuit.clone())
        .context("the goal")?;
    let texts: Vec<&str> = goal
        .criteria
        .iter()
        .map(|line| line.text.as_str())
        .collect();
    assert_eq!(
        texts,
        [
            "the flag is parsed",
            "`make docs` passes",
            "the README names the flag"
        ]
    );
    assert_eq!(
        goal.criteria[1].check,
        CriterionCheck::Command("make docs".to_owned())
    );
    assert_eq!(goal.criteria[2].check, CriterionCheck::Review);
    // Nothing to check is refused, and the lines stay.
    let empty = pursue::act(
        &host,
        GoalAction::EditLines {
            task: None,
            lines: Vec::new(),
        },
    )
    .await;
    assert!(empty.is_err());
    assert_eq!(
        host.read(|state| state.goal.pursuit.as_ref().map(|goal| goal.criteria.len())),
        Some(3)
    );
    Ok(())
}

const FIX_THEN_REVIEW: &str = "name: fix-then-review\ndescription: One fix, reviewed in a phase \
    of its own\nphases:\n  - name: Fix\n    detail: one worker fixes it\n    standard: do\n    \
    steps:\n      - { kind: approval, approval: understand }\n      - { kind: build, input: goal, \
    different_provider_from: author, label: \"fix:{item}\", review_in: Review }\n  - name: \
    Review\n    detail: another provider reviews the fix\n    standard: check\n    steps: []\n  \
    - name: Merge\n    detail: the merge\n    standard: decide\n    steps:\n      - { kind: \
    merge, label: \"merge:{branch}\" }\n";

/// A build that shows its reviews in a later phase runs as a pipeline: that phase starts with
/// the first review, while the build's own phase is still running.
#[tokio::test]
async fn a_review_phase_starts_with_its_first_review() -> anyhow::Result<()> {
    let (mut crew, _) = crew(Vec::new())?;
    crew.settings.workflow = "fix-then-review".to_owned();
    crew.repo
        .write(".argonauts/workflows/fix-then-review.yaml", FIX_THEN_REVIEW)?;
    let outcome = crew.start(RunMode::Crew, "add a --verbose flag").await?;
    let outcome = drive(&crew, outcome, Vec::new(), 4).await?;
    let marks: Vec<(String, &str)> = outcome
        .events
        .iter()
        .filter_map(|event| match &event.kind {
            RunEventKind::Workflow(WorkflowEvent::PhaseStarted(mark)) => {
                Some((mark.name.clone(), "started"))
            }
            RunEventKind::Workflow(WorkflowEvent::PhaseEnded { phase, .. }) => {
                Some((phase.name.clone(), "ended"))
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

/// With `understand.when = "main"`, a main agent that skips Understand brings its own open
/// questions to you before anything writes.
#[tokio::test]
async fn the_main_agents_own_questions_come_to_you_when_it_skips_understand() -> anyhow::Result<()>
{
    let mut brief = support::brief(Some("maker-checker"), json!([]), None, "one small fix");
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
    // The main agent runs another example than the workflow's, which only "any" lets it do.
    crew.settings.workflows.orchestrator_designs = OrchestratorDesigns::ANY;
    crew.settings.workflows.adapt_at_phase_ends = false;
    crew.settings.understand.when = UnderstandWhen::Main;
    crew.settings.approvals.answers = Some(Answerer::You);
    let outcome = crew.start(RunMode::Crew, "add a --verbose flag").await?;
    // No readings ran: the Captain example's own reading went, and the main agent's question waits.
    assert!(
        !declared(&outcome.events)
            .iter()
            .any(|title| title == "Understand")
    );
    let question = open(&outcome.events).context("the main agent's question")?;
    assert_eq!(question.kind, ApprovalKind::Understand);
    let labels: Vec<&str> = question
        .options
        .iter()
        .map(|option| option.label.as_str())
        .collect();
    assert_eq!(labels, ["debug", "trace", LET_THE_MAIN_AGENT_DECIDE]);
    assert!(question.options[0].recommended);
    Ok(())
}

/// A crew run whose main agent designs it from the maker-checker example and asks nothing, with
/// Understand the main agent's to add or off, so nothing reads the request.
fn unread(when: UnderstandWhen, yours: bool) -> anyhow::Result<Crew> {
    let brief = support::brief(Some("maker-checker"), json!([]), None, "one small fix");
    let (mut crew, _) = crew_with(
        vec![met()],
        Own {
            brief: Some(brief),
            ..Own::default()
        },
    )?;
    // The main agent runs another example than the workflow's, which only "any" lets it do.
    crew.settings.workflows.orchestrator_designs = OrchestratorDesigns::ANY;
    crew.settings.workflows.adapt_at_phase_ends = false;
    crew.settings.understand.when = when;
    if yours {
        crew.settings.approvals.answers = Some(Answerer::You);
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

/// The go-ahead's title. A goal is no kind of run, so there's one.
const GO_AHEAD: &str = "Run this workflow?";

const UNREAD: [UnderstandWhen; 2] = [UnderstandWhen::Main, UnderstandWhen::Off];

#[tokio::test]
async fn with_nothing_read_a_supervised_run_asks_the_go_ahead_before_it_builds()
-> anyhow::Result<()> {
    for when in UNREAD {
        let crew = unread(when, true)?;
        let outcome = crew.start(RunMode::Crew, "add a --verbose flag").await?;
        let asked =
            open(&outcome.events).with_context(|| format!("{when:?}: the go-ahead waits"))?;
        assert_eq!(asked.kind, ApprovalKind::Understand, "{when:?}");
        assert_eq!(asked.title, GO_AHEAD, "{when:?}");
        assert!(
            first(&outcome.events, build_started).is_none(),
            "{when:?}: a build started before the go-ahead"
        );
        // Yes runs it, through its check and the merge.
        let after = drive(&crew, outcome, Vec::new(), 8).await?;
        finished(&after);
        assert!(merged(&after.events), "{when:?}");
    }
    Ok(())
}

#[tokio::test]
async fn with_nothing_read_an_auto_run_gets_the_go_ahead_before_it_builds() -> anyhow::Result<()> {
    for when in UNREAD {
        let crew = unread(when, false)?;
        let outcome = run(&crew).await?;
        finished(&outcome);
        let events = &outcome.events;
        let asked = first(events, |kind| {
            matches!(kind, RunEventKind::Approval(ApprovalEvent::Opened(opened))
                if opened.title == GO_AHEAD)
        })
        .with_context(|| format!("{when:?}: no go-ahead"))?;
        let approved = first(events, |kind| {
            matches!(kind, RunEventKind::Goal(GoalEvent::Approved { .. }))
        })
        .with_context(|| format!("{when:?}: no approved goal"))?;
        let built =
            first(events, build_started).with_context(|| format!("{when:?}: nothing built"))?;
        assert!(asked < approved && approved < built, "{when:?}");
        assert!(merged(events), "{when:?}");
    }
    Ok(())
}

#[tokio::test]
async fn in_auto_mode_the_main_agent_answers_an_always_ask_question_too() -> anyhow::Result<()> {
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
            Decider::MainAgent
        )]
    );
    assert!(merged(&outcome.events));
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
        matches!(outcome.exit, EngineExit::Waiting(_)),
        "{:?}",
        outcome.exit
    );
    let asked = open(&outcome.events).context("the security question waits for you")?;
    assert_eq!(asked.kind, ApprovalKind::Understand);
    assert!(
        asked.title.starts_with("Who may turn on --verbose?")
            || asked.title.starts_with("What should the goal say?"),
        "{}",
        asked.title
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
    let mut crew = unread(UnderstandWhen::Main, false)?;
    crew.settings.approvals.ask_you_about = vec!["go-ahead".to_owned()];
    let outcome = run(&crew).await?;
    assert!(
        matches!(outcome.exit, EngineExit::Waiting(_)),
        "{:?}",
        outcome.exit
    );
    let asked = open(&outcome.events).context("the go-ahead waits for you")?;
    assert_eq!(asked.title, "Run this workflow?");
    assert!(first(&outcome.events, build_started).is_none());
    Ok(())
}

/// In autopilot nothing waits for you, so the go-ahead `approvals.ask_you_about` names is
/// weighed as autopilot weighs it, never kept for you.
#[tokio::test]
async fn ask_you_about_leaves_the_go_ahead_to_autopilot() -> anyhow::Result<()> {
    let mut crew = unread(UnderstandWhen::Main, false)?;
    crew.settings.approvals.ask_you_about = vec!["go-ahead".to_owned()];
    crew.settings.approvals.autopilot = true;
    let outcome = run(&crew).await?;
    let go_ahead = outcome
        .events
        .iter()
        .find_map(|event| match &event.kind {
            RunEventKind::Approval(ApprovalEvent::Opened(opened))
                if opened.title == "Run this workflow?" =>
            {
                Some((**opened).clone())
            }
            _ => None,
        })
        .context("the go-ahead")?;
    assert_eq!(go_ahead.mode, ApprovalMode::Autopilot);
    assert!(!go_ahead.needs_you);
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
async fn a_tasks_agents_go_by_its_item_and_its_review_sits_with_its_build() -> anyhow::Result<()> {
    let (mut crew, _) = crew(vec![met()])?;
    crew.settings.divide.predictions = true;
    let outcome = run(&crew).await?;
    finished(&outcome);
    let events = &outcome.events;
    let labels: Vec<String> = sessions(events)
        .into_iter()
        .map(|(label, _)| label)
        .collect();
    // The planner keyed its task `flag`, titled "Add the --verbose flag".
    for expected in ["build:flag", "review:flag", "predict:flag"] {
        assert!(
            labels.iter().any(|label| label == expected),
            "{expected}: {labels:?}"
        );
    }
    assert!(
        labels.iter().all(|label| !label.contains("task-")),
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
    let crew = unread(UnderstandWhen::Main, true)?;
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
async fn no_round_is_checked_by_a_model_that_wrote_it() -> anyhow::Result<()> {
    // Claude Code is the main agent and Codex is the only other provider, the common setup.
    let (mut crew, _) = crew_of(vec![not_yet(), not_yet(), met()], Own::default(), false)?;
    crew.settings.goal.until = Some(UNTIL.to_owned());
    crew.settings.goal.stuck_after = 2;
    // With both providers reading the request, no judge is left to approve the goal in
    // your place; this is about the checks.
    crew.settings.understand.when = UnderstandWhen::Off;
    let outcome = run(&crew).await?;
    let events = &outcome.events;
    let workers = tasks(events);
    let checkers = sessions(events);
    let mut seen = 0;
    // Round 1 is the workflow, whose tasks are the plan's. Rounds 2 and 3 each build one task.
    for n in 2..=3 {
        let Some(worker) = workers
            .iter()
            .find(|(id, _)| *id == format!("try-{n}"))
            .and_then(|(_, worker)| worker.clone())
        else {
            continue;
        };
        let checker = checkers
            .iter()
            .find(|(label, _)| *label == format!("check:{n}"))
            .map(|(_, model)| model.clone())
            .with_context(|| {
                format!(
                    "check:{n}: {checkers:?} {:?} {:?} {:?}",
                    checked(events),
                    outcome.exit,
                    support::names(events)
                )
            })?;
        assert_ne!(checker, worker, "round {n} was checked by its own author");
        seen += 1;
    }
    // The round after the change of approach went to Codex, and so was checked by the main
    // agent's provider.
    assert_eq!(
        seen,
        2,
        "{workers:?} {:?} {:?}",
        outcome.exit,
        support::names(events)
            .iter()
            .rev()
            .take(12)
            .collect::<Vec<_>>()
    );
    Ok(())
}
