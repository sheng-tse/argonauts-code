//! Dividing the work, driven at the plan approval step on a real temp repository: a run whose
//! goal is approved and whose plan is sealed divides it, opens the plan approval, and does
//! what each answer says. The pure rules (graph, ring, router, limits, contracts first, the
//! dependency block) have unit tests beside their code.

use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, bail};
use serde_json::{Value, json};

use argonauts_core::api::ApprovalAnswer;
use argonauts_core::engine::RepoContext;
use argonauts_core::events::approval::{
    AnswerAction, ApprovalDecided, ApprovalEvent, ApprovalKind, ApprovalOpened, ApprovalPacket,
    PlanPacket, WayObjection,
};
use argonauts_core::events::council::{
    CouncilEvent, CouncilEventKind, CouncilMessage, MessageKind, Point, PointStance, PointState,
    Seat, SeatRole, Seating, Speaker, Stance, TextPart,
};
use argonauts_core::events::goal::{Criterion, GoalEvent, GoalFile, GoalSource};
use argonauts_core::events::plan::{DivideEvent, JudgingResult, PlanEvent, PlanTally};
use argonauts_core::events::run::{Party, RunLifecycle, RunMode, RunStarted};
use argonauts_core::events::task::{MergeEvent, TaskEvent, TaskKind, TaskState, Way};
use argonauts_core::events::workflow::StepEnd;
use argonauts_core::events::{RunEvent, RunEventKind};
use argonauts_core::names::{DraftId, MessageId, SeatId, WorkflowId};
use argonauts_core::session_runner::SessionSpec;
use argonauts_core::settings::Settings;
use argonauts_core::settings::approvals::ApprovalMode;
use argonauts_core::settings::council::SameModelCheck;
use argonauts_core::settings::protocol::JudgingMethod;
use argonauts_core::steps::StepName;
use argonauts_core::workflow::{StandardPhase, StepRef};
use argonauts_core::{RepoId, RunId, TaskId};
use argonauts_crew::approvals;
use argonauts_crew::divide;
use argonauts_crew::engine::host::Host;
use argonauts_crew::merge::branch;
use argonauts_crew::state::RunState;
use argonauts_crew::workflow::{StepAt, StepCall, StepDone, runner, yaml};
use argonauts_testkit::deadline;
use argonauts_testkit::repo::TempRepo;
use argonauts_testkit::runner::{FakeRunner, FakeSession};
use argonauts_testkit::services::{FakeServices, MemorySink, engine_context_in};

use crate::support::shared_in;

const CREW: [(&str, &str); 3] = [
    ("claude-code/claude-opus", "anthropic"),
    ("codex/gpt-5.6", "openai"),
    ("agy/gemini-3.6-pro", "google"),
];

type Step = tokio::task::JoinHandle<anyhow::Result<StepDone>>;

/// The run just before the plan approval: understand approved, one plan sealed and chosen.
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
        Self::built(
            files,
            new_dependencies,
            configure,
            answers,
            REQUEST,
            Vec::new(),
        )
    }

    /// A run whose request is `request`.
    fn asking(request: &str, configure: impl FnOnce(&mut Settings)) -> anyhow::Result<Self> {
        Self::built(&[], &[], configure, answer, request, Vec::new())
    }

    /// A run whose request is `request`, after a council talked as `talk` says.
    fn after_talk(
        request: &str,
        talk: Vec<RunEventKind>,
        configure: impl FnOnce(&mut Settings),
    ) -> anyhow::Result<Self> {
        Self::built(&[], &[], configure, answer, request, talk)
    }

    fn built(
        files: &[(&str, &str)],
        new_dependencies: &[&str],
        configure: impl FnOnce(&mut Settings),
        answers: fn(&SessionSpec) -> FakeSession,
        request: &str,
        talk: Vec<RunEventKind>,
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
        for (worker, provider) in CREW {
            services
                .workers
                .add(FakeRunner::cli(worker, provider).respond_with(answers));
            settings.crew.add(worker.parse()?);
        }
        settings.crew.main = CREW[0].0.parse()?;
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
        let history = history(&run, &context, &base, new_dependencies, request, talk)?;
        let (engine, sink, _commands) =
            engine_context_in(&services, settings, context, history.clone());
        let shared = shared_in(&repo, &engine, RunState::fold(&history))?;
        Ok(Self {
            repo,
            host: Host::new(Arc::new(shared)),
            sink,
            _services: services,
        })
    }

    fn start(&self) -> Step {
        let host = self.host.clone();
        tokio::spawn(async move { Ok(divide::plan_approval_step(&host, &step_call()).await?) })
    }

    /// The plan approval as a workflow runs it, so its step round is recorded.
    fn start_workflow(&self) -> anyhow::Result<Step> {
        let workflow = yaml::parse(APPROVAL_ONLY).map_err(anyhow::Error::msg)?;
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

    /// Waits for the plan approval numbered `n` (from 1) and answers it.
    async fn answer(
        &self,
        step: &mut Step,
        n: usize,
        action: AnswerAction,
        text: Option<&str>,
    ) -> anyhow::Result<ApprovalOpened> {
        let opened = tokio::time::timeout(deadline::scaled(Duration::from_secs(20)), async {
            loop {
                if let Some(opened) = plan_approvals(&self.sink.events()).into_iter().nth(n - 1) {
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
        approvals::answer(
            &self.host,
            ApprovalAnswer {
                id: opened.id.clone(),
                action,
                text: text.map(str::to_owned),
                evidence_sha256: opened.evidence_sha256.clone(),
                tasks: Vec::new(),
            },
        )
        .map_err(anyhow::Error::msg)?;
        Ok(opened)
    }
}

const APPROVAL_ONLY: &str = r#"
name: plan-approval
description: The plan approval alone
phases:
  - name: Plan
    standard: plan
    steps:
      - kind: approval
        approval: plan
"#;

#[tokio::test]
async fn plan_approval_after_divide() -> anyhow::Result<()> {
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
        .position(|event| matches!(&event.kind, RunEventKind::Approval(ApprovalEvent::Opened(o)) if o.id == opened.id))
        .context("no Approval::Opened")?;
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
    assert_eq!(count(|e| matches!(e, DivideEvent::Worker { .. })), 5);
    assert_eq!(count(|e| matches!(e, DivideEvent::Reviewers { .. })), 5);
    assert_eq!(count(|e| matches!(e, DivideEvent::Predicted { .. })), 5);
    assert_eq!(count(|e| matches!(e, DivideEvent::Limit(_))), 1);
    assert!(
        !events[..opened_at]
            .iter()
            .any(|e| matches!(e.kind, RunEventKind::Task(_)))
    );

    let packet = plan_packet(&opened)?;
    let kinds: Vec<&TaskKind> = packet.graph.tasks.iter().map(|task| &task.kind).collect();
    assert_eq!(kinds.len(), 5);
    assert_eq!(kinds[0], &TaskKind::Interface);
    assert_eq!(packet.acceptance_tests.len(), 2);
    for task in &packet.graph.tasks {
        let worker = task.worker.as_ref().context("a worker for every task")?;
        assert_eq!(task.reviewers.len(), 1);
        assert_ne!(
            provider(&task.reviewers[0].to_string()),
            provider(&worker.to_string())
        );
        assert!(task.predictions.is_some());
    }
    let sum: f64 = packet
        .limit
        .lines
        .iter()
        .map(|line| line.amount.amount)
        .sum();
    assert!((sum - packet.limit.total.amount).abs() < 1e-9);

    let created: Vec<&RunEvent> = events[opened_at..]
        .iter()
        .filter(|e| matches!(e.kind, RunEventKind::Task(TaskEvent::Created(_))))
        .collect();
    assert_eq!(created.len(), 5);
    assert!(events.iter().any(|e| matches!(
        &e.kind,
        RunEventKind::Task(TaskEvent::State {
            to: TaskState::Waiting,
            ..
        })
    )));
    Ok(())
}

/// A task the plan gives no way takes the rule of thumb's, and the record says so from the
/// graph on, so the run list can say how each task runs: the plan's two tasks on every request
/// by Maker-checker, the rest in their worker's own session.
#[tokio::test]
async fn a_task_the_plan_gives_no_way_takes_the_rule_of_thumbs_from_the_graph_on()
-> anyhow::Result<()> {
    use argonauts_core::events::task::Way;

    let stage = Stage::new(&[], &[], |_| {})?;
    let mut step = stage.start();
    stage
        .answer(&mut step, 1, AnswerAction::Approve, None)
        .await?;
    step.await??;
    let events = stage.sink.events();
    let ways: Vec<Option<Way>> = events
        .iter()
        .flat_map(|event| match &event.kind {
            RunEventKind::Divide(DivideEvent::Graph(graph)) => graph.tasks.clone(),
            RunEventKind::Task(TaskEvent::Created(spec)) => vec![(**spec).clone()],
            _ => Vec::new(),
        })
        .map(|spec| spec.way.map(|way| way.how))
        .collect();
    assert_eq!(ways.len(), 10, "five in the graph and five created");
    let (graph, created) = ways.split_at(5);
    assert_eq!(graph, created, "the tasks keep the graph's ways");
    let maker_checker = Some(Way::Workflow {
        name: argonauts_core::workflow::WorkflowRef::new("maker-checker"),
    });
    let by_maker_checker = graph.iter().filter(|way| **way == maker_checker).count();
    let own_session = graph
        .iter()
        .filter(|way| **way == Some(Way::OwnSession))
        .count();
    assert_eq!((by_maker_checker, own_session), (2, 3), "{ways:?}");
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
                RunEventKind::Merge(MergeEvent::DependenciesAdded { .. })
            )
        })
        .context("no DependenciesAdded")?;
    let RunEventKind::Merge(MergeEvent::DependenciesAdded { packages, sha }) = &events[added].kind
    else {
        bail!("not DependenciesAdded");
    };
    assert_eq!(packages, &["redis>=5".to_owned()]);
    // Every task exists before the dependencies land, and none has started.
    let created = events[..added]
        .iter()
        .filter(|e| matches!(e.kind, RunEventKind::Task(TaskEvent::Created(_))))
        .count();
    assert_eq!(created, 5);
    assert!(!events.iter().any(|e| matches!(
        &e.kind,
        RunEventKind::Task(TaskEvent::State {
            to: TaskState::Running,
            ..
        })
    )));

    let message = stage.repo.git(&["log", "-1", "--format=%B", sha])?;
    assert!(message.contains("Argonauts-Code: run-01"), "{message}");
    let files = stage.repo.git(&["show", "--name-only", "--format=", sha])?;
    assert_eq!(files.trim(), "requirements.txt");
    let run_branch = branch::run_branch(&RunId::new("run-01")?);
    let head = stage.repo.git(&["rev-parse", &run_branch])?;
    assert_eq!(head.trim(), sha);
    let requirements = stage
        .repo
        .git(&["show", &format!("{sha}:requirements.txt")])?;
    assert_eq!(requirements, "flask==3.0\nredis>=5");
    Ok(())
}

#[tokio::test]
async fn unlisted_dependency_is_flagged_at_the_approval() -> anyhow::Result<()> {
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
            .any(|e| matches!(e.kind, RunEventKind::Task(_)))
    );
    Ok(())
}

#[tokio::test]
async fn plan_approve_with_edits_becomes_decision() -> anyhow::Result<()> {
    let stage = Stage::new(&[], &[], |settings| settings.divide.predictions = false)?;
    let mut step = stage.start_workflow()?;
    // An edit that breaks a rule reopens the approval with the reason.
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
    // Kept under Argonauts Code's home, never on the run's branch.
    let state_dir = argonauts_crew::divide::repo_state_dir(&stage.host);
    let file = format!("decisions/{}-your-edits.md", decided.id);
    let record = std::fs::read_to_string(state_dir.join(&file))?;
    assert!(record.contains("Keep the store small."));
    let touched = stage.repo.git(&[
        "log",
        "--all",
        "--name-only",
        "--format=",
        "--",
        ".argonauts/decisions",
    ])?;
    assert!(touched.trim().is_empty(), "{touched}");

    let store = events
        .iter()
        .find_map(|e| match &e.kind {
            RunEventKind::Task(TaskEvent::Created(spec)) if spec.id.as_str() == "task-002" => {
                Some(spec.clone())
            }
            _ => None,
        })
        .context("task-002 was never created")?;
    assert_eq!(store.limit.limit.map(|limit| limit.amount), Some(2.5));
    Ok(())
}

#[tokio::test]
async fn a_failed_prediction_is_left_out() -> anyhow::Result<()> {
    fn answers(spec: &SessionSpec) -> FakeSession {
        match (spec.step, spec.task.as_ref().map(|task| task.as_str())) {
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
    for task in &packet.graph.tasks {
        assert_eq!(task.predictions.is_some(), task.id.as_str() != "task-002");
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
        RunEventKind::Merge(MergeEvent::DependenciesAdded { .. })
    )));
    assert_eq!(
        events
            .iter()
            .filter(|e| matches!(e.kind, RunEventKind::Task(TaskEvent::Created(_))))
            .count(),
        5
    );
    Ok(())
}

fn answer(spec: &SessionSpec) -> FakeSession {
    match spec.step {
        Some(StepName::PlanToTasks) => FakeSession::structured(proposal()),
        Some(StepName::Predict) => FakeSession::structured(json!({
            "p_pass_checks_first_try": 0.7,
            "p_within_time_cap": 0.8,
            "p_at_most_two_tries": 0.9,
        })),
        _ => FakeSession::completed("done"),
    }
}

fn proposal() -> Value {
    let task = |key: &str, kind: &str, owned: &str, depends: &[&str], criteria: &[&str]| {
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
        "tasks": [
            task("api", "interface", "src/limiter/api.rs", &[], &[]),
            task("store", "build", "src/limiter/store/**", &["api"], &["G1"]),
            task("middleware", "build", "src/middleware/**", &["api"], &["G2"]),
        ]
    })
}

const REQUEST: &str = "Rate-limit the API per key.";

fn history(
    run: &RunId,
    context: &RepoContext,
    base: &str,
    new_dependencies: &[&str],
    request: &str,
    talk: Vec<RunEventKind>,
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
        source: GoalSource::Readings,
        path: context.root.join(".argonauts/goal.md"),
        sha256: "0".repeat(64),
    };
    let started = RunStarted {
        request: request.to_owned(),
        title: "Rate limits".to_owned(),
        mode: RunMode::Crew,
        forced: true,
        workflow: "council".to_owned(),
        estimate: None,
        captain_or_crew: None,
        conversation_turn: None,
        continues: None,
        repo_root: context.root.clone(),
        base_branch: None,
        base_commit: base.to_owned(),
        uncommitted: Vec::new(),
        uncommitted_included: false,
        settings_sha256: String::new(),
        settings: Value::Null,
        overrides: Vec::new(),
        cli_versions: Default::default(),
        version: "0.0.1".to_owned(),
        outside_agent: None,
        instructions: Vec::new(),
        required_steps: Vec::new(),
        starts_from: None,
    };
    let mut kinds = vec![
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
    kinds.extend(talk);
    Ok(kinds
        .into_iter()
        .enumerate()
        .map(|(seq, kind)| RunEvent {
            seq: seq as u64,
            ts: time::OffsetDateTime::UNIX_EPOCH,
            run_id: run.clone(),
            task_id: None,
            session: None,
            source: "argonauts-code".to_owned(),
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
        step: serde_json::from_value(json!({"kind": "approval", "approval": "plan"}))
            .expect("an approval step"),
        item: None,
    }
}

fn plan_approvals(events: &[RunEvent]) -> Vec<ApprovalOpened> {
    events
        .iter()
        .filter_map(|event| match &event.kind {
            RunEventKind::Approval(ApprovalEvent::Opened(opened))
                if opened.kind == ApprovalKind::Plan =>
            {
                Some((**opened).clone())
            }
            _ => None,
        })
        .collect()
}

fn plan_packet(opened: &ApprovalOpened) -> anyhow::Result<&PlanPacket> {
    match &opened.packet {
        ApprovalPacket::Plan(packet) => Ok(packet),
        other => bail!("not a plan packet: {other:?}"),
    }
}

fn provider(worker: &str) -> &'static str {
    CREW.iter()
        .find(|(name, _)| *name == worker)
        .map_or("unknown", |(_, provider)| provider)
}

/// The step's end, or an error once it waits too long, as on an approval that opened again
/// and that the test never answers.
async fn ends(step: Step) -> anyhow::Result<StepDone> {
    tokio::time::timeout(deadline::scaled(Duration::from_secs(20)), step)
        .await
        .context("the step still waits")??
}

/// Each task as the approved plan created it: its title, its way, and whether you asked for it.
fn created_ways(events: &[RunEvent]) -> Vec<(String, Option<(Way, bool)>)> {
    events
        .iter()
        .filter_map(|event| match &event.kind {
            RunEventKind::Task(TaskEvent::Created(spec)) => Some((
                spec.title.clone(),
                spec.way
                    .as_ref()
                    .map(|way| (way.how.clone(), way.asked_by_you)),
            )),
            _ => None,
        })
        .collect()
}

fn workflow_way(name: &str) -> Way {
    Way::Workflow {
        name: argonauts_core::workflow::WorkflowRef::new(name),
    }
}

#[tokio::test]
async fn a_way_your_request_names_binds_and_the_rest_follow_the_rule_of_thumb() -> anyhow::Result<()>
{
    let stage = Stage::asking(
        "Rate-limit the API per key: the store by Maker-checker.",
        |s| s.divide.predictions = false,
    )?;
    let mut step = stage.start();
    stage
        .answer(&mut step, 1, AnswerAction::Approve, None)
        .await?;
    ends(step).await?;

    let created = created_ways(&stage.sink.events());
    let way_of = |title: &str| {
        created
            .iter()
            .find(|(created, _)| created == title)
            .and_then(|(_, way)| way.clone())
    };
    assert_eq!(way_of("store"), Some((workflow_way("maker-checker"), true)));
    assert_eq!(
        way_of("middleware"),
        Some((workflow_way("maker-checker"), false))
    );
    assert_eq!(way_of("api"), Some((Way::OwnSession, false)));
    assert!(
        created.iter().all(|(_, way)| way.is_some()),
        "every task is done some way: {created:?}"
    );
    Ok(())
}

#[tokio::test]
async fn a_way_the_plan_leaves_out_holds_work_until_you_approve_it_again() -> anyhow::Result<()> {
    let stage = Stage::asking(
        "Rate-limit the API per key, and the docs across agents.",
        |s| s.divide.predictions = false,
    )?;
    let mut step = stage.start();
    let first = stage
        .answer(&mut step, 1, AnswerAction::Approve, None)
        .await?;
    let lacks = "You asked for work done by agents, and no task in the plan is.";
    assert!(plan_packet(&first)?.flags.iter().any(|flag| flag == lacks));
    let again = stage
        .answer(&mut step, 2, AnswerAction::Approve, None)
        .await?;
    assert!(
        plan_packet(&again)?
            .flags
            .iter()
            .any(|flag| flag == &format!("Before work starts: {lacks}")),
        "{:?}",
        plan_packet(&again)?.flags
    );
    ends(step).await?;

    let events = stage.sink.events();
    assert_eq!(
        plan_approvals(&events).len(),
        2,
        "approved twice, it goes ahead"
    );
    assert!(events.iter().any(|event| matches!(
        &event.kind,
        RunEventKind::Note(note) if note.text == format!("The plan went ahead as approved: {lacks}")
    )));
    assert_eq!(created_ways(&events).len(), 5);
    Ok(())
}

#[tokio::test]
async fn a_way_you_change_at_the_approval_binds_and_is_recorded() -> anyhow::Result<()> {
    let stage = Stage::new(&[], &[], |settings| settings.divide.predictions = false)?;
    let mut step = stage.start_workflow()?;
    stage
        .answer(
            &mut step,
            1,
            AnswerAction::ApproveWithEdits,
            Some("task-002 way 3 agents"),
        )
        .await?;
    let reopened = stage
        .answer(
            &mut step,
            2,
            AnswerAction::ApproveWithEdits,
            Some("task-002 way maker-checker"),
        )
        .await?;
    assert!(
        plan_packet(&reopened)?.flags.iter().any(|flag| flag
            == "Your edits weren't applied: store can't take 3 agents: its paths don't split \
                into parts that share no files."),
        "{:?}",
        plan_packet(&reopened)?.flags
    );
    ends(step).await?;

    let events = stage.sink.events();
    let way_of_store = created_ways(&events)
        .into_iter()
        .find(|(title, _)| title == "store")
        .and_then(|(_, way)| way);
    assert_eq!(way_of_store, Some((workflow_way("maker-checker"), true)));
    let decided = events
        .iter()
        .find_map(|e| match &e.kind {
            RunEventKind::Plan(PlanEvent::Decided(decision)) => Some(decision.clone()),
            _ => None,
        })
        .context("no Plan::Decided")?;
    assert!(
        decided.rationale.ends_with(
            "You changed how store is done: by Maker-checker, where the note keeper had it in \
             its own session."
        ),
        "{}",
        decided.rationale
    );
    Ok(())
}

#[tokio::test]
async fn a_workflow_one_task_cant_follow_is_named_before_work_starts() -> anyhow::Result<()> {
    let stage = Stage::asking(
        "Rate-limit the API per key: the store by Plan-first.",
        |s| s.divide.predictions = false,
    )?;
    // Its first step is the orchestrator's, which plans a whole request.
    stage.repo.write(
        ".argonauts/workflows/plan-first.toml",
        "name = \"plan-first\"\n[[step]]\nname = \"Plan\"\nrole = \"orchestrator\"\n\
         [[step]]\nname = \"Build\"\nrole = \"worker\"\n",
    )?;
    let mut step = stage.start();
    let first = stage
        .answer(&mut step, 1, AnswerAction::SendBack, Some("Try again."))
        .await?;
    ends(step).await?;
    assert!(
        plan_packet(&first)?.flags.iter().any(|flag| flag
            == "store: Plan-first can't do one task: its Plan step is the orchestrator's."),
        "{:?}",
        plan_packet(&first)?.flags
    );
    Ok(())
}

/// In auto the run may settle the plan for you, except a plan that breaks a way you asked for:
/// that one waits for you.
#[tokio::test]
async fn a_plan_that_breaks_your_way_waits_for_you_in_auto() -> anyhow::Result<()> {
    let stage = Stage::asking(
        "Rate-limit the API per key: the store by Plan-first.",
        |s| {
            s.divide.predictions = false;
            s.approvals.plan.mode = ApprovalMode::Auto;
        },
    )?;
    stage.repo.write(
        ".argonauts/workflows/plan-first.toml",
        "name = \"plan-first\"\n[[step]]\nname = \"Plan\"\nrole = \"orchestrator\"\n\
         [[step]]\nname = \"Build\"\nrole = \"worker\"\n",
    )?;
    let mut step = stage.start();
    let first = stage
        .answer(&mut step, 1, AnswerAction::SendBack, Some("Try again."))
        .await?;
    ends(step).await?;
    assert_eq!(first.mode, ApprovalMode::Supervised);
    assert!(first.needs_you);
    Ok(())
}

/// On autopilot nothing waits for you: a plan that breaks a way you asked for opens and is
/// approved as it stands at once in your place, one line says what it lacks, and the run's
/// closing answer lists it among what autopilot decided.
#[tokio::test]
async fn a_plan_that_breaks_your_way_goes_ahead_on_autopilot_and_says_so() -> anyhow::Result<()> {
    let stage = Stage::asking(
        "Rate-limit the API per key: the store by Plan-first.",
        |s| {
            s.divide.predictions = false;
            s.approvals.plan.mode = ApprovalMode::Autopilot;
        },
    )?;
    stage.repo.write(
        ".argonauts/workflows/plan-first.toml",
        "name = \"plan-first\"\n[[step]]\nname = \"Plan\"\nrole = \"orchestrator\"\n\
         [[step]]\nname = \"Build\"\nrole = \"worker\"\n",
    )?;
    let done = ends(stage.start()).await?;
    assert_eq!(
        done.summary
            .as_deref()
            .map(|summary| summary.split(':').next()),
        Some(Some("Plan approved on autopilot")),
        "{done:?}"
    );
    let events = stage.sink.events();
    let opened = plan_approvals(&events);
    assert_eq!(opened.len(), 1, "{opened:?}");
    assert!(!opened[0].needs_you, "nothing asked you");
    let decided: Vec<&ApprovalDecided> = events
        .iter()
        .filter_map(|event| match &event.kind {
            RunEventKind::Approval(ApprovalEvent::Decided(decided))
                if decided.id == opened[0].id =>
            {
                Some(decided)
            }
            _ => None,
        })
        .collect();
    assert_eq!(decided.len(), 1, "{decided:?}");
    assert_eq!(decided[0].by, Party::Autopilot);
    assert_eq!(decided[0].action, AnswerAction::Approve);
    assert_eq!(
        stage.host.read(approvals::autopilot::decided),
        ["\"Approve this plan?\" took \"Approve the plan\"".to_owned()]
    );
    let said: Vec<&str> = events
        .iter()
        .filter_map(|event| match &event.kind {
            RunEventKind::Note(note) if note.text.starts_with("Autopilot approved the plan") => {
                Some(note.text.as_str())
            }
            _ => None,
        })
        .collect();
    assert_eq!(said.len(), 1, "{said:?}");
    assert!(
        said[0]
            .ends_with("store: Plan-first can't do one task: its Plan step is the orchestrator's."),
        "{said:?}"
    );
    Ok(())
}

#[tokio::test]
async fn a_council_that_asks_before_a_same_maker_check_asks_once_at_the_plan() -> anyhow::Result<()>
{
    let flags_with = |check: SameModelCheck| async move {
        let stage = Stage::asking(
            "Rate-limit the API per key: the store by Maker-checker.",
            |s| {
                s.divide.predictions = false;
                s.workflow = "council".to_owned();
                s.council.same_model_check = check;
                s.crew.clear();
                s.crew.add(CREW[0].0.parse().expect("a model"));
            },
        )?;
        let mut step = stage.start();
        let first = stage
            .answer(&mut step, 1, AnswerAction::SendBack, Some("Try again."))
            .await?;
        ends(step).await?;
        anyhow::Ok(plan_packet(&first)?.flags.clone())
    };

    let asked = flags_with(SameModelCheck::Ask).await?;
    let asks: Vec<&String> = asked
        .iter()
        .filter(|flag| flag.starts_with("Same maker: "))
        .collect();
    assert_eq!(asks.len(), 1, "{asked:?}");
    assert!(
        asks[0].contains("checks store") && asks[0].ends_with("Approving the plan lets it."),
        "{asks:?}"
    );
    let allowed = flags_with(SameModelCheck::Allow).await?;
    assert!(
        !allowed.iter().any(|flag| flag.starts_with("Same maker: ")),
        "{allowed:?}"
    );
    Ok(())
}

/// A council where `objector` objects to your one point, `yours`, saying `why`.
fn objected(objector: &SeatId, yours: &str, why: &str) -> anyhow::Result<Vec<RunEventKind>> {
    let talk = StepRef {
        workflow: WorkflowId::parse("wf-001").context("a workflow id")?,
        phase: 1,
        path: vec![0],
    };
    let mut seats = Vec::new();
    for (at, (worker, _)) in CREW.iter().enumerate() {
        seats.push(Seat {
            id: SeatId::numbered(at as u32 + 1),
            model: worker.parse()?,
            role: SeatRole::Proposer,
            number: None,
        });
    }
    let against = vec![PointStance {
        point: 1,
        stance: Stance::Object,
    }];
    let kinds = [
        CouncilEventKind::Seated(Seating {
            seats,
            notes: Some(SeatId::numbered(1)),
        }),
        CouncilEventKind::Points {
            points: vec![Point {
                number: 1,
                words: yours.to_owned(),
                state: PointState::Yours,
                raised_by: None,
                sides: Vec::new(),
            }],
        },
        CouncilEventKind::Message(Box::new(CouncilMessage {
            id: MessageId::numbered(1),
            from: Speaker::Seat(objector.clone()),
            kind: MessageKind::Talk,
            text: vec![TextPart::Words(why.to_owned())],
            reply_to: None,
            mentions: Vec::new(),
            stances: against.clone(),
            hidden: 0,
            hidden_names: Vec::new(),
            guesses_hidden: 0,
            rewritten: false,
            declined: false,
            started_before: None,
        })),
        CouncilEventKind::Stances {
            seat: objector.clone(),
            stances: against,
        },
    ];
    Ok(kinds
        .into_iter()
        .map(|kind| {
            RunEventKind::Council(CouncilEvent {
                talk: talk.clone(),
                kind,
            })
        })
        .collect())
}

/// A task done by a workflow carries how many steps the workflow declares, which its row
/// counts done of before they start.
#[tokio::test]
async fn a_task_done_by_a_workflow_carries_its_steps() -> anyhow::Result<()> {
    let stage = Stage::asking(
        "Rate-limit the API per key: the store by Maker-checker.",
        |s| {
            s.divide.predictions = false;
        },
    )?;
    let mut step = stage.start();
    stage
        .answer(&mut step, 1, AnswerAction::Approve, None)
        .await?;
    ends(step).await?;
    let steps = stage
        .sink
        .events()
        .iter()
        .find_map(|event| match &event.kind {
            RunEventKind::Task(TaskEvent::Created(spec)) if spec.title == "store" => {
                spec.way.as_ref().map(|way| way.steps)
            }
            _ => None,
        });
    assert_eq!(steps, Some(Some(2)), "Maker-checker builds, then checks");
    Ok(())
}

/// The seat you name for the work you ask a way for does that work.
#[tokio::test]
async fn the_seat_you_name_with_a_way_does_that_task() -> anyhow::Result<()> {
    let stored = |request: &'static str| async move {
        let talk = objected(&SeatId::numbered(2), "the limits", "Fine.")?;
        let stage = Stage::after_talk(request, talk, |s| s.divide.predictions = false)?;
        let mut step = stage.start();
        stage
            .answer(&mut step, 1, AnswerAction::Approve, None)
            .await?;
        ends(step).await?;
        let worker = stage
            .sink
            .events()
            .iter()
            .find_map(|event| match &event.kind {
                RunEventKind::Task(TaskEvent::Created(spec)) if spec.title == "store" => {
                    Some(spec.worker.clone())
                }
                _ => None,
            });
        anyhow::Ok(worker.flatten())
    };
    let gpt: argonauts_core::names::ModelRef = CREW[1].0.parse()?;
    let named =
        stored("Rate-limit the API per key: let GPT 5.6 do the store by Maker-checker.").await?;
    assert_eq!(named, Some(gpt.clone()));
    let unnamed = stored("Rate-limit the API per key: the store by Maker-checker.").await?;
    assert_ne!(
        unnamed,
        Some(gpt),
        "the plan alone gives the store to another model"
    );
    Ok(())
}

#[tokio::test]
async fn a_seats_objection_to_your_way_is_shown_and_recorded_and_yours_stands() -> anyhow::Result<()>
{
    let talk = objected(
        &SeatId::numbered(2),
        "the store by Maker-checker",
        "A store this small needs no checker. Its own session is enough.",
    )?;
    let stage = Stage::after_talk(
        "Rate-limit the API per key: the store by Maker-checker.",
        talk,
        |s| s.divide.predictions = false,
    )?;
    let mut step = stage.start();
    stage
        .answer(
            &mut step,
            1,
            AnswerAction::ApproveWithEdits,
            Some("task-001 way maker-checker"),
        )
        .await?;
    ends(step).await?;

    let events = stage.sink.events();
    let (shown, offer) = events
        .iter()
        .find_map(|e| match &e.kind {
            RunEventKind::Approval(ApprovalEvent::Opened(opened)) => match &opened.packet {
                ApprovalPacket::Plan(plan) => Some((plan.objections.clone(), plan.offer.clone())),
                _ => None,
            },
            _ => None,
        })
        .context("no plan approval")?;
    // Approve with edits offers the ways the council's setup allows, own session first.
    let offer = offer.context("a plan with ways offers them")?;
    assert_eq!(offer.ways.first(), Some(&Way::OwnSession));
    assert_eq!(
        shown,
        [WayObjection {
            task: TaskId::numbered(2),
            seat: SeatId::numbered(2),
            reason: vec![TextPart::Words(
                "A store this small needs no checker".to_owned()
            )],
        }]
    );
    let decided = events
        .iter()
        .find_map(|e| match &e.kind {
            RunEventKind::Plan(PlanEvent::Decided(decision)) => Some(decision.clone()),
            _ => None,
        })
        .context("no Plan::Decided")?;
    assert!(
        decided.rationale.ends_with(
            "store is done by Maker-checker, as you asked. A seat objected: A store this small \
             needs no checker · yours stands"
        ),
        "{}",
        decided.rationale
    );
    Ok(())
}
