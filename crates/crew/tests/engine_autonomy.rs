//! The autonomous defaults end to end, with the real approval policy over fake workers: a run
//! goes from goal to merge without asking you, the lead answering the crew's questions, and
//! each case that must still ask does, saying why.

mod support;

use std::path::Path;
use std::sync::Arc;

use anyhow::Context;

use argonauts_core::engine::EngineExit;
use argonauts_core::events::card::IntegrationEvent;
use argonauts_core::events::checkpoint::{
    CheckpointEvent, CheckpointKind, CheckpointPacket, PolicyCheck,
};
use argonauts_core::events::goal::{GoalEvent, GoalFile};
use argonauts_core::events::run::{Party, RunLifecycle, RunMode};
use argonauts_core::events::session::EndReason;
use argonauts_core::events::{RunEvent, RunEventKind};
use argonauts_core::names::WorkerRef;
use argonauts_core::settings::approvals::{Answerer, ApprovalMode, DecideWith};
use argonauts_core::settings::protocol::GateCommand;
use argonauts_core::steps::StepName;
use argonauts_core::worker::SessionSpec;
use argonauts_core::workflow::WorkflowRef;
use argonauts_crew::engine::shared::with_git_env;
use argonauts_testkit::{FakeSession, FakeWorker};

use support::{Crew, Outcome, names};

const FILE: &str = "verbose.txt";

/// The approver picked for every checkpoint: the cheapest model of a third company.
const APPROVER: &str = "antigravity-cli/gemini-3.6-flash-low";

/// A crew of two companies' agents, each building `FILE`, with passing gates. A third
/// company's CLI is installed but not in the crew, so it can judge what both wrote.
fn crew() -> anyhow::Result<Crew> {
    let mut crew = Crew::new()?;
    with_gates(&mut crew);
    enlist(&mut crew, "claude-code/opus", "claude");
    enlist(&mut crew, "codex/gpt-x", "gpt");
    crew.services.workers.add(
        FakeWorker::harness("antigravity-cli", "gemini")
            .with_cheapest_model("gemini-3.6-flash-low")
            .respond_with(answer),
    );
    Ok(crew)
}

/// A crew member that plans one card for `FILE`, writes it when it builds, and otherwise gives
/// the smallest valid answer.
fn enlist(crew: &mut Crew, worker: &str, family: &str) {
    enlist_asking(crew, worker, family, None);
}

/// [`enlist`], finding the restatements differ on a detail, which `question` asks you about.
fn enlist_asking(crew: &mut Crew, worker: &str, family: &str, question: Option<&'static str>) {
    enlist_with(crew, worker, family, question, "detail", Vec::new());
}

/// What a crew member does at a step in place of the defaults.
#[derive(Debug, Clone)]
enum Own {
    Answer(serde_json::Value),
    Crash,
}

/// [`enlist_asking`], the difference tagged `tag`, doing `own` at the steps they name, such as
/// answering the goal's questions when it leads.
fn enlist_with(
    crew: &mut Crew,
    worker: &str,
    family: &str,
    question: Option<&'static str>,
    tag: &'static str,
    own: Vec<(StepName, Own)>,
) {
    let fake = FakeWorker::harness(worker, family).respond_with(move |spec: &SessionSpec| {
        match own.iter().find(|(step, _)| spec.step == Some(*step)) {
            Some((_, Own::Answer(own))) => return FakeSession::structured(own.clone()),
            Some((_, Own::Crash)) => {
                return FakeSession::ended(EndReason::Crashed {
                    message: "segfault".to_owned(),
                });
            }
            None => {}
        }
        match (spec.step, question) {
            (Some(StepName::Differ), Some(question)) => {
                return FakeSession::structured(serde_json::json!({
                    "agreements": [],
                    "differences": [{
                        "topic": "the log level",
                        "readings": [
                            {"label": "A", "reading": "debug"},
                            {"label": "B", "reading": "trace"}
                        ],
                        "tag": tag,
                        "question": {
                            "text": question,
                            "options": ["debug", "trace"],
                            "recommended": 0,
                            "basis": "A reads it as debug"
                        }
                    }]
                }));
            }
            (Some(StepName::Build), _) => {
                std::fs::write(spec.workdir.join(FILE), "--verbose\n")
                    .expect("the build session writes its file");
            }
            (Some(StepName::PlanToCards), _) => {
                return FakeSession::structured(serde_json::json!({
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
                }));
            }
            _ => {}
        }
        answer(spec)
    });
    crew.services.workers.add(fake);
    let reference: WorkerRef = worker.parse().expect("a worker reference");
    if crew.settings.crew.is_empty() {
        crew.settings.default_agent = reference.clone();
    }
    crew.settings.crew.push(reference);
}

/// An approval citing the default rules as the approver step, else the smallest valid answer
/// for the step's schema, or a plain end when it has none.
fn answer(spec: &SessionSpec) -> FakeSession {
    if spec.step == Some(StepName::ModelApprover) {
        return FakeSession::structured(serde_json::json!({
            "verdict": "approve",
            "reasons": ["the evidence meets the rules"],
            "rules_cited": ["R7"]
        }));
    }
    match &spec.json_schema {
        Some(schema) => FakeSession::structured(support::fill(schema)),
        None => FakeSession::completed("done"),
    }
}

/// A test command each card runs and a suite the final check runs, both passing. The plan is
/// one build card, so it goes without an interface card first.
fn with_gates(crew: &mut Crew) {
    crew.settings.divide.contracts_first = false;
    let passing = |name: &str| GateCommand {
        name: name.to_owned(),
        command: vec!["true".to_owned()],
    };
    crew.settings.gates.run = vec![passing("tests")];
    crew.settings.gates.full_suite = vec![passing("suite")];
}

/// Starts the run with the real policy in place of the fakes' asking one, and drives it until
/// it parks or finishes. The policy keeps its records in the repo's state folder, as it does
/// under `~/.argonauts/state`.
async fn run(crew: &Crew) -> anyhow::Result<Outcome> {
    run_in(crew, None).await
}

/// [`run`], with Shift+Tab's `mode` chosen before the run's first decision.
async fn run_in(crew: &Crew, mode: Option<ApprovalMode>) -> anyhow::Result<Outcome> {
    let repo = crew.repo_context()?;
    let state = repo
        .run_dir
        .parent()
        .and_then(Path::parent)
        .context("the run folder is under the repo's state folder")?;
    std::fs::create_dir_all(state)?;
    let started = crew.run_started(RunMode::Crew, "add a --verbose flag")?;
    let mut history = vec![started.clone()];
    if let Some(mode) = mode {
        history.push(RunEvent {
            seq: 1,
            kind: RunEventKind::Run(RunLifecycle::ApprovalModeChanged {
                mode,
                by: Party::User,
            }),
            ..started
        });
    }
    let (mut context, sink, _commands) = crew.context(history)?;
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

fn checks(events: &[RunEvent]) -> Vec<&PolicyCheck> {
    events
        .iter()
        .filter_map(|event| match &event.kind {
            RunEventKind::Checkpoint(CheckpointEvent::Checked(check)) => Some(check),
            _ => None,
        })
        .collect()
}

/// The kinds of checkpoint still open, in the order they opened.
fn open_kinds(events: &[RunEvent]) -> Vec<CheckpointKind> {
    let mut open = Vec::new();
    for event in events {
        match &event.kind {
            RunEventKind::Checkpoint(CheckpointEvent::Opened(opened)) => {
                open.push((opened.id.clone(), opened.kind));
            }
            RunEventKind::Checkpoint(CheckpointEvent::Decided(decided)) => {
                open.retain(|(id, _)| *id != decided.id);
            }
            RunEventKind::Checkpoint(CheckpointEvent::Withdrawn { id, .. }) => {
                open.retain(|(open, _)| open != id);
            }
            _ => {}
        }
    }
    open.into_iter().map(|(_, kind)| kind).collect()
}

/// Every reason the policy gave on checkpoints of `kind`, shadow records included.
fn reasons(events: &[RunEvent], kind: CheckpointKind) -> Vec<String> {
    let ids: Vec<_> = events
        .iter()
        .filter_map(|event| match &event.kind {
            RunEventKind::Checkpoint(CheckpointEvent::Opened(opened)) if opened.kind == kind => {
                Some(opened.id.clone())
            }
            _ => None,
        })
        .collect();
    checks(events)
        .into_iter()
        .filter(|check| ids.contains(&check.id))
        .flat_map(|check| {
            check
                .reasons
                .iter()
                .cloned()
                .chain(check.violations.iter().map(|v| v.detail.clone()))
        })
        .collect()
}

/// Each decided checkpoint's kind and who decided it, in order.
fn decided(events: &[RunEvent]) -> Vec<(CheckpointKind, Party)> {
    let mut kinds = Vec::new();
    let mut found = Vec::new();
    for event in events {
        match &event.kind {
            RunEventKind::Checkpoint(CheckpointEvent::Opened(opened)) => {
                kinds.push((opened.id.clone(), opened.kind));
            }
            RunEventKind::Checkpoint(CheckpointEvent::Decided(decided)) => {
                if let Some((_, kind)) = kinds.iter().find(|(id, _)| *id == decided.id) {
                    found.push((*kind, decided.by.clone()));
                }
            }
            _ => {}
        }
    }
    found
}

fn approver_sessions(specs: &[SessionSpec]) -> usize {
    specs
        .iter()
        .filter(|spec| spec.step == Some(StepName::ModelApprover))
        .count()
}

#[tokio::test]
async fn a_run_goes_from_goal_to_merge_without_asking() -> anyhow::Result<()> {
    let crew = crew()?;
    let outcome = run(&crew).await?;
    let events = &outcome.events;
    assert!(
        matches!(outcome.exit, EngineExit::Finished(_)),
        "{:?}: open {:?}, checks {:#?}",
        outcome.exit,
        open_kinds(events),
        checks(events)
    );
    let landed = events.iter().any(|event| {
        matches!(
            event.kind,
            RunEventKind::Integration(IntegrationEvent::Landed { .. })
        )
    });
    assert!(landed, "{:?}", names(events));

    // The goal and the plan were decided by your rules and the cheapest model of a company
    // that wrote none of them, and the merge by the merge rules alone. You were never asked.
    let approver: WorkerRef = APPROVER.parse()?;
    let by_approver = Party::Approver {
        model: approver.clone(),
    };
    let expected = [
        (CheckpointKind::Goal, by_approver.clone()),
        (CheckpointKind::Plan, by_approver),
        (CheckpointKind::Merge, Party::Rules),
    ];
    assert_eq!(decided(events), expected);
    let agy = crew.services.workers.fake("antigravity-cli").expect("agy");
    assert_eq!(approver_sessions(&agy.specs()), 2);
    assert!(agy.specs().iter().all(|spec| spec.worker == approver));
    Ok(())
}

#[tokio::test]
async fn with_no_agent_of_another_company_the_goal_asks_with_a_warning() -> anyhow::Result<()> {
    let mut crew = Crew::new()?;
    with_gates(&mut crew);
    enlist(&mut crew, "claude-code/opus", "claude");
    enlist(&mut crew, "claude-code/sonnet", "claude");
    let outcome = run(&crew).await?;
    assert!(
        matches!(outcome.exit, EngineExit::Parked(_)),
        "{:?}",
        outcome.exit
    );
    assert_eq!(open_kinds(&outcome.events), [CheckpointKind::Goal]);
    let why = reasons(&outcome.events, CheckpointKind::Goal).join("\n");
    assert!(why.contains("another company than anthropic"), "{why}");
    assert!(why.contains("set approvals.approver.model"), "{why}");
    Ok(())
}

#[tokio::test]
async fn a_merge_past_the_daily_cap_asks() -> anyhow::Result<()> {
    let mut crew = crew()?;
    crew.settings.limits.auto_merges_per_day = 0;
    let outcome = run(&crew).await?;
    assert!(
        matches!(outcome.exit, EngineExit::Parked(_)),
        "{:?}",
        outcome.exit
    );
    assert_eq!(open_kinds(&outcome.events), [CheckpointKind::Merge]);
    let why = reasons(&outcome.events, CheckpointKind::Merge).join("\n");
    assert!(why.contains("the cap is 0"), "{why}");
    Ok(())
}

#[tokio::test]
async fn a_merge_the_approver_rejects_asks() -> anyhow::Result<()> {
    let mut crew = Crew::new()?;
    with_gates(&mut crew);
    crew.settings.approvals.merge.decide_with =
        argonauts_core::settings::approvals::DecideWith::RulesAndModel;
    enlist(&mut crew, "claude-code/opus", "claude");
    enlist(&mut crew, "codex/gpt-x", "gpt");
    let rejecting = FakeWorker::harness("antigravity-cli", "gemini")
        .with_cheapest_model("gemini-3.6-flash-low")
        .respond_with(|spec: &SessionSpec| {
            if spec.step == Some(StepName::ModelApprover) && is_merge(spec) {
                return FakeSession::structured(serde_json::json!({
                    "verdict": "reject",
                    "reasons": ["the change isn't what the goal asked for"],
                    "rules_cited": []
                }));
            }
            answer(spec)
        });
    crew.services.workers.add(rejecting);
    let outcome = run(&crew).await?;
    assert!(
        matches!(outcome.exit, EngineExit::Parked(_)),
        "{:?}",
        outcome.exit
    );
    assert_eq!(open_kinds(&outcome.events), [CheckpointKind::Merge]);
    let why = reasons(&outcome.events, CheckpointKind::Merge).join("\n");
    assert!(why.contains("isn't what the goal asked for"), "{why}");
    Ok(())
}

fn is_merge(spec: &SessionSpec) -> bool {
    spec.prompt
        .iter()
        .any(|part| part.text.contains("combined diff"))
}

#[tokio::test]
async fn a_merge_you_keep_supervised_asks() -> anyhow::Result<()> {
    let mut crew = crew()?;
    crew.settings.approvals.merge.mode = ApprovalMode::Supervised;
    let outcome = run(&crew).await?;
    assert!(
        matches!(outcome.exit, EngineExit::Parked(_)),
        "{:?}",
        outcome.exit
    );
    assert_eq!(open_kinds(&outcome.events), [CheckpointKind::Merge]);
    Ok(())
}

const QUESTION: Option<&str> = Some("Which log level does --verbose turn on?");

/// The crew with both members raising [`QUESTION`] about a detail, the lead answering it with
/// `lead`.
fn asking_crew(lead: Option<serde_json::Value>) -> anyhow::Result<Crew> {
    asking_crew_about("detail", lead.map(Own::Answer))
}

/// [`asking_crew`], the question tagged `tag`, the lead doing `lead` when it's asked.
fn asking_crew_about(tag: &'static str, lead: Option<Own>) -> anyhow::Result<Crew> {
    let approver = FakeWorker::harness("antigravity-cli", "gemini")
        .with_cheapest_model("gemini-3.6-flash-low")
        .respond_with(answer);
    crew_asking(tag, lead, approver)
}

fn asking_crew_judged_by(
    lead: Option<serde_json::Value>,
    approver: FakeWorker,
) -> anyhow::Result<Crew> {
    crew_asking("detail", lead.map(Own::Answer), approver)
}

fn crew_asking(tag: &'static str, lead: Option<Own>, approver: FakeWorker) -> anyhow::Result<Crew> {
    let mut crew = crew()?;
    crew.services.workers = Default::default();
    crew.settings.crew.clear();
    let own: Vec<(StepName, Own)> = lead
        .iter()
        .map(|lead| (StepName::AnswerQuestions, lead.clone()))
        .collect();
    enlist_with(
        &mut crew,
        "claude-code/opus",
        "claude",
        QUESTION,
        tag,
        own.clone(),
    );
    enlist_with(&mut crew, "codex/gpt-x", "gpt", QUESTION, tag, own);
    crew.services.workers.add(approver);
    Ok(crew)
}

fn lead_picks_trace() -> serde_json::Value {
    serde_json::json!({"answers": [{
        "n": 1,
        "option": 1,
        "why": "the README calls trace the developer level",
        "needs_user": null
    }]})
}

fn approved_goal(events: &[RunEvent]) -> Option<&GoalFile> {
    events.iter().find_map(|event| match &event.kind {
        RunEventKind::Goal(GoalEvent::Approved { file, .. }) => Some(file),
        _ => None,
    })
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

/// Why each card still open came to you, as its card says.
fn why_asked(events: &[RunEvent], kind: CheckpointKind) -> Vec<String> {
    let open: Vec<_> = events
        .iter()
        .filter_map(|event| match &event.kind {
            RunEventKind::Checkpoint(CheckpointEvent::Opened(opened)) if opened.kind == kind => {
                Some(opened.id.clone())
            }
            _ => None,
        })
        .collect();
    checks(events)
        .into_iter()
        .filter(|check| !check.shadow && check.verdict.is_none() && open.contains(&check.id))
        .flat_map(|check| check.reasons.clone())
        .collect()
}

#[tokio::test]
async fn in_auto_the_lead_answers_the_goals_questions_and_nothing_asks() -> anyhow::Result<()> {
    let crew = asking_crew(Some(lead_picks_trace()))?;
    let outcome = run(&crew).await?;
    let events = &outcome.events;
    assert!(
        matches!(outcome.exit, EngineExit::Finished(_)),
        "{:?}: open {:?}, checks {:#?}",
        outcome.exit,
        open_kinds(events),
        checks(events)
    );

    // The question never became a card: the lead's answer is an assumption the approver checked.
    let questions = events
        .iter()
        .filter(|event| {
            matches!(&event.kind, RunEventKind::Checkpoint(CheckpointEvent::Opened(opened))
                if opened.title.starts_with("What should the goal say"))
        })
        .count();
    assert_eq!(questions, 0, "{:?}", names(events));
    let goal = approved_goal(events).expect("the goal was approved");
    let assumed: Vec<_> = goal
        .assumptions
        .iter()
        .filter(|assumption| assumption.by_lead())
        .collect();
    assert_eq!(assumed.len(), 1, "{:?}", goal.assumptions);
    assert!(assumed[0].text.ends_with(": trace"), "{:?}", assumed[0]);
    assert!(assumed[0].basis.contains("the README calls trace"));
    assert!(
        notes(events)
            .iter()
            .any(|note| note.starts_with("The lead answered 1 of the goal's questions")),
        "{:?}",
        notes(events)
    );

    let approver: WorkerRef = APPROVER.parse()?;
    let by_approver = Party::Approver { model: approver };
    let expected = [
        (CheckpointKind::Goal, by_approver.clone()),
        (CheckpointKind::Plan, by_approver),
        (CheckpointKind::Merge, Party::Rules),
    ];
    assert_eq!(decided(events), expected);

    // The default agent leads the goal phase, so it answered, once.
    let claude = crew
        .services
        .workers
        .fake("claude-code/opus")
        .expect("claude");
    let asked: Vec<_> = claude
        .specs()
        .into_iter()
        .filter(|spec| spec.step == Some(StepName::AnswerQuestions))
        .collect();
    assert_eq!(asked.len(), 1);
    assert!(asked[0].prompt.iter().any(|part| {
        part.text
            .contains("Which log level does --verbose turn on?")
    }));
    Ok(())
}

#[tokio::test]
async fn questions_you_answer_ask_even_in_auto() -> anyhow::Result<()> {
    let mut crew = asking_crew(Some(lead_picks_trace()))?;
    crew.settings.approvals.answered_by = Some(Answerer::You);
    let outcome = run(&crew).await?;
    assert!(
        matches!(outcome.exit, EngineExit::Parked(_)),
        "{:?}",
        outcome.exit
    );
    assert_eq!(open_kinds(&outcome.events), [CheckpointKind::Goal]);
    assert!(decided(&outcome.events).is_empty());
    let why = why_asked(&outcome.events, CheckpointKind::Goal).join("\n");
    assert!(why.contains("yours to answer"), "{why}");
    Ok(())
}

#[tokio::test]
async fn a_question_that_turns_on_your_fact_asks_and_says_so() -> anyhow::Result<()> {
    let needs_you = serde_json::json!({"answers": [{
        "n": 1,
        "option": null,
        "why": "nothing in the repo says",
        "needs_user": "which level your log collector keeps"
    }]});
    let crew = asking_crew(Some(needs_you))?;
    let outcome = run(&crew).await?;
    assert!(
        matches!(outcome.exit, EngineExit::Parked(_)),
        "{:?}",
        outcome.exit
    );
    assert_eq!(open_kinds(&outcome.events), [CheckpointKind::Goal]);
    let why = why_asked(&outcome.events, CheckpointKind::Goal);
    assert_eq!(
        why,
        ["the lead needs a fact only you have, which level your log collector keeps"]
    );
    Ok(())
}

/// How many goal questions became cards for you.
fn question_cards(events: &[RunEvent]) -> usize {
    events
        .iter()
        .filter(|event| {
            matches!(&event.kind, RunEventKind::Checkpoint(CheckpointEvent::Opened(opened))
                if opened.kind == CheckpointKind::Goal
                    && matches!(&opened.packet, CheckpointPacket::Goal(goal) if !goal.questions.is_empty()))
        })
        .count()
}

fn lead_asked(crew: &Crew) -> usize {
    crew.services
        .workers
        .fake("claude-code/opus")
        .expect("claude")
        .specs()
        .iter()
        .filter(|spec| spec.step == Some(StepName::AnswerQuestions))
        .count()
}

/// The review's probe: a security question, the lead's session crashing, and your goal rules
/// deciding alone. In auto `goal.always_ask` still keeps it yours, so it never reaches the
/// lead, nothing stands in for your answer, and the run waits for you.
#[tokio::test]
async fn an_always_ask_question_stays_yours_in_auto() -> anyhow::Result<()> {
    let mut crew = asking_crew_about("security", Some(Own::Crash))?;
    crew.settings.approvals.goal.decide_with = DecideWith::Rules;
    let outcome = run(&crew).await?;
    let events = &outcome.events;
    assert!(
        matches!(outcome.exit, EngineExit::Parked(_)),
        "{:?}",
        outcome.exit
    );
    assert_eq!(open_kinds(events), [CheckpointKind::Goal]);
    assert_eq!(question_cards(events), 1);
    assert!(decided(events).is_empty());
    assert!(approved_goal(events).is_none());
    assert_eq!(lead_asked(&crew), 0, "the lead never saw it");
    assert!(
        !notes(events)
            .iter()
            .any(|note| note.starts_with("The lead answered")),
        "{:?}",
        notes(events)
    );
    let why = why_asked(events, CheckpointKind::Goal);
    assert_eq!(why, ["goal.always_ask keeps security questions yours"]);
    Ok(())
}

/// When the lead's session fails, its questions come to you: the crew's recommended answer
/// never stands in, and nothing calls it the lead's.
#[tokio::test]
async fn when_the_lead_fails_its_questions_come_to_you() -> anyhow::Result<()> {
    let crew = asking_crew_about("detail", Some(Own::Crash))?;
    let outcome = run(&crew).await?;
    let events = &outcome.events;
    assert!(
        matches!(outcome.exit, EngineExit::Parked(_)),
        "{:?}",
        outcome.exit
    );
    assert_eq!(question_cards(events), 1);
    assert!(approved_goal(events).is_none());
    assert_eq!(lead_asked(&crew), 1);
    let said = notes(events);
    assert!(
        said.iter().any(|note| note
            .starts_with("The lead couldn't answer the goal's questions, so they come to you")),
        "{said:?}"
    );
    assert!(
        !said
            .iter()
            .any(|note| note.starts_with("The lead answered")),
        "{said:?}"
    );
    assert_eq!(
        why_asked(events, CheckpointKind::Goal),
        ["the lead couldn't answer it"]
    );
    Ok(())
}

/// Chosen yourself, `answered_by = "lead"` lets the lead answer every question, the ones
/// `goal.always_ask` names too, so the run goes on without you.
#[tokio::test]
async fn with_the_lead_chosen_it_answers_always_ask_questions_too() -> anyhow::Result<()> {
    let mut crew = asking_crew_about("security", Some(Own::Answer(lead_picks_trace())))?;
    crew.settings.approvals.answered_by = Some(Answerer::Lead);
    let outcome = run(&crew).await?;
    let events = &outcome.events;
    assert!(
        matches!(outcome.exit, EngineExit::Finished(_)),
        "{:?}: open {:?}, checks {:#?}",
        outcome.exit,
        open_kinds(events),
        checks(events)
    );
    assert_eq!(question_cards(events), 0);
    let goal = approved_goal(events).expect("the goal was approved");
    assert!(
        goal.assumptions
            .iter()
            .any(|assumption| assumption.by_lead())
    );
    Ok(())
}

/// A must-ask difference held back over the question limit counts as an open question, so no
/// approver settles the goal in your place, though the lead answers in auto.
#[tokio::test]
async fn a_held_back_must_ask_difference_still_waits_for_you() -> anyhow::Result<()> {
    let mut crew = asking_crew_about("scope", Some(Own::Answer(lead_picks_trace())))?;
    crew.settings.goal.max_questions = 0;
    let outcome = run(&crew).await?;
    let events = &outcome.events;
    assert!(
        matches!(outcome.exit, EngineExit::Parked(_)),
        "{:?}",
        outcome.exit
    );
    assert_eq!(open_kinds(events), [CheckpointKind::Goal]);
    assert_eq!(question_cards(events), 0, "held back, not asked");
    assert!(approved_goal(events).is_none());
    let why = why_asked(events, CheckpointKind::Goal).join("\n");
    assert!(
        why.contains("1 question is open, and it's yours to answer"),
        "{why}"
    );
    Ok(())
}

#[tokio::test]
async fn in_supervised_mode_the_questions_come_to_you() -> anyhow::Result<()> {
    let crew = asking_crew(Some(lead_picks_trace()))?;
    let outcome = run_in(&crew, Some(ApprovalMode::Supervised)).await?;
    assert!(
        matches!(outcome.exit, EngineExit::Parked(_)),
        "{:?}",
        outcome.exit
    );
    assert_eq!(open_kinds(&outcome.events), [CheckpointKind::Goal]);
    let claude = crew
        .services
        .workers
        .fake("claude-code/opus")
        .expect("claude");
    assert!(
        claude
            .specs()
            .iter()
            .all(|spec| spec.step != Some(StepName::AnswerQuestions))
    );
    // Supervised waits for you anyway, so the card has no auto reason to give.
    assert!(why_asked(&outcome.events, CheckpointKind::Goal).is_empty());
    Ok(())
}

#[tokio::test]
async fn the_lead_s_answers_the_approver_rejects_ask_with_its_reason() -> anyhow::Result<()> {
    let unsure = FakeWorker::harness("antigravity-cli", "gemini")
        .with_cheapest_model("gemini-3.6-flash-low")
        .respond_with(|spec: &SessionSpec| {
            if spec.step == Some(StepName::ModelApprover) {
                return FakeSession::structured(serde_json::json!({
                    "verdict": "escalate",
                    "reasons": ["the request doesn't settle the log level"],
                    "rules_cited": []
                }));
            }
            answer(spec)
        });
    let crew = asking_crew_judged_by(Some(lead_picks_trace()), unsure)?;
    let outcome = run(&crew).await?;
    assert!(
        matches!(outcome.exit, EngineExit::Parked(_)),
        "{:?}",
        outcome.exit
    );
    assert_eq!(open_kinds(&outcome.events), [CheckpointKind::Goal]);
    let why = why_asked(&outcome.events, CheckpointKind::Goal).join("\n");
    assert!(why.contains("doesn't settle the log level"), "{why}");
    Ok(())
}

/// The sample run's case: All hands, the lead picks a workflow that drafts no plan, and in auto
/// mode its plan approval used to ask, since its evidence named no commit and no facts.
#[tokio::test]
async fn a_workflow_the_lead_picks_goes_to_merge_without_asking() -> anyhow::Result<()> {
    let mut crew = Crew::new()?;
    with_gates(&mut crew);
    let design = serde_json::json!({
        "template": "solo-reviewed",
        "params": [],
        "composed": null,
        "why": "One small change, reviewed by another family"
    });
    let own = vec![(StepName::DesignWorkflow, Own::Answer(design))];
    enlist_with(&mut crew, "claude-code/opus", "claude", None, "detail", own);
    enlist(&mut crew, "codex/gpt-x", "gpt");
    crew.services.workers.add(
        FakeWorker::harness("antigravity-cli", "gemini")
            .with_cheapest_model("gemini-3.6-flash-low")
            .respond_with(answer),
    );
    crew.settings.triage.all_hands = true;
    crew.settings.workflows.adapt_at_phase_ends = false;

    let outcome = run(&crew).await?;
    let events = &outcome.events;
    assert!(
        matches!(outcome.exit, EngineExit::Finished(_)),
        "{:?}: open {:?}, checks {:#?}",
        outcome.exit,
        open_kinds(events),
        checks(events)
    );
    let picked = events.iter().any(|event| {
        matches!(&event.kind, RunEventKind::Workflow(argonauts_core::events::workflow::WorkflowEvent::Started(started))
            if started.from == Some(WorkflowRef::new("solo-reviewed")))
    });
    assert!(picked, "{:?}", names(events));
    let plans: Vec<_> = decided(events)
        .into_iter()
        .filter(|(kind, _)| *kind == CheckpointKind::Plan)
        .collect();
    let approver: WorkerRef = APPROVER.parse()?;
    assert_eq!(
        plans,
        [(CheckpointKind::Plan, Party::Approver { model: approver })]
    );
    Ok(())
}

#[tokio::test]
async fn a_plan_that_owns_a_protected_path_asks() -> anyhow::Result<()> {
    let mut crew = crew()?;
    crew.settings.limits.protected_paths = vec![FILE.to_owned()];
    let outcome = run(&crew).await?;
    assert!(
        matches!(outcome.exit, EngineExit::Parked(_)),
        "{:?}",
        outcome.exit
    );
    assert_eq!(open_kinds(&outcome.events), [CheckpointKind::Plan]);
    let why = reasons(&outcome.events, CheckpointKind::Plan).join("\n");
    assert!(why.contains("protected paths"), "{why}");
    Ok(())
}

/// `gpt` and `gpt-oss` are two families, so they may review each other's cards, but one
/// company, so their review doesn't let auto merge.
#[tokio::test]
async fn a_merge_reviewed_only_within_one_company_asks() -> anyhow::Result<()> {
    let mut crew = Crew::new()?;
    with_gates(&mut crew);
    crew.services
        .registry
        .add(serde_json::from_value(serde_json::json!({
            "id": "opencode/gpt-oss-120b",
            "family": "gpt-oss",
            "kind": "harness",
            "access": "local",
            "cost": {"unit": "none"},
        }))?);
    enlist(&mut crew, "codex/gpt-x", "gpt");
    enlist(&mut crew, "opencode/gpt-oss-120b", "gpt-oss");
    crew.services.workers.add(
        FakeWorker::harness("antigravity-cli", "gemini")
            .with_cheapest_model("gemini-3.6-flash-low")
            .respond_with(answer),
    );
    let outcome = run(&crew).await?;
    assert!(
        matches!(outcome.exit, EngineExit::Parked(_)),
        "{:?}",
        outcome.exit
    );
    assert_eq!(open_kinds(&outcome.events), [CheckpointKind::Merge]);
    let why = reasons(&outcome.events, CheckpointKind::Merge).join("\n");
    assert!(why.contains("reviewer of its owner's own company"), "{why}");
    Ok(())
}

/// With no handoff list set, a stopped card's candidates are the other installed CLIs that can
/// take work: none of the stopped vendor's, no endpoint, none that's down, best record on this
/// kind of card first, then the most quota left.
#[tokio::test]
async fn a_stopped_card_goes_to_the_other_installed_clis() -> anyhow::Result<()> {
    use argonauts_core::clock::Clock;
    use argonauts_core::events::card::{CardKind, CardSpec};
    use argonauts_core::limits::{
        HealthState, QuotaBoard, QuotaReading, QuotaSource, QuotaStatus, TrackRecord,
    };
    use argonauts_core::settings::handoff::FallbackEntry;
    use argonauts_crew::handoff::defaults;

    let mut crew = Crew::new()?;
    enlist(&mut crew, "claude-code/opus", "claude");
    let services = &crew.services;
    for (worker, family) in [
        ("claude-code/sonnet", "claude"),
        ("codex/gpt-x", "gpt"),
        ("antigravity-cli", "gemini"),
        ("opencode/glm-5", "glm"),
        ("hermes", "qwen"),
    ] {
        services.workers.add(FakeWorker::harness(worker, family));
    }
    services
        .workers
        .add(FakeWorker::endpoint("openrouter/deepseek", "deepseek"));
    let r = |text: &str| -> WorkerRef { text.parse().expect("a worker reference") };
    services.health.set(
        &r("hermes").vendor(),
        HealthState::Down,
        Some("not logged in"),
    );
    let record = |passed| TrackRecord { passed, of: 5 };
    services
        .track_records
        .set(r("codex/gpt-x"), "build", record(4));
    services
        .track_records
        .set(r("antigravity-cli"), "build", record(4));
    services
        .track_records
        .set(r("opencode/glm-5"), "build", record(1));
    for (vendor, used) in [("codex", 0.7), ("antigravity-cli", 0.2)] {
        services.quota.observe(QuotaReading {
            vendor: r(vendor).vendor(),
            status: QuotaStatus::Allowed,
            used: Some(used),
            resets_at: None,
            window: None,
            source: QuotaSource::UsageProbe,
            observed_at: services.clock.now(),
            detail: None,
        });
    }

    let (host, _) = crew.host(RunMode::Crew)?;
    let stopped = r("claude-code/opus");
    let card = CardSpec {
        id: "card-001".parse()?,
        title: "Add the --verbose flag".to_owned(),
        task: "Add a --verbose flag.".to_owned(),
        kind: CardKind::Build,
        owned_paths: vec![FILE.to_owned()],
        read_only_paths: Vec::new(),
        forbidden_paths: Vec::new(),
        hidden_paths: Vec::new(),
        depends_on: Vec::new(),
        criteria: Vec::new(),
        owner: Some(stopped.clone()),
        reviewers: Vec::new(),
        budget: Default::default(),
        predictions: None,
    };
    let expected: Vec<FallbackEntry> = ["antigravity-cli", "codex/gpt-x", "opencode/glm-5"]
        .into_iter()
        .map(|worker| FallbackEntry::Worker(r(worker)))
        .collect();
    assert!(defaults::apply(&host, &card, &stopped));
    assert_eq!(defaults::candidates(&host, &card, &stopped), expected);
    Ok(())
}
