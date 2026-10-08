//! The autonomous defaults end to end, with the real approval policy over fake workers: a run
//! goes from goal to merge without asking you, the lead answering the crew's questions, and
//! each case that must still ask does, saying why.

mod support;

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use anyhow::Context;

use argonauts_core::budget::BudgetDimension;
use argonauts_core::engine::EngineExit;
use argonauts_core::events::card::IntegrationEvent;
use argonauts_core::events::checkpoint::{
    AnswerAction, CheckpointEvent, CheckpointKind, CheckpointPacket, PolicyCheck,
};
use argonauts_core::events::goal::{GoalEvent, GoalFile};
use argonauts_core::events::run::{Party, RunLifecycle, RunMode};
use argonauts_core::events::session::EndReason;
use argonauts_core::events::{RunEvent, RunEventKind};
use argonauts_core::names::{RoleName, WorkerRef};
use argonauts_core::settings::approvals::{
    Answerer, ApprovalMode, AtHardLimit, DecideWith, Decides,
};
use argonauts_core::settings::handoff::{CrewRole, FallbackEntry};
use argonauts_core::settings::protocol::GateCommand;
use argonauts_core::settings::workflows::LeadDesigns;
use argonauts_core::steps::StepName;
use argonauts_core::worker::SessionSpec;
use argonauts_core::workflow::WorkflowRef;
use argonauts_crew::engine::shared::with_git_env;
use argonauts_testkit::{FakeSession, FakeWorker};

use support::{Crew, Outcome, names};

const FILE: &str = "verbose.txt";
const NOTES: &str = "notes.txt";

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
    /// The first such session on `card` across the crew hits its budget; the rest go on.
    BudgetHitOnce {
        card: &'static str,
        hit: Arc<AtomicBool>,
    },
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
            Some((_, Own::BudgetHitOnce { card, hit }))
                if spec.card.as_ref().is_some_and(|on| on.as_str() == *card)
                    && !hit.swap(true, Ordering::SeqCst) =>
            {
                return FakeSession::ended(EndReason::BudgetHit {
                    dimension: BudgetDimension::Spend,
                });
            }
            Some((_, Own::BudgetHitOnce { .. })) | None => {}
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
                // A plan's second card, when it has one, writes a file of its own.
                let file = match spec.card.as_ref().map(|card| card.as_str()) {
                    Some("task-002") => NOTES,
                    _ => FILE,
                };
                std::fs::write(spec.workdir.join(file), "--verbose\n")
                    .expect("the build session writes its file");
                // Written with the agent's edit tool, as a CLI reports it.
                return answer(spec).with_event(support::wrote(file));
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
        crew.settings.main_agent = reference.clone();
    }
    crew.settings.crew.add(reference);
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
    crew.settings.checks.run = vec![passing("tests")];
    crew.settings.checks.full_suite = vec![passing("suite")];
}

/// Starts the run with the real policy in place of the fakes' asking one, and drives it until
/// it parks or finishes. The policy keeps its records in the repo's state folder, as it does
/// under `~/.argonauts/state`.
async fn run(crew: &Crew) -> anyhow::Result<Outcome> {
    run_in(crew, None).await
}

/// [`run`], with Shift+Tab's `mode` chosen before the run's first decision.
async fn run_in(crew: &Crew, mode: Option<ApprovalMode>) -> anyhow::Result<Outcome> {
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
    drive(crew, history).await
}

/// Drives the engine over `history`, a new run or a parked one, with the real policy in place
/// of the fakes' asking one, until it parks or finishes. Its events include `history`.
async fn drive(crew: &Crew, history: Vec<RunEvent>) -> anyhow::Result<Outcome> {
    let (mut context, sink, _commands) = crew.context(history)?;
    context.services.policy = real_policy(crew, &context.settings)?;
    let engine = argonauts_crew::engine_for(&crew.settings, RunMode::Crew).run(context);
    let exit = with_git_env(crew.git_env(), engine).await;
    Ok(Outcome {
        exit,
        events: sink.events(),
    })
}

/// The real policy, keeping its records in the repo's state folder, as it does under
/// `~/.argonauts/state`.
fn real_policy(
    crew: &Crew,
    settings: &Arc<argonauts_core::settings::Settings>,
) -> anyhow::Result<Arc<dyn argonauts_core::policy::ApprovalPolicy>> {
    let repo = crew.repo_context()?;
    let state = repo
        .run_dir
        .parent()
        .and_then(Path::parent)
        .context("the run folder is under the repo's state folder")?;
    std::fs::create_dir_all(state)?;
    let services = &crew.services;
    Ok(argonauts_policy::approval_policy_with_models(
        settings.clone(),
        Arc::new(|| None),
        state,
        services.workers.clone(),
        services.registry.clone(),
        services.steps.clone(),
    ))
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
async fn with_no_agent_of_another_provider_the_goal_asks_with_a_warning() -> anyhow::Result<()> {
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
    assert!(why.contains("another provider than Anthropic"), "{why}");
    assert!(why.contains("pick an approver in /approvals"), "{why}");
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

/// In full auto a hard limit does what the checkpoint's `at_hard_limit` says: the merge past
/// the daily cap opens a pull request, and set to park it waits for you with one line why.
#[tokio::test]
async fn in_autopilot_a_hard_limit_does_what_you_set() -> anyhow::Result<()> {
    let mut crew = crew()?;
    crew.settings.limits.auto_merges_per_day = 0;
    crew.settings.approvals.autopilot = true;
    crew.settings.approvals.merge.at_hard_limit = AtHardLimit::OpenPullRequest;
    let outcome = run(&crew).await?;
    assert!(
        matches!(
            outcome.exit,
            EngineExit::Finished(argonauts_core::events::run::RunOutcome::PullRequest)
        ),
        "{:?}: open {:?}",
        outcome.exit,
        open_kinds(&outcome.events)
    );
    assert!(decided(&outcome.events).contains(&(CheckpointKind::Merge, Party::Autopilot)));
    let said = notes(&outcome.events);
    assert!(
        said.iter()
            .any(|note| note.contains("with \"Open a pull request\" instead of asking you")),
        "{said:?}"
    );

    crew.settings.approvals.merge.at_hard_limit = AtHardLimit::Park;
    let outcome = run(&crew).await?;
    assert!(
        matches!(outcome.exit, EngineExit::Parked(_)),
        "{:?}",
        outcome.exit
    );
    assert_eq!(open_kinds(&outcome.events), [CheckpointKind::Merge]);
    let said = notes(&outcome.events);
    assert!(
        said.iter()
            .any(|note| note.contains("to you, as your settings say for a hard limit")),
        "{said:?}"
    );
    Ok(())
}

/// Another round can't change the daily merge cap, so a merge set to send back at a hard limit
/// stops there instead: the run ends with its branch kept, and the merge checkpoint opens once.
#[tokio::test]
async fn in_autopilot_a_cap_another_round_cant_change_stops_instead_of_sending_back()
-> anyhow::Result<()> {
    let mut crew = crew()?;
    crew.settings.limits.auto_merges_per_day = 0;
    crew.settings.approvals.autopilot = true;
    crew.settings.approvals.merge.at_hard_limit = AtHardLimit::SendBack;
    let outcome = tokio::time::timeout(std::time::Duration::from_secs(60), run(&crew))
        .await
        .context("the run never ended")??;
    assert!(
        matches!(
            outcome.exit,
            EngineExit::Finished(argonauts_core::events::run::RunOutcome::BranchLeft)
        ),
        "{:?}",
        outcome.exit
    );
    let merges: Vec<_> = decided(&outcome.events)
        .into_iter()
        .filter(|(kind, _)| *kind == CheckpointKind::Merge)
        .collect();
    assert_eq!(merges, [(CheckpointKind::Merge, Party::Autopilot)]);
    Ok(())
}

/// A plan set to send back at a hard limit another round could clear goes back once, telling
/// the lead which limit stopped it. When the next plan stops at a hard limit too, full auto
/// runs the work as a checked solo run instead of asking. The solo card can't
/// land the protected path either, so it's deferred with its work kept, and nothing waits.
#[tokio::test]
async fn in_autopilot_a_hard_limit_sends_back_once_and_says_why() -> anyhow::Result<()> {
    let mut crew = crew()?;
    crew.settings.limits.protected_paths = vec![FILE.to_owned()];
    crew.settings.approvals.autopilot = true;
    crew.settings.approvals.plan.at_hard_limit = AtHardLimit::SendBack;
    let outcome = tokio::time::timeout(std::time::Duration::from_secs(60), run(&crew))
        .await
        .context("the run never ended")??;
    let sent_back: Vec<String> = outcome
        .events
        .iter()
        .filter_map(|event| match &event.kind {
            RunEventKind::Checkpoint(CheckpointEvent::Decided(decided))
                if decided.by == Party::Autopilot && decided.action == AnswerAction::SendBack =>
            {
                decided.text.clone()
            }
            _ => None,
        })
        .collect();
    assert_eq!(sent_back.len(), 1, "{sent_back:?}");
    assert!(
        sent_back[0].starts_with("Sent back at a hard limit: ")
            && sent_back[0].contains("protected"),
        "{sent_back:?}"
    );
    let plans = outcome
        .events
        .iter()
        .filter(|event| {
            matches!(&event.kind, RunEventKind::Checkpoint(CheckpointEvent::Opened(opened))
                if opened.kind == CheckpointKind::Plan)
        })
        .count();
    assert_eq!(plans, 2);
    let solo = outcome.events.iter().any(|event| {
        matches!(&event.kind, RunEventKind::Checkpoint(CheckpointEvent::Decided(decided))
            if decided.by == Party::Autopilot && decided.action == AnswerAction::SwitchToSolo)
    });
    assert!(solo, "{:#?}", notes(&outcome.events));
    assert!(
        matches!(outcome.exit, EngineExit::Finished(_)),
        "{:?}: open {:?}",
        outcome.exit,
        open_kinds(&outcome.events)
    );
    assert!(waited_for_you(&outcome.events).is_empty());
    assert!(!crew.repo.root().join(FILE).exists());
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

    // In full auto the same merge keeps the branch instead, and says why.
    let outcome = run_in(&crew, Some(ApprovalMode::Autopilot)).await?;
    assert!(
        matches!(
            outcome.exit,
            EngineExit::Finished(argonauts_core::events::run::RunOutcome::BranchLeft)
        ),
        "{:?}: open {:?}",
        outcome.exit,
        open_kinds(&outcome.events)
    );
    assert!(decided(&outcome.events).contains(&(CheckpointKind::Merge, Party::Autopilot)));
    let said = notes(&outcome.events);
    assert!(
        said.iter().any(
            |note| note.contains("with \"Keep the branch\" instead of asking you")
                && note.contains("isn't what the goal asked for")
        ),
        "{said:?}"
    );
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

/// The lead's role goes only to the crew's other agent, which fails the same way, so nobody
/// answers in the lead's place.
fn only_the_crew_leads(crew: &mut Crew) -> anyhow::Result<()> {
    let codex: WorkerRef = "codex/gpt-x".parse()?;
    let lead = CrewRole::of(RoleName::PhaseLead);
    let lead = crew.settings.fallback.roles.entry(lead).or_default();
    lead.order = vec![FallbackEntry::Worker(codex)];
    Ok(())
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
            .any(|note| note.starts_with("The main agent answered 1 of the goal's questions")),
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
    crew.settings.approvals.answers = Some(Answerer::You);
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
        ["the main agent needs a fact only you have, which level your log collector keeps"]
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
/// deciding alone. In auto `approvals.ask_you_about` keeps it yours, so it never reaches the
/// lead, nothing stands in for your answer, and the run waits for you.
#[tokio::test]
async fn a_topic_ask_you_about_names_stays_yours_in_auto() -> anyhow::Result<()> {
    let mut crew = asking_crew_about("security", Some(Own::Crash))?;
    crew.settings.approvals.goal.decide_with = DecideWith::Rules;
    // `understand.always_ask` only makes it a question; this keeps it yours.
    crew.settings.approvals.ask_you_about = vec!["Security".to_owned()];
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
    assert_eq!(lead_asked(&crew), 0, "the main agent never saw it");
    assert!(
        !notes(events)
            .iter()
            .any(|note| note.starts_with("The main agent answered")),
        "{:?}",
        notes(events)
    );
    let why = why_asked(events, CheckpointKind::Goal);
    assert_eq!(
        why,
        ["your /approvals settings keep security questions yours"]
    );
    Ok(())
}

/// A model picked to answer answers in full auto only. In auto a question
/// `approvals.ask_you_about` names stays yours all the same, and the picked model is never
/// asked.
#[tokio::test]
async fn a_picked_model_leaves_ask_you_about_questions_yours_in_auto() -> anyhow::Result<()> {
    let mut crew = asking_crew_about("security", Some(Own::Crash))?;
    crew.settings.approvals.goal.decide_with = DecideWith::Rules;
    crew.settings.approvals.ask_you_about = vec!["security".to_owned()];
    crew.settings.approvals.answers = Some(Answerer::Model("opencode/glm-5".parse()?));
    crew.services
        .workers
        .add(FakeWorker::harness("opencode", "zhipu").respond_with(answer));
    let outcome = run(&crew).await?;
    let events = &outcome.events;
    assert!(
        matches!(outcome.exit, EngineExit::Parked(_)),
        "{:?}",
        outcome.exit
    );
    assert_eq!(question_cards(events), 1);
    let why = why_asked(events, CheckpointKind::Goal);
    assert_eq!(
        why,
        ["your /approvals settings keep security questions yours"]
    );
    let picked = crew
        .services
        .workers
        .fake("opencode")
        .expect("the picked model");
    assert!(
        picked
            .specs()
            .iter()
            .all(|spec| spec.step != Some(StepName::AnswerQuestions)),
        "the picked model answered outside autopilot"
    );
    Ok(())
}

/// When the lead's session fails, its questions come to you: the crew's recommended answer
/// never stands in, and nothing calls it the lead's.
#[tokio::test]
async fn when_the_lead_fails_its_questions_come_to_you() -> anyhow::Result<()> {
    let mut crew = asking_crew_about("detail", Some(Own::Crash))?;
    only_the_crew_leads(&mut crew)?;
    let outcome = run(&crew).await?;
    let events = &outcome.events;
    assert!(
        matches!(outcome.exit, EngineExit::Parked(_)),
        "{:?}",
        outcome.exit
    );
    assert_eq!(question_cards(events), 1);
    assert!(approved_goal(events).is_none());
    // A crash is a vendor failure, so the lead is asked once more before it counts as failed.
    assert_eq!(lead_asked(&crew), 2);
    let said = notes(events);
    assert!(
        said.iter().any(|note| note.starts_with(
            "The main agent couldn't answer the goal's questions, so they come to you"
        )),
        "{said:?}"
    );
    assert!(
        !said
            .iter()
            .any(|note| note.starts_with("The main agent answered")),
        "{said:?}"
    );
    assert_eq!(
        why_asked(events, CheckpointKind::Goal),
        ["the main agent couldn't answer it"]
    );
    Ok(())
}

/// Full auto as /approvals sets it up: the one switch on, a model you picked answering the
/// crew's questions in the lead's place, the topics `approvals.ask_you_about` names too, and an
/// approver you picked checking its answers. The lead is never asked.
#[tokio::test]
async fn a_picked_model_answers_in_the_leads_place_and_a_picked_approver_checks()
-> anyhow::Result<()> {
    let mut crew = asking_crew_about("security", None)?;
    let picked: WorkerRef = "opencode/glm-5".parse()?;
    crew.services
        .workers
        .add(
            FakeWorker::harness("opencode", "zhipu").respond_with(|spec: &SessionSpec| {
                if spec.step == Some(StepName::AnswerQuestions) {
                    return FakeSession::structured(lead_picks_trace());
                }
                answer(spec)
            }),
        );
    let approver: WorkerRef = "antigravity-cli/gemini-3.6-pro".parse()?;
    crew.settings.approvals.autopilot = true;
    crew.settings.approvals.answers = Some(Answerer::Model(picked.clone()));
    crew.settings.approvals.decides = approver.clone().into();
    crew.settings.approvals.ask_you_about = vec!["security".to_owned()];

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
    assert_eq!(lead_asked(&crew), 0, "the main agent was never asked");
    let answerer = crew
        .services
        .workers
        .fake("opencode")
        .expect("the picked model");
    let asked: Vec<SessionSpec> = answerer
        .specs()
        .into_iter()
        .filter(|spec| spec.step == Some(StepName::AnswerQuestions))
        .collect();
    assert_eq!(asked.len(), 1);
    assert_eq!(asked[0].worker, picked);

    let goal = approved_goal(events).expect("the goal was approved");
    let assumed: Vec<_> = goal
        .assumptions
        .iter()
        .filter(|assumption| assumption.by_lead())
        .collect();
    assert_eq!(assumed.len(), 1, "{:?}", goal.assumptions);
    assert!(assumed[0].text.ends_with(": trace"), "{:?}", assumed[0]);
    // Named as people read it, never by its id.
    let named = argonauts_core::display_names::DisplayNames::default().worker(&picked);
    assert_ne!(named, picked.to_string());
    assert!(
        notes(events)
            .iter()
            .any(|note| note.starts_with(&format!("{named} answered 1 of the goal's questions"))),
        "{:?}",
        notes(events)
    );

    // The switch put every checkpoint in full auto, and the approver you picked checked.
    let modes: Vec<ApprovalMode> = events
        .iter()
        .filter_map(|event| match &event.kind {
            RunEventKind::Checkpoint(CheckpointEvent::Opened(opened)) => Some(opened.mode),
            _ => None,
        })
        .collect();
    assert!(!modes.is_empty());
    assert!(
        modes.iter().all(|mode| *mode == ApprovalMode::Autopilot),
        "{modes:?}"
    );
    let by_approver = Party::Approver {
        model: approver.clone(),
    };
    assert!(
        decided(events).contains(&(CheckpointKind::Goal, by_approver)),
        "{:?}",
        decided(events)
    );
    let checked: Vec<SessionSpec> = crew
        .services
        .workers
        .fake("antigravity-cli")
        .expect("the approver")
        .specs()
        .into_iter()
        .filter(|spec| spec.step == Some(StepName::ModelApprover))
        .collect();
    assert!(!checked.is_empty());
    assert!(checked.iter().all(|spec| spec.worker == approver));
    Ok(())
}

/// Chosen yourself, `answers = "lead"` lets the lead answer every question, the ones
/// `understand.always_ask` makes questions too, so the run goes on without you.
#[tokio::test]
async fn with_the_lead_chosen_it_answers_always_ask_questions_too() -> anyhow::Result<()> {
    let mut crew = asking_crew_about("security", Some(Own::Answer(lead_picks_trace())))?;
    crew.settings.approvals.answers = Some(Answerer::Main);
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

/// A model picked to answer that can't, such as one no agent provides, leaves the goal's
/// questions to the lead in full auto, after one line that says so, and the run goes on.
#[tokio::test]
async fn a_picked_model_that_cant_answer_the_goal_leaves_it_to_the_lead() -> anyhow::Result<()> {
    let mut crew = asking_crew_about("security", Some(Own::Answer(lead_picks_trace())))?;
    crew.settings.approvals.autopilot = true;
    crew.settings.approvals.answers = Some(Answerer::Model("nobody/x".parse()?));
    let outcome = run(&crew).await?;
    let events = &outcome.events;
    assert!(
        matches!(outcome.exit, EngineExit::Finished(_)),
        "{:?}: open {:?}, checks {:#?}",
        outcome.exit,
        open_kinds(events),
        checks(events)
    );
    assert_eq!(lead_asked(&crew), 1, "the main agent answered in its place");
    let said = notes(events).join("\n");
    assert!(
        said.contains("couldn't answer the goal's questions, so the main agent answers them"),
        "{said}"
    );
    let goal = approved_goal(events).expect("the goal was approved");
    assert!(
        goal.assumptions
            .iter()
            .any(|assumption| assumption.by_lead() && assumption.text.ends_with(": trace")),
        "{:?}",
        goal.assumptions
    );
    Ok(())
}

/// A must-ask difference held back over the question limit counts as an open question, so no
/// approver settles the goal in your place, though the lead answers in auto.
#[tokio::test]
async fn a_held_back_difference_ask_you_about_names_still_waits_for_you() -> anyhow::Result<()> {
    let mut crew = asking_crew_about("scope", Some(Own::Answer(lead_picks_trace())))?;
    crew.settings.understand.max_questions = 0;
    crew.settings.approvals.ask_you_about = vec!["scope".to_owned()];
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

/// The sample run's case: All hands, the lead picks a workflow that drafts no plan, which
/// `lead_designs = "any"` allows, and in auto mode its plan approval used to ask, since its
/// evidence named no commit and no facts.
#[tokio::test]
async fn a_workflow_the_lead_picks_goes_to_merge_without_asking() -> anyhow::Result<()> {
    let mut crew = Crew::new()?;
    with_gates(&mut crew);
    let design = support::brief(
        Some("maker-checker"),
        serde_json::json!([]),
        None,
        "One small change, reviewed by another provider",
    );
    let own = vec![(StepName::DesignWorkflow, Own::Answer(design))];
    enlist_with(&mut crew, "claude-code/opus", "claude", None, "detail", own);
    enlist(&mut crew, "codex/gpt-x", "gpt");
    crew.services.workers.add(
        FakeWorker::harness("antigravity-cli", "gemini")
            .with_cheapest_model("gemini-3.6-flash-low")
            .respond_with(answer),
    );
    crew.settings.ultracode = true;
    crew.settings.workflows.lead_designs = LeadDesigns::ANY;
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
            if started.from == Some(WorkflowRef::new("maker-checker")))
    });
    assert!(picked, "{:?}", names(events));
    // One card before anything writes: Understand's, which the approver settles in auto. A
    // solo build needs no plan, so no plan checkpoint follows it.
    let approver: WorkerRef = APPROVER.parse()?;
    let starts: Vec<_> = decided(events)
        .into_iter()
        .filter(|(kind, _)| matches!(kind, CheckpointKind::Goal | CheckpointKind::Plan))
        .collect();
    assert_eq!(
        starts,
        [(CheckpointKind::Goal, Party::Approver { model: approver })]
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
    assert!(
        why.contains("reviewer of its worker's own provider"),
        "{why}"
    );
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
            wrap_up: false,
        });
    }

    let (host, _) = crew.host(RunMode::Crew)?;
    let stopped = r("claude-code/opus");
    let card = CardSpec {
        id: "task-001".parse()?,
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
        item: None,
    };
    let expected: Vec<FallbackEntry> = ["antigravity-cli", "codex/gpt-x", "opencode/glm-5"]
        .into_iter()
        .map(|worker| FallbackEntry::Worker(r(worker)))
        .collect();
    assert!(defaults::apply(&host, &card, &stopped));
    assert_eq!(defaults::candidates(&host, &card, &stopped), expected);
    Ok(())
}

/// Full auto, from request to merge with no card. The differs disagree on a
/// scope question, which `approvals.ask_you_about` keeps yours in auto, and the card's first
/// build hits its budget with no reserve to release. The lead answers the question as an assumption the
/// approver checks, the budget hit ends that attempt safely and says so, and the run merges.
#[tokio::test]
async fn autopilot_goes_from_request_to_merge_with_no_card() -> anyhow::Result<()> {
    let mut crew = crew()?;
    crew.services.workers = Default::default();
    crew.settings.crew.clear();
    // Limits of your own, since this run is about one being reached.
    crew.settings.limits.budget.spend_per_run = Some(20.0);
    crew.settings.limits.budget.per_task_usd = Some(3.0);
    let hit = Arc::new(AtomicBool::new(false));
    let card = |key: &str, file: &str| {
        serde_json::json!({
            "key": key,
            "title": format!("Write {file}"),
            "task": format!("Write {file}."),
            "kind": "build",
            "owned_paths": [file],
            "read_only_paths": [],
            "depends_on": [],
            "criteria": [],
        })
    };
    let plan = serde_json::json!({"cards": [card("flag", FILE), card("notes", NOTES)]});
    for (worker, family) in [("claude-code/opus", "claude"), ("codex/gpt-x", "gpt")] {
        let own = vec![
            (StepName::AnswerQuestions, Own::Answer(lead_picks_trace())),
            (StepName::PlanToCards, Own::Answer(plan.clone())),
            (
                StepName::Build,
                Own::BudgetHitOnce {
                    card: "task-001",
                    hit: hit.clone(),
                },
            ),
        ];
        enlist_with(&mut crew, worker, family, QUESTION, "scope", own);
    }
    crew.services.workers.add(
        FakeWorker::harness("antigravity-cli", "gemini")
            .with_cheapest_model("gemini-3.6-flash-low")
            .respond_with(answer),
    );
    crew.settings.approvals.ask_you_about = vec!["scope".to_owned()];

    let outcome = run_in(&crew, Some(ApprovalMode::Autopilot)).await?;
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
    assert!(hit.load(Ordering::SeqCst), "no build hit its limit");
    // The card past its budget waits, deferred with its work kept, and the other one landed.
    assert!(crew.repo.root().join(NOTES).exists());
    assert!(!crew.repo.root().join(FILE).exists());

    // Nothing waited for you: no card asked, and none was answered by you.
    let asked = waited_for_you(events);
    assert!(asked.is_empty(), "{asked:#?}");
    assert!(
        decided(events).iter().all(|(_, by)| *by != Party::User),
        "{:?}",
        decided(events)
    );
    assert_eq!(question_cards(events), 0);

    // The scope question is the lead's assumption, which the approver checked with the goal.
    let goal = approved_goal(events).expect("the goal was approved");
    assert!(
        goal.assumptions
            .iter()
            .any(|assumption| assumption.by_lead() && assumption.text.ends_with(": trace")),
        "{:?}",
        goal.assumptions
    );
    let approver: WorkerRef = APPROVER.parse()?;
    assert!(decided(events).contains(&(CheckpointKind::Goal, Party::Approver { model: approver })));

    // The budget past its reserve ended that card's part safely instead of asking, and the
    // transcript says what full auto chose and why.
    assert!(decided(events).contains(&(CheckpointKind::Budget, Party::Autopilot)));
    let said = notes(events);
    let ended = |what: &str, why: &str| {
        said.iter().any(|note| {
            note.starts_with("Autopilot answered \"task-001")
                && note.contains(&format!(
                    "with \"{what}\" instead of asking you, since {why}"
                ))
        })
    };
    assert!(
        ended("Stop and ask me", "the remaining tasks need"),
        "{said:?}"
    );
    assert!(ended("Defer it", "no fallback"), "{said:?}");

    // What the policy found is kept for review, and the closing answer can list each ending
    // as the transcript said it.
    let state = argonauts_crew::state::RunState::fold(events);
    let endings = argonauts_crew::checkpoints::autopilot::endings(&state);
    assert_eq!(endings.len(), 2, "{endings:#?}");
    for ending in &endings {
        assert!(
            said.contains(&ending.as_str()),
            "{ending} isn't in {said:#?}"
        );
    }
    Ok(())
}

/// The checkpoints that waited for you: the policy left them to you, and full auto didn't
/// settle them in your place.
fn waited_for_you(events: &[RunEvent]) -> Vec<&PolicyCheck> {
    let settled: Vec<_> = events
        .iter()
        .filter_map(|event| match &event.kind {
            RunEventKind::Checkpoint(CheckpointEvent::Decided(decided))
                if decided.by == Party::Autopilot =>
            {
                Some(decided.id.clone())
            }
            _ => None,
        })
        .collect();
    checks(events)
        .into_iter()
        .filter(|check| !check.shadow && check.verdict.is_none() && !settled.contains(&check.id))
        .collect()
}

/// Shift+Tab to full auto while a goal question waits for you leaves it to the lead, as if
/// full auto had been on: the lead's answer becomes the goal's assumption, which the approver
/// checks with the goal. Nothing in the transcript names another answer.
#[tokio::test]
async fn autopilot_leaves_the_waiting_goal_question_to_the_lead() -> anyhow::Result<()> {
    let mut crew = asking_crew_about("security", Some(Own::Answer(lead_picks_trace())))?;
    crew.settings.approvals.ask_you_about = vec!["security".to_owned()];
    let outcome = run(&crew).await?;
    assert!(
        matches!(outcome.exit, EngineExit::Parked(_)),
        "{:?}",
        outcome.exit
    );
    assert_eq!(
        why_asked(&outcome.events, CheckpointKind::Goal),
        ["your /approvals settings keep security questions yours"]
    );

    let (host, sink) = crew.host_over(outcome.events.clone())?;
    argonauts_crew::engine::commands::approval_mode(&host, ApprovalMode::Autopilot)?;
    let mut events = outcome.events.clone();
    events.extend(sink.events());
    assert!(open_kinds(&events).is_empty(), "{:?}", open_kinds(&events));
    let withdrawn: Vec<_> = events
        .iter()
        .filter_map(|event| match &event.kind {
            RunEventKind::Checkpoint(CheckpointEvent::Withdrawn { reason, .. }) => {
                Some(reason.as_str())
            }
            _ => None,
        })
        .collect();
    assert_eq!(
        withdrawn,
        [argonauts_crew::checkpoints::autopilot::LEFT_TO_THE_LEAD]
    );

    let resumed = drive(&crew, events).await?;
    let events = &resumed.events;
    assert!(
        matches!(resumed.exit, EngineExit::Finished(_)),
        "{:?}: open {:?}",
        resumed.exit,
        open_kinds(events)
    );
    let goal = approved_goal(events).expect("the goal was approved");
    let assumed = goal
        .assumptions
        .iter()
        .find(|assumption| assumption.by_lead())
        .expect("the main agent's answer is the goal's assumption");
    assert!(assumed.text.ends_with(": trace"), "{assumed:?}");
    let approver: WorkerRef = APPROVER.parse()?;
    assert!(decided(events).contains(&(CheckpointKind::Goal, Party::Approver { model: approver })));
    let said = notes(events);
    assert!(
        !said
            .iter()
            .any(|note| note.starts_with("Autopilot answered \"What should the goal say?")),
        "{said:?}"
    );
    Ok(())
}

/// An All hands crew whose lead answers the workflow design with `design`.
fn designing_crew(design: serde_json::Value) -> anyhow::Result<Crew> {
    let mut crew = Crew::new()?;
    with_gates(&mut crew);
    let own = vec![(StepName::DesignWorkflow, Own::Answer(design))];
    enlist_with(&mut crew, "claude-code/opus", "claude", None, "detail", own);
    enlist(&mut crew, "codex/gpt-x", "gpt");
    crew.services.workers.add(
        FakeWorker::harness("antigravity-cli", "gemini")
            .with_cheapest_model("gemini-3.6-flash-low")
            .respond_with(answer),
    );
    crew.settings.ultracode = true;
    crew.settings.workflows.adapt_at_phase_ends = false;
    Ok(crew)
}

fn started_from(events: &[RunEvent]) -> Vec<Option<WorkflowRef>> {
    events
        .iter()
        .filter_map(|event| match &event.kind {
            RunEventKind::Workflow(argonauts_core::events::workflow::WorkflowEvent::Started(
                started,
            )) => Some(started.from.clone()),
            _ => None,
        })
        .collect()
}

/// The sample run's case: All hands on council, and the lead would run the task
/// as maker-checker. It may only size the preset, so the run stays council and says why.
#[tokio::test]
async fn the_lead_never_swaps_the_preset_for_another_workflow() -> anyhow::Result<()> {
    let crew = designing_crew(support::brief(
        Some("maker-checker"),
        serde_json::json!([]),
        None,
        "One small change, reviewed by another provider",
    ))?;
    let outcome = run(&crew).await?;
    let events = &outcome.events;
    assert!(
        matches!(outcome.exit, EngineExit::Finished(_)),
        "{:?}: open {:?}",
        outcome.exit,
        open_kinds(events)
    );
    let from = started_from(events);
    assert!(
        from.contains(&Some(WorkflowRef::new("council"))),
        "{from:?}"
    );
    assert!(
        !from.contains(&Some(WorkflowRef::new("maker-checker"))),
        "{from:?}"
    );

    // It saw only the preset, and was told why its pick didn't hold.
    let lead = crew
        .services
        .workers
        .fake("claude-code/opus")
        .expect("claude");
    let designs: Vec<_> = lead
        .specs()
        .into_iter()
        .filter(|spec| spec.step == Some(StepName::DesignWorkflow))
        .collect();
    assert_eq!(designs.len(), 2);
    let text = |spec: &SessionSpec| {
        spec.prompt
            .iter()
            .map(|part| part.text.clone())
            .collect::<Vec<_>>()
            .join("\n")
    };
    assert!(text(&designs[0]).contains("## The workflow's steps"));
    assert!(!text(&designs[0]).contains("### maker-checker"));
    assert!(
        text(&designs[1]).contains("keep `council`, the workflow's steps"),
        "{}",
        text(&designs[1])
    );
    assert!(
        notes(events).iter().any(|note| note.starts_with(
            "The main agent's workflow broke the rules twice, so council runs as written."
        )),
        "{:?}",
        notes(events)
    );
    Ok(())
}

/// A design that leaves out what the brief asks for is said in one plain line, and what the
/// validator said waits for Ctrl+O. The preset runs as written.
#[tokio::test]
async fn an_incomplete_design_is_said_plainly() -> anyhow::Result<()> {
    let crew = designing_crew(serde_json::json!({ "why": "One small change" }))?;
    let outcome = run(&crew).await?;
    let events = &outcome.events;
    assert!(
        matches!(outcome.exit, EngineExit::Finished(_)),
        "{:?}: open {:?}",
        outcome.exit,
        open_kinds(events)
    );
    let note = events
        .iter()
        .find_map(|event| match &event.kind {
            RunEventKind::Note(note) if note.line().starts_with("The main agent's design") => {
                Some(note.clone())
            }
            _ => None,
        })
        .context("the design's note")?;
    assert_eq!(
        note.line(),
        "The main agent's design came back incomplete, so council runs as written."
    );
    let detail = note.detail().context("what the validator said")?;
    assert!(
        detail.contains("\"based_on\" is a required property"),
        "{detail}"
    );
    let from = started_from(events);
    assert!(
        from.contains(&Some(WorkflowRef::new("council"))),
        "{from:?}"
    );
    Ok(())
}

/// The lead may size the preset, here leaving out the critique, and one line says so and why.
#[tokio::test]
async fn the_lead_sizes_the_preset_and_says_what_it_changed() -> anyhow::Result<()> {
    let mut sized =
        argonauts_crew::workflow::builtin::load("council").map_err(|err| anyhow::anyhow!(err))?;
    for phase in &mut sized.phases {
        for step in &mut phase.steps {
            step.steps
                .retain(|step| step.name.as_deref() != Some("critique"));
        }
    }
    let composed =
        argonauts_crew::workflow::yaml::print(&sized).map_err(|err| anyhow::anyhow!(err))?;
    let crew = designing_crew(support::brief(
        Some("council"),
        serde_json::json!([]),
        Some(&composed),
        "One task needs no debate.",
    ))?;
    let outcome = run(&crew).await?;
    let events = &outcome.events;
    assert!(
        matches!(outcome.exit, EngineExit::Finished(_)),
        "{:?}: open {:?}",
        outcome.exit,
        open_kinds(events)
    );
    let from = started_from(events);
    assert!(
        from.contains(&Some(WorkflowRef::new("council"))),
        "{from:?}"
    );
    let said = notes(events);
    assert!(
        said.contains(
            &"The main agent changed the workflow's steps: left out critique, since one task needs no \
              debate."
        ),
        "{said:?}"
    );
    Ok(())
}

/// In full auto even a question the lead couldn't answer doesn't wait: it takes the crew's
/// recommended answer, as an assumption that says so, for the approver to check.
#[tokio::test]
async fn in_autopilot_a_question_the_lead_fails_takes_the_recommended_answer() -> anyhow::Result<()>
{
    let mut crew = asking_crew_about("scope", Some(Own::Crash))?;
    only_the_crew_leads(&mut crew)?;
    let outcome = run_in(&crew, Some(ApprovalMode::Autopilot)).await?;
    let events = &outcome.events;
    assert!(
        matches!(outcome.exit, EngineExit::Finished(_)),
        "{:?}: open {:?}",
        outcome.exit,
        open_kinds(events)
    );
    assert_eq!(question_cards(events), 0);
    // Asked once more after its crash.
    assert_eq!(lead_asked(&crew), 2);
    let goal = approved_goal(events).expect("the goal was approved");
    let assumed = goal
        .assumptions
        .iter()
        .find(|assumption| assumption.by_lead())
        .expect("the fallback answer is the main agent's assumption");
    assert!(assumed.text.ends_with(": debug"), "{assumed:?}");
    assert!(
        assumed.basis.contains(
            "autopilot took the crew's recommended answer, since the main agent couldn't"
        ),
        "{assumed:?}"
    );
    assert!(
        notes(events).iter().any(|note| note.starts_with(
            "The main agent couldn't answer the goal's questions, so autopilot takes the crew's"
        )),
        "{:?}",
        notes(events)
    );
    Ok(())
}

fn landed(events: &[RunEvent]) -> bool {
    events.iter().any(|event| {
        matches!(
            event.kind,
            RunEventKind::Integration(IntegrationEvent::Landed { .. })
        )
    })
}

fn merged(exit: &EngineExit) -> bool {
    matches!(
        exit,
        EngineExit::Finished(argonauts_core::events::run::RunOutcome::Merged)
    )
}

/// A setup with All hands: a Claude Code, a Codex, and an agy agent
/// leaves no approver of a company outside the crew. Full auto has a model of another company
/// than the lead's check the goal, the lead's answers in it, and the run lands.
#[tokio::test]
async fn autopilot_with_every_company_in_the_crew_lands() -> anyhow::Result<()> {
    let mut crew = Crew::new()?;
    with_gates(&mut crew);
    let keep = serde_json::json!({
        "template": "council",
        "params": [],
        "composed": null,
        "why": "It fits"
    });
    let own = vec![(StepName::DesignWorkflow, Own::Answer(keep))];
    enlist_with(&mut crew, "claude-code/opus", "claude", None, "detail", own);
    enlist(&mut crew, "codex/gpt-x", "gpt");
    enlist(&mut crew, "antigravity-cli/gemini-3.6-flash-low", "gemini");
    crew.settings.ultracode = true;
    crew.settings.workflows.adapt_at_phase_ends = false;

    let outcome = run_in(&crew, Some(ApprovalMode::Autopilot)).await?;
    let events = &outcome.events;
    assert!(
        merged(&outcome.exit),
        "{:?}: open {:?}, notes {:#?}",
        outcome.exit,
        open_kinds(events),
        notes(events)
    );
    assert!(landed(events), "{:?}", names(events));
    // Every company restated the goal, so none was left outside its authors.
    let mut restaters: Vec<&str> = events
        .iter()
        .filter_map(|event| match &event.kind {
            RunEventKind::Goal(GoalEvent::Restated { worker, .. }) => Some(worker.worker.as_str()),
            _ => None,
        })
        .collect();
    restaters.sort_unstable();
    restaters.dedup();
    assert_eq!(restaters, ["antigravity-cli", "claude-code", "codex"]);
    let goal = decided(events)
        .into_iter()
        .find(|(kind, _)| *kind == CheckpointKind::Goal)
        .map(|(_, by)| by);
    let Some(Party::Approver { model }) = goal else {
        panic!("the goal wasn't approved by a model: {goal:?}");
    };
    assert_ne!(
        model.worker.as_str(),
        "claude-code",
        "the main agent checked itself"
    );
    assert!(waited_for_you(events).is_empty());
    Ok(())
}

/// Nobody answers spot checks in full auto, so the ones waiting never pause it: the approver's
/// approvals go through, each drawn for a spot check, and the run lands.
#[tokio::test]
async fn autopilot_goes_on_past_waiting_spot_checks() -> anyhow::Result<()> {
    let mut crew = crew()?;
    crew.settings.approvals.spot_check_sample = 1.0;
    crew.settings.approvals.pause_auto_when_spot_checks_waiting = 1;
    let outcome = run_in(&crew, Some(ApprovalMode::Autopilot)).await?;
    let events = &outcome.events;
    assert!(
        merged(&outcome.exit),
        "{:?}: open {:?}, notes {:#?}",
        outcome.exit,
        open_kinds(events),
        notes(events)
    );
    assert!(landed(events));
    let queued = events
        .iter()
        .filter(|event| {
            matches!(
                event.kind,
                RunEventKind::Checkpoint(CheckpointEvent::SpotCheckQueued { .. })
            )
        })
        .count();
    assert!(queued >= 2, "{queued} spot checks");
    Ok(())
}

/// With no model of another company to run, full auto goes ahead on your rules alone, keeps the
/// lead's answers as assumptions, and says so in plain words with where to pick an approver.
#[tokio::test]
async fn autopilot_with_no_approver_goes_ahead_on_your_rules() -> anyhow::Result<()> {
    let mut crew = Crew::new()?;
    with_gates(&mut crew);
    enlist(&mut crew, "claude-code/opus", "claude");
    enlist(&mut crew, "claude-code/sonnet", "claude");
    let outcome = run_in(&crew, Some(ApprovalMode::Autopilot)).await?;
    let events = &outcome.events;
    assert!(
        merged(&outcome.exit),
        "{:?}: open {:?}, notes {:#?}",
        outcome.exit,
        open_kinds(events),
        notes(events)
    );
    assert!(decided(events).contains(&(CheckpointKind::Goal, Party::Rules)));
    let said = notes(events);
    assert!(
        said.contains(
            &"\"Run this workflow?\": No model you can run comes from another provider than \
              Anthropic, so no approver can judge this; your rules decided. Pick an approver in \
              /approvals, or install and log in to an agent from another provider."
        ),
        "{said:#?}"
    );
    Ok(())
}

/// A plan nothing can approve, here kept for you by `presence = required`, parks the run with
/// one line saying why. Full auto never abandons it.
#[tokio::test]
async fn autopilot_parks_on_a_plan_it_cant_approve() -> anyhow::Result<()> {
    let mut crew = crew()?;
    crew.settings.approvals.presence = argonauts_core::settings::approvals::Presence::Required;
    let outcome = run_in(&crew, Some(ApprovalMode::Autopilot)).await?;
    let events = &outcome.events;
    assert!(
        matches!(outcome.exit, EngineExit::Parked(_)),
        "{:?}",
        outcome.exit
    );
    assert_eq!(open_kinds(events), [CheckpointKind::Plan]);
    let said = notes(events);
    assert!(
        said.iter().any(|note| note
            == &"Autopilot left \"Approve this plan?\" to you, since your /approvals settings \
                 keep it for you at the terminal."),
        "{said:#?}"
    );
    Ok(())
}

/// A crew of Claude Code, Codex, and Antigravity CLI, with Codex ready to answer a question.
fn three_company_crew() -> anyhow::Result<Crew> {
    let mut crew = Crew::new()?;
    enlist(&mut crew, "claude-code/opus", "claude");
    let codex = FakeWorker::harness("codex/gpt-x", "gpt").respond_with(|spec: &SessionSpec| {
        if spec.step.is_none() {
            return FakeSession::completed("Log to stderr only; src/cli.rs already has --verbose.");
        }
        answer(spec)
    });
    crew.services.workers.add(codex);
    crew.settings.crew.add("codex/gpt-x".parse()?);
    crew.services.workers.add(
        FakeWorker::harness("antigravity-cli", "gemini")
            .with_cheapest_model("gemini-3.6-flash-low")
            .respond_with(answer),
    );
    Ok(crew)
}

/// The events of a run in full auto where card-001, owned by Claude Code, reports a question
/// meant for you.
async fn ask_in_autopilot(crew: &Crew) -> anyhow::Result<Vec<RunEvent>> {
    let started = crew.run_started(RunMode::Crew, "add a --verbose flag")?;
    let autopilot = RunEvent {
        seq: 1,
        kind: RunEventKind::Run(RunLifecycle::ApprovalModeChanged {
            mode: ApprovalMode::Autopilot,
            by: Party::User,
        }),
        ..started.clone()
    };
    let asking = Asking::new(crew, vec![started, autopilot])?;
    with_git_env(
        crew.git_env(),
        argonauts_crew::blackboard::on_report(&asking.host, &asking.card, &asking.report),
    )
    .await?;
    Ok(asking.sink.events())
}

/// A running task whose agent's report asks a question meant for you.
struct Asking {
    host: argonauts_crew::engine::host::Host,
    sink: Arc<argonauts_testkit::MemorySink>,
    card: argonauts_core::CardId,
    report: argonauts_core::steps::card::CardReport,
}

impl Asking {
    fn new(crew: &Crew, history: Vec<RunEvent>) -> anyhow::Result<Self> {
        use argonauts_core::events::card::{CardEvent, CardKind, CardSpec, CardState};
        use argonauts_core::steps::card::{CardReport, ReportStatus};

        let state = argonauts_crew::state::RunState::fold(&history);
        let (mut context, sink, _commands) = crew.context(history)?;
        context.services.policy = real_policy(crew, &context.settings)?;
        let shared = argonauts_crew::engine::shared::Shared::new(&context, state)?;
        let host = argonauts_crew::engine::host::Host::new(Arc::new(shared));

        let card = argonauts_core::CardId::new("task-001")?;
        let owner: WorkerRef = "claude-code/opus".parse()?;
        let spec = CardSpec {
            id: card.clone(),
            title: "Add the --verbose flag".to_owned(),
            task: String::new(),
            kind: CardKind::Build,
            owned_paths: vec![FILE.to_owned()],
            read_only_paths: Vec::new(),
            forbidden_paths: Vec::new(),
            hidden_paths: Vec::new(),
            depends_on: Vec::new(),
            criteria: Vec::new(),
            owner: Some(owner),
            reviewers: Vec::new(),
            budget: Default::default(),
            predictions: None,
            item: None,
        };
        host.record(
            Some(&card),
            None,
            RunEventKind::Card(CardEvent::Created(Box::new(spec))),
        )?;
        host.record(
            Some(&card),
            None,
            RunEventKind::Card(CardEvent::State {
                from: None,
                to: CardState::Running,
                reason: String::new(),
            }),
        )?;
        let report = CardReport {
            status: ReportStatus::Done,
            summary: "Done.".to_owned(),
            files_changed: Vec::new(),
            tests_run: Vec::new(),
            concerns: Vec::new(),
            questions: vec!["Should --verbose also write to a log file?".to_owned()],
            change_requests: Vec::new(),
            memory_proposals: Vec::new(),
            answer: String::new(),
        };
        Ok(Self {
            host,
            sink,
            card,
            report,
        })
    }
}

/// Esc on an agent's question means "carry on without me": the question goes
/// back to the agent at once, which is told to decide and say what it assumed, a line says so,
/// and the closing answer lists it. Nothing waits.
#[tokio::test]
async fn a_question_you_leave_goes_back_to_its_agent() -> anyhow::Result<()> {
    use argonauts_core::api::CheckpointAnswer;
    use argonauts_core::events::checkpoint::{AnswerAction, LEFT_TO_THE_AGENT};

    let crew = three_company_crew()?;
    let started = crew.run_started(RunMode::Crew, "add a --verbose flag")?;
    let asking = Asking::new(&crew, vec![started])?;
    let ask = with_git_env(
        crew.git_env(),
        argonauts_crew::blackboard::on_report(&asking.host, &asking.card, &asking.report),
    );
    let leave = async {
        let id = loop {
            let open = asking
                .sink
                .events()
                .into_iter()
                .find_map(|event| match event.kind {
                    RunEventKind::Checkpoint(CheckpointEvent::Opened(opened))
                        if opened.kind == CheckpointKind::Question =>
                    {
                        Some(opened.id)
                    }
                    _ => None,
                });
            if let Some(id) = open {
                break id;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        };
        let left = CheckpointAnswer {
            id,
            action: AnswerAction::CarryOn,
            text: None,
            evidence_sha256: String::new(),
            cards: Vec::new(),
        };
        argonauts_crew::checkpoints::answer(&asking.host, left.clone())
            .expect("the question is left");
        // It is decided now, so a second Esc has nothing to leave.
        let again = argonauts_crew::checkpoints::answer(&asking.host, left);
        assert_eq!(again, Err("already decided".to_owned()));
    };
    let (asked, ()) = tokio::join!(ask, leave);
    asked?;

    let events = asking.sink.events();
    assert!(open_kinds(&events).is_empty(), "something still waits");
    let answer = posted_answer(&events);
    assert_eq!(answer.from, Party::Declined);
    assert_eq!(answer.text, LEFT_TO_THE_AGENT);
    assert!(decided(&events).contains(&(CheckpointKind::Question, Party::Declined)));
    let line = "You left \"Question from build:add-the\" to the agent, which decides and says \
                what it assumed.";
    assert!(notes(&events).contains(&line), "{:#?}", notes(&events));
    let state = argonauts_crew::state::RunState::fold(&events);
    assert_eq!(
        argonauts_crew::checkpoints::autopilot::endings(&state),
        [line]
    );
    Ok(())
}

/// The answer posted to the agent's question.
fn posted_answer(events: &[RunEvent]) -> argonauts_core::events::shared::BlackboardMessage {
    use argonauts_core::events::shared::{BlackboardEvent, BlackboardKind};
    events
        .iter()
        .find_map(|event| match &event.kind {
            RunEventKind::Blackboard(BlackboardEvent::Posted(message))
                if message.kind == BlackboardKind::Answer =>
            {
                Some(message.clone())
            }
            _ => None,
        })
        .expect("the question was answered")
}

/// Full auto: an agent's question meant for you, from Claude Code with Claude Code
/// the default agent too, goes to a model of another company, and a third company's approver
/// checks the answer before it's sent. One line names the asking card's label and the models,
/// never a card id, and the run's status keeps the answer for you to review.
#[tokio::test]
async fn in_autopilot_an_agents_question_is_answered_elsewhere_and_checked() -> anyhow::Result<()> {
    let crew = three_company_crew()?;
    let events = ask_in_autopilot(&crew).await?;
    let answer = posted_answer(&events);
    assert!(
        matches!(&answer.from, Party::Agent { worker, .. } if worker.worker.as_str() == "codex"),
        "{:?}",
        answer.from
    );
    assert!(answer.text.contains("stderr"), "{}", answer.text);
    let approver: WorkerRef = APPROVER.parse()?;
    assert!(
        decided(&events).contains(&(
            CheckpointKind::Question,
            Party::Approver {
                model: approver.clone()
            }
        )),
        "{:?}",
        decided(&events)
    );
    let said = notes(&events);
    assert!(
        said.contains(
            &"Autopilot answered the question from build:add-the (Opus on Claude Code) for you \
              with GPT X on Codex's answer, which Gemini 3.6 Flash Low on Antigravity CLI \
              checked."
        ),
        "{said:#?}"
    );
    assert!(
        !said.iter().any(|note| note.contains("task-001")),
        "{said:#?}"
    );
    let status = argonauts_crew::status::summary(&events, &argonauts_crew::status::Viewer::User);
    assert!(
        status.goal.iter().any(|line| line.starts_with(
            "Answered for you on autopilot · Should --verbose also write to a log file: Log to \
             stderr only"
        )),
        "{:#?}",
        status.goal
    );
    Ok(())
}

/// A model you picked to answer, checked by an approver of its own company, can't be checked,
/// so the agent is told to use its best judgment, and the line says why the approver couldn't.
#[tokio::test]
async fn in_autopilot_an_answer_no_one_can_check_goes_as_best_judgment() -> anyhow::Result<()> {
    let mut crew = three_company_crew()?;
    crew.services
        .workers
        .add(FakeWorker::harness("codex/gpt-y", "gpt").respond_with(answer));
    crew.settings.approvals.answers = Some(Answerer::Model("codex/gpt-x".parse()?));
    crew.settings.approvals.decides = Decides::Model("codex/gpt-y".parse()?);
    let events = ask_in_autopilot(&crew).await?;
    let answer = posted_answer(&events);
    assert_eq!(answer.from, Party::Autopilot);
    assert_eq!(
        answer.text,
        argonauts_crew::checkpoints::autopilot::OWN_ANSWER
    );
    let said = notes(&events).join("\n");
    assert!(said.contains("Go with your best judgment"), "{said}");
    assert!(said.contains("same provider as"), "{said}");
    assert!(
        !said.contains("no rules decide this kind of approval"),
        "{said}"
    );
    Ok(())
}

/// A model you picked from the asker's own company never answers it: the lead's rule picks a
/// model of another company instead.
#[tokio::test]
async fn a_picked_model_never_answers_its_own_companys_agent() -> anyhow::Result<()> {
    let mut crew = three_company_crew()?;
    crew.settings.approvals.answers = Some(Answerer::Model("claude-code/opus".parse()?));
    let events = ask_in_autopilot(&crew).await?;
    let answer = posted_answer(&events);
    assert!(
        matches!(&answer.from, Party::Agent { worker, .. } if worker.worker.as_str() == "codex"),
        "{:?}",
        answer.from
    );
    Ok(())
}

/// A picked model that can't start, such as one no agent provides, leaves the answer to the
/// lead, and one line says so. The card's report goes on.
#[tokio::test]
async fn a_picked_model_that_cant_answer_leaves_it_to_the_lead() -> anyhow::Result<()> {
    let mut crew = three_company_crew()?;
    crew.settings.approvals.answers = Some(Answerer::Model("nobody/x".parse()?));
    let events = ask_in_autopilot(&crew).await?;
    let answer = posted_answer(&events);
    assert!(
        matches!(&answer.from, Party::Agent { worker, .. } if worker.worker.as_str() == "codex"),
        "{:?}",
        answer.from
    );
    let said = notes(&events).join("\n");
    assert!(
        said.contains(
            "couldn't answer the question from build:add-the, so GPT X on Codex answers it"
        ),
        "{said}"
    );
    Ok(())
}

/// At a phase's end too, the lead may size what's left of the preset, never swap it
/// for another template. Its pick of maker-checker is refused, and the run stays council.
#[tokio::test]
async fn at_a_phase_end_the_lead_keeps_the_preset() -> anyhow::Result<()> {
    let picks = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let mut crew = Crew::new()?;
    with_gates(&mut crew);
    let lead = {
        let picks = picks.clone();
        FakeWorker::harness("claude-code/opus", "claude").respond_with(move |spec: &SessionSpec| {
            if spec.step == Some(StepName::DesignWorkflow) {
                // The run's design keeps the preset; each phase end wants another template.
                let template = if picks.fetch_add(1, Ordering::SeqCst) == 0 {
                    "council"
                } else {
                    "maker-checker"
                };
                return FakeSession::structured(support::brief(
                    Some(template),
                    serde_json::json!([]),
                    None,
                    "One small change",
                ));
            }
            if spec.step == Some(StepName::Build) {
                std::fs::write(spec.workdir.join(FILE), "--verbose\n")
                    .expect("the build session writes its file");
                return answer(spec).with_event(support::wrote(FILE));
            }
            if spec.step == Some(StepName::PlanToCards) {
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
            answer(spec)
        })
    };
    crew.services.workers.add(lead);
    crew.settings.main_agent = "claude-code/opus".parse()?;
    crew.settings.crew.add("claude-code/opus".parse()?);
    enlist(&mut crew, "codex/gpt-x", "gpt");
    crew.services.workers.add(
        FakeWorker::harness("antigravity-cli", "gemini")
            .with_cheapest_model("gemini-3.6-flash-low")
            .respond_with(answer),
    );
    crew.settings.ultracode = true;
    crew.settings.workflows.adapt_at_phase_ends = true;

    let outcome = run(&crew).await?;
    let events = &outcome.events;
    assert!(
        picks.load(Ordering::SeqCst) > 1,
        "no phase end asked the main agent"
    );
    let changed = events.iter().any(|event| {
        matches!(
            event.kind,
            RunEventKind::Workflow(argonauts_core::events::workflow::WorkflowEvent::Changed(_))
        )
    });
    assert!(!changed, "the workflow changed: {:?}", names(events));
    let from = started_from(events);
    assert!(
        !from.contains(&Some(WorkflowRef::new("maker-checker"))),
        "{from:?}"
    );
    let said = notes(events);
    assert!(
        said.iter().any(|note| note
            .contains("broke the rules twice, so the workflow goes on unchanged")
            && note.contains("keep `council`, the workflow's steps")),
        "{said:#?}"
    );
    Ok(())
}
