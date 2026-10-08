//! Dividing the work, driven at the plan checkpoint step on a real temp repository: a run whose
//! goal is approved and whose plan is sealed divides it, opens the plan checkpoint, and does
//! what each answer says. The pure rules (graph, ring, router, budgets, contracts first, the
//! dependency block) have unit tests beside their code.

use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, bail};
use serde_json::{Value, json};

use argonauts_core::api::CheckpointAnswer;
use argonauts_core::engine::RepoContext;
use argonauts_core::events::card::{CardEvent, CardKind, CardState, IntegrationEvent};
use argonauts_core::events::checkpoint::{
    AnswerAction, CheckpointEvent, CheckpointKind, CheckpointOpened, CheckpointPacket, PlanPacket,
};
use argonauts_core::events::goal::{Criterion, GoalEvent, GoalFile, GoalSource};
use argonauts_core::events::plan::{DivideEvent, JudgingResult, PlanEvent, PlanTally};
use argonauts_core::events::run::{Party, RunLifecycle, RunMode, RunStarted};
use argonauts_core::events::workflow::StepEnd;
use argonauts_core::events::{RunEvent, RunEventKind};
use argonauts_core::names::{DraftId, WorkflowId};
use argonauts_core::settings::Settings;
use argonauts_core::settings::protocol::JudgingMethod;
use argonauts_core::steps::StepName;
use argonauts_core::worker::SessionSpec;
use argonauts_core::workflow::{StandardPhase, StepRef};
use argonauts_core::{RepoId, RunId};
use argonauts_crew::checkpoints;
use argonauts_crew::divide;
use argonauts_crew::engine::host::Host;
use argonauts_crew::engine::shared::Shared;
use argonauts_crew::integrate::branch;
use argonauts_crew::state::RunState;
use argonauts_crew::workflow::{StepAt, StepCall, StepDone, runner, yaml};
use argonauts_testkit::repo::TempRepo;
use argonauts_testkit::services::{FakeServices, MemorySink, engine_context_in};
use argonauts_testkit::worker::{FakeSession, FakeWorker};

const CREW: [(&str, &str); 3] = [
    ("claude-code/claude-opus", "claude"),
    ("codex/gpt-5.6", "gpt"),
    ("agy/gemini-3.6-pro", "gemini"),
];

type Step = tokio::task::JoinHandle<anyhow::Result<StepDone>>;

/// The run just before the plan checkpoint: goal approved, one plan sealed and chosen.
struct Stage {
    repo: TempRepo,
    host: Host,
    sink: Arc<MemorySink>,
    _services: FakeServices,
}

impl Stage {
    /// `files` are committed before the run starts, so they're in its base.
    fn new(
        files: &[(&str, &str)],
        new_dependencies: &[&str],
        configure: impl FnOnce(&mut Settings),
    ) -> anyhow::Result<Self> {
        Self::answering(files, new_dependencies, configure, answer)
    }

    /// Every agent answers with `answers`.
    fn answering(
        files: &[(&str, &str)],
        new_dependencies: &[&str],
        configure: impl FnOnce(&mut Settings),
        answers: fn(&SessionSpec) -> FakeSession,
    ) -> anyhow::Result<Self> {
        let repo = TempRepo::new()?;
        for (path, text) in files {
            repo.write(path, text)?;
            repo.git(&["add", path])?;
        }
        if !files.is_empty() {
            repo.git(&["commit", "--quiet", "-m", "the project's files"])?;
        }
        let base = repo.git(&["rev-parse", "HEAD"])?.trim().to_owned();
        let services = FakeServices::new();
        let mut settings = Settings::default();
        settings.crew.clear();
        for (worker, family) in CREW {
            services
                .workers
                .add(FakeWorker::harness(worker, family).respond_with(answers));
            settings.crew.add(worker.parse()?);
        }
        settings.main_agent = CREW[0].0.parse()?;
        configure(&mut settings);
        let run = RunId::new("run-01")?;
        let state = repo.home().join("state/repo");
        let context = RepoContext {
            id: RepoId::from_canonical_path(repo.root()),
            root: repo.root().to_path_buf(),
            run_dir: state.join("runs").join(run.as_str()),
            worktrees: repo.home().join("worktrees/repo/run-01"),
            base_worktree: repo.home().join("worktrees/repo/run-01/base"),
        };
        let history = history(&run, &context, &base, new_dependencies)?;
        let (engine, sink, _commands) =
            engine_context_in(&services, settings, context, history.clone());
        let shared = Shared::new(&engine, RunState::fold(&history))?;
        Ok(Self {
            repo,
            host: Host::new(Arc::new(shared)),
            sink,
            _services: services,
        })
    }

    fn start(&self) -> Step {
        let host = self.host.clone();
        tokio::spawn(async move { Ok(divide::plan_checkpoint_step(&host, &step_call()).await?) })
    }

    /// The plan checkpoint as a workflow runs it, so its step round is recorded.
    fn start_workflow(&self) -> anyhow::Result<Step> {
        let workflow = yaml::parse(CHECKPOINT_ONLY).map_err(anyhow::Error::msg)?;
        let id = runner::start(&self.host, None, workflow, None)?;
        let host = self.host.clone();
        Ok(tokio::spawn(async move {
            let flow = runner::run(&host, &id).await?;
            Ok(StepDone {
                end: StepEnd::Done,
                summary: None,
                flow,
            })
        }))
    }

    /// Waits for the plan checkpoint numbered `n` (from 1) and answers it.
    async fn answer(
        &self,
        step: &mut Step,
        n: usize,
        action: AnswerAction,
        text: Option<&str>,
    ) -> anyhow::Result<CheckpointOpened> {
        let opened = tokio::time::timeout(Duration::from_secs(20), async {
            loop {
                if let Some(opened) = plan_checkpoints(&self.sink.events()).into_iter().nth(n - 1) {
                    return Ok(opened);
                }
                if step.is_finished() {
                    let ended = (&mut *step).await?;
                    bail!("the step ended before plan approval {n} opened: {ended:?}");
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .context("the plan approval never opened")??;
        checkpoints::answer(
            &self.host,
            CheckpointAnswer {
                id: opened.id.clone(),
                action,
                text: text.map(str::to_owned),
                evidence_sha256: opened.evidence_sha256.clone(),
                cards: Vec::new(),
            },
        )
        .map_err(anyhow::Error::msg)?;
        Ok(opened)
    }
}

const CHECKPOINT_ONLY: &str = r#"
name: plan-checkpoint
description: The plan approval alone
phases:
  - name: Plan
    standard: plan
    steps:
      - kind: checkpoint
        checkpoint: plan
"#;

#[tokio::test]
async fn plan_checkpoint_after_divide() -> anyhow::Result<()> {
    let stage = Stage::new(&[], &[], |_| {})?;
    let mut step = stage.start();
    let opened = stage
        .answer(&mut step, 1, AnswerAction::Approve, None)
        .await?;
    let done = step.await??;
    assert_eq!(done.end, StepEnd::Done);

    let events = stage.sink.events();
    let opened_at = events
        .iter()
        .position(|event| matches!(&event.kind, RunEventKind::Checkpoint(CheckpointEvent::Opened(o)) if o.id == opened.id))
        .context("no Checkpoint::Opened")?;
    let divided: Vec<&DivideEvent> = events[..opened_at]
        .iter()
        .filter_map(|event| match &event.kind {
            RunEventKind::Divide(divide) => Some(divide),
            _ => None,
        })
        .collect();
    assert!(
        matches!(divided.last(), Some(DivideEvent::Graph(_))),
        "the graph comes last"
    );
    let count = |pick: fn(&DivideEvent) -> bool| divided.iter().filter(|e| pick(e)).count();
    assert_eq!(count(|e| matches!(e, DivideEvent::Owner { .. })), 5);
    assert_eq!(count(|e| matches!(e, DivideEvent::Reviewers { .. })), 5);
    assert_eq!(count(|e| matches!(e, DivideEvent::Predicted { .. })), 5);
    assert_eq!(count(|e| matches!(e, DivideEvent::Budget(_))), 1);
    assert!(
        !events[..opened_at]
            .iter()
            .any(|e| matches!(e.kind, RunEventKind::Card(_)))
    );

    let packet = plan_packet(&opened)?;
    let kinds: Vec<&CardKind> = packet.graph.cards.iter().map(|card| &card.kind).collect();
    assert_eq!(kinds.len(), 5);
    assert_eq!(kinds[0], &CardKind::Interface);
    assert_eq!(packet.acceptance_tests.len(), 2);
    for card in &packet.graph.cards {
        let owner = card.owner.as_ref().context("a worker for every task")?;
        assert_eq!(card.reviewers.len(), 1);
        assert_ne!(
            family(&card.reviewers[0].to_string()),
            family(&owner.to_string())
        );
        assert!(card.predictions.is_some());
    }
    let sum: f64 = packet
        .budget
        .lines
        .iter()
        .map(|line| line.amount.amount)
        .sum();
    assert!((sum - packet.budget.total.amount).abs() < 1e-9);

    let created: Vec<&RunEvent> = events[opened_at..]
        .iter()
        .filter(|e| matches!(e.kind, RunEventKind::Card(CardEvent::Created(_))))
        .collect();
    assert_eq!(created.len(), 5);
    assert!(events.iter().any(|e| matches!(
        &e.kind,
        RunEventKind::Card(CardEvent::State {
            to: CardState::Waiting,
            ..
        })
    )));
    Ok(())
}

#[tokio::test]
async fn dependencies_added_by_conductor_before_build() -> anyhow::Result<()> {
    let files = [("requirements.txt", "flask==3.0\n")];
    let stage = Stage::new(&files, &["redis>=5"], |settings| {
        settings.divide.predictions = false;
        settings.limits.dependency_allowlist = vec!["redis".to_owned()];
    })?;

    let mut step = stage.start();
    stage
        .answer(&mut step, 1, AnswerAction::Approve, None)
        .await?;
    step.await??;

    let events = stage.sink.events();
    let added = events
        .iter()
        .position(|e| {
            matches!(
                e.kind,
                RunEventKind::Integration(IntegrationEvent::DependenciesAdded { .. })
            )
        })
        .context("no DependenciesAdded")?;
    let RunEventKind::Integration(IntegrationEvent::DependenciesAdded { packages, sha }) =
        &events[added].kind
    else {
        bail!("not DependenciesAdded");
    };
    assert_eq!(packages, &["redis>=5".to_owned()]);
    // Every card exists before the dependencies land, and none has started.
    let created = events[..added]
        .iter()
        .filter(|e| matches!(e.kind, RunEventKind::Card(CardEvent::Created(_))))
        .count();
    assert_eq!(created, 5);
    assert!(!events.iter().any(|e| matches!(
        &e.kind,
        RunEventKind::Card(CardEvent::State {
            to: CardState::Running,
            ..
        })
    )));

    let message = stage.repo.git(&["log", "-1", "--format=%B", sha])?;
    assert!(message.contains("Argonauts-Conductor: run-01"), "{message}");
    let files = stage.repo.git(&["show", "--name-only", "--format=", sha])?;
    assert_eq!(files.trim(), "requirements.txt");
    let integration = branch::integration_branch(&RunId::new("run-01")?);
    let head = stage.repo.git(&["rev-parse", &integration])?;
    assert_eq!(head.trim(), sha);
    let requirements = stage
        .repo
        .git(&["show", &format!("{sha}:requirements.txt")])?;
    assert_eq!(requirements, "flask==3.0\nredis>=5");
    Ok(())
}

#[tokio::test]
async fn unlisted_dependency_is_flagged_at_the_checkpoint() -> anyhow::Result<()> {
    let stage = Stage::new(&[], &["left-pad"], |settings| {
        settings.divide.predictions = false
    })?;
    let mut step = stage.start();
    let opened = stage
        .answer(
            &mut step,
            1,
            AnswerAction::SendBack,
            Some("No new packages."),
        )
        .await?;
    let done = step.await??;
    assert_eq!(done.flow, argonauts_crew::workflow::Flow::Continue);
    let packet = plan_packet(&opened)?;
    assert!(packet.flags.iter().any(|flag| flag.contains("left-pad")));
    assert!(
        opened
            .options
            .iter()
            .all(|option| option.action != AnswerAction::Approve)
    );
    assert!(
        !stage
            .sink
            .events()
            .iter()
            .any(|e| matches!(e.kind, RunEventKind::Card(_)))
    );
    Ok(())
}

#[tokio::test]
async fn plan_approve_with_edits_becomes_decision() -> anyhow::Result<()> {
    let stage = Stage::new(&[], &[], |settings| settings.divide.predictions = false)?;
    let mut step = stage.start_workflow()?;
    // An edit that breaks a rule reopens the checkpoint with the reason.
    stage
        .answer(
            &mut step,
            1,
            AnswerAction::ApproveWithEdits,
            Some("task-002 paths src/limiter/api.rs"),
        )
        .await?;
    let note = "Keep the store small.\ntask-002 limit 2.5";
    let reopened = stage
        .answer(&mut step, 2, AnswerAction::ApproveWithEdits, Some(note))
        .await?;
    assert!(
        plan_packet(&reopened)?
            .flags
            .iter()
            .any(|flag| flag.contains("Your edits weren't applied") && flag.contains("overlap"))
    );
    step.await??;

    let events = stage.sink.events();
    let decided = events
        .iter()
        .find_map(|e| match &e.kind {
            RunEventKind::Plan(PlanEvent::Decided(decision)) => Some(decision.clone()),
            _ => None,
        })
        .context("no Plan::Decided")?;
    assert_eq!(decided.title, "Your edits");
    assert_eq!(decided.choice, note);
    let integration = branch::integration_branch(&RunId::new("run-01")?);
    let record = stage.repo.git(&[
        "show",
        &format!(
            "{integration}:.argonauts/decisions/{}-your-edits.md",
            decided.id
        ),
    ])?;
    assert!(record.contains("Keep the store small."));

    let store = events
        .iter()
        .find_map(|e| match &e.kind {
            RunEventKind::Card(CardEvent::Created(spec)) if spec.id.as_str() == "task-002" => {
                Some(spec.clone())
            }
            _ => None,
        })
        .context("task-002 was never created")?;
    assert_eq!(store.budget.limit.map(|limit| limit.amount), Some(2.5));
    Ok(())
}

#[tokio::test]
async fn a_failed_prediction_is_left_out() -> anyhow::Result<()> {
    fn answers(spec: &SessionSpec) -> FakeSession {
        match (spec.step, spec.card.as_ref().map(|card| card.as_str())) {
            // Not the schema `Predict` asks for.
            (Some(StepName::Predict), Some("task-002")) => {
                FakeSession::structured(json!({ "guess": "soon" }))
            }
            _ => answer(spec),
        }
    }
    let stage = Stage::answering(&[], &[], |_| {}, answers)?;
    let mut step = stage.start();
    let opened = stage
        .answer(&mut step, 1, AnswerAction::Approve, None)
        .await?;
    step.await??;
    let packet = plan_packet(&opened)?;
    for card in &packet.graph.cards {
        assert_eq!(card.predictions.is_some(), card.id.as_str() != "task-002");
    }
    let events = stage.sink.events();
    let predicted = events
        .iter()
        .filter(|e| matches!(e.kind, RunEventKind::Divide(DivideEvent::Predicted { .. })))
        .count();
    assert_eq!(predicted, 4);
    assert!(events.iter().any(|e| matches!(
        &e.kind,
        RunEventKind::Note(note) if note.text.contains("task-002's prediction is left out")
    )));
    Ok(())
}

#[tokio::test]
async fn a_dependency_the_conductor_cant_add_is_yours() -> anyhow::Result<()> {
    // The manifest isn't at the repository root.
    let files = [("web/package.json", "{}\n")];
    let stage = Stage::new(&files, &["left-pad"], |settings| {
        settings.limits.dependency_allowlist = vec!["left-pad".to_owned()];
        settings.divide.predictions = false;
    })?;
    let mut step = stage.start_workflow()?;
    let opened = stage
        .answer(&mut step, 1, AnswerAction::Approve, None)
        .await?;
    assert!(
        plan_packet(&opened)?
            .flags
            .iter()
            .any(|flag| flag.contains("can't add left-pad") && flag.contains("yourself")),
        "{:?}",
        plan_packet(&opened)?.flags
    );
    assert!(
        opened
            .options
            .iter()
            .any(|option| option.action == AnswerAction::Approve)
    );
    step.await??;
    let events = stage.sink.events();
    assert!(!events.iter().any(|e| matches!(
        e.kind,
        RunEventKind::Integration(IntegrationEvent::DependenciesAdded { .. })
    )));
    assert_eq!(
        events
            .iter()
            .filter(|e| matches!(e.kind, RunEventKind::Card(CardEvent::Created(_))))
            .count(),
        5
    );
    Ok(())
}

fn answer(spec: &SessionSpec) -> FakeSession {
    match spec.step {
        Some(StepName::PlanToCards) => FakeSession::structured(proposal()),
        Some(StepName::Predict) => FakeSession::structured(json!({
            "p_pass_gates_first_try": 0.7,
            "p_within_time_cap": 0.8,
            "p_at_most_two_attempts": 0.9,
        })),
        _ => FakeSession::completed("done"),
    }
}

fn proposal() -> Value {
    let card = |key: &str, kind: &str, owned: &str, depends: &[&str], criteria: &[&str]| {
        json!({
            "key": key,
            "title": key,
            "task": format!("Build the {key}."),
            "kind": kind,
            "owned_paths": [owned],
            "read_only_paths": [],
            "depends_on": depends,
            "criteria": criteria,
        })
    };
    json!({
        "cards": [
            card("api", "interface", "src/limiter/api.rs", &[], &[]),
            card("store", "build", "src/limiter/store/**", &["api"], &["G1"]),
            card("middleware", "build", "src/middleware/**", &["api"], &["G2"]),
        ]
    })
}

fn history(
    run: &RunId,
    context: &RepoContext,
    base: &str,
    new_dependencies: &[&str],
) -> anyhow::Result<Vec<RunEvent>> {
    let drafts = context.run_dir.join("drafts");
    std::fs::create_dir_all(&drafts)?;
    let plan = json!({
        "summary": "Keep rate-limit counters in Redis behind a middleware.",
        "approach": "A store, a middleware, and one interface between them.",
        "pieces": [],
        "choices": [],
        "risks": [],
        "test_strategy": "Acceptance tests per task.",
        "new_dependencies": new_dependencies,
    });
    let path = drafts.join("dr-001.md");
    std::fs::write(&path, "# Plan\n\nKeep rate-limit counters in Redis.\n")?;
    std::fs::write(drafts.join("dr-001.json"), serde_json::to_string(&plan)?)?;
    let goal = GoalFile {
        run: run.clone(),
        version: 1,
        summary: "Rate-limit the API per key.".to_owned(),
        criteria: vec![
            Criterion::new("G1", "Counters survive restarts."),
            Criterion::new("G2", "Over the limit gets 429."),
        ],
        in_scope: Vec::new(),
        out_of_scope: Vec::new(),
        assumptions: Vec::new(),
        answers: Vec::new(),
        source: GoalSource::Restatements,
        path: context.root.join(".argonauts/goal.md"),
        sha256: "0".repeat(64),
    };
    let started = RunStarted {
        request: "Rate-limit the API per key.".to_owned(),
        title: "Rate limits".to_owned(),
        mode: RunMode::Crew,
        forced: true,
        workflow: "council".to_owned(),
        estimate: None,
        triage: None,
        chat_turn: None,
        continues: None,
        repo_root: context.root.clone(),
        base_branch: None,
        base_commit: base.to_owned(),
        uncommitted: Vec::new(),
        uncommitted_included: false,
        settings_sha256: String::new(),
        settings: Value::Null,
        overrides: Vec::new(),
        harness_versions: Default::default(),
        version: "0.0.1".to_owned(),
        front_end: None,
        instructions: Vec::new(),
        required_steps: Vec::new(),
    };
    let kinds = vec![
        RunEventKind::Run(RunLifecycle::Started(Box::new(started))),
        RunEventKind::Goal(GoalEvent::Approved {
            file: goal,
            by: Party::User,
        }),
        RunEventKind::Plan(PlanEvent::Sealed {
            draft: DraftId::numbered(1),
            label: 1,
            worker: CREW[1].0.parse()?,
            round: 1,
            summary: "Redis counters.".to_owned(),
            path,
            sha256: "1".repeat(64),
        }),
        RunEventKind::Plan(PlanEvent::Judged(JudgingResult {
            method: JudgingMethod::Cross,
            plans: vec![PlanTally {
                label: 1,
                author: CREW[1].0.parse()?,
                mean: 8.0,
                spread: 0.5,
                counted: 2,
                dropped: 0,
            }],
            winner: Some(1),
            margin: 2.0,
            near_tie: false,
        })),
    ];
    Ok(kinds
        .into_iter()
        .enumerate()
        .map(|(seq, kind)| RunEvent {
            seq: seq as u64,
            ts: time::OffsetDateTime::UNIX_EPOCH,
            run_id: run.clone(),
            card_id: None,
            session: None,
            source: "conductor".to_owned(),
            kind,
        })
        .collect())
}

fn step_call() -> StepCall {
    StepCall {
        at: StepAt {
            step: StepRef {
                workflow: WorkflowId::parse("wf-001").expect("a workflow id"),
                phase: 2,
                path: vec![0],
            },
            round: 1,
        },
        depth: 0,
        standard: StandardPhase::Decide,
        step: serde_json::from_value(json!({"kind": "checkpoint", "checkpoint": "plan"}))
            .expect("an approval step"),
        item: None,
    }
}

fn plan_checkpoints(events: &[RunEvent]) -> Vec<CheckpointOpened> {
    events
        .iter()
        .filter_map(|event| match &event.kind {
            RunEventKind::Checkpoint(CheckpointEvent::Opened(opened))
                if opened.kind == CheckpointKind::Plan =>
            {
                Some((**opened).clone())
            }
            _ => None,
        })
        .collect()
}

fn plan_packet(opened: &CheckpointOpened) -> anyhow::Result<&PlanPacket> {
    match &opened.packet {
        CheckpointPacket::Plan(packet) => Ok(packet),
        other => bail!("not a plan packet: {other:?}"),
    }
}

fn family(worker: &str) -> &'static str {
    CREW.iter()
        .find(|(name, _)| *name == worker)
        .map_or("unknown", |(_, family)| family)
}
