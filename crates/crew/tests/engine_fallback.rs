//! Fallbacks with checkpoints: the strategies, which fallback list applies,
//! checkpoint tags, commit authors in git notes, `/rewind`, and what happens when the original
//! is back. Each test drives the fallback code on a real temp repo through the engine's `Host`.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Context, bail, ensure};
use serde_json::json;
use time::macros::datetime;
use tokio::sync::mpsc;

use argonauts_core::api::ApprovalAnswer;
use argonauts_core::engine::{EngineCommand, RepoContext};
use argonauts_core::events::agent_status::{FallbackEvent, FallbackPlan, JobRef, JobWait};
use argonauts_core::events::approval::{
    AnswerAction, ApprovalDecided, ApprovalEvent, ApprovalKind, ApprovalOpened, ApprovalPacket,
    FreeText, MergePacket,
};
use argonauts_core::events::goal::{GoalEvent, GoalFile, GoalSource};
use argonauts_core::events::run::{Party, RunLifecycle, RunMode, RunStarted};
use argonauts_core::events::session::{EndReason, SessionEvent, SessionStarted};
use argonauts_core::events::shared::{BlackboardKind, BlackboardMessage};
use argonauts_core::events::task::{
    MergeEvent, ReviewEvent, TaskEvent, TaskKind, TaskLimit, TaskSpec, TaskState,
};
use argonauts_core::events::workflow::{StepStarted, WorkflowEvent, WorkflowStarted};
use argonauts_core::events::{RunEvent, RunEventKind};
use argonauts_core::fallback::CheckpointTag;
use argonauts_core::limits::{
    Candidate, CandidateQuery, CostEstimate, FallbackNote, FallbackPlanner, HealthState,
    PlanUsageBoard, PlanUsageReading, PlanUsageSource, PlanUsageStatus, RecoveryAction, Shortlist,
    WorkSoFar,
};
use argonauts_core::names::{ApprovalId, ModelRef, RoleName, SessionKey, WorkflowId};
use argonauts_core::session_runner::{Access, RunnerEvent, RunnerKind};
use argonauts_core::settings::Settings;
use argonauts_core::settings::approvals::{ApprovalMode, DecideWith};
use argonauts_core::settings::crew::CrewRole;
use argonauts_core::settings::fallback::{
    AgentFallbacks, FallbackEntry, FallbackMode, FallbackSettings, FallbackStrategy, FallbackTable,
    OnReturn, Pick, UnknownTier,
};
use argonauts_core::settings::sessions::SessionLifetime;
use argonauts_core::steps::StepName;
use argonauts_core::workflow::{StandardPhase, StepKind, StepRef, Workflow};
use argonauts_core::{AgentEvent, CommitInfo, DiffStat, RepoId, RunId, Spend, TaskId};
use argonauts_crew::approvals;
use argonauts_crew::engine::commands;
use argonauts_crew::engine::host::Host;
use argonauts_crew::engine::shared::Shared;
use argonauts_crew::fallback::on_return::Applied;
use argonauts_crew::fallback::{self, on_return, rewind};
use argonauts_crew::merge::branch::ensure_run_branch;
use argonauts_crew::roles::display_name;
use argonauts_crew::state::RunState;
use argonauts_crew::state::blackboard::Addressee;
use argonauts_testkit::repo::TempRepo;
use argonauts_testkit::runner::{FakeRunner, FakeSession, session_end};
use argonauts_testkit::services::{FakeServices, MemorySink, TEST_RUN, engine_context_in};

const OPUS: &str = "claude-code/opus";
const CODEX: &str = "codex/gpt-5.6";
const AGY: &str = "agy/gemini-3.6";
/// The task's ring reviewers, of other providers than its planned worker.
const GPT_REVIEWER: &str = "codex/gpt-5.6-mini";
const GEMINI_REVIEWER: &str = "agy/gemini-3.6-flash";

fn worker(name: &str) -> ModelRef {
    name.parse().expect("a valid model")
}

fn task(n: u32) -> TaskId {
    TaskId::numbered(n)
}

/// A run with a stopped task: `task-001`, planned for claude-code/opus, in its own worktree
/// with one commit by it, waiting for a decision.
struct Bench {
    services: FakeServices,
    repo: TempRepo,
    layout: RepoContext,
    host: Host,
    sink: Arc<MemorySink>,
    _commands: mpsc::Sender<EngineCommand>,
}

struct Setup {
    settings: Settings,
    planner: Option<Arc<dyn FallbackPlanner>>,
    /// Extra tasks planned for claude-code/opus, with no worktree yet.
    more_tasks: u32,
}

impl Setup {
    fn new(settings: Settings) -> Self {
        Self {
            settings,
            planner: None,
            more_tasks: 0,
        }
    }
}

impl Bench {
    async fn new() -> anyhow::Result<Self> {
        Self::with(Setup::new(Settings::default())).await
    }

    async fn with(setup: Setup) -> anyhow::Result<Self> {
        let services = FakeServices::new();
        let repo = TempRepo::new()?;
        let base = repo.head()?;
        let state_dir = repo.scratch().join("argonauts");
        let layout = RepoContext {
            id: RepoId::from_canonical_path(repo.root()),
            root: repo.root().to_path_buf(),
            run_dir: state_dir.join("state/runs").join(TEST_RUN),
            worktrees: state_dir.join("worktrees").join(TEST_RUN),
            base_worktree: state_dir.join("worktrees").join(TEST_RUN).join("base"),
        };
        let tree = layout.worktrees.join(task(1).as_str());
        std::fs::create_dir_all(&layout.worktrees)?;
        let tree_arg = tree.to_string_lossy().into_owned();
        repo.git(&[
            "worktree",
            "add",
            "--quiet",
            "-b",
            &branch(1),
            &tree_arg,
            &base,
        ])?;
        let first = commit_in(
            &repo,
            &tree,
            "store.rs",
            "fn store() {}\n",
            "Start the store",
        )?;

        let mut history = vec![
            (None, started(&repo, &base)),
            (None, understood(&repo)),
            (None, workflow_started()?),
            (None, build_step()?),
            (
                Some(task(1)),
                RunEventKind::Task(TaskEvent::Created(Box::new(spec(1)))),
            ),
            (
                Some(task(1)),
                RunEventKind::Task(TaskEvent::Worktree {
                    path: tree.clone(),
                    branch: branch(1),
                    base: base.clone(),
                }),
            ),
            (Some(task(1)), stopped_state(None)),
            (Some(task(1)), committed(1, &first, Some(OPUS))),
        ];
        for n in 2..2 + setup.more_tasks {
            history.push((
                Some(task(n)),
                RunEventKind::Task(TaskEvent::Created(Box::new(spec(n)))),
            ));
            history.push((Some(task(n)), stopped_state(None)));
        }
        let history: Vec<RunEvent> = history
            .into_iter()
            .zip(0..)
            .map(|((task, kind), seq)| event(seq, task, kind))
            .collect();

        let (mut context, sink, commands) =
            engine_context_in(&services, setup.settings, layout.clone(), history.clone());
        if let Some(planner) = setup.planner {
            context.services.fallback = planner;
        }
        let shared = Shared::new(&context, RunState::fold(&history))?;
        Ok(Self {
            services,
            repo,
            layout,
            host: Host::new(Arc::new(shared)),
            sink,
            _commands: commands,
        })
    }

    fn tree(&self) -> PathBuf {
        self.layout.worktrees.join(task(1).as_str())
    }

    fn worker(&self, task: &TaskId) -> Option<ModelRef> {
        self.host.read(|state| {
            state
                .tasks
                .get(task)
                .and_then(|record| record.worker.clone())
        })
    }

    fn debt(&self, task: &TaskId) -> Option<argonauts_core::fallback::QualityDebt> {
        self.host
            .read(|state| state.tasks.get(task).and_then(|record| record.debt.clone()))
    }

    fn notes(&self) -> Vec<String> {
        self.sink
            .events()
            .into_iter()
            .filter_map(|event| match event.kind {
                RunEventKind::Note(note) => Some(note.text),
                _ => None,
            })
            .collect()
    }

    fn task_events(&self) -> Vec<TaskEvent> {
        self.sink
            .events()
            .into_iter()
            .filter_map(|event| match event.kind {
                RunEventKind::Task(task) => Some(task),
                _ => None,
            })
            .collect()
    }

    fn fallback_events(&self) -> Vec<FallbackEvent> {
        self.sink
            .events()
            .into_iter()
            .filter_map(|event| match event.kind {
                RunEventKind::Fallback(fallback) => Some(fallback),
                _ => None,
            })
            .collect()
    }

    /// What the run said about a workflow of its own, each with the workflow it is about. The
    /// workflow's view shows the latest under its header.
    fn workflow_notes(&self) -> Vec<(Option<WorkflowId>, String)> {
        self.sink
            .events()
            .into_iter()
            .filter_map(|event| match event.kind {
                RunEventKind::Workflow(WorkflowEvent::Note(note)) => {
                    Some((note.workflow, note.text))
                }
                _ => None,
            })
            .collect()
    }

    fn record(&self, task: &TaskId, kind: RunEventKind) -> anyhow::Result<()> {
        self.host.record(Some(task), None, kind)?;
        Ok(())
    }

    /// The task's build session `n`, by `by` at the run's build step, which ended for `reason`.
    fn built_until(
        &self,
        task: &TaskId,
        n: u32,
        by: &str,
        reason: EndReason,
    ) -> anyhow::Result<()> {
        let key = SessionKey::numbered(n);
        let by = worker(by);
        let started = SessionStarted {
            agent_id: by.agent_id(),
            provider: by.agent.clone(),
            worker: by,
            kind: RunnerKind::Cli,
            role: RoleName::Worker,
            step: Some(StepName::Build),
            access: Access::Build,
            lifetime: SessionLifetime::Task,
            resumed_from: None,
            follow_key: None,
            workflow_step: Some(build_at()),
            label: None,
            asked: None,
        };
        let session = |event| RunEventKind::Session(event);
        self.host.record(
            Some(task),
            Some(&key),
            session(SessionEvent::Started(Box::new(started))),
        )?;
        self.host.record(
            Some(task),
            Some(&key),
            session(SessionEvent::Ended(Box::new(session_end(
                reason, None, None,
            )))),
        )?;
        Ok(())
    }

    /// The task's agent stopped again: back to `NeedsDecision`, as recovery leaves it.
    fn stop_again(&self, task: &TaskId) -> anyhow::Result<()> {
        let from = self
            .host
            .read(|state| state.tasks.get(task).map(|record| record.state));
        self.record(task, stopped_state(from))
    }

    /// Moves the task through `path`, as its pipeline would.
    fn move_through(&self, task: &TaskId, path: &[TaskState]) -> anyhow::Result<()> {
        for to in path {
            let from = self.state(task);
            self.record(
                task,
                RunEventKind::Task(TaskEvent::State {
                    from,
                    to: *to,
                    reason: "moved by the test".to_owned(),
                }),
            )?;
        }
        Ok(())
    }

    fn state(&self, task: &TaskId) -> Option<TaskState> {
        self.host
            .read(|state| state.tasks.get(task).map(|record| record.state))
    }

    /// Reviews recorded on the task, by reviewer.
    fn reviewed_by(&self, task: &TaskId) -> Vec<ModelRef> {
        self.host.read(|state| {
            state
                .tasks
                .get(task)
                .map(|record| {
                    record
                        .reviews
                        .iter()
                        .filter_map(|event| match event {
                            ReviewEvent::Reviewed { reviewer, .. } => Some(reviewer.clone()),
                            _ => None,
                        })
                        .collect()
                })
                .unwrap_or_default()
        })
    }

    /// A commit by `by` in task-001's worktree, recorded as Argonauts Code records it.
    fn work(&self, file: &str, by: Option<&str>) -> anyhow::Result<String> {
        let sha = commit_in(&self.repo, &self.tree(), file, "work\n", file)?;
        self.record(&task(1), committed(1, &sha, by))?;
        Ok(sha)
    }

    async fn fallback(&self, task: &TaskId, to: &str) -> anyhow::Result<()> {
        let action = RecoveryAction::Fallback {
            to: worker(to),
            note: Box::new(note(task)),
            checkpoint: Some(tag(task, 0)),
        };
        fallback::apply(&self.host, task, action).await?;
        Ok(())
    }

    fn tag_target(&self, n: u32) -> anyhow::Result<String> {
        self.repo.git(&[
            "rev-parse",
            &format!("{}^{{commit}}", tag(&task(1), n).tag_name()),
        ])
    }

    /// Waits for an approval of `kind` that isn't decided yet.
    async fn opened(&self, kind: ApprovalKind) -> anyhow::Result<ApprovalOpened> {
        for _ in 0..200 {
            let found = self.host.read(|state| {
                state
                    .approvals
                    .open()
                    .find(|record| record.opened.kind == kind)
                    .map(|record| record.opened.clone())
            });
            if let Some(opened) = found {
                return Ok(opened);
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        bail!("no {kind:?} approval opened")
    }

    fn answer(&self, opened: &ApprovalOpened, action: AnswerAction) -> anyhow::Result<()> {
        approvals::answer(
            &self.host,
            ApprovalAnswer {
                id: opened.id.clone(),
                action,
                text: None,
                evidence_sha256: opened.evidence_sha256.clone(),
                tasks: Vec::new(),
            },
        )
        .map_err(anyhow::Error::msg)
    }
}

fn event(seq: u64, task: Option<TaskId>, kind: RunEventKind) -> RunEvent {
    RunEvent {
        seq,
        ts: datetime!(2026-09-30 10:00 UTC),
        run_id: RunId::new(TEST_RUN).expect("the test run id"),
        task_id: task,
        session: None,
        source: "argonauts-code".to_owned(),
        kind,
    }
}

fn started(repo: &TempRepo, base: &str) -> RunEventKind {
    RunEventKind::Run(RunLifecycle::Started(Box::new(RunStarted {
        request: "Add a Redis store".to_owned(),
        title: "Redis store".to_owned(),
        // Captain, so build sessions need only the approved goal, not a plan as well.
        mode: RunMode::Captain,
        forced: true,
        workflow: "default".to_owned(),
        estimate: None,
        captain_or_crew: None,
        conversation_turn: None,
        continues: None,
        repo_root: repo.root().to_path_buf(),
        base_branch: Some("main".to_owned()),
        base_commit: base.to_owned(),
        uncommitted: Vec::new(),
        uncommitted_included: false,
        settings_sha256: String::new(),
        settings: serde_json::Value::Null,
        overrides: Vec::new(),
        cli_versions: Default::default(),
        version: "0.0.1".to_owned(),
        outside_agent: None,
        instructions: Vec::new(),
        required_steps: Vec::new(),
    })))
}

/// Build sessions start only under an approved goal.
fn understood(repo: &TempRepo) -> RunEventKind {
    RunEventKind::Goal(GoalEvent::Approved {
        file: GoalFile {
            run: RunId::new(TEST_RUN).expect("the test run id"),
            version: 1,
            summary: "A Redis store".to_owned(),
            criteria: Vec::new(),
            in_scope: Vec::new(),
            out_of_scope: Vec::new(),
            assumptions: Vec::new(),
            answers: Vec::new(),
            source: GoalSource::Prompt,
            path: repo.root().join(".argonauts/goal.md"),
            sha256: "0".repeat(64),
        },
        by: Party::User,
    })
}

fn workflow_started() -> anyhow::Result<RunEventKind> {
    let workflow: Workflow = serde_json::from_value(json!({
        "name": "build-only",
        "phases": [{"name": "Build", "standard": "do", "steps": [{"kind": "build"}]}],
    }))?;
    Ok(RunEventKind::Workflow(WorkflowEvent::Started(Box::new(
        WorkflowStarted {
            id: WorkflowId::numbered(1),
            from: None,
            version: "1".to_owned(),
            parent: None,
            workflow,
        },
    ))))
}

/// The build step of the run's own workflow.
fn build_at() -> StepRef {
    StepRef {
        workflow: WorkflowId::numbered(1),
        phase: 0,
        path: vec![0],
    }
}

fn build_step() -> anyhow::Result<RunEventKind> {
    Ok(RunEventKind::Workflow(WorkflowEvent::StepStarted(
        StepStarted {
            step: build_at(),
            kind: StepKind::Build,
            standard: StandardPhase::Do,
            round: 1,
        },
    )))
}

fn spec(n: u32) -> TaskSpec {
    TaskSpec {
        id: task(n),
        title: format!("Task {n}"),
        task: "Build the Redis store".to_owned(),
        kind: TaskKind::Build,
        owned_paths: Vec::new(),
        read_only_paths: Vec::new(),
        forbidden_paths: Vec::new(),
        hidden_paths: Vec::new(),
        depends_on: Vec::new(),
        criteria: Vec::new(),
        worker: Some(worker(OPUS)),
        reviewers: vec![worker(GPT_REVIEWER), worker(GEMINI_REVIEWER)],
        limit: TaskLimit::default(),
        predictions: None,
        item: None,
    }
}

fn branch(n: u32) -> String {
    format!("argonauts/{TEST_RUN}/{}", task(n))
}

fn stopped_state(from: Option<TaskState>) -> RunEventKind {
    RunEventKind::Task(TaskEvent::State {
        from,
        to: TaskState::NeedsDecision,
        reason: "usage limit".to_owned(),
    })
}

fn committed(n: u32, sha: &str, by: Option<&str>) -> RunEventKind {
    RunEventKind::Task(TaskEvent::Committed(CommitInfo {
        branch: branch(n),
        sha: Some(sha.to_owned()),
        message: "work".to_owned(),
        stat: DiffStat::default(),
        by: by.map(worker),
    }))
}

fn tag(task: &TaskId, n: u32) -> CheckpointTag {
    CheckpointTag {
        run: RunId::new(TEST_RUN).expect("the test run id"),
        task: task.clone(),
        n,
    }
}

fn note(task: &TaskId) -> FallbackNote {
    let spec = spec(
        task.as_str()
            .trim_start_matches("task-")
            .parse()
            .unwrap_or(1),
    );
    FallbackNote {
        task: spec,
        goal_sha256: None,
        notes: Vec::new(),
        work: WorkSoFar {
            branch: format!("argonauts/{TEST_RUN}/{task}"),
            commit: None,
            stat: DiffStat::default(),
        },
        happened: Vec::new(),
        last_todo: Vec::new(),
        checks: Vec::new(),
        why_stopped: "usage limit, resets at 3:45 PM".to_owned(),
        last_words: None,
        last_message: None,
        limit_left: None,
        transcript: None,
        summary: None,
    }
}

fn candidate(name: &str) -> Candidate {
    Candidate {
        worker: worker(name),
        plan_usage_left: Some(0.5),
        plan_usage_text: "50%".to_owned(),
        track_record: None,
        cost: CostEstimate::PlanUsage(0.1),
        needs_confirm: false,
        may_run_out: false,
    }
}

fn git_in(repo: &TempRepo, dir: &Path, args: &[&str]) -> anyhow::Result<String> {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .envs(repo.envs())
        .output()
        .context("running git")?;
    ensure!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_owned())
}

fn commit_in(
    repo: &TempRepo,
    dir: &Path,
    file: &str,
    text: &str,
    message: &str,
) -> anyhow::Result<String> {
    std::fs::write(dir.join(file), text)?;
    git_in(repo, dir, &["add", "-A"])?;
    git_in(repo, dir, &["commit", "--quiet", "-m", message])?;
    git_in(repo, dir, &["rev-parse", "HEAD"])
}

/// A planner that walks the fallback tables the way the limits service does, with a fixed
/// strategy when one is given.
struct WalkingPlanner {
    settings: Settings,
    strategy: Option<FallbackStrategy>,
}

impl WalkingPlanner {
    fn applied(
        &self,
        query: &CandidateQuery,
    ) -> argonauts_core::settings::fallback::AppliedFallback {
        self.settings
            .fallback
            .applied_to(query.role, &query.stopped.agent_id())
    }
}

impl FallbackPlanner for WalkingPlanner {
    fn candidates(&self, query: &CandidateQuery) -> Vec<Candidate> {
        self.applied(query)
            .candidates
            .iter()
            .map(|entry| candidate(&entry.model().to_string()))
            .collect()
    }

    fn note(&self, task: &TaskSpec, _events: &[RunEvent]) -> FallbackNote {
        note(&task.id)
    }

    fn shortlist(&self, query: &CandidateQuery) -> Shortlist {
        let applied = self.applied(query);
        Shortlist {
            strategy: self.strategy.unwrap_or(applied.strategy),
            race_count: applied.together,
            pick: Pick::FirstPassing,
            candidates: self.candidates(query),
        }
    }
}

fn entries(workers: &[&str]) -> Vec<FallbackEntry> {
    workers
        .iter()
        .map(|name| FallbackEntry::Model(worker(name)))
        .collect()
}

fn report(summary: &str) -> FakeSession {
    FakeSession::structured(json!({
        "status": "done",
        "summary": summary,
        "files_changed": [],
        "tests_run": [],
        "concerns": [],
        "questions": [],
        "change_requests": [],
        "memory_proposals": [],
    }))
}

fn approval() -> FakeSession {
    verdict("approve", "Looks right.")
}

fn verdict(verdict: &str, summary: &str) -> FakeSession {
    let comments = if verdict == "request_changes" {
        json!([{ "path": "store.rs", "line": 1, "severity": "medium", "text": "Handle errors" }])
    } else {
        json!([])
    };
    FakeSession::structured(json!({
        "verdict": verdict,
        "summary": summary,
        "comments": comments,
        "tamper_suspected": false,
    }))
}

/// A race candidate that writes `file` in its tree, then works until it's stopped.
fn slow_candidate(name: &str, provider: &str, file: &'static str) -> FakeRunner {
    FakeRunner::cli(name, provider).respond_with(move |spec| {
        let _ = std::fs::write(spec.workdir.join(file), "// half done\n");
        // Written with its edit tool, as a CLI reports it.
        FakeSession::until_stopped(vec![RunnerEvent::Agent(AgentEvent::FileChanged {
            path: file.to_owned(),
            kind: "add".to_owned(),
        })])
    })
}

#[tokio::test]
async fn fallback_strategy_first() -> anyhow::Result<()> {
    let bench = Bench::new().await?;
    let c1 = task(1);
    bench.fallback(&c1, CODEX).await?;
    assert_eq!(bench.worker(&c1), Some(worker(CODEX)));
    assert!(bench.fallback_events().iter().any(|event| matches!(
        event,
        FallbackEvent::FellBack { to, by: Party::Rules, .. } if *to == worker(CODEX)
    )));
    assert!(bench.task_events().iter().any(|event| matches!(
        event,
        TaskEvent::State {
            to: TaskState::Ready,
            ..
        }
    )));
    // An unknown tier counts as weaker, so the task owes a review by its original.
    let debt = bench.debt(&c1).context("debt after the first fallback")?;
    assert_eq!(
        (debt.since, &debt.by, &debt.needs),
        (1, &worker(CODEX), &worker(OPUS))
    );

    // The fallback runs out too: it falls back to the next candidate at fallback-2, and the debt
    // still names the strongest worker before it.
    bench.work("codex.rs", Some(CODEX))?;
    bench.stop_again(&c1)?;
    bench.fallback(&c1, AGY).await?;
    assert_eq!(bench.worker(&c1), Some(worker(AGY)));
    let debt = bench.debt(&c1).context("debt after the second fallback")?;
    assert_eq!(
        (debt.since, &debt.by, &debt.needs),
        (1, &worker(AGY), &worker(OPUS))
    );
    assert!(bench.tag_target(2).is_ok());
    Ok(())
}

#[tokio::test]
async fn fallback_strategy_spread() -> anyhow::Result<()> {
    let mut fallback = FallbackSettings::built_in();
    fallback.order = entries(&[CODEX, AGY]);
    let settings = Settings {
        fallback,
        ..Settings::default()
    };
    let mut setup = Setup::new(settings.clone());
    setup.more_tasks = 1;
    setup.planner = Some(Arc::new(WalkingPlanner {
        settings,
        strategy: Some(FallbackStrategy::Spread),
    }));
    let bench = Bench::with(setup).await?;

    // The limits service proposes Codex for both of Claude Code's tasks; spread balances them.
    bench.fallback(&task(1), CODEX).await?;
    bench.fallback(&task(2), CODEX).await?;
    assert_eq!(bench.worker(&task(1)), Some(worker(CODEX)));
    assert_eq!(bench.worker(&task(2)), Some(worker(AGY)));
    Ok(())
}

#[tokio::test]
async fn fallback_strategy_race() -> anyhow::Result<()> {
    let bench = Bench::new().await?;
    let codex = FakeRunner::cli(CODEX, "openai").then(FakeSession::ended(EndReason::Crashed {
        message: "segfault".to_owned(),
    }));
    let agy = FakeRunner::cli(AGY, "google").then(report("Store done"));
    let reviewer = FakeRunner::cli(GPT_REVIEWER, "openai").then(approval());
    bench.services.workers.add(codex);
    bench.services.workers.add(agy);
    bench.services.workers.add(reviewer);

    let c1 = task(1);
    let action = RecoveryAction::Race {
        to: vec![worker(CODEX), worker(AGY)],
        pick: Pick::FirstPassing,
        note: Box::new(note(&c1)),
        checkpoint: Some(tag(&c1, 1)),
    };
    fallback::apply(&bench.host, &c1, action).await?;

    assert_eq!(bench.worker(&c1), Some(worker(AGY)));
    // Agy's try won only once a reviewer of another provider approved it.
    assert_eq!(bench.reviewed_by(&c1), vec![worker(GPT_REVIEWER)]);
    let tagged = bench
        .task_events()
        .into_iter()
        .find_map(|event| match event {
            TaskEvent::CheckpointTagged { n, to, .. } => Some((n, to)),
            _ => None,
        });
    assert_eq!(tagged, Some((1, vec![worker(CODEX), worker(AGY)])));
    Ok(())
}

#[tokio::test]
async fn fallback_race_winner_passes_checks_and_review() -> anyhow::Result<()> {
    let bench = Bench::new().await?;
    // Codex is done first, but its review rejects the approach; Agy's slower try wins.
    let codex = FakeRunner::cli(CODEX, "openai").then(report("Store done fast"));
    let agy = FakeRunner::cli(AGY, "google")
        .then(report("Store done").with_delay(Duration::from_millis(300)));
    let gemini = FakeRunner::cli(GEMINI_REVIEWER, "google")
        .then(verdict("reject", "This store loses writes."));
    let gpt = FakeRunner::cli(GPT_REVIEWER, "openai").then(approval());
    bench.services.workers.add(codex);
    bench.services.workers.add(agy);
    bench.services.workers.add(gemini);
    bench.services.workers.add(gpt);

    let c1 = task(1);
    let action = RecoveryAction::Race {
        to: vec![worker(CODEX), worker(AGY)],
        pick: Pick::FirstPassing,
        note: Box::new(note(&c1)),
        checkpoint: None,
    };
    fallback::apply(&bench.host, &c1, action).await?;

    assert_eq!(bench.worker(&c1), Some(worker(AGY)));
    assert_eq!(
        bench.reviewed_by(&c1),
        vec![worker(GEMINI_REVIEWER), worker(GPT_REVIEWER)]
    );
    // The transcript says who took it together, and who passed.
    let said = bench.notes();
    let both = said
        .iter()
        .find(|note| note.contains("both take"))
        .context("no line on who takes it")?;
    assert!(
        both.ends_with("· the first to pass the checks wins · /fallback changes this"),
        "{both}"
    );
    assert!(
        said.iter()
            .any(|note| note.contains("passed first") && note.ends_with("try stopped")),
        "{said:?}"
    );
    Ok(())
}

/// `pick = "judge"`: every fallback runs to its end, and the judge picks among those that
/// passed. It reads their diffs by number, never by name.
#[tokio::test]
async fn together_with_a_judge_keeps_the_try_the_judge_picks() -> anyhow::Result<()> {
    let mut settings = Settings::default();
    settings.roles.entry(RoleName::Judge).or_default().models = vec![worker(OPUS)];
    let bench = Bench::with(Setup::new(settings)).await?;
    // Codex passes first, and the judge still prefers the slower try.
    let codex = FakeRunner::cli(CODEX, "openai").then(report("Store done fast"));
    let agy = FakeRunner::cli(AGY, "google")
        .then(report("Store done").with_delay(Duration::from_millis(300)));
    let asked = Arc::new(Mutex::new(Vec::new()));
    let judge = FakeRunner::cli(OPUS, "anthropic").respond_with({
        let asked = asked.clone();
        move |spec| {
            let labels: Vec<String> = spec.prompt.iter().map(|part| part.label.clone()).collect();
            asked.lock().unwrap().push((spec.step, labels));
            FakeSession::structured(json!({"pick": 2, "why": "It keeps writes in order."}))
        }
    });
    bench.services.workers.add(codex);
    bench.services.workers.add(agy);
    bench.services.workers.add(judge);
    bench
        .services
        .workers
        .add(FakeRunner::cli(GEMINI_REVIEWER, "google").then(approval()));
    bench
        .services
        .workers
        .add(FakeRunner::cli(GPT_REVIEWER, "openai").then(approval()));

    let c1 = task(1);
    let action = RecoveryAction::Race {
        to: vec![worker(CODEX), worker(AGY)],
        pick: Pick::Judge,
        note: Box::new(note(&c1)),
        checkpoint: None,
    };
    fallback::apply(&bench.host, &c1, action).await?;

    assert_eq!(bench.worker(&c1), Some(worker(AGY)));
    let asked = asked.lock().unwrap();
    let [(step, labels)] = asked.as_slice() else {
        anyhow::bail!("the judge was asked {} times", asked.len());
    };
    assert_eq!(*step, Some(argonauts_core::steps::StepName::PickTry));
    // Each try's diff is a part of its own, numbered and not named.
    let tries: Vec<&str> = labels
        .iter()
        .map(String::as_str)
        .filter(|label| label.starts_with("try "))
        .collect();
    assert_eq!(tries, ["try 1", "try 2"], "{labels:?}");
    let said = bench.notes();
    assert!(
        said.iter().any(|note| note.contains("the judge")
            && note.contains("out of 2 that passed")
            && note.contains("It keeps writes in order.")),
        "{said:?}"
    );
    Ok(())
}

/// With no judge to ask, the first try that passed is kept, and a note says why.
#[tokio::test]
async fn a_judge_that_cannot_answer_leaves_the_first_to_pass() -> anyhow::Result<()> {
    let mut settings = Settings::default();
    settings.roles.entry(RoleName::Judge).or_default().models = vec![worker(OPUS)];
    let bench = Bench::with(Setup::new(settings)).await?;
    let codex = FakeRunner::cli(CODEX, "openai").then(report("Store done fast"));
    let agy = FakeRunner::cli(AGY, "google")
        .then(report("Store done").with_delay(Duration::from_millis(300)));
    let judge = FakeRunner::cli(OPUS, "anthropic").then(FakeSession::ended(EndReason::Crashed {
        message: "segfault".to_owned(),
    }));
    for fake in [
        codex,
        agy,
        judge,
        FakeRunner::cli(GEMINI_REVIEWER, "google").then(approval()),
        FakeRunner::cli(GPT_REVIEWER, "openai").then(approval()),
    ] {
        bench.services.workers.add(fake);
    }

    let c1 = task(1);
    let action = RecoveryAction::Race {
        to: vec![worker(CODEX), worker(AGY)],
        pick: Pick::Judge,
        note: Box::new(note(&c1)),
        checkpoint: None,
    };
    fallback::apply(&bench.host, &c1, action).await?;

    assert_eq!(bench.worker(&c1), Some(worker(CODEX)));
    let said = bench.notes();
    assert!(
        said.iter()
            .any(|note| note.contains("try passed first and is kept, since")),
        "{said:?}"
    );
    Ok(())
}

#[tokio::test]
async fn fallback_race_keeps_loser_work() -> anyhow::Result<()> {
    let bench = Bench::new().await?;
    let c1 = task(1);
    // Codex's try is slow, and has work in its worktree when Agy's try wins.
    bench
        .services
        .workers
        .add(slow_candidate(CODEX, "openai", "codex-notes.rs"));
    let agy = FakeRunner::cli(AGY, "google").then(report("Store done"));
    let reviewer = FakeRunner::cli(GPT_REVIEWER, "openai").then(approval());
    bench.services.workers.add(agy);
    bench.services.workers.add(reviewer);

    let action = RecoveryAction::Race {
        to: vec![worker(CODEX), worker(AGY)],
        pick: Pick::FirstPassing,
        note: Box::new(note(&c1)),
        checkpoint: None,
    };
    fallback::apply(&bench.host, &c1, action).await?;

    assert_eq!(bench.worker(&c1), Some(worker(AGY)));
    // The loser's session ended before its tree was committed, as its own recorded work.
    let loser_branch = format!("{}-try-1", branch(1));
    let kept = bench
        .repo
        .git(&["show", &format!("{loser_branch}:codex-notes.rs")])?;
    assert_eq!(kept, "// half done");
    let commit = bench
        .task_events()
        .into_iter()
        .find_map(|event| match event {
            TaskEvent::Committed(commit) if commit.branch == loser_branch => Some(commit),
            _ => None,
        })
        .context("the loser's commit is recorded")?;
    assert_eq!(commit.by, Some(worker(CODEX)));
    let sha = commit.sha.context("the loser's commit sha")?;
    let note = bench
        .repo
        .git(&["notes", "--ref=refs/notes/argonauts", "show", &sha])?;
    assert!(note.contains(CODEX), "{note}");
    let still_running = bench.host.read(|state| state.sessions.running().count());
    assert_eq!(still_running, 0);
    Ok(())
}

#[tokio::test]
async fn a_second_race_never_reuses_try_trees() -> anyhow::Result<()> {
    let bench = Bench::new().await?;
    let c1 = task(1);
    bench
        .services
        .workers
        .add(slow_candidate(CODEX, "openai", "codex-notes.rs"));
    let agy = FakeRunner::cli(AGY, "google")
        .then(report("Store done"))
        .then(report("Store done again"));
    let reviewer = FakeRunner::cli(GPT_REVIEWER, "openai")
        .then(approval())
        .then(approval());
    bench.services.workers.add(agy);
    bench.services.workers.add(reviewer);
    let race = || RecoveryAction::Race {
        to: vec![worker(CODEX), worker(AGY)],
        pick: Pick::FirstPassing,
        note: Box::new(note(&c1)),
        checkpoint: None,
    };

    fallback::apply(&bench.host, &c1, race()).await?;
    let first_loser = format!("{}-try-1", branch(1));
    let kept = bench.repo.git(&["rev-parse", &first_loser])?;
    bench.stop_again(&c1)?;
    fallback::apply(&bench.host, &c1, race()).await?;

    // The second race took tries 3 and 4, and the first loser's branch didn't move.
    assert_eq!(bench.repo.git(&["rev-parse", &first_loser])?, kept);
    for n in [3, 4] {
        let tree = bench.layout.worktrees.join(format!("{c1}-try-{n}"));
        assert!(tree.exists(), "{}", tree.display());
    }
    Ok(())
}

#[tokio::test]
async fn fallback_precedence_every_level_set() -> anyhow::Result<()> {
    let level = |n: u32| format!("codex/level-{n}");
    let claude = worker(OPUS).agent_id();
    let every_level = || {
        let mut settings = Settings::default();
        let mut top = FallbackTable::built_in();
        top.order = entries(&[&level(5)]);
        top.replace.insert(
            claude.clone(),
            AgentFallbacks {
                with: entries(&[&level(4)]),
                ..AgentFallbacks::default()
            },
        );
        let mut role = FallbackTable {
            order: entries(&[&level(3)]),
            ..FallbackTable::default()
        };
        role.replace.insert(
            claude.clone(),
            AgentFallbacks {
                with: entries(&[&level(2)]),
                ..AgentFallbacks::default()
            },
        );
        settings.fallback.every_role = top;
        settings.fallback.roles.insert(CrewRole::Worker, role);
        settings
    };

    // Levels 2 to 5: the most specific list that's set gives the proposal, and the engine
    // applies it. Each round clears the level that won the one before.
    for expected in 2..=5 {
        let mut settings = every_level();
        if expected > 2
            && let Some(role) = settings.fallback.roles.get_mut(&CrewRole::Worker)
        {
            role.replace.clear();
        }
        if expected > 3 {
            settings.fallback.roles.remove(&CrewRole::Worker);
        }
        if expected > 4 {
            settings.fallback.replace.clear();
        }
        let planner = Arc::new(WalkingPlanner {
            settings: settings.clone(),
            strategy: None,
        });
        let mut setup = Setup::new(settings);
        setup.planner = Some(planner.clone());
        let bench = Bench::with(setup).await?;
        let record = bench.host.read(|state| state.tasks.get(&task(1)).cloned());
        let record = record.context("task-001")?;
        let query =
            argonauts_crew::fallback::choose::query(&bench.host, &record, &worker(OPUS), false);
        let proposed = planner.shortlist(&query).candidates;
        let proposed = proposed.first().context("a proposal")?.worker.to_string();
        assert_eq!(proposed, level(expected), "level {expected}");
        bench.fallback(&task(1), &proposed).await?;
        assert_eq!(bench.worker(&task(1)), Some(worker(&level(expected))));
    }

    // Level 1: your pick on the task's fallback question wins over every list.
    let settings = every_level();
    let mut setup = Setup::new(settings.clone());
    setup.planner = Some(Arc::new(WalkingPlanner {
        settings,
        strategy: None,
    }));
    let bench = Bench::with(setup).await?;
    let pick = worker("agy/your-pick");
    let id = ApprovalId::numbered(900);
    let plan = FallbackPlan {
        task: task(1),
        title: "Task 1".to_owned(),
        stopped: worker(OPUS),
        reason: "usage limit".to_owned(),
        resets_at: None,
        work: DiffStat::default(),
        checks_ran: false,
        candidates: vec![candidate(&level(2)), candidate("agy/your-pick")],
        mode: FallbackMode::Ask,
        strategy: FallbackStrategy::First,
        race_count: 1,
        also_moving: Vec::new(),
    };
    bench.record(
        &task(1),
        RunEventKind::Approval(ApprovalEvent::Opened(Box::new(ApprovalOpened {
            id: id.clone(),
            kind: ApprovalKind::Fallback,
            title: "claude-code/opus stopped · task-001".to_owned(),
            task: Some(task(1)),
            packet: ApprovalPacket::Fallback(plan),
            evidence_sha256: "e".repeat(64),
            mode: ApprovalMode::Supervised,
            decide_with: DecideWith::Rules,
            options: Vec::new(),
            free_text: FreeText::None,
            needs_you: true,
        }))),
    )?;
    bench.record(
        &task(1),
        RunEventKind::Approval(ApprovalEvent::Decided(ApprovalDecided {
            id,
            action: AnswerAction::Candidate {
                worker: pick.clone(),
            },
            text: None,
            by: Party::User,
            evidence_sha256: "e".repeat(64),
            auto_preview: None,
            tasks: Vec::new(),
        })),
    )?;
    bench.fallback(&task(1), &level(2)).await?;
    assert_eq!(bench.worker(&task(1)), Some(pick.clone()));

    // A pick whose agent is down doesn't hold; the lists decide again.
    bench.stop_again(&task(1))?;
    bench
        .services
        .health
        .set(&pick.agent_id(), HealthState::Down, Some("outage"));
    bench.fallback(&task(1), &level(2)).await?;
    assert_eq!(bench.worker(&task(1)), Some(worker(&level(2))));
    Ok(())
}

#[tokio::test]
async fn checkpoint_tag_at_every_fallback() -> anyhow::Result<()> {
    let bench = Bench::new().await?;
    let c1 = task(1);
    let first = bench.repo.git(&["rev-parse", &branch(1)])?;
    bench.fallback(&c1, CODEX).await?;
    let second = bench.work("codex.rs", Some(CODEX))?;
    bench.stop_again(&c1)?;
    bench.fallback(&c1, AGY).await?;
    // More work after the tag leaves it in place.
    bench.work("agy.rs", Some(AGY))?;

    assert_eq!(bench.tag_target(1)?, first);
    assert_eq!(bench.tag_target(2)?, second);
    let message =
        bench
            .repo
            .git(&["tag", "-l", "--format=%(contents)", &tag(&c1, 2).tag_name()])?;
    assert!(message.contains(&format!("from: {CODEX}")), "{message}");
    assert!(message.contains(&format!("to: {AGY}")), "{message}");
    assert!(message.contains("reason: usage limit"), "{message}");

    let tagged: Vec<(u32, String)> = bench
        .task_events()
        .into_iter()
        .filter_map(|event| match event {
            TaskEvent::CheckpointTagged { n, commit, .. } => Some((n, commit)),
            _ => None,
        })
        .collect();
    assert_eq!(tagged, vec![(1, first), (2, second)]);
    let listed = fallback::tags::list(&bench.host, &c1).await?;
    assert_eq!(listed.len(), 2);
    Ok(())
}

#[tokio::test]
async fn commit_authors_recorded_in_git_notes() -> anyhow::Result<()> {
    let bench = Bench::new().await?;
    let c1 = task(1);
    let note = |sha: &str| {
        bench
            .repo
            .git(&["notes", "--ref=refs/notes/argonauts", "show", sha])
    };
    let by_opus = bench.repo.git(&["rev-parse", &branch(1)])?;
    // Your own commit, after taking the task over, gets no note.
    let yours = bench.work("yours.rs", None)?;
    bench.fallback(&c1, CODEX).await?;
    let by_codex = bench.work("codex.rs", Some(CODEX))?;
    bench.stop_again(&c1)?;
    bench.fallback(&c1, AGY).await?;

    assert_eq!(note(&by_opus)?, format!("author: {OPUS}"));
    assert_eq!(note(&by_codex)?, format!("author: {CODEX}"));
    assert!(note(&yours).is_err());
    // Commit messages stay clean.
    assert_eq!(
        bench.repo.git(&["log", "-1", "--format=%B", &by_codex])?,
        "codex.rs"
    );
    Ok(())
}

#[tokio::test]
async fn rewind_keeps_replaced_work() -> anyhow::Result<()> {
    let bench = Bench::new().await?;
    let c1 = task(1);
    bench.fallback(&c1, CODEX).await?;
    let checkpoint = bench.tag_target(1)?;
    let replaced = bench.work("codex.rs", Some(CODEX))?;
    bench.stop_again(&c1)?;

    // `/rewind` reaches the engine as a command over the control socket.
    assert!(rewind_command(&bench, &c1, 2).await?.is_err());
    rewind_command(&bench, &c1, 1)
        .await?
        .map_err(anyhow::Error::msg)?;

    assert_eq!(
        git_in(&bench.repo, &bench.tree(), &["rev-parse", "HEAD"])?,
        checkpoint
    );
    assert!(!bench.tree().join("codex.rs").exists());
    let kept_on = tag(&c1, 1).replaced_branch();
    assert_eq!(bench.repo.git(&["rev-parse", &kept_on])?, replaced);
    assert!(bench.task_events().iter().any(|event| matches!(
        event,
        TaskEvent::Rewound { to: 1, kept_on: branch } if *branch == kept_on
    )));
    // The debt was for work that is off the branch now, and the task is back in line.
    assert_eq!(bench.debt(&c1), None);
    assert_eq!(bench.state(&c1), Some(TaskState::Ready));

    // Work that merged is never rewound; a new change undoes it.
    for event in [
        MergeEvent::Merged {
            task: c1.clone(),
            sha: checkpoint.clone(),
        },
        MergeEvent::RunMerged {
            target: "main".to_owned(),
            sha: checkpoint,
            fast_forward: true,
        },
    ] {
        bench.host.record(None, None, RunEventKind::Merge(event))?;
    }
    let refused = rewind_command(&bench, &c1, 1).await?;
    assert!(refused.is_err_and(|why| why.contains("merged")));
    Ok(())
}

/// At the merge approval, `/rewind` takes a merged task back to its checkpoint on both
/// branches: the run's branch gets one commit undoing the work after the tag, the task stays
/// merged as it was there, and the merge question opens again on the new branch.
#[tokio::test]
async fn rewind_undoes_a_merged_task_on_the_run_branch() -> anyhow::Result<()> {
    let bench = Bench::new().await?;
    let c1 = task(1);
    bench.fallback(&c1, CODEX).await?;
    let checkpoint = bench.tag_target(1)?;
    let replaced = bench.work("codex.rs", Some(CODEX))?;
    let run_tree = merge(&bench).await?;
    let merged = git_in(&bench.repo, &run_tree, &["rev-parse", "HEAD"])?;
    let merge_approval = open_merge_approval(&bench)?;

    rewind_command(&bench, &c1, 1)
        .await?
        .map_err(anyhow::Error::msg)?;

    assert_eq!(bench.repo.git(&["rev-parse", &branch(1)])?, checkpoint);
    let kept_on = tag(&c1, 1).replaced_branch();
    assert_eq!(bench.repo.git(&["rev-parse", &kept_on])?, replaced);
    assert!(run_tree.join("store.rs").exists());
    assert!(!run_tree.join("codex.rs").exists());
    let undo = git_in(&bench.repo, &run_tree, &["rev-parse", "HEAD"])?;
    assert_eq!(
        git_in(&bench.repo, &run_tree, &["rev-parse", "HEAD^"])?,
        merged
    );
    let message = git_in(&bench.repo, &run_tree, &["log", "-1", "--format=%B", &undo])?;
    assert!(
        message.starts_with("Rewind task-001 to fallback-1") && message.contains("Argonauts-Code"),
        "{message}"
    );
    assert_eq!(bench.state(&c1), Some(TaskState::Merged));
    assert!(bench.sink.events().iter().any(|event| matches!(
        &event.kind,
        RunEventKind::Approval(ApprovalEvent::Withdrawn { id, .. }) if *id == merge_approval
    )));
    Ok(())
}

/// Undoing a merged task's work that later work on the run's branch built on would
/// conflict, so nothing changes on either branch. Nothing changes while another task merges
/// into the run's branch either.
#[tokio::test]
async fn a_rewind_of_a_merged_task_that_cant_apply_changes_nothing() -> anyhow::Result<()> {
    let mut setup = Setup::new(Settings::default());
    setup.more_tasks = 1;
    let bench = Bench::with(setup).await?;
    let c1 = task(1);
    bench.fallback(&c1, CODEX).await?;
    bench.work("codex.rs", Some(CODEX))?;
    let run_tree = merge(&bench).await?;
    commit_in(
        &bench.repo,
        &run_tree,
        "codex.rs",
        "built on\n",
        "Later work",
    )?;
    let heads = || -> anyhow::Result<[String; 2]> {
        Ok([
            bench.repo.git(&["rev-parse", &branch(1)])?,
            git_in(&bench.repo, &run_tree, &["rev-parse", "HEAD"])?,
        ])
    };
    let before = heads()?;

    let refused = rewind_command(&bench, &c1, 1).await?;
    assert!(
        refused.as_ref().is_err_and(|why| why.contains("conflicts")),
        "{refused:?}"
    );
    assert_eq!(heads()?, before);

    bench.move_through(&task(2), &[TaskState::Merging])?;
    let refused = rewind_command(&bench, &c1, 1).await?;
    assert!(
        refused
            .as_ref()
            .is_err_and(|why| why.contains("being merged")),
        "{refused:?}"
    );
    assert_eq!(heads()?, before);
    assert!(
        !bench
            .task_events()
            .iter()
            .any(|event| matches!(event, TaskEvent::Rewound { .. }))
    );
    Ok(())
}

/// Merges task-001's branch into the run branch as its merge does, and marks the task
/// merged. Returns the run's worktree.
async fn merge(bench: &Bench) -> anyhow::Result<PathBuf> {
    let run_tree = ensure_run_branch(&bench.host).await?;
    let merge = [
        "merge",
        "--no-ff",
        "--quiet",
        "-m",
        "Merge task-001",
        &branch(1),
    ];
    git_in(&bench.repo, &run_tree, &merge)?;
    let sha = git_in(&bench.repo, &run_tree, &["rev-parse", "HEAD"])?;
    bench.record(
        &task(1),
        RunEventKind::Merge(MergeEvent::Merged { task: task(1), sha }),
    )?;
    bench.move_through(&task(1), &[TaskState::Merged])?;
    Ok(run_tree)
}

/// A merge question waiting for you, as the merge step opens it.
fn open_merge_approval(bench: &Bench) -> anyhow::Result<ApprovalId> {
    let id = ApprovalId::numbered(800);
    let packet = MergePacket {
        target: Some("main".to_owned()),
        branch: format!("argonauts/{TEST_RUN}/merged"),
        rows: Vec::new(),
        suite_passed_on_latest_main: true,
        checks: Vec::new(),
        combined: DiffStat::default(),
        conductor: DiffStat::default(),
        diff_path: PathBuf::new(),
        tree_sha: String::new(),
        spent: Spend::default(),
        limit: None,
        partial: false,
        in_the_way: Vec::new(),
        edited: Vec::new(),
        refused: None,
        both_changed: Vec::new(),
        asks_which_branch: false,
        again: None,
    };
    bench.host.record(
        None,
        None,
        RunEventKind::Approval(ApprovalEvent::Opened(Box::new(ApprovalOpened {
            id: id.clone(),
            kind: ApprovalKind::Merge,
            title: "Merge this run into main?".to_owned(),
            task: None,
            packet: ApprovalPacket::Merge(packet),
            evidence_sha256: "m".repeat(64),
            mode: ApprovalMode::Supervised,
            decide_with: DecideWith::Rules,
            options: Vec::new(),
            free_text: FreeText::None,
            needs_you: true,
        }))),
    )?;
    Ok(id)
}

async fn rewind_command(
    bench: &Bench,
    task: &TaskId,
    checkpoint: u32,
) -> anyhow::Result<Result<(), String>> {
    let (reply, receiver) = tokio::sync::oneshot::channel();
    let command = EngineCommand::Rewind {
        task: task.clone(),
        checkpoint,
        reply,
    };
    commands::handle(&bench.host, command).await?;
    Ok(receiver.await?)
}

#[tokio::test]
async fn rewind_refused_unless_the_task_is_at_rest() -> anyhow::Result<()> {
    let bench = Bench::new().await?;
    let c1 = task(1);
    bench.fallback(&c1, CODEX).await?;
    let head = bench.repo.git(&["rev-parse", &branch(1)])?;
    // Ready, then the checks and review: the branch is about to be worked on or judged.
    for path in [
        &[][..],
        &[TaskState::Running, TaskState::Checking][..],
        &[TaskState::InReview][..],
        &[TaskState::Merging][..],
    ] {
        bench.move_through(&c1, path)?;
        let refused = rewind::rewind(&bench.host, &c1, 1).await;
        assert!(refused.is_err(), "{:?}", bench.state(&c1));
    }
    assert_eq!(bench.repo.git(&["rev-parse", &branch(1)])?, head);
    Ok(())
}

/// Hands task-001 to Codex (with debt), and sets the return rule.
async fn fell_back_to_codex(on_return: Option<OnReturn>) -> anyhow::Result<Bench> {
    let mut settings = Settings {
        fallback: FallbackSettings::built_in(),
        ..Settings::default()
    };
    settings.fallback.on_return = on_return.or(settings.fallback.on_return);
    let bench = Bench::with(Setup::new(settings)).await?;
    bench.fallback(&task(1), CODEX).await?;
    bench.work("codex.rs", Some(CODEX))?;
    ensure!(bench.debt(&task(1)).is_some(), "the fallback left no debt");
    Ok(bench)
}

/// Without debt the task stays with its fallback when the original is back. Plan usage that
/// ran out counts until its reset passes; then the notice names the agent the way the screen
/// does and says how to move the task back.
#[tokio::test]
async fn original_back_after_its_plan_usage_reset_leaves_a_notice() -> anyhow::Result<()> {
    let mut settings = Settings {
        fallback: FallbackSettings::built_in(),
        ..Settings::default()
    };
    settings.fallback.unknown_tier = Some(UnknownTier::Equal);
    let bench = Bench::with(Setup::new(settings)).await?;
    let c1 = task(1);
    bench.fallback(&c1, CODEX).await?;
    ensure!(
        bench.debt(&c1).is_none(),
        "a fallback to an equal left debt"
    );
    let claude = worker(OPUS).agent_id();
    let rejected = |resets_at| PlanUsageReading {
        agent_id: claude.clone(),
        status: PlanUsageStatus::Rejected,
        used: Some(1.0),
        resets_at: Some(resets_at),
        window: Some("five_hour".to_owned()),
        source: PlanUsageSource::RateLimitEvent,
        observed_at: bench.host.now(),
        detail: None,
        wrap_up: false,
    };
    let notes = || -> Vec<String> {
        bench
            .sink
            .events()
            .into_iter()
            .filter_map(|event| match event.kind {
                RunEventKind::Note(note) if note.text.contains("is back") => Some(note.text),
                _ => None,
            })
            .collect()
    };

    bench
        .services
        .plan_usage
        .observe(rejected(bench.host.now() + time::Duration::hours(1)));
    on_return::agent_back(&bench.host, &claude).await?;
    assert!(notes().is_empty(), "{:?}", notes());

    bench
        .services
        .plan_usage
        .observe(rejected(bench.host.now() - time::Duration::seconds(1)));
    on_return::agent_back(&bench.host, &claude).await?;
    assert_eq!(
        notes(),
        [format!(
            "Claude Code is back · task-001 stays with {CODEX} · /reassign task-001 {OPUS} moves it back"
        )]
    );
    assert_eq!(bench.worker(&c1), Some(worker(CODEX)));
    Ok(())
}

/// A task that fell back to another model of its own CLI is offered back once the model it
/// was planned for is back, though the CLI never stopped. While that model is still out or
/// under test, nothing is said, and the CLI's own return doesn't offer it either.
#[tokio::test]
async fn a_model_back_on_its_own_offers_the_task_that_stayed_on_its_cli() -> anyhow::Result<()> {
    use argonauts_core::limits::{ModelState, Trouble};

    const SONNET: &str = "claude-code/sonnet";
    let mut settings = Settings {
        fallback: FallbackSettings::built_in(),
        ..Settings::default()
    };
    settings.fallback.unknown_tier = Some(UnknownTier::Equal);
    let bench = Bench::with(Setup::new(settings)).await?;
    let c1 = task(1);
    bench.fallback(&c1, SONNET).await?;
    let opus = worker(OPUS);
    let notes = || -> Vec<String> {
        let events = bench.sink.events().into_iter();
        let said = events.filter_map(|event| match event.kind {
            RunEventKind::Note(note) if note.text.contains("is back") => Some(note.text),
            _ => None,
        });
        said.collect()
    };

    let health = &bench.services.health;
    health.set_model(&opus, ModelState::Out, Some(Trouble::Overload));
    on_return::model_back(&bench.host, &opus).await?;
    on_return::agent_back(&bench.host, &opus.agent_id()).await?;
    health.set_model(&opus, ModelState::Testing, None);
    on_return::model_back(&bench.host, &opus).await?;
    assert!(notes().is_empty(), "{:?}", notes());

    health.set_model(&opus, ModelState::Healthy, None);
    // Another model's return says nothing of this task.
    on_return::model_back(&bench.host, &worker(SONNET)).await?;
    assert!(notes().is_empty(), "{:?}", notes());
    on_return::model_back(&bench.host, &opus).await?;
    assert_eq!(
        notes(),
        [format!(
            "Opus on Claude Code is back · task-001 stays with {SONNET} · /reassign task-001 {OPUS} moves it back"
        )]
    );
    assert_eq!(bench.worker(&c1), Some(worker(SONNET)));
    Ok(())
}

#[tokio::test]
async fn on_return_ask() -> anyhow::Result<()> {
    let bench = fell_back_to_codex(Some(OnReturn::Ask)).await?;
    let c1 = task(1);
    on_return::agent_back(&bench.host, &worker(OPUS).agent_id()).await?;
    let opened = bench.opened(ApprovalKind::OriginalReturned).await?;
    let ApprovalPacket::OriginalReturned(packet) = &opened.packet else {
        bail!("not an original-returned packet");
    };
    assert_eq!((packet.since, packet.commits), (1, 1));
    assert_eq!(packet.fallback, worker(CODEX));
    let keys: Vec<char> = opened.options.iter().map(|option| option.key).collect();
    assert_eq!(keys, ['r', 'o', 'u', 'y', 't', 'd']);

    bench.answer(&opened, AnswerAction::GiveBack)?;
    // The task isn't at rest, so its pipeline applies the choice at its next safe point.
    let mut changed = false;
    for _ in 0..200 {
        let applied = on_return::apply_pending(&bench.host, &c1).await?;
        changed = matches!(applied, Applied::NewWorker { .. });
        if changed {
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    assert!(changed);
    assert_eq!(bench.worker(&c1), Some(worker(OPUS)));
    Ok(())
}

#[tokio::test]
async fn on_return_keep() -> anyhow::Result<()> {
    let bench = fell_back_to_codex(Some(OnReturn::Keep)).await?;
    let c1 = task(1);
    on_return::agent_back(&bench.host, &worker(OPUS).agent_id()).await?;
    assert_eq!(
        on_return::apply_pending(&bench.host, &c1).await?,
        Applied::Nothing
    );
    assert_eq!(bench.worker(&c1), Some(worker(CODEX)));
    assert!(
        bench.debt(&c1).is_some(),
        "the debt stays for the merge rule"
    );
    Ok(())
}

#[tokio::test]
async fn on_return_review() -> anyhow::Result<()> {
    let bench = fell_back_to_codex(Some(OnReturn::Review)).await?;
    bench
        .services
        .workers
        .add(FakeRunner::cli(OPUS, "anthropic").then(approval()));
    let c1 = task(1);
    // Before the checks pass, the original has nothing finished to review.
    bench.move_through(&c1, &[TaskState::Running, TaskState::Checking])?;
    assert_eq!(
        on_return::apply_pending(&bench.host, &c1).await?,
        Applied::Nothing
    );
    assert!(bench.reviewed_by(&c1).is_empty());

    // The original reviews once, though the ring's reviewers are other providers and Codex owns
    // the task; its approval clears the debt, and the task's own review follows.
    bench.move_through(&c1, &[TaskState::InReview])?;
    assert_eq!(
        on_return::apply_pending(&bench.host, &c1).await?,
        Applied::Nothing
    );
    assert_eq!(bench.worker(&c1), Some(worker(CODEX)));
    assert_eq!(bench.debt(&c1), None);
    assert!(bench.task_events().iter().any(|event| matches!(
        event,
        TaskEvent::DebtCleared { by } if *by == worker(OPUS)
    )));
    assert_eq!(bench.state(&c1), Some(TaskState::InReview));
    assert_eq!(
        on_return::apply_pending(&bench.host, &c1).await?,
        Applied::Nothing
    );
    assert_eq!(bench.reviewed_by(&c1), vec![worker(OPUS)]);
    Ok(())
}

#[tokio::test]
async fn on_return_review_sends_changes_back() -> anyhow::Result<()> {
    let bench = fell_back_to_codex(Some(OnReturn::Review)).await?;
    bench.services.workers.add(
        FakeRunner::cli(OPUS, "anthropic")
            .then(verdict("request_changes", "Writes aren't retried.")),
    );
    let c1 = task(1);
    bench.move_through(
        &c1,
        &[TaskState::Running, TaskState::Checking, TaskState::InReview],
    )?;
    let applied = on_return::apply_pending(&bench.host, &c1).await?;
    let Applied::Changes { comments, .. } = applied else {
        bail!("the original's changes weren't sent back: {applied:?}");
    };
    assert!(comments.iter().any(|line| line.contains("Handle errors")));
    assert_eq!(bench.state(&c1), Some(TaskState::Running));
    assert_eq!(bench.worker(&c1), Some(worker(CODEX)));
    // Its review happened, so the debt is paid, and it isn't asked again.
    assert_eq!(bench.debt(&c1), None);
    assert_eq!(
        on_return::apply_pending(&bench.host, &c1).await?,
        Applied::Nothing
    );
    Ok(())
}

#[tokio::test]
async fn on_return_give_back() -> anyhow::Result<()> {
    let bench = fell_back_to_codex(Some(OnReturn::GiveBack)).await?;
    let c1 = task(1);
    // At rest, so the choice applies as soon as the agent is back.
    bench.stop_again(&c1)?;
    on_return::agent_back(&bench.host, &worker(OPUS).agent_id()).await?;
    assert_eq!(bench.worker(&c1), Some(worker(OPUS)));
    assert!(bench.task_events().iter().any(|event| matches!(
        event,
        TaskEvent::Reassigned { from: Some(from), to, .. }
            if *from == worker(CODEX) && *to == worker(OPUS)
    )));
    // It's told what changed since the checkpoint.
    assert!(
        bench
            .fallback_events()
            .iter()
            .any(|event| matches!(event, FallbackEvent::Note { .. }))
    );
    Ok(())
}

#[tokio::test]
async fn on_return_redo() -> anyhow::Result<()> {
    let bench = fell_back_to_codex(Some(OnReturn::Redo)).await?;
    let c1 = task(1);
    let checkpoint = bench.tag_target(1)?;
    let replaced = bench.repo.git(&["rev-parse", &branch(1)])?;
    let applied = on_return::apply_pending(&bench.host, &c1).await?;
    assert!(matches!(applied, Applied::NewWorker { .. }), "{applied:?}");

    assert_eq!(bench.worker(&c1), Some(worker(OPUS)));
    assert_eq!(
        git_in(&bench.repo, &bench.tree(), &["rev-parse", "HEAD"])?,
        checkpoint
    );
    assert_eq!(
        bench
            .repo
            .git(&["rev-parse", &tag(&c1, 1).replaced_branch()])?,
        replaced
    );
    assert_eq!(bench.debt(&c1), None);
    Ok(())
}

#[tokio::test]
async fn on_return_redo_while_checking() -> anyhow::Result<()> {
    let bench = fell_back_to_codex(Some(OnReturn::Redo)).await?;
    let c1 = task(1);
    let checkpoint = bench.tag_target(1)?;
    // The original came back while Codex's session ran; the choice applies once it ended.
    bench.move_through(&c1, &[TaskState::Running, TaskState::Checking])?;
    let applied = on_return::apply_pending(&bench.host, &c1).await?;
    assert!(matches!(applied, Applied::NewWorker { .. }), "{applied:?}");

    // The checks never run on the checkpoint's half-done work: the original redoes it.
    assert_eq!(bench.state(&c1), Some(TaskState::Running));
    assert_eq!(bench.worker(&c1), Some(worker(OPUS)));
    assert_eq!(
        git_in(&bench.repo, &bench.tree(), &["rev-parse", "HEAD"])?,
        checkpoint
    );
    assert_eq!(bench.debt(&c1), None);
    Ok(())
}

#[tokio::test]
async fn on_return_give_back_while_in_review() -> anyhow::Result<()> {
    let bench = fell_back_to_codex(Some(OnReturn::GiveBack)).await?;
    let c1 = task(1);
    bench.move_through(
        &c1,
        &[TaskState::Running, TaskState::Checking, TaskState::InReview],
    )?;
    let applied = on_return::apply_pending(&bench.host, &c1).await?;
    assert!(matches!(applied, Applied::NewWorker { .. }), "{applied:?}");
    assert_eq!(bench.state(&c1), Some(TaskState::Running));
    assert_eq!(bench.worker(&c1), Some(worker(OPUS)));
    Ok(())
}

/// Workers stay on their CLI, Claude Code is down, and Codex could take their tasks. `more`
/// further tasks are planned for Claude Code.
async fn kept_on_a_cli_that_is_down(more: u32) -> anyhow::Result<Bench> {
    let mut settings = Settings::default();
    settings.fallback.same_cli = Some(true);
    let mut setup = Setup::new(settings);
    setup.more_tasks = more;
    let bench = Bench::with(setup).await?;
    bench.services.health.set(
        &worker(OPUS).agent_id(),
        HealthState::Down,
        Some("two of its models are out"),
    );
    bench
        .services
        .fallback
        .set_candidates(vec![candidate(CODEX)]);
    Ok(bench)
}

/// The task starts to wait for Claude Code, kept off Codex, as recovery has it wait.
async fn waits_for_its_cli(host: &Host, task: &TaskId) -> anyhow::Result<()> {
    let action = RecoveryAction::Wait {
        agent_id: worker(OPUS).agent_id(),
        until: Some(datetime!(2026-09-30 14:05 UTC)),
        kept_off: Some(worker(CODEX).agent_id()),
    };
    fallback::apply(host, task, action).await?;
    Ok(())
}

/// The open question about a CLI that is down, once it lists `paused` tasks.
async fn down_question(host: &Host, paused: usize) -> anyhow::Result<ApprovalOpened> {
    for _ in 0..200 {
        let found = host.read(|state| {
            state
                .approvals
                .open()
                .map(|record| record.opened.clone())
                .find(|opened| {
                    matches!(
                        &opened.packet,
                        ApprovalPacket::AgentDown(down) if down.paused.len() == paused
                    )
                })
        });
        if let Some(opened) = found {
            return Ok(opened);
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    bail!("no question about a CLI that is down lists {paused} tasks")
}

async fn eventually(what: &str, mut holds: impl FnMut() -> bool) -> anyhow::Result<()> {
    for _ in 0..200 {
        if holds() {
            return Ok(());
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    bail!("never happened: {what}")
}

fn answer_on(host: &Host, opened: &ApprovalOpened, action: AnswerAction) -> anyhow::Result<()> {
    let answer = ApprovalAnswer {
        id: opened.id.clone(),
        action,
        text: None,
        evidence_sha256: opened.evidence_sha256.clone(),
        tasks: Vec::new(),
    };
    approvals::answer(host, answer).map_err(anyhow::Error::msg)
}

fn open_questions(host: &Host, kind: ApprovalKind) -> usize {
    host.read(|state| {
        state
            .approvals
            .open()
            .filter(|record| record.opened.kind == kind)
            .count()
    })
}

/// With `same_cli` and its CLI down, a task waits and one question asks about every task that
/// does: it names the setting, offers waiting first, and knows the agent that fits them all.
/// One agent for every paused task moves them all to it.
#[tokio::test]
async fn a_cli_that_is_down_asks_once_about_every_task_kept_on_it() -> anyhow::Result<()> {
    let bench = kept_on_a_cli_that_is_down(1).await?;
    let (first, second) = (task(1), task(2));
    waits_for_its_cli(&bench.host, &first).await?;
    let alone = down_question(&bench.host, 1).await?;
    assert_eq!(alone.task, None, "it asks about the CLI, not one task");
    assert_eq!(alone.title, "Claude Code is down · 1 task paused");

    waits_for_its_cli(&bench.host, &second).await?;
    let both = down_question(&bench.host, 2).await?;
    assert_eq!(open_questions(&bench.host, ApprovalKind::AgentDown), 1);
    assert_eq!(both.title, "Claude Code is down · 2 tasks paused");
    let ApprovalPacket::AgentDown(down) = &both.packet else {
        bail!("{:?}", both.packet);
    };
    assert_eq!(down.kept_by.as_deref(), Some("same_cli"));
    assert_eq!(down.fits_all, Some(worker(CODEX)));
    assert_eq!(down.evidence, "two of its models are out");
    let offered: Vec<&str> = both.options.iter().map(|o| o.label.as_str()).collect();
    assert_eq!(
        offered,
        [
            "Wait for Claude Code",
            "One agent for every paused task",
            "Choose per task"
        ]
    );

    bench.answer(&both, AnswerAction::OneForAll)?;
    eventually("both tasks moved to Codex", || {
        [&first, &second]
            .iter()
            .all(|task| bench.worker(task) == Some(worker(CODEX)))
    })
    .await?;
    assert_eq!(bench.state(&first), Some(TaskState::Ready));
    assert_eq!(bench.state(&second), Some(TaskState::Ready));
    Ok(())
}

/// The question about a CLI that is down takes a note. Whatever you answer, the note waits
/// for whoever works each paused task next, the agent the tasks moved to or the one they
/// wait for, and reaches it once. A reviewer of the task is not told, and an answer read
/// again from the log passes nothing on again.
#[tokio::test]
async fn a_note_with_the_answer_reaches_each_paused_tasks_worker_once() -> anyhow::Result<()> {
    const NOTE: &str = "the staging key is in the vault, not in .env";
    for (action, works_next) in [(AnswerAction::OneForAll, CODEX), (AnswerAction::Wait, OPUS)] {
        let bench = kept_on_a_cli_that_is_down(1).await?;
        let (first, second) = (task(1), task(2));
        waits_for_its_cli(&bench.host, &first).await?;
        waits_for_its_cli(&bench.host, &second).await?;
        let both = down_question(&bench.host, 2).await?;
        assert_eq!(both.free_text, FreeText::Guidance);

        let answer = ApprovalAnswer {
            id: both.id.clone(),
            action: action.clone(),
            text: Some(format!("  {NOTE}\n")),
            evidence_sha256: both.evidence_sha256.clone(),
            tasks: Vec::new(),
        };
        approvals::answer(&bench.host, answer).map_err(anyhow::Error::msg)?;
        if action == AnswerAction::OneForAll {
            eventually("both tasks moved to Codex", || {
                [&first, &second]
                    .iter()
                    .all(|task| bench.worker(task) == Some(worker(CODEX)))
            })
            .await?;
        }
        // What the next session of `role` on `task` finds waiting for it.
        let waits_for = |task: &TaskId, role: RoleName| {
            let who = Addressee {
                session: SessionKey::numbered(90),
                worker: worker(works_next),
                role,
                step: None,
                task: Some(task.clone()),
            };
            bench.host.read(|state| {
                let waiting = state.blackboard.pending_for(&who);
                let said = |message: &&BlackboardMessage| {
                    (message.kind, message.from.clone(), message.text.clone())
                };
                waiting.iter().map(said).collect::<Vec<_>>()
            })
        };
        let yours = (BlackboardKind::Mention, Party::User, NOTE.to_owned());
        for task in [&first, &second] {
            assert_eq!(
                waits_for(task, RoleName::Worker),
                std::slice::from_ref(&yours),
                "{action:?}"
            );
            assert_eq!(waits_for(task, RoleName::Reviewer), [], "{action:?}");
        }

        // The run wakes and reads the answer back from its log.
        fallback::down::ask_again(&bench.host)?;
        tokio::time::sleep(Duration::from_millis(100)).await;
        let posted = bench.host.read(|state| state.blackboard.messages.len());
        assert_eq!(posted, 2, "{action:?}: one for each task, and no more");
    }
    Ok(())
}

/// With an answer and no note, or a note of blanks, nobody is told anything.
#[tokio::test]
async fn an_answer_with_no_note_tells_the_paused_tasks_nothing() -> anyhow::Result<()> {
    for text in [None, Some("  \n".to_owned())] {
        let bench = kept_on_a_cli_that_is_down(0).await?;
        waits_for_its_cli(&bench.host, &task(1)).await?;
        let asked = down_question(&bench.host, 1).await?;
        let answer = ApprovalAnswer {
            id: asked.id.clone(),
            action: AnswerAction::Wait,
            text,
            evidence_sha256: asked.evidence_sha256.clone(),
            tasks: Vec::new(),
        };
        approvals::answer(&bench.host, answer).map_err(anyhow::Error::msg)?;
        assert_eq!(bench.host.read(|state| state.blackboard.messages.len()), 0);
    }
    Ok(())
}

/// A build task waits for Claude Code, and so does the review of another task, which stopped
/// the run. The question counts both, the review as a reviewer's, and knows the one agent
/// that may take both: Codex could build, but only Antigravity CLI may review. One agent for
/// every paused task hands it the build task and lets the review go to it too.
#[tokio::test]
async fn a_review_that_waits_for_its_cli_is_one_of_the_paused_tasks() -> anyhow::Result<()> {
    let bench = kept_on_a_cli_that_is_down(1).await?;
    let (built, reviewed) = (task(1), task(2));
    bench
        .services
        .fallback
        .set_candidates(vec![candidate(CODEX), candidate(AGY)]);
    waits_for_its_cli(&bench.host, &built).await?;
    let review = JobWait {
        job: JobRef {
            step: None,
            round: None,
            role: RoleName::Reviewer,
            does: Some(StepName::Review),
            task: Some(reviewed.clone()),
        },
        stopped: worker(OPUS),
        elsewhere: vec![worker(AGY)],
        title: "Tests for due dates".to_owned(),
    };
    fallback::down::job_waits(&bench.host, &review)?;

    let both = down_question(&bench.host, 2).await?;
    assert_eq!(both.title, "Claude Code is down · 2 tasks paused");
    let ApprovalPacket::AgentDown(down) = &both.packet else {
        bail!("{:?}", both.packet);
    };
    let listed: Vec<_> = down.paused.iter().map(|p| (&p.task, p.role)).collect();
    assert_eq!(
        listed,
        [(&built, RoleName::Worker), (&reviewed, RoleName::Reviewer)]
    );
    assert_eq!(down.paused[1].title, "Tests for due dates");
    assert_eq!(down.fits_all, Some(worker(AGY)));

    // The same review stopping the run again is the same wait, and the same question.
    fallback::down::job_waits(&bench.host, &review)?;
    assert_eq!(
        bench.host.read(|state| state.run.waiting_jobs.clone()),
        std::slice::from_ref(&review)
    );
    assert_eq!(down_question(&bench.host, 2).await?.id, both.id);

    bench.answer(&both, AnswerAction::OneForAll)?;
    let freed = bench.host.read(|state| {
        let to = state
            .run
            .freed(&review.job)
            .map(Option::<&ModelRef>::cloned);
        (to, state.run.waiting_jobs.len())
    });
    assert_eq!(
        freed,
        (Some(Some(worker(AGY))), 0),
        "the review is free as soon as you answered"
    );
    eventually("the build task moved to Antigravity CLI", || {
        bench.worker(&built) == Some(worker(AGY))
    })
    .await?;
    assert_eq!(
        bench.worker(&reviewed),
        Some(worker(OPUS)),
        "its worker stays"
    );
    Ok(())
}

/// A job with no task of its own, such as a step of the main agent's, is listed under where
/// its step sits. Choosing per task lets it leave its CLI for whoever its role's fallbacks
/// come to, and waiting leaves it where it is.
#[tokio::test]
async fn a_step_of_the_main_agents_that_waits_is_listed_by_its_place() -> anyhow::Result<()> {
    let step = JobWait {
        job: JobRef {
            step: Some(StepRef {
                workflow: WorkflowId::numbered(1),
                phase: 2,
                path: vec![0, 1],
            }),
            round: Some(1),
            role: RoleName::Orchestrator,
            does: None,
            task: None,
        },
        stopped: worker(OPUS),
        elsewhere: vec![worker(CODEX)],
        title: "The orchestrator".to_owned(),
    };
    for (answer, freed) in [
        (AnswerAction::Wait, None),
        (AnswerAction::PerTask, Some(None)),
    ] {
        let bench = kept_on_a_cli_that_is_down(0).await?;
        fallback::down::job_waits(&bench.host, &step)?;
        let asked = down_question(&bench.host, 1).await?;
        let ApprovalPacket::AgentDown(down) = &asked.packet else {
            bail!("{:?}", asked.packet);
        };
        assert_eq!(down.paused[0].task.as_str(), "phase-3-step-1.2");
        assert_eq!(down.paused[0].role, RoleName::Orchestrator);
        assert_eq!(down.fits_all, Some(worker(CODEX)));

        bench.answer(&asked, answer.clone())?;
        let now = bench
            .host
            .read(|state| state.run.freed(&step.job).map(Option::<&ModelRef>::cloned));
        assert_eq!(now, freed, "{answer:?}");
    }
    Ok(())
}

/// Waiting is what the setting already does, so the question never holds the run: it waits
/// for the CLI. Answered with a wait, nothing asks again for the same try, and the tasks go
/// back in line when the CLI is tried.
#[tokio::test]
async fn waiting_for_a_cli_that_is_down_is_what_same_cli_already_does() -> anyhow::Result<()> {
    use argonauts_core::events::run::WaitReason;
    use argonauts_crew::engine::wait;

    let bench = kept_on_a_cli_that_is_down(0).await?;
    let (first, claude) = (task(1), worker(OPUS).agent_id());
    waits_for_its_cli(&bench.host, &first).await?;
    let asked = down_question(&bench.host, 1).await?;
    let waits_for = bench.host.read(|state| wait::reason(state, 0, None));
    assert!(
        matches!(&waits_for, Some(WaitReason::WaitingForAgent { agent_id, .. }) if *agent_id == claude),
        "{waits_for:?}"
    );

    bench.answer(&asked, AnswerAction::Wait)?;
    fallback::down::ask_again(&bench.host)?;
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(open_questions(&bench.host, ApprovalKind::AgentDown), 0);
    assert_eq!(bench.state(&first), Some(TaskState::NeedsDecision));
    assert_eq!(bench.worker(&first), Some(worker(OPUS)));

    bench
        .services
        .health
        .set(&claude, HealthState::Testing, None);
    fallback::ask::agent_back(&bench.host, &claude)?;
    assert_eq!(bench.state(&first), Some(TaskState::Ready));
    assert_eq!(bench.worker(&first), Some(worker(OPUS)));
    Ok(())
}

/// The CLI is tried again before anyone answered: the question is withdrawn and the task
/// goes on with its own worker.
#[tokio::test]
async fn the_question_about_a_cli_that_is_down_goes_when_the_cli_is_tried() -> anyhow::Result<()> {
    let bench = kept_on_a_cli_that_is_down(0).await?;
    let (first, claude) = (task(1), worker(OPUS).agent_id());
    waits_for_its_cli(&bench.host, &first).await?;
    let asked = down_question(&bench.host, 1).await?;

    bench
        .services
        .health
        .set(&claude, HealthState::Testing, None);
    fallback::ask::agent_back(&bench.host, &claude)?;
    let withdrawn = bench.host.read(|state| {
        state
            .approvals
            .by_id
            .get(&asked.id)
            .and_then(|record| record.withdrawn.clone())
    });
    assert_eq!(withdrawn.as_deref(), Some("Claude Code is tried again"));
    assert_eq!(bench.state(&first), Some(TaskState::Ready));
    Ok(())
}

/// A second CLI that goes down while the first is asked about leaves that question alone.
/// Its task waits like the others, and is asked about once the first question is settled.
#[tokio::test]
async fn a_second_cli_that_is_down_waits_its_turn_to_ask() -> anyhow::Result<()> {
    let bench = kept_on_a_cli_that_is_down(1).await?;
    let (first, second, codex) = (task(1), task(2), worker(CODEX).agent_id());
    waits_for_its_cli(&bench.host, &first).await?;
    let about_claude = down_question(&bench.host, 1).await?;

    bench.record(
        &second,
        RunEventKind::Task(TaskEvent::Reassigned {
            from: Some(worker(OPUS)),
            to: worker(CODEX),
            by: Party::Rules,
            rule_override: None,
        }),
    )?;
    bench
        .services
        .health
        .set(&codex, HealthState::Down, Some("it can't be reached"));
    bench.services.fallback.set_candidates(vec![candidate(AGY)]);
    let action = RecoveryAction::Wait {
        agent_id: codex.clone(),
        until: Some(datetime!(2026-09-30 14:05 UTC)),
        kept_off: Some(worker(AGY).agent_id()),
    };
    fallback::apply(&bench.host, &second, action).await?;
    tokio::time::sleep(Duration::from_millis(100)).await;
    let open = bench.opened(ApprovalKind::AgentDown).await?;
    assert_eq!(open.id, about_claude.id, "the first question stays");
    assert_eq!(open_questions(&bench.host, ApprovalKind::AgentDown), 1);

    bench.answer(&about_claude, AnswerAction::Wait)?;
    let about_codex = down_question(&bench.host, 1).await?;
    assert_eq!(about_codex.title, "Codex is down · 1 task paused");
    Ok(())
}

/// A run that only waits has no engine, so the one that wakes for your answer acts on it:
/// choosing per task opens each task's own question, with agents from every CLI, and the
/// one you pick takes that task.
#[tokio::test]
async fn the_engine_that_wakes_acts_on_the_answer_about_a_cli_that_is_down() -> anyhow::Result<()> {
    let bench = kept_on_a_cli_that_is_down(1).await?;
    let (first, second) = (task(1), task(2));
    waits_for_its_cli(&bench.host, &first).await?;
    waits_for_its_cli(&bench.host, &second).await?;
    let both = down_question(&bench.host, 2).await?;

    let history = bench.sink.events();
    let settings = bench.host.settings().clone();
    let (context, _sink, _commands) = engine_context_in(
        &bench.services,
        settings,
        bench.layout.clone(),
        history.clone(),
    );
    let woke = Host::new(Arc::new(Shared::new(&context, RunState::fold(&history))?));
    answer_on(&woke, &both, AnswerAction::PerTask)?;
    fallback::down::ask_again(&woke)?;
    eventually("each task has its own question", || {
        open_questions(&woke, ApprovalKind::Fallback) == 2
    })
    .await?;
    assert_eq!(open_questions(&woke, ApprovalKind::AgentDown), 0);

    let own = woke.read(|state| {
        state
            .approvals
            .open_for(ApprovalKind::Fallback, Some(&second))
            .map(|record| record.opened.clone())
    });
    let own = own.context("task-002 has no question of its own")?;
    let pick = AnswerAction::Candidate {
        worker: worker(CODEX),
    };
    answer_on(&woke, &own, pick)?;
    let worker_of = |task: &TaskId| {
        woke.read(|state| {
            state
                .tasks
                .get(task)
                .and_then(|record| record.worker.clone())
        })
    };
    eventually("task-002 moved to Codex", || {
        worker_of(&second) == Some(worker(CODEX))
    })
    .await?;
    assert_eq!(
        worker_of(&first),
        Some(worker(OPUS)),
        "the other still waits"
    );
    Ok(())
}

/// A task that falls back says so about its workflow as well, in the line the workflow's view
/// shows under its header: who stopped, from how the task's last session with that worker
/// ended, and who it fell back to. A worker with no session that ended for a failure of its
/// own gives no reason.
#[tokio::test]
async fn a_tasks_fallback_is_said_for_its_workflows_view() -> anyhow::Result<()> {
    let bench = Bench::new().await?;
    let c1 = task(1);
    let name = |model: &str| display_name(&worker(model));
    bench.built_until(&c1, 1, OPUS, EndReason::UsageLimit { resets_at: None })?;
    bench.fallback(&c1, CODEX).await?;
    let after_a_limit = format!(
        "{}'s session ended early (its usage limit) · fell back to {}",
        name(OPUS),
        name(CODEX)
    );
    let about = Some(WorkflowId::numbered(1));
    assert_eq!(
        bench.workflow_notes(),
        [(about.clone(), after_a_limit.clone())]
    );
    assert!(
        before_in(&bench.sink.events(), is_fell_back, is_workflow_note),
        "the line follows the fallback it tells of"
    );

    // Codex stops with no session of its own on record: the line names nobody's failure.
    bench.stop_again(&c1)?;
    bench.fallback(&c1, AGY).await?;
    let with_no_reason = format!("{} stopped · fell back to {}", name(CODEX), name(AGY));
    assert_eq!(
        bench.workflow_notes(),
        [(about.clone(), after_a_limit), (about, with_no_reason)]
    );
    Ok(())
}

/// A task given back to its first worker says that about its workflow too, so the line under
/// the view's header no longer reads as if the fallback still held the task.
#[tokio::test]
async fn a_task_given_back_is_said_for_its_workflows_view() -> anyhow::Result<()> {
    let mut settings = Settings {
        fallback: FallbackSettings::built_in(),
        ..Settings::default()
    };
    settings.fallback.on_return = Some(OnReturn::GiveBack);
    let bench = Bench::with(Setup::new(settings)).await?;
    let c1 = task(1);
    bench.built_until(&c1, 1, OPUS, EndReason::UsageLimit { resets_at: None })?;
    bench.fallback(&c1, CODEX).await?;
    bench.stop_again(&c1)?;
    on_return::agent_back(&bench.host, &worker(OPUS).agent_id()).await?;
    assert_eq!(bench.worker(&c1), Some(worker(OPUS)));

    let said = bench.workflow_notes();
    let (about, line) = said.last().context("a line about the workflow")?;
    assert_eq!(about, &Some(WorkflowId::numbered(1)));
    let back = format!("{} is back · ", display_name(&worker(OPUS)));
    assert!(
        line.starts_with(&back) && line.ends_with(" carries on with it"),
        "{line}"
    );
    assert_eq!(
        said.len(),
        2,
        "one line for the fallback, one for the return"
    );
    Ok(())
}

fn is_fell_back(kind: &RunEventKind) -> bool {
    matches!(kind, RunEventKind::Fallback(FallbackEvent::FellBack { .. }))
}

fn is_workflow_note(kind: &RunEventKind) -> bool {
    matches!(kind, RunEventKind::Workflow(WorkflowEvent::Note(_)))
}

/// Whether the first event `earlier` holds for comes before the first `later` holds for.
fn before_in(
    events: &[RunEvent],
    earlier: fn(&RunEventKind) -> bool,
    later: fn(&RunEventKind) -> bool,
) -> bool {
    let at = |holds: fn(&RunEventKind) -> bool| events.iter().position(|event| holds(&event.kind));
    matches!((at(earlier), at(later)), (Some(first), Some(second)) if first < second)
}

/// A planner as `same_cli` leaves the real one: a fallback is chosen from the stopped CLI's
/// other model alone, and `elsewhere` joins it once every valid agent is asked for.
struct KeptOnItsCli {
    own: &'static str,
    elsewhere: Option<&'static str>,
}

impl FallbackPlanner for KeptOnItsCli {
    fn candidates(&self, query: &CandidateQuery) -> Vec<Candidate> {
        let elsewhere = self.elsewhere.filter(|_| query.all_valid);
        std::iter::once(self.own)
            .chain(elsewhere)
            .map(candidate)
            .collect()
    }

    fn note(&self, task: &TaskSpec, _events: &[RunEvent]) -> FallbackNote {
        note(&task.id)
    }
}

/// With `same_cli` a task's fallback stays on its CLI, and the line says the setting asked for
/// it only where another CLI could have taken the task, and never where you picked the worker.
#[tokio::test]
async fn a_tasks_line_names_same_cli_only_where_it_kept_another_cli_out() -> anyhow::Result<()> {
    const SONNET: &str = "claude-code/sonnet";
    let overloaded = || EndReason::CliError {
        category: argonauts_core::ErrorCategory::Overloaded,
        message: "Overloaded".to_owned(),
    };
    let stayed = format!(
        "{} is overloaded · fell back to {}",
        display_name(&worker(OPUS)),
        display_name(&worker(SONNET))
    );
    // Opus stopped on an overload, and Sonnet is the one fallback on its CLI.
    let on_its_cli = |elsewhere: Option<&'static str>| async move {
        let mut settings = Settings::default();
        settings.fallback.same_cli = Some(true);
        let mut setup = Setup::new(settings);
        setup.planner = Some(Arc::new(KeptOnItsCli {
            own: SONNET,
            elsewhere,
        }));
        let bench = Bench::with(setup).await?;
        bench.built_until(&task(1), 1, OPUS, overloaded())?;
        anyhow::Ok(bench)
    };
    let said = |bench: &Bench| {
        let notes = bench.workflow_notes();
        notes.last().map(|(_, text)| text.clone())
    };

    // Codex could take it, so the setting is why it stayed on Claude Code.
    let bench = on_its_cli(Some(CODEX)).await?;
    bench.fallback(&task(1), SONNET).await?;
    assert_eq!(bench.worker(&task(1)), Some(worker(SONNET)));
    assert_eq!(said(&bench), Some(format!("{stayed}, as same_cli asks")));

    // Nobody else could: it would have stayed anyway.
    let bench = on_its_cli(None).await?;
    bench.fallback(&task(1), SONNET).await?;
    assert_eq!(said(&bench), Some(stayed.clone()));

    // Your own pick on the task's question is yours, whatever the setting.
    let bench = on_its_cli(Some(CODEX)).await?;
    let (c1, sonnet) = (task(1), worker(SONNET));
    fallback::assign::fallback(&bench.host, &c1, &sonnet, &note(&c1), false, Party::User).await?;
    assert_eq!(said(&bench), Some(stayed));

    // Off, the task goes where its fallbacks say, and nothing is said of the setting.
    let bench = Bench::new().await?;
    bench.built_until(&task(1), 1, OPUS, overloaded())?;
    bench.fallback(&task(1), CODEX).await?;
    let elsewhere = format!(
        "{} is overloaded · fell back to {}",
        display_name(&worker(OPUS)),
        display_name(&worker(CODEX))
    );
    assert_eq!(said(&bench), Some(elsewhere));
    Ok(())
}
