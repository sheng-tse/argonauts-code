//! The autonomous defaults end to end, with the real approval policy over fake workers: a run
//! goes from goal to merge without asking you, and each case that must still ask does.

mod support;

use std::path::Path;
use std::sync::Arc;

use anyhow::Context;

use argonauts_core::engine::EngineExit;
use argonauts_core::events::card::IntegrationEvent;
use argonauts_core::events::checkpoint::{CheckpointEvent, CheckpointKind, PolicyCheck};
use argonauts_core::events::run::{Party, RunMode};
use argonauts_core::events::{RunEvent, RunEventKind};
use argonauts_core::names::WorkerRef;
use argonauts_core::settings::approvals::ApprovalMode;
use argonauts_core::settings::protocol::GateCommand;
use argonauts_core::steps::StepName;
use argonauts_core::worker::SessionSpec;
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

/// [`enlist`], finding the restatements differ on scope, which `question` asks you about.
fn enlist_asking(crew: &mut Crew, worker: &str, family: &str, question: Option<&'static str>) {
    let fake = FakeWorker::harness(worker, family).respond_with(move |spec: &SessionSpec| {
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
                        "tag": "scope",
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

#[tokio::test]
async fn an_open_question_at_the_goal_asks() -> anyhow::Result<()> {
    let mut crew = crew()?;
    crew.services.workers = Default::default();
    crew.settings.crew.clear();
    let question = Some("Which log level does --verbose turn on?");
    enlist_asking(&mut crew, "claude-code/opus", "claude", question);
    enlist_asking(&mut crew, "codex/gpt-x", "gpt", question);
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
    let open = open_kinds(&outcome.events);
    assert!(
        open.iter()
            .all(|kind| matches!(kind, CheckpointKind::Goal | CheckpointKind::Question)),
        "{open:?}"
    );
    assert!(!open.is_empty());
    assert!(decided(&outcome.events).is_empty());
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
