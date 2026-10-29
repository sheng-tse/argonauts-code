//! The merge step: main merged in when it moved, the final check, the merge approval, landing
//! by your answer, and the merge rule for tasks with quality debt. Each test runs the step on a
//! real temp repo through the engine's `Host` and answers its approval as you would.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use anyhow::{Context, bail, ensure};
use serde_json::json;
use time::macros::datetime;
use tokio::sync::mpsc;

use argonauts_core::api::ApprovalAnswer;
use argonauts_core::engine::{EngineCommand, RepoContext};
use argonauts_core::events::agent_status::AgentStatusEvent;
use argonauts_core::events::approval::{
    AnswerAction, ApprovalDecided, ApprovalEvent, ApprovalKind, ApprovalOpened, ApprovalPacket,
    FreeText, PlanPacket,
};
use argonauts_core::events::goal::{Criterion, GoalEvent, GoalFile, GoalSource};
use argonauts_core::events::plan::{LimitPlan, TaskGraph};
use argonauts_core::events::run::{Party, RunLifecycle, RunMode, RunOutcome, RunStarted};
use argonauts_core::events::session::EndReason;
use argonauts_core::events::shared::RunReviewEvent;
use argonauts_core::events::task::{
    CheckKind, CheckOutcome, MergeEvent, TaskEvent, TaskKind, TaskLimit, TaskSpec, TaskState,
};
use argonauts_core::events::workflow::{StepStarted, WorkflowEvent, WorkflowStarted};
use argonauts_core::events::{RunEvent, RunEventKind};
use argonauts_core::fallback::QualityDebt;
use argonauts_core::limits::{AgentHealth, HealthState};
use argonauts_core::names::{ApprovalId, ModelRef, RoleName, WorkflowId};
use argonauts_core::policy::{HardLimit, PolicyOutcome, Verdict};
use argonauts_core::session_runner::Access;
use argonauts_core::settings::approvals::{ApprovalMode, DecideWith};
use argonauts_core::settings::asking::WhenDeclined;
use argonauts_core::settings::delivery::{DeliveryCommits, DeliveryWrite, UnbornLanding};
use argonauts_core::settings::fallback::{MergeRule, Retry, UnknownTier};
use argonauts_core::settings::limit::{LimitAmount, LimitUnit};
use argonauts_core::settings::protocol::CheckCommand;
use argonauts_core::settings::{Settings, Span};
use argonauts_core::steps::StepName;
use argonauts_core::steps::task::{MemoryProposal, ReportStatus, TaskReport};
use argonauts_core::workflow::{StandardPhase, Step, StepKind, StepRef, Workflow};
use argonauts_core::{RepoId, RunId, TaskId, unborn};
use argonauts_crew::approvals;
use argonauts_crew::engine::host::Host;
use argonauts_crew::error::EngineError;
use argonauts_crew::merge::in_the_way::{MERGE_LATER, MOVE_ASIDE, TRY_AGAIN};
use argonauts_crew::merge::step::merge_step;
use argonauts_crew::merge::{branch, final_check};
use argonauts_crew::run_review;
use argonauts_crew::state::RunState;
use argonauts_crew::workflow::{Flow, StepAt, StepCall, StepDone};
use argonauts_testkit::deadline;
use argonauts_testkit::repo::TempRepo;
use argonauts_testkit::runner::{FakeRunner, FakeSession};
use argonauts_testkit::services::{FakeServices, MemorySink, TEST_RUN, engine_context_in};

use crate::support::shared_in;

const OPUS: &str = "claude-code/opus";
const CODEX: &str = "codex/gpt-5.6";
const GEMINI: &str = "agy/gemini-3";
/// The task's ring reviewer, of another provider than its planned worker.
const AGY: &str = "agy/gemini-3.6";
/// The merger. Each test that wants its answers adds it, so the other agents' answers
/// go to the sessions they were written for.
const MERGER: &str = "opencode/merger";

fn worker(name: &str) -> ModelRef {
    name.parse().expect("a valid model")
}

/// A crew run whose one task is merged: the run branch holds its work, and main is
/// checked out in the repository.
struct Bench {
    services: FakeServices,
    repo: TempRepo,
    host: Host,
    sink: Arc<MemorySink>,
    run_tree: PathBuf,
    _commands: mpsc::Sender<EngineCommand>,
}

impl Bench {
    async fn new(debt: Option<QualityDebt>) -> anyhow::Result<Self> {
        Self::with(vec![spec(&TaskId::numbered(1))], debt, suite_settings()).await
    }

    /// `tasks` all merged, in order, the first one with `debt`.
    async fn with(
        tasks: Vec<TaskSpec>,
        debt: Option<QualityDebt>,
        settings: Settings,
    ) -> anyhow::Result<Self> {
        Self::with_work(tasks, debt, settings, &[]).await
    }

    /// As [`Bench::with`], with `files` beside the first task's work. The engine's own git
    /// commits them, so the leave-out settings decide what goes in.
    async fn with_work(
        tasks: Vec<TaskSpec>,
        debt: Option<QualityDebt>,
        settings: Settings,
        files: &[(&str, &str)],
    ) -> anyhow::Result<Self> {
        let repo = TempRepo::new()?;
        let base = repo.head()?;
        Self::in_repo(repo, &base, tasks, debt, settings, files).await
    }

    /// As [`Bench::with_work`], for a run that started from `base` in `repo`.
    async fn in_repo(
        repo: TempRepo,
        base: &str,
        tasks: Vec<TaskSpec>,
        debt: Option<QualityDebt>,
        settings: Settings,
        files: &[(&str, &str)],
    ) -> anyhow::Result<Self> {
        let services = FakeServices::new();
        let state_dir = repo.scratch().join("argonauts");
        let layout = RepoContext {
            id: RepoId::from_canonical_path(repo.root()),
            root: repo.root().to_path_buf(),
            run_dir: state_dir.join("state/runs").join(TEST_RUN),
            worktrees: state_dir.join("worktrees").join(TEST_RUN),
            base_worktree: state_dir.join("worktrees").join(TEST_RUN).join("base"),
        };
        let mut history = vec![
            (None, started(&repo, base)),
            (None, understood()?),
            (None, plan_opened(&tasks)),
            (None, plan_approved()),
            (None, workflow_started()?),
            (None, merge_step_started()),
        ];
        for task in &tasks {
            let id = Some(task.id.clone());
            history.push((
                id.clone(),
                RunEventKind::Task(TaskEvent::Created(Box::new(task.clone()))),
            ));
            history.push((
                id,
                RunEventKind::Task(TaskEvent::State {
                    from: None,
                    to: TaskState::Merged,
                    reason: "merged and tested".to_owned(),
                }),
            ));
        }
        if let (Some(debt), Some(task)) = (debt, tasks.first()) {
            let task = task.id.clone();
            history.push((
                Some(task.clone()),
                RunEventKind::Task(TaskEvent::Reassigned {
                    from: Some(worker(OPUS)),
                    to: debt.by.clone(),
                    by: Party::Rules,
                    rule_override: None,
                }),
            ));
            history.push((Some(task), RunEventKind::Task(TaskEvent::QualityDebt(debt))));
        }
        let history: Vec<RunEvent> = history
            .into_iter()
            .zip(0..)
            .map(|((task, kind), seq)| event(seq, task, kind))
            .collect();
        let (context, sink, commands) =
            engine_context_in(&services, settings, layout, history.clone());
        let shared = shared_in(&repo, &context, RunState::fold(&history))?;
        let host = Host::new(Arc::new(shared));

        // Each task's work, merged into the run branch as Argonauts Code does.
        let run_tree = branch::ensure_run_branch(&host).await?;
        for (i, task) in tasks.iter().enumerate() {
            let (file, text) = if i == 0 {
                ("store.rs".to_owned(), "fn store() {}\n".to_owned())
            } else {
                (format!("{}.rs", task.id), format!("// {}\n", task.id))
            };
            let message = format!("Merge {}", task.id);
            if i == 0 && !files.is_empty() {
                std::fs::write(run_tree.join(&file), &text)?;
                for (path, text) in files {
                    let path = run_tree.join(path);
                    if let Some(folder) = path.parent() {
                        std::fs::create_dir_all(folder)?;
                    }
                    std::fs::write(path, text)?;
                }
                host.git()
                    .commit_work(&run_tree, &message)
                    .await?
                    .sha
                    .context("nothing to commit")?;
            } else {
                commit_in(&repo, &run_tree, &file, &text, &message)?;
            }
            host.record(
                None,
                None,
                RunEventKind::Merge(MergeEvent::Merged {
                    task: task.id.clone(),
                    sha: git_in(&repo, &run_tree, &["rev-parse", "HEAD"])?,
                }),
            )?;
        }
        Ok(Self {
            services,
            repo,
            host,
            sink,
            run_tree,
            _commands: commands,
        })
    }

    fn run_head(&self) -> anyhow::Result<String> {
        git_in(&self.repo, &self.run_tree, &["rev-parse", "HEAD"])
    }

    /// `rev` holds the run as it lands by default: the run's files, in one commit of its own
    /// with one parent, and none of the run's own commits or merges.
    fn merged_into(&self, rev: &str) -> anyhow::Result<()> {
        let root = self.repo.root();
        let tree = |rev: &str| git_in(&self.repo, root, &["rev-parse", &format!("{rev}^{{tree}}")]);
        let head = self.run_head()?;
        anyhow::ensure!(
            tree(rev)? == tree(&head)?,
            "{rev} doesn't hold the run's files"
        );
        let with_parents = git_in(&self.repo, root, &["rev-list", "--parents", "-n", "1", rev])?;
        anyhow::ensure!(
            with_parents.split_whitespace().count() == 2,
            "{rev} isn't one commit on your branch: {with_parents}"
        );
        let merged = git_in(
            &self.repo,
            root,
            &["merge-base", "--is-ancestor", &head, rev],
        );
        anyhow::ensure!(merged.is_err(), "{rev} carries the run's own history");
        Ok(())
    }

    /// A commit on main in the repository's own checkout.
    fn move_main(&self, file: &str) -> anyhow::Result<String> {
        self.repo.write(file, "main moved\n")?;
        self.repo.commit_all(file)
    }

    fn merge_events(&self) -> Vec<MergeEvent> {
        self.sink
            .events()
            .into_iter()
            .filter_map(|event| match event.kind {
                RunEventKind::Merge(event) => Some(event),
                _ => None,
            })
            .collect()
    }

    /// Waits for a merge approval other than the ones in `seen`.
    async fn next_merge_approval(&self, seen: &[ApprovalId]) -> anyhow::Result<ApprovalOpened> {
        for _ in 0..400 {
            let found = self.host.read(|state| {
                state
                    .approvals
                    .open()
                    .find(|record| {
                        record.opened.kind == ApprovalKind::Merge
                            && !seen.contains(&record.opened.id)
                    })
                    .map(|record| record.opened.clone())
            });
            if let Some(opened) = found {
                return Ok(opened);
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        bail!("no merge approval opened")
    }

    /// Waits for an open "original is back" question.
    async fn return_approval(&self) -> anyhow::Result<ApprovalOpened> {
        for _ in 0..400 {
            let found = self.host.read(|state| {
                state
                    .approvals
                    .open_for(ApprovalKind::OriginalReturned, Some(&TaskId::numbered(1)))
                    .map(|record| record.opened.clone())
            });
            if let Some(opened) = found {
                return Ok(opened);
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        bail!("no \"original is back\" question opened")
    }

    /// The run saw `name`'s agent go down, and the board has it healthy again.
    fn out_and_back(&self, name: &str) -> anyhow::Result<()> {
        let health = AgentHealth {
            agent_id: worker(name).agent_id(),
            state: HealthState::Down,
            since: datetime!(2026-09-30 10:00 UTC),
            reason: Some("usage limit".to_owned()),
            next_test_at: None,
            disabled: None,
        };
        self.host.record(
            None,
            None,
            RunEventKind::AgentStatus(AgentStatusEvent::Health(health)),
        )?;
        Ok(())
    }

    fn answer(&self, opened: &ApprovalOpened, action: AnswerAction) -> anyhow::Result<()> {
        self.answer_with(opened, action, None, Vec::new())
    }

    fn answer_with(
        &self,
        opened: &ApprovalOpened,
        action: AnswerAction,
        text: Option<&str>,
        tasks: Vec<TaskId>,
    ) -> anyhow::Result<()> {
        approvals::answer(
            &self.host,
            ApprovalAnswer {
                id: opened.id.clone(),
                action,
                text: text.map(str::to_owned),
                evidence_sha256: opened.evidence_sha256.clone(),
                tasks,
            },
        )
        .map_err(anyhow::Error::msg)
    }

    /// Tasks moved out of `Merged`, in order.
    fn sent_back(&self) -> Vec<TaskId> {
        self.sink
            .events()
            .into_iter()
            .filter_map(|event| match (event.task_id, event.kind) {
                (
                    Some(task),
                    RunEventKind::Task(TaskEvent::State {
                        from: Some(TaskState::Merged),
                        ..
                    }),
                ) => Some(task),
                _ => None,
            })
            .collect()
    }

    fn phase_events(&self) -> usize {
        self.sink
            .events()
            .iter()
            .filter(|event| matches!(event.kind, RunEventKind::Phase(_)))
            .count()
    }

    fn final_checks(&self) -> usize {
        self.merge_events()
            .iter()
            .filter(|event| matches!(event, MergeEvent::FinalCheck { .. }))
            .count()
    }

    /// Runs the merge step while `answering` plays your part.
    async fn run(
        &self,
        answering: impl std::future::Future<Output = anyhow::Result<()>>,
    ) -> anyhow::Result<StepDone> {
        let call = step_call()?;
        let step = merge_step(&self.host, &call);
        let (done, answered) = tokio::join!(
            async { tokio::time::timeout(deadline::scaled(Duration::from_secs(20)), step).await },
            answering
        );
        // A step that failed explains an answer that never came.
        if let Ok(Err(err)) = &done {
            bail!("the merge step failed: {err}");
        }
        answered?;
        Ok(done.context("the merge step timed out")??)
    }
}

fn suite_settings() -> Settings {
    let mut settings = Settings::default();
    settings.checks.full_suite = vec![CheckCommand {
        name: "suite".to_owned(),
        command: vec!["true".to_owned()],
    }];
    settings.crew.merger = vec![worker(MERGER)];
    settings
}

/// A worker that writes `file` in its worktree and reports done.
fn fixing_worker(name: &str, provider: &str, file: &'static str) -> FakeRunner {
    FakeRunner::cli(name, provider).respond_with(move |spec| {
        let _ = std::fs::write(spec.workdir.join(file), "fixed\n");
        FakeSession::structured(json!({
            "status": "done", "summary": "made the change", "files_changed": [file],
            "tests_run": [], "concerns": [], "questions": [], "change_requests": [],
            "memory_proposals": []
        }))
    })
}

/// A reviewer that gives `verdicts` in turn, then approves.
fn reviewer(name: &str, provider: &str, verdicts: &'static [&'static str]) -> FakeRunner {
    let given = AtomicUsize::new(0);
    FakeRunner::cli(name, provider).respond_with(move |_| {
        let turn = given.fetch_add(1, Ordering::SeqCst);
        let verdict = verdicts.get(turn).copied().unwrap_or("approve");
        FakeSession::structured(json!({
            "verdict": verdict,
            "summary": "looked at it",
            "comments": [],
            "tamper_suspected": false,
        }))
    })
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
        mode: RunMode::Crew,
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
        starts_from: None,
    })))
}

/// Worker sessions of tasks sent back need the approved goal.
fn understood() -> anyhow::Result<RunEventKind> {
    let file = GoalFile {
        run: RunId::new(TEST_RUN)?,
        version: 1,
        summary: "Add a Redis store".to_owned(),
        criteria: vec![Criterion::new("G1", "It stores.")],
        in_scope: Vec::new(),
        out_of_scope: Vec::new(),
        assumptions: Vec::new(),
        answers: Vec::new(),
        source: GoalSource::Readings,
        path: PathBuf::from(".argonauts/goal/run-01.md"),
        sha256: "0".repeat(64),
    };
    Ok(RunEventKind::Goal(GoalEvent::Approved {
        file,
        by: Party::User,
    }))
}

fn plan_opened(tasks: &[TaskSpec]) -> RunEventKind {
    let plan = ApprovalOpened {
        id: ApprovalId::numbered(1),
        kind: ApprovalKind::Plan,
        title: "Approve this plan?".to_owned(),
        task: None,
        packet: ApprovalPacket::Plan(Box::new(PlanPacket {
            summary: "The plan".to_owned(),
            judging: None,
            near_tie: false,
            decisions: Vec::new(),
            graph: TaskGraph {
                tasks: tasks.to_vec(),
                critical_path: Vec::new(),
            },
            acceptance_tests: Vec::new(),
            limit: LimitPlan {
                lines: Vec::new(),
                total: LimitAmount {
                    unit: LimitUnit::Usd,
                    amount: 10.0,
                },
                estimate_low: 0.0,
                estimate_high: 0.0,
            },
            new_dependencies: Vec::new(),
            flags: Vec::new(),
            workflow: None,
            seats: Vec::new(),
            objections: Vec::new(),
            offer: None,
        })),
        evidence_sha256: "0".repeat(64),
        mode: ApprovalMode::Supervised,
        decide_with: DecideWith::Rules,
        when_declined: WhenDeclined::Decide,
        options: Vec::new(),
        free_text: FreeText::None,
        needs_you: true,
    };
    RunEventKind::Approval(ApprovalEvent::Opened(Box::new(plan)))
}

fn plan_approved() -> RunEventKind {
    RunEventKind::Approval(ApprovalEvent::Decided(ApprovalDecided {
        id: ApprovalId::numbered(1),
        action: AnswerAction::Approve,
        text: None,
        by: Party::User,
        evidence_sha256: "0".repeat(64),
        auto_preview: None,
        tasks: Vec::new(),
    }))
}

fn step_ref() -> StepRef {
    StepRef {
        workflow: WorkflowId::numbered(1),
        phase: 0,
        path: vec![0],
    }
}

fn workflow_started() -> anyhow::Result<RunEventKind> {
    let workflow: Workflow = serde_json::from_value(json!({
        "name": "merge-only",
        "phases": [{"name": "Merge", "standard": "decide", "steps": [{"kind": "merge"}]}],
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

fn merge_step_started() -> RunEventKind {
    RunEventKind::Workflow(WorkflowEvent::StepStarted(StepStarted {
        step: step_ref(),
        kind: StepKind::Merge,
        standard: StandardPhase::Decide,
        round: 1,
    }))
}

fn step_call() -> anyhow::Result<StepCall> {
    let step: Step = serde_json::from_value(json!({"kind": "merge"}))?;
    Ok(StepCall {
        at: StepAt {
            step: step_ref(),
            round: 1,
        },
        depth: 0,
        standard: StandardPhase::Decide,
        step,
        item: None,
    })
}

fn spec(task: &TaskId) -> TaskSpec {
    TaskSpec {
        id: task.clone(),
        title: "Redis store".to_owned(),
        task: "Build the Redis store".to_owned(),
        kind: TaskKind::Build,
        owned_paths: Vec::new(),
        read_only_paths: Vec::new(),
        forbidden_paths: Vec::new(),
        hidden_paths: Vec::new(),
        depends_on: Vec::new(),
        criteria: Vec::new(),
        worker: Some(worker(OPUS)),
        reviewers: vec![worker(AGY)],
        limit: TaskLimit::default(),
        predictions: None,
        item: None,
        way: None,
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

fn keys(opened: &ApprovalOpened) -> Vec<char> {
    opened.options.iter().map(|option| option.key).collect()
}

#[tokio::test]
async fn main_moved_merged_in_and_rechecked() -> anyhow::Result<()> {
    let bench = Bench::new(None).await?;
    let moved_before = bench.move_main("before.txt")?;

    let done = bench
        .run(async {
            let opened = bench.next_merge_approval(&[]).await?;
            // The question names the branch it merges into; the run's id stays in the detail views.
            assert_eq!(opened.title, "Merge this run into main?");
            // Main moves again while the merge waits for you.
            bench.move_main("while-waiting.txt")?;
            bench.answer(&opened, AnswerAction::Merge)
        })
        .await?;
    assert_eq!(done.flow, Flow::Finish(RunOutcome::Merged));

    let events = bench.merge_events();
    let moved: Vec<&String> = events
        .iter()
        .filter_map(|event| match event {
            MergeEvent::MainMoved { to, .. } => Some(to),
            _ => None,
        })
        .collect();
    assert_eq!(moved.len(), 2, "{events:?}");
    assert_eq!(moved[0], &moved_before);
    let merged_in = events
        .iter()
        .filter(|event| matches!(event, MergeEvent::MainMergedIn { .. }))
        .count();
    assert_eq!(merged_in, 2);
    let checks: Vec<bool> = events
        .iter()
        .filter_map(|event| match event {
            MergeEvent::FinalCheck { passed } => Some(*passed),
            _ => None,
        })
        .collect();
    assert_eq!(checks, [true, true], "the suite ran again on the new main");

    // The run's own diff didn't change, so the approval carried over and main fast-forwarded.
    bench.merged_into("HEAD")?;
    for file in ["store.rs", "before.txt", "while-waiting.txt"] {
        assert!(bench.repo.root().join(file).exists(), "{file}");
    }
    let requests = bench.services.checks.requests();
    assert_eq!(requests.len(), 2);
    assert!(
        requests
            .iter()
            .all(|request| request.workdir == bench.run_tree)
    );
    Ok(())
}

/// A merger that resolves the conflict in `store.rs` with `resolution`, or reports
/// `status` without touching it, and writes the message the change lands with.
fn merging(status: &'static str, resolution: Option<&'static str>) -> FakeRunner {
    FakeRunner::cli(MERGER, "opencode").respond_with(move |spec| {
        if spec.step != Some(StepName::ResolveConflict) {
            return FakeSession::structured(json!({
                "subject": "Add a store",
                "body": "Keeps what the app saves.",
            }));
        }
        if let Some(text) = resolution {
            let _ = std::fs::write(spec.workdir.join("store.rs"), text);
        }
        FakeSession::structured(json!({
            "status": status, "summary": "merged main", "files_changed": ["store.rs"],
            "tests_run": [], "concerns": ["main renamed what the task builds on"],
            "questions": [], "change_requests": [], "memory_proposals": []
        }))
    })
}

/// Your branch moved and changed the file the run wrote too: the merger resolves the
/// conflict on the run's branch, the suite runs on its merge, and the run lands with both.
#[tokio::test]
async fn a_main_that_conflicts_goes_to_the_merger() -> anyhow::Result<()> {
    let bench = Bench::new(None).await?;
    bench.repo.write("store.rs", "fn main_store() {}\n")?;
    bench.repo.commit_all("a store on main")?;
    let both = "fn store() {}\nfn main_store() {}\n";
    let merger = bench.services.workers.add(merging("done", Some(both)));

    let done = bench
        .run(async {
            let opened = bench.next_merge_approval(&[]).await?;
            bench.answer(&opened, AnswerAction::Merge)
        })
        .await?;
    assert_eq!(done.flow, Flow::Finish(RunOutcome::Merged));

    let specs: Vec<_> = merger
        .specs()
        .into_iter()
        .filter(|spec| spec.step == Some(StepName::ResolveConflict))
        .collect();
    assert_eq!(specs.len(), 1, "one resolution");
    assert_eq!(specs[0].task, None);
    assert_eq!(specs[0].access, Access::Build);
    assert_eq!(specs[0].workdir, bench.run_tree);
    assert_eq!(
        std::fs::read_to_string(bench.repo.root().join("store.rs"))?,
        both
    );
    let events = bench.merge_events();
    assert!(
        events
            .iter()
            .any(|event| matches!(event, MergeEvent::MainMergedIn { .. })),
        "{events:?}"
    );
    assert!(
        events
            .iter()
            .any(|event| matches!(event, MergeEvent::FinalCheck { passed: true })),
        "the suite ran on the merger's merge"
    );
    Ok(())
}

/// The merger writes the message the change lands with while the approval waits, and
/// the run's trailer follows it.
#[tokio::test]
async fn the_merger_writes_the_merged_message() -> anyhow::Result<()> {
    let bench = Bench::new(None).await?;
    let merger = bench.services.workers.add(merging("done", None));

    let done = bench
        .run(async {
            let opened = bench.next_merge_approval(&[]).await?;
            // An answer doesn't wait for the message, so this one comes once it's written.
            bench.commit_message_session(true).await?;
            bench.answer(&opened, AnswerAction::Merge)
        })
        .await?;
    assert_eq!(done.flow, Flow::Finish(RunOutcome::Merged));
    bench.merged_into("HEAD")?;
    let message = bench.repo.git(&["log", "-1", "--format=%B", "HEAD"])?;
    assert_eq!(
        message.trim_end(),
        format!("Add a store\n\nKeeps what the app saves.\n\nArgonauts-Run: {TEST_RUN}")
    );
    let specs = merger.specs();
    assert_eq!(specs.len(), 1, "one message, written once");
    // It is given the change, so it has no tools to fail with and no git to run.
    assert_eq!(specs[0].access, Access::NoTools);
    let given: Vec<&str> = specs[0].prompt.iter().map(|p| p.text.as_str()).collect();
    let given = given.join("\n");
    assert!(given.contains("+fn store() {}"), "{given}");
    assert!(given.contains("- store.rs (+1 -0)"), "{given}");
    assert!(!given.contains("git diff"), "{given}");
    assert_eq!(specs[0].task, None);
    assert_eq!(specs[0].workdir, bench.run_tree);
    // It's the Merge step's agent, so the run's view shows it there, on its own model.
    let started = bench
        .host
        .read(|state| state.sessions.get(&specs[0].key).cloned())
        .context("the merger's session")?
        .started;
    assert_eq!(started.workflow_step, Some(step_ref()));
    assert_eq!(started.worker, worker(MERGER));
    assert_eq!(started.label.as_deref(), Some("commit message"));
    // Its step says it writes the message, which a label a script gave can't.
    assert_eq!(started.step, Some(StepName::CommitMessage));
    assert_eq!(
        specs[0].json_schema,
        StepName::CommitMessage.output_schema()
    );
    Ok(())
}

/// The suite passed, then your branch moved and its new commits broke it: the merger
/// fixes what the merge broke on the run's branch, and the suite runs again. The run's own
/// change grew with the fix, so you approve again before it lands.
#[tokio::test]
async fn the_merger_fixes_what_a_moved_main_broke() -> anyhow::Result<()> {
    let bench = Bench::new(None).await?;
    let merger = bench.services.workers.add(fixing_merger());

    let done = bench
        .run(async {
            let first = bench.next_merge_approval(&[]).await?;
            bench.move_main("while-waiting.txt")?;
            bench
                .services
                .checks
                .fail_next("the store breaks with the new main");
            bench.answer(&first, AnswerAction::Merge)?;
            let again = bench
                .next_merge_approval(std::slice::from_ref(&first.id))
                .await?;
            bench.answer(&again, AnswerAction::Merge)
        })
        .await?;
    assert_eq!(done.flow, Flow::Finish(RunOutcome::Merged));

    let fixes: Vec<_> = merger
        .specs()
        .into_iter()
        .filter(|spec| spec.access == Access::Build)
        .collect();
    assert_eq!(fixes.len(), 1, "one fix");
    assert_eq!(fixes[0].workdir, bench.run_tree);
    for file in ["store.rs", "while-waiting.txt", "fixed.txt"] {
        assert!(bench.repo.root().join(file).exists(), "{file}");
    }
    assert_eq!(
        bench.services.checks.requests().len(),
        3,
        "the suite, then after main came in, then after the fix"
    );
    let checks: Vec<bool> = bench
        .merge_events()
        .iter()
        .filter_map(|event| match event {
            MergeEvent::FinalCheck { passed } => Some(*passed),
            _ => None,
        })
        .collect();
    assert_eq!(checks, [true, true]);
    Ok(())
}

/// A merger that fixes what a moved main broke by adding `fixed.txt`, and writes the message
/// the change lands with.
fn fixing_merger() -> FakeRunner {
    FakeRunner::cli(MERGER, "opencode").respond_with(|spec| {
        if spec.access != Access::Build {
            return FakeSession::structured(json!({ "subject": "Add a store", "body": "" }));
        }
        let _ = std::fs::write(spec.workdir.join("fixed.txt"), "fixed\n");
        done_report("fixed the store for the new main", &["fixed.txt"])
    })
}

/// The summaries of the final check's commands, in the order they ran.
fn final_check_summaries(bench: &Bench) -> Vec<(bool, String)> {
    bench
        .sink
        .events()
        .into_iter()
        .filter(|event| {
            let scope = event.task_id.as_ref().map(TaskId::as_str);
            scope == Some(final_check::CHECK_SCOPE)
        })
        .filter_map(|event| match event.kind {
            RunEventKind::Check(outcome) => Some((outcome.passed, outcome.summary)),
            _ => None,
        })
        .collect()
}

/// Main moved while the run worked, before any final check, and its commits break a check the
/// run's own work passes. The check is run on that work alone to tell whose doing it is, its
/// failure says main broke it, and the merger fixes it as it does once a check has passed.
#[tokio::test]
async fn a_check_that_main_broke_before_the_first_final_check_is_fixed_too() -> anyhow::Result<()> {
    let bench = Bench::new(None).await?;
    let merger = bench.services.workers.add(fixing_merger());
    bench.move_main("before.txt")?;
    let checks = &bench.services.checks;
    // One test failed before the run, and still does on the run's own work. With main in, two do.
    checks.push_at_start(suite_failed("15 passed, 1 failed"));
    checks.push(suite_failed("15 passed, 2 failed"));
    checks.push(suite_failed("15 passed, 1 failed"));

    let done = bench
        .run(async {
            let opened = bench.next_merge_approval(&[]).await?;
            bench.answer(&opened, AnswerAction::Merge)
        })
        .await?;
    assert_eq!(done.flow, Flow::Finish(RunOutcome::Merged));

    let fixes = merger
        .specs()
        .into_iter()
        .filter(|spec| spec.access == Access::Build)
        .count();
    assert_eq!(fixes, 1, "the merger fixed what main broke");
    let requests = bench.services.checks.requests();
    assert_eq!(
        requests.len(),
        3,
        "with main in, on the run's own work, then after the fix"
    );
    assert_ne!(
        requests[1].workdir, bench.run_tree,
        "the run's own work is checked in a worktree of its own"
    );
    let summaries = final_check_summaries(&bench);
    assert_eq!(
        summaries.first(),
        Some(&(false, "1 test broke after main came in".to_owned())),
        "{summaries:?}"
    );
    assert!(summaries.last().is_some_and(|(passed, _)| *passed));
    for file in ["store.rs", "before.txt", "fixed.txt"] {
        assert!(bench.repo.root().join(file).exists(), "{file}");
    }
    Ok(())
}

/// A check that fails on the run's own work too isn't main's doing, so nobody is called in to
/// fix it and the failure reads as it ran.
#[tokio::test]
async fn a_check_the_run_itself_broke_is_not_blamed_on_a_moved_main() -> anyhow::Result<()> {
    let bench = Bench::new(None).await?;
    let merger = bench.services.workers.add(fixing_merger());
    bench.move_main("before.txt")?;
    bench
        .services
        .checks
        .push(suite_failed("15 passed, 2 failed"));
    bench
        .services
        .checks
        .push(suite_failed("15 passed, 2 failed"));

    let check = final_check::run(&bench.host).await?;
    assert!(!check.passed);
    assert_eq!(check.checks[0].summary, "15 passed, 2 failed");
    let fixes = merger
        .specs()
        .into_iter()
        .filter(|spec| spec.access == Access::Build)
        .count();
    assert_eq!(fixes, 0);
    Ok(())
}

/// A merger that can't answer leaves the message plain code writes.
#[tokio::test]
async fn without_the_mergers_message_code_writes_it() -> anyhow::Result<()> {
    let bench = Bench::new(None).await?;
    bench.services.workers.add(
        FakeRunner::cli(MERGER, "opencode")
            .respond_with(|_| FakeSession::structured(json!({ "subject": " " }))),
    );

    let done = bench
        .run(async {
            let opened = bench.next_merge_approval(&[]).await?;
            bench.answer(&opened, AnswerAction::Merge)
        })
        .await?;
    assert_eq!(done.flow, Flow::Finish(RunOutcome::Merged));
    let message = bench.repo.git(&["log", "-1", "--format=%B", "HEAD"])?;
    assert!(
        message
            .trim_end()
            .ends_with(&format!("Argonauts-Run: {TEST_RUN}")),
        "{message}"
    );
    assert!(!message.starts_with(' '), "{message}");
    Ok(())
}

/// Your answer doesn't wait for the message: a merger still writing when you approve is
/// stopped, and the change lands with the message plain code writes.
#[tokio::test]
async fn an_answer_given_first_does_not_wait_for_the_message() -> anyhow::Result<()> {
    let bench = Bench::new(None).await?;
    let merger = bench.services.workers.add(
        FakeRunner::cli(MERGER, "opencode").respond_with(|_| FakeSession::until_stopped(vec![])),
    );

    let done = bench
        .run(async {
            let opened = bench.next_merge_approval(&[]).await?;
            bench.commit_message_session(false).await?;
            bench.answer(&opened, AnswerAction::Merge)
        })
        .await?;
    assert_eq!(done.flow, Flow::Finish(RunOutcome::Merged));
    bench.merged_into("HEAD")?;
    let message = bench.repo.git(&["log", "-1", "--format=%B", "HEAD"])?;
    assert!(
        message
            .trim_end()
            .ends_with(&format!("Argonauts-Run: {TEST_RUN}")),
        "{message}"
    );
    let specs = merger.specs();
    assert_eq!(specs.len(), 1);
    let end = bench
        .host
        .read(|state| state.sessions.get(&specs[0].key).cloned())
        .context("the merger's session")?
        .end
        .context("the session was left running")?;
    assert_eq!(end.reason, EndReason::Stopped);
    Ok(())
}

impl Bench {
    /// Waits until the merger's commit message session has started, or `ended` too.
    async fn commit_message_session(&self, ended: bool) -> anyhow::Result<()> {
        for _ in 0..400 {
            let found = self.host.read(|state| {
                state.sessions.by_key.values().any(|session| {
                    session.started.label.as_deref() == Some("commit message")
                        && (!ended || session.end.is_some())
                })
            });
            if found {
                return Ok(());
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        bail!("the merger never wrote a commit message")
    }
}

/// Settings where an agent that fails is asked again at once, the merger is one model, and the
/// main agent, which breaks ties, is another.
fn fallback_settings() -> Settings {
    let mut settings = suite_settings();
    settings.fallback.retry = Some(Retry::without_wait());
    settings.run_host.wait_grace = Span::new("0s");
    settings.crew.main = worker(OPUS);
    settings
}

fn crashed() -> FakeSession {
    FakeSession::ended(EndReason::Crashed {
        message: "segfault".to_owned(),
    })
}

/// A report that says the job is done.
fn done_report(summary: &str, files: &[&str]) -> FakeSession {
    FakeSession::structured(json!({
        "status": "done", "summary": summary, "files_changed": files, "tests_run": [],
        "concerns": [], "questions": [], "change_requests": [], "memory_proposals": []
    }))
}

/// Your branch moved and conflicts, and the merger's agent fails: the conflict passes to the
/// tie-breaker, and when that fails too, to a fallback, as a conflict between tasks does.
#[tokio::test]
async fn a_moved_main_conflict_passes_on_when_the_merger_fails() -> anyhow::Result<()> {
    let bench = Bench::with(vec![spec(&TaskId::numbered(1))], None, fallback_settings()).await?;
    bench.repo.write("store.rs", "fn main_store() {}\n")?;
    bench.repo.commit_all("a store on main")?;
    let both = "fn store() {}\nfn main_store() {}\n";
    let failing = |name: &str, provider: &str| {
        FakeRunner::cli(name, provider).respond_with(|spec| {
            if spec.step == Some(StepName::ResolveConflict) {
                return crashed();
            }
            FakeSession::structured(json!({ "subject": "Add a store", "body": "" }))
        })
    };
    let merger = bench.services.workers.add(failing(MERGER, "opencode"));
    let tie_breaker = bench.services.workers.add(failing(OPUS, "anthropic"));
    let fallback = bench
        .services
        .workers
        .add(FakeRunner::cli(CODEX, "openai").respond_with(move |spec| {
            let _ = std::fs::write(spec.workdir.join("store.rs"), both);
            done_report("merged main", &["store.rs"])
        }));

    let done = bench
        .run(async {
            let opened = bench.next_merge_approval(&[]).await?;
            bench.answer(&opened, AnswerAction::Merge)
        })
        .await?;
    assert_eq!(done.flow, Flow::Finish(RunOutcome::Merged));
    assert_eq!(
        std::fs::read_to_string(bench.repo.root().join("store.rs"))?,
        both
    );
    let resolving = |fake: &FakeRunner| {
        fake.specs()
            .into_iter()
            .filter(|spec| spec.step == Some(StepName::ResolveConflict))
            .collect::<Vec<_>>()
    };
    assert!(!resolving(&merger).is_empty(), "the merger went first");
    assert!(!resolving(&tie_breaker).is_empty(), "then the tie-breaker");
    let settled = resolving(&fallback);
    assert_eq!(settled.len(), 1, "then a fallback, which settled it");
    assert_eq!(settled[0].workdir, bench.run_tree);
    assert_eq!(settled[0].access, Access::Build);
    let label = bench
        .host
        .read(|state| state.sessions.get(&settled[0].key).cloned())
        .context("the fallback's session")?
        .started
        .label;
    assert_eq!(
        label.as_deref().map(|l| l.starts_with("settle conflict")),
        Some(true)
    );
    Ok(())
}

/// Checks that passed break once your moved branch is merged in, and the merger's agent fails:
/// the fix passes to the tie-breaker instead of leaving the checks broken.
#[tokio::test]
async fn a_check_broken_by_a_moved_main_passes_on_when_the_merger_fails() -> anyhow::Result<()> {
    let bench = Bench::with(vec![spec(&TaskId::numbered(1))], None, fallback_settings()).await?;
    let merger = bench
        .services
        .workers
        .add(FakeRunner::cli(MERGER, "opencode").respond_with(|spec| {
            if spec.access == Access::Build {
                // Something half done, which the next taker must not inherit.
                let _ = std::fs::write(spec.workdir.join("store.rs"), "broken\n");
                return crashed();
            }
            FakeSession::structured(json!({ "subject": "Add a store", "body": "" }))
        }));
    let tie_breaker = bench
        .services
        .workers
        .add(FakeRunner::cli(OPUS, "anthropic").respond_with(|spec| {
            let untouched = std::fs::read_to_string(spec.workdir.join("store.rs"))
                .is_ok_and(|text| text == "fn store() {}\n");
            assert!(untouched, "the failed fix was left in the tree");
            let _ = std::fs::write(spec.workdir.join("fixed.txt"), "fixed\n");
            done_report("fixed the store for the new main", &["fixed.txt"])
        }));

    let done = bench
        .run(async {
            let first = bench.next_merge_approval(&[]).await?;
            bench.move_main("while-waiting.txt")?;
            bench
                .services
                .checks
                .fail_next("the store breaks with the new main");
            bench.answer(&first, AnswerAction::Merge)?;
            let again = bench
                .next_merge_approval(std::slice::from_ref(&first.id))
                .await?;
            bench.answer(&again, AnswerAction::Merge)
        })
        .await?;
    assert_eq!(done.flow, Flow::Finish(RunOutcome::Merged));
    let fixes = |fake: &FakeRunner| {
        fake.specs()
            .into_iter()
            .filter(|spec| spec.access == Access::Build)
            .collect::<Vec<_>>()
    };
    assert!(!fixes(&merger).is_empty(), "the merger went first");
    let fixed = fixes(&tie_breaker);
    assert_eq!(fixed.len(), 1, "then the tie-breaker, which fixed it");
    let label = bench
        .host
        .read(|state| state.sessions.get(&fixed[0].key).cloned())
        .context("the fix's session")?
        .started
        .label;
    assert_eq!(label.as_deref(), Some("fix after main moved-2"));
    for file in ["store.rs", "while-waiting.txt", "fixed.txt"] {
        assert!(bench.repo.root().join(file).exists(), "{file}");
    }
    Ok(())
}

/// When the merger says the two sides can't both hold, the merge is undone and the
/// approval says why, in the merger's words.
#[tokio::test]
async fn a_main_the_merger_cannot_reconcile_is_left_to_you() -> anyhow::Result<()> {
    let bench = Bench::new(None).await?;
    let before = bench.run_head()?;
    bench.repo.write("store.rs", "fn main_store() {}\n")?;
    bench.repo.commit_all("a store on main")?;
    bench.services.workers.add(merging("needs_decision", None));

    let call = step_call()?;
    let step = merge_step(&bench.host, &call);
    let watching = async {
        let opened = bench.next_merge_approval(&[]).await?;
        let evidence = approvals::evidence_of(&bench.host, &opened)?;
        ensure!(
            evidence.agent_outputs.iter().any(|line| line.contains(
                "main moved and conflicts with the run's work in store.rs, and the two can't \
                 both hold: main renamed what the task builds on"
            )),
            "{:?}",
            evidence.agent_outputs
        );
        anyhow::Ok(())
    };
    tokio::select! {
        watched = watching => watched?,
        ended = step => bail!("the merge step ended first: {ended:?}"),
    }
    assert_eq!(bench.run_head()?, before, "the merge was undone");
    let merging = git_in(
        &bench.repo,
        &bench.run_tree,
        &["rev-parse", "--quiet", "--verify", "MERGE_HEAD"],
    );
    assert!(merging.is_err(), "no merge is left open");
    Ok(())
}

#[tokio::test]
async fn merge_fast_forward_or_pr() -> anyhow::Result<()> {
    // Main unchanged and clean: a fast-forward.
    let bench = Bench::new(None).await?;
    let done = bench
        .run(async {
            let opened = bench.next_merge_approval(&[]).await?;
            assert_eq!(keys(&opened), ['y', 'p', 'b', 'k', 'x', 'd']);
            let ApprovalPacket::Merge(packet) = &opened.packet else {
                bail!("not a merge packet");
            };
            assert_eq!(packet.branch, branch::run_branch(bench.host.run_id()));
            assert_eq!(packet.rows.len(), 1);
            assert!(packet.suite_passed_on_latest_main);
            assert!(packet.diff_path.exists());
            bench.answer(&opened, AnswerAction::Merge)
        })
        .await?;
    assert_eq!(done.flow, Flow::Finish(RunOutcome::Merged));
    bench.merged_into("HEAD")?;
    assert!(bench.merge_events().contains(&MergeEvent::RunMerged {
        target: "main".to_owned(),
        sha: bench.repo.head()?,
        fast_forward: true,
    }));

    // Main not checked out: only its branch moves.
    let bench = Bench::new(None).await?;
    bench
        .repo
        .git(&["checkout", "--quiet", "-b", "elsewhere"])?;
    let done = bench
        .run(async {
            let opened = bench.next_merge_approval(&[]).await?;
            bench.answer(&opened, AnswerAction::Merge)
        })
        .await?;
    assert_eq!(done.flow, Flow::Finish(RunOutcome::Merged));
    bench.merged_into("main")?;
    assert_eq!(bench.repo.git(&["branch", "--show-current"])?, "elsewhere");

    // A pull request with nowhere to open it: the run doesn't end as one. The question comes
    // back saying why, and the run's work stays on its branch.
    let bench = Bench::new(None).await?;
    let main_before = bench.repo.head()?;
    let done = bench
        .run(async {
            let opened = bench.next_merge_approval(&[]).await?;
            bench.answer(&opened, AnswerAction::PullRequest)?;
            let again = bench
                .next_merge_approval(std::slice::from_ref(&opened.id))
                .await?;
            bench.answer(&again, AnswerAction::KeepBranch)
        })
        .await?;
    assert_eq!(done.flow, Flow::Finish(RunOutcome::BranchLeft));
    assert_eq!(bench.repo.head()?, main_before);
    assert!(
        !bench
            .merge_events()
            .iter()
            .any(|event| matches!(event, MergeEvent::PullRequest { .. })),
        "nothing was opened, so no pull request is recorded"
    );
    Ok(())
}

#[tokio::test]
async fn merge_refused_when_head_moved_after_approval() -> anyhow::Result<()> {
    let bench = Bench::new(None).await?;
    let first_id = std::sync::Mutex::new(None);
    let done = bench
        .run(async {
            let first = bench.next_merge_approval(&[]).await?;
            *first_id.lock().expect("lock") = Some(first.id.clone());
            // Something moves the run branch after the question was shown.
            commit_in(
                &bench.repo,
                &bench.run_tree,
                "late.rs",
                "late\n",
                "late change",
            )?;
            bench.answer(&first, AnswerAction::Merge)?;
            // Landing is refused and the approval opens again with the new head.
            let second = bench
                .next_merge_approval(std::slice::from_ref(&first.id))
                .await?;
            ensure!(second.evidence_sha256 != first.evidence_sha256);
            bench.answer(&second, AnswerAction::Merge)
        })
        .await?;
    assert_eq!(done.flow, Flow::Finish(RunOutcome::Merged));

    let first_id = first_id.lock().expect("lock").clone().context("first id")?;
    let refused = bench
        .sink
        .events()
        .into_iter()
        .any(|event| match event.kind {
            RunEventKind::Approval(ApprovalEvent::Checked(check)) => {
                check.id == first_id
                    && check
                        .violations
                        .iter()
                        .any(|violation| violation.limit == HardLimit::HeadMoved)
            }
            _ => false,
        });
    assert!(refused, "no HeadMoved refusal recorded");
    // What merged is what you approved the second time, late change included.
    bench.merged_into("HEAD")?;
    assert!(bench.repo.root().join("late.rs").exists());
    Ok(())
}

#[tokio::test]
async fn merge_rule_original_out_and_back() -> anyhow::Result<()> {
    let debt = QualityDebt {
        since: 1,
        by: worker(CODEX),
        needs: worker(OPUS),
    };
    let bench = Bench::new(Some(debt.clone())).await?;
    let claude = worker(OPUS).agent_id();
    bench
        .services
        .health
        .set(&claude, HealthState::Down, Some("usage limit"));
    bench.services.workers.add(
        FakeRunner::cli(OPUS, "anthropic").then(FakeSession::structured(json!({
            "verdict": "approve",
            "summary": "Checked Codex's work.",
            "comments": [],
            "tamper_suspected": false,
        }))),
    );

    let done = bench
        .run(async {
            // The original is out: the question lists the debt and offers to wait for it.
            let first = bench.next_merge_approval(&[]).await?;
            let ApprovalPacket::Merge(packet) = &first.packet else {
                bail!("not a merge packet");
            };
            assert_eq!(packet.rows[0].debt, vec![debt.clone()]);
            let evidence = approvals::evidence_of(&bench.host, &first)?;
            // The task goes by its label, as the merge table names it.
            ensure!(
                evidence.facts.unreviewed_debt == Some(vec!["build:redis-store".to_owned()]),
                "{:?}",
                evidence.facts.unreviewed_debt
            );
            let options = keys(&first);
            ensure!(options.contains(&'w'), "{options:?}");
            ensure!(options.contains(&'y'), "{options:?}");
            let merge_anyway = first
                .options
                .iter()
                .find(|option| option.action == AnswerAction::Merge)
                .context("merge anyway")?;
            ensure!(
                merge_anyway.label.contains("anyway"),
                "{}",
                merge_anyway.label
            );
            bench.answer(&first, AnswerAction::Wait)?;

            // It comes back: its review runs first and clears the debt.
            tokio::time::sleep(Duration::from_millis(50)).await;
            bench
                .services
                .health
                .set(&claude, HealthState::Healthy, None);
            let second = bench
                .next_merge_approval(std::slice::from_ref(&first.id))
                .await?;
            let ApprovalPacket::Merge(packet) = &second.packet else {
                bail!("not a merge packet");
            };
            ensure!(packet.rows[0].debt.is_empty());
            let evidence = approvals::evidence_of(&bench.host, &second)?;
            ensure!(evidence.facts.unreviewed_debt == Some(Vec::new()));
            ensure!(!keys(&second).contains(&'w'));
            bench.answer(&second, AnswerAction::Merge)
        })
        .await?;
    assert_eq!(done.flow, Flow::Finish(RunOutcome::Merged));
    let cleared = bench.sink.events().into_iter().any(|event| {
        matches!(
            event.kind,
            RunEventKind::Task(TaskEvent::DebtCleared { ref by }) if *by == worker(OPUS)
        )
    });
    assert!(cleared);
    Ok(())
}

fn codex_debt() -> QualityDebt {
    QualityDebt {
        since: 1,
        by: worker(CODEX),
        needs: worker(OPUS),
    }
}

/// The original back while its task waits at the merge question: with `on_return = "ask"` its
/// question opens in place of the merge question, and its review clears the debt.
#[tokio::test]
async fn the_original_back_at_the_merge_question_asks_first() -> anyhow::Result<()> {
    let bench = Bench::new(Some(codex_debt())).await?;
    bench.out_and_back(OPUS)?;
    bench.services.workers.add(reviewer(OPUS, "anthropic", &[]));

    let done = bench
        .run(async {
            let back = bench.return_approval().await?;
            ensure!(
                keys(&back) == ['r', 'o', 'u', 'y', 't', 'd'],
                "{:?}",
                keys(&back)
            );
            let merge_tasks = bench.host.read(|state| {
                state
                    .approvals
                    .open_for(ApprovalKind::Merge, None)
                    .is_some()
            });
            ensure!(!merge_tasks, "the merge approval opened beside it");
            bench.answer(&back, AnswerAction::UpgradeReview)?;
            let merge = bench.next_merge_approval(&[]).await?;
            let ApprovalPacket::Merge(packet) = &merge.packet else {
                bail!("not a merge packet");
            };
            ensure!(packet.rows[0].debt.is_empty(), "{:?}", packet.rows[0].debt);
            bench.answer(&merge, AnswerAction::Merge)
        })
        .await?;
    assert_eq!(done.flow, Flow::Finish(RunOutcome::Merged));
    assert!(bench.sink.events().iter().any(|event| matches!(
        &event.kind,
        RunEventKind::Task(TaskEvent::DebtCleared { by }) if *by == worker(OPUS)
    )));
    Ok(())
}

/// Keeping the fallback's work from the "original is back" question skips the original's
/// review, and the merge question lists the debt for you.
#[tokio::test]
async fn keeping_the_fallback_at_the_merge_question_leaves_the_debt() -> anyhow::Result<()> {
    let bench = Bench::new(Some(codex_debt())).await?;
    bench.out_and_back(OPUS)?;
    let original = bench.services.workers.add(reviewer(OPUS, "anthropic", &[]));

    let done = bench
        .run(async {
            let back = bench.return_approval().await?;
            bench.answer(&back, AnswerAction::KeepFallback)?;
            let merge = bench.next_merge_approval(&[]).await?;
            let ApprovalPacket::Merge(packet) = &merge.packet else {
                bail!("not a merge packet");
            };
            ensure!(packet.rows[0].debt == vec![codex_debt()], "{packet:?}");
            ensure!(keys(&merge).contains(&'w'), "{:?}", keys(&merge));
            bench.answer(&merge, AnswerAction::Merge)
        })
        .await?;
    assert_eq!(done.flow, Flow::Finish(RunOutcome::Merged));
    assert!(original.specs().is_empty(), "the original reviewed anyway");
    Ok(())
}

/// Giving the task back from the merge approval: the original works on it from the run's branch,
/// it's merged again, and its own review then clears the debt.
#[tokio::test]
async fn the_original_takes_its_task_back_from_the_merge_approval() -> anyhow::Result<()> {
    let bench = Bench::new(Some(codex_debt())).await?;
    bench.out_and_back(OPUS)?;
    let original = bench
        .services
        .workers
        .add(FakeRunner::cli(OPUS, "anthropic").respond_with(|spec| {
            if spec.role == RoleName::Reviewer {
                return FakeSession::structured(json!({
                    "verdict": "approve", "summary": "checked it", "comments": [],
                    "tamper_suspected": false,
                }));
            }
            let _ = std::fs::write(spec.workdir.join("back.rs"), "back\n");
            FakeSession::structured(json!({
                "status": "done", "summary": "finished it", "files_changed": ["back.rs"],
                "tests_run": [], "concerns": [], "questions": [], "change_requests": [],
                "memory_proposals": []
            }))
        }));
    bench.services.workers.add(reviewer(AGY, "google", &[]));

    let done = bench
        .run(async {
            let back = bench.return_approval().await?;
            bench.answer(&back, AnswerAction::GiveBack)?;
            let merge = bench.next_merge_approval(&[]).await?;
            let ApprovalPacket::Merge(packet) = &merge.packet else {
                bail!("not a merge packet");
            };
            ensure!(packet.rows[0].debt.is_empty(), "{:?}", packet.rows[0].debt);
            bench.answer(&merge, AnswerAction::KeepBranch)
        })
        .await?;
    assert_eq!(done.flow, Flow::Finish(RunOutcome::BranchLeft));
    assert_eq!(bench.sent_back(), [TaskId::numbered(1)]);
    let now_with = bench
        .host
        .read(|state| state.tasks.get(&TaskId::numbered(1))?.worker.clone());
    assert_eq!(now_with, Some(worker(OPUS)));
    let worked = original
        .specs()
        .into_iter()
        .find(|spec| spec.role == RoleName::Worker)
        .context("the original never worked on it")?;
    assert!(
        worked
            .prompt
            .iter()
            .any(|part| part.text.contains("is back and continues the task"))
    );
    assert!(bench.run_tree.join("back.rs").exists());
    Ok(())
}

/// A merge question withdrawn while it waits, as `/rewind` does once the run's branch moves, opens
/// again on what's true now.
#[tokio::test]
async fn a_withdrawn_merge_question_opens_again() -> anyhow::Result<()> {
    let bench = Bench::new(None).await?;
    let done = bench
        .run(async {
            let first = bench.next_merge_approval(&[]).await?;
            approvals::withdraw(&bench.host, &first.id, "you rewound task-001 to fallback-1")?;
            let second = bench
                .next_merge_approval(std::slice::from_ref(&first.id))
                .await?;
            let evidence = approvals::evidence_of(&bench.host, &second)?;
            ensure!(
                evidence
                    .agent_outputs
                    .iter()
                    .any(|line| line.contains("you rewound task-001")),
                "{:?}",
                evidence.agent_outputs
            );
            bench.answer(&second, AnswerAction::Merge)
        })
        .await?;
    assert_eq!(done.flow, Flow::Finish(RunOutcome::Merged));
    Ok(())
}

fn choose(label: &str) -> AnswerAction {
    AnswerAction::Choose {
        value: label.to_owned(),
    }
}

impl Bench {
    /// The question asking about your files in the way, other than the ones in `seen`.
    async fn in_the_way_approval(&self, seen: &[ApprovalId]) -> anyhow::Result<ApprovalOpened> {
        let opened = self.next_merge_approval(seen).await?;
        let ApprovalPacket::Merge(packet) = &opened.packet else {
            bail!("not a merge packet");
        };
        ensure!(
            packet.waits_on_you(),
            "a merge approval without files in the way: {}",
            opened.title
        );
        Ok(opened)
    }

    fn run_merged(&self) -> bool {
        self.merge_events()
            .iter()
            .any(|event| matches!(event, MergeEvent::RunMerged { .. }))
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
}

/// A merge that waits on a file of yours lands by itself once you've moved the file: nobody
/// has to come back and answer.
#[tokio::test]
async fn a_waiting_merge_lands_by_itself_once_your_file_is_out_of_the_way() -> anyhow::Result<()> {
    let bench = Bench::new(None).await?;
    bench.repo.write("store.rs", "// my own draft\n")?;
    let done = bench
        .run(async {
            let first = bench.next_merge_approval(&[]).await?;
            bench.answer(&first, AnswerAction::Merge)?;
            let asked = bench
                .in_the_way_approval(std::slice::from_ref(&first.id))
                .await?;
            ensure!(!bench.run_merged(), "merged over your file");
            std::fs::rename(
                bench.repo.root().join("store.rs"),
                bench.repo.scratch().join("my-draft.rs"),
            )?;
            // No answer: the run looks again by itself.
            drop(asked);
            Ok(())
        })
        .await?;
    assert_eq!(done.flow, Flow::Finish(RunOutcome::Merged));
    bench.merged_into("HEAD")?;
    assert_eq!(
        std::fs::read_to_string(bench.repo.root().join("store.rs"))?,
        "fn store() {}\n"
    );
    Ok(())
}

/// Files of yours that already hold what the run brings aren't in its way: an untracked
/// file with the same bytes and an edit that matches the run's, as a landing cut short
/// leaves them. The run merges without asking, and nothing of yours is left aside.
#[tokio::test]
async fn files_that_already_hold_the_runs_work_are_taken_as_they_are() -> anyhow::Result<()> {
    let bench = Bench::with_work(
        vec![spec(&TaskId::numbered(1))],
        None,
        suite_settings(),
        &[("README.md", "# From the run\n")],
    )
    .await?;
    bench.repo.write("store.rs", "fn store() {}\n")?;
    bench.repo.write("README.md", "# From the run\n")?;
    let done = bench.run(merged(&bench)).await?;
    assert_eq!(done.flow, Flow::Finish(RunOutcome::Merged));
    bench.merged_into("HEAD")?;
    let waited = bench
        .merge_events()
        .iter()
        .any(|event| matches!(event, MergeEvent::WaitingToMerge { .. }));
    assert!(!waited, "waited on files that hold the run's work");
    assert_eq!(bench.repo.git(&["status", "--porcelain"])?, "");
    assert!(!bench.host.repo().run_dir.join("landing-aside").exists());
    assert!(
        bench.notes().contains(
            &"2 files of yours already held what the run brings, so they stayed as you left \
              them: README.md, store.rs"
                .to_owned()
        ),
        "{:?}",
        bench.notes()
    );
    Ok(())
}

/// The same holds when the run only writes your files: a file that matches the run's
/// version already stays as it is, and the rest is written.
#[tokio::test]
async fn writing_files_takes_yours_that_already_match() -> anyhow::Result<()> {
    let mut settings = suite_settings();
    settings.delivery.write = DeliveryWrite::Files;
    let bench = Bench::with_work(
        vec![spec(&TaskId::numbered(1))],
        None,
        settings,
        &[("README.md", "# From the run\n")],
    )
    .await?;
    let base = bench.repo.head()?;
    bench.repo.write("store.rs", "fn store() {}\n")?;
    let done = bench.run(merged(&bench)).await?;
    assert_eq!(done.flow, Flow::Finish(RunOutcome::Merged));
    assert_eq!(bench.repo.head()?, base);
    assert_eq!(
        std::fs::read_to_string(bench.repo.root().join("README.md"))?,
        "# From the run\n"
    );
    let mut status: Vec<String> = bench
        .repo
        .git(&["status", "--porcelain"])?
        .lines()
        .map(|line| line.trim().to_owned())
        .collect();
    status.sort();
    assert_eq!(status, ["?? store.rs", "M README.md"]);
    Ok(())
}

/// A landing cut short with a file of yours set aside puts it back before anything else,
/// whatever the answer.
#[tokio::test]
async fn a_file_left_set_aside_goes_back_before_the_run_lands() -> anyhow::Result<()> {
    let bench = Bench::new(None).await?;
    let aside = bench
        .host
        .repo()
        .run_dir
        .join("landing-aside/notes/todo.txt");
    std::fs::create_dir_all(aside.parent().context("no folder")?)?;
    std::fs::write(&aside, "mine\n")?;
    let done = bench.run(merged(&bench)).await?;
    assert_eq!(done.flow, Flow::Finish(RunOutcome::Merged));
    assert_eq!(
        std::fs::read_to_string(bench.repo.root().join("notes/todo.txt"))?,
        "mine\n"
    );
    assert!(!bench.host.repo().run_dir.join("landing-aside").exists());
    Ok(())
}

/// Answers the merge approval with Merge.
async fn merged(bench: &Bench) -> anyhow::Result<()> {
    let opened = bench.next_merge_approval(&[]).await?;
    bench.answer(&opened, AnswerAction::Merge)
}

/// `delivery.commits = "as_run"` lands the run's history as it happened, and `"per_task"` one
/// commit for each step the run took.
#[tokio::test]
async fn delivery_can_keep_the_runs_history_or_a_commit_per_task() -> anyhow::Result<()> {
    let mut as_run = suite_settings();
    as_run.delivery.commits = DeliveryCommits::AsRun;
    let bench = Bench::with(vec![spec(&TaskId::numbered(1))], None, as_run).await?;
    let done = bench.run(merged(&bench)).await?;
    assert_eq!(done.flow, Flow::Finish(RunOutcome::Merged));
    assert_eq!(bench.repo.head()?, bench.run_head()?);

    let mut per_task = suite_settings();
    per_task.delivery.commits = DeliveryCommits::PerTask;
    let bench = Bench::with(vec![spec(&TaskId::numbered(1))], None, per_task).await?;
    let base = bench.repo.head()?;
    let steps = git_in(
        &bench.repo,
        &bench.run_tree,
        &[
            "rev-list",
            "--first-parent",
            "--count",
            &format!("{base}..HEAD"),
        ],
    )?;
    let done = bench.run(merged(&bench)).await?;
    assert_eq!(done.flow, Flow::Finish(RunOutcome::Merged));
    let root = bench.repo.root();
    let merged = git_in(
        &bench.repo,
        root,
        &["rev-list", "--count", &format!("{base}..main")],
    )?;
    assert_eq!(merged, steps, "one commit for each step of the run");
    let merges = git_in(
        &bench.repo,
        root,
        &["rev-list", "--merges", &format!("{base}..main")],
    )?;
    assert!(merges.is_empty(), "a merge commit reached main: {merges}");
    assert_eq!(
        git_in(&bench.repo, root, &["rev-parse", "main^{tree}"])?,
        git_in(&bench.repo, &bench.run_tree, &["rev-parse", "HEAD^{tree}"])?
    );
    Ok(())
}

/// `delivery.write = "files"` writes the run's changes into your checkout and commits
/// nothing: your branch and your index stay as they were, and what's yours in the way still
/// makes the run wait.
#[tokio::test]
async fn delivery_can_write_your_files_and_leave_the_commit_to_you() -> anyhow::Result<()> {
    let mut settings = suite_settings();
    settings.delivery.write = DeliveryWrite::Files;
    let bench = Bench::with(vec![spec(&TaskId::numbered(1))], None, settings).await?;
    let base = bench.repo.head()?;
    // Yours: a staged edit to a file the run leaves alone.
    bench.repo.write("README.md", "# Mine\n")?;
    bench.repo.git(&["add", "README.md"])?;
    let done = bench
        .run(async {
            let opened = bench.next_merge_approval(&[]).await?;
            bench.answer(&opened, AnswerAction::Merge)
        })
        .await?;
    assert_eq!(done.flow, Flow::Finish(RunOutcome::Merged));
    assert_eq!(bench.repo.head()?, base, "your branch didn't move");
    assert_eq!(
        std::fs::read_to_string(bench.repo.root().join("store.rs"))?,
        "fn store() {}\n"
    );
    let status = bench.repo.git(&["status", "--porcelain"])?;
    assert!(
        status.contains("M  README.md"),
        "your staged edit: {status}"
    );
    assert!(
        status.contains("?? store.rs"),
        "the run's file, uncommitted: {status}"
    );
    assert!(
        bench
            .notes()
            .iter()
            .any(|note| note.starts_with("The run's changes are in your files, uncommitted")),
        "{:?}",
        bench.notes()
    );
    assert!(!bench.host.repo().run_dir.join("landing.index").exists());
    Ok(())
}

/// Under `delivery.write = "files"`, files the run wrote that you then committed, as after a
/// landing cut short before it was recorded: your branch holds the run's work in a commit of
/// yours. That reads as merged, and the run review's notes don't follow your branch.
#[tokio::test]
async fn your_commit_of_the_runs_files_reads_as_merged() -> anyhow::Result<()> {
    let mut settings = suite_settings();
    settings.delivery.write = DeliveryWrite::Files;
    settings.evolution.propose_every = 0;
    let bench = Bench::with(vec![spec(&TaskId::numbered(1))], None, settings).await?;
    let done = bench
        .run(async {
            let opened = bench.next_merge_approval(&[]).await?;
            let head = bench.run_head()?;
            for file in bench
                .repo
                .git(&["ls-tree", "-r", "--name-only", &head])?
                .lines()
            {
                let text = bench.repo.git(&["show", &format!("{head}:{file}")])?;
                bench.repo.write(file, &format!("{text}\n"))?;
                bench.repo.git(&["add", file])?;
            }
            bench.repo.git(&["commit", "-q", "-m", "Mine"])?;
            bench.answer(&opened, AnswerAction::Merge)
        })
        .await?;
    assert_eq!(done.flow, Flow::Finish(RunOutcome::Merged));
    let mine = bench.repo.head()?;
    let fast_forward = bench.merge_events().iter().find_map(|event| match event {
        MergeEvent::RunMerged { fast_forward, .. } => Some(*fast_forward),
        _ => None,
    });
    assert_eq!(
        fast_forward,
        Some(false),
        "your branch was taken as the run's"
    );
    run_review::run(&bench.host).await?;
    assert_eq!(bench.repo.head()?, mine, "nothing followed your commit");
    Ok(())
}

/// A run in a repository that had no commit when it started: your files with nothing on your
/// branch, README.md staged, and the run's base a snapshot of them with no parent at the
/// run's own ref. The run changes README.md beside its own new file.
async fn started_before_your_first_commit() -> anyhow::Result<Bench> {
    before_your_first_commit_with(suite_settings()).await
}

async fn before_your_first_commit_with(settings: Settings) -> anyhow::Result<Bench> {
    let repo = TempRepo::new()?;
    let tree = repo.git(&["rev-parse", "HEAD^{tree}"])?;
    let snapshot = repo.git(&["commit-tree", &tree, "-m", "your files"])?;
    let run = RunId::new(TEST_RUN)?;
    repo.git(&["update-ref", &unborn::base_ref(&run), &snapshot])?;
    repo.git(&["update-ref", "-d", "refs/heads/main"])?;
    ensure!(repo.head().is_err(), "the branch still has a commit");
    Bench::in_repo(
        repo,
        &snapshot,
        vec![spec(&TaskId::numbered(1))],
        None,
        settings,
        &[("README.md", "# From the run\n")],
    )
    .await
}

/// With no commit to move forward from, the run's work comes back as files: nothing is
/// committed or staged, your branch still has nothing on it, and the file you staged, which
/// your index lists as a change of yours, isn't taken for one in the way.
#[tokio::test]
async fn a_run_started_before_your_first_commit_lands_as_files() -> anyhow::Result<()> {
    let bench = started_before_your_first_commit().await?;
    let staged = bench.repo.git(&["diff", "--cached", "--name-only"])?;
    let done = bench
        .run(async {
            let opened = bench.next_merge_approval(&[]).await?;
            // No branch moves, so the question names what the run's work goes into.
            ensure!(
                opened.title == "Merge this run into your files?",
                "{}",
                opened.title
            );
            bench.answer(&opened, AnswerAction::Merge)
        })
        .await?;
    assert_eq!(done.flow, Flow::Finish(RunOutcome::Merged));
    let merged_into = bench
        .merge_events()
        .into_iter()
        .find_map(|event| match event {
            MergeEvent::RunMerged {
                target,
                fast_forward,
                ..
            } => Some((target, fast_forward)),
            _ => None,
        });
    assert_eq!(merged_into, Some(("your files".to_owned(), false)));

    let root = bench.repo.root();
    assert_eq!(
        std::fs::read_to_string(root.join("README.md"))?,
        "# From the run\n"
    );
    assert_eq!(
        std::fs::read_to_string(root.join("store.rs"))?,
        "fn store() {}\n"
    );
    assert!(
        bench.repo.head().is_err(),
        "a first commit was made for you"
    );
    assert_eq!(
        bench.repo.git(&["diff", "--cached", "--name-only"])?,
        staged,
        "your staging area changed"
    );
    assert_eq!(
        bench.repo.git(&["show", ":README.md"])?,
        "# Test repo",
        "what you staged is still what you staged"
    );
    assert!(
        bench
            .notes()
            .iter()
            .any(|note| note.starts_with("The run's changes are in your files for you to commit")),
        "{:?}",
        bench.notes()
    );
    let moved = bench
        .merge_events()
        .iter()
        .any(|event| matches!(event, MergeEvent::MainMoved { .. }));
    assert!(!moved, "nothing of your branch to bring in");
    for scratch in ["landing.index", "in-the-way.index"] {
        assert!(
            !bench.host.repo().run_dir.join(scratch).exists(),
            "{scratch}"
        );
    }
    Ok(())
}

/// A file you changed since the snapshot is yours, so the run waits and names it, and lands
/// by itself once the file is as it was.
#[tokio::test]
async fn an_edit_since_the_snapshot_makes_the_files_wait() -> anyhow::Result<()> {
    let bench = started_before_your_first_commit().await?;
    bench.repo.write("README.md", "# Mine now\n")?;
    let done = bench
        .run(async {
            let first = bench.next_merge_approval(&[]).await?;
            bench.answer(&first, AnswerAction::Merge)?;
            let asked = bench
                .in_the_way_approval(std::slice::from_ref(&first.id))
                .await?;
            ensure!(!bench.run_merged(), "wrote over your edit");
            let waiting = bench
                .host
                .read(|state| state.merge.waiting_to_merge.clone());
            ensure!(waiting == ["README.md"], "{waiting:?}");
            ensure!(
                std::fs::read_to_string(bench.repo.root().join("README.md"))? == "# Mine now\n",
                "your edit is gone"
            );
            ensure!(
                !bench.repo.root().join("store.rs").exists(),
                "half of the run was written"
            );
            bench.repo.write("README.md", "# Test repo\n")?;
            drop(asked);
            Ok(())
        })
        .await?;
    assert_eq!(done.flow, Flow::Finish(RunOutcome::Merged));
    assert_eq!(
        std::fs::read_to_string(bench.repo.root().join("README.md"))?,
        "# From the run\n"
    );
    Ok(())
}

/// A landing as files that was cut short after the writing, before the run recorded it: your
/// files hold the run's work already. Taken up again, that reads as merged, and never as
/// edits of yours in the run's way.
#[tokio::test]
async fn files_written_before_a_crash_are_not_in_their_own_way() -> anyhow::Result<()> {
    let before_a_commit = started_before_your_first_commit().await?;
    let mut settings = suite_settings();
    settings.delivery.write = DeliveryWrite::Files;
    let asked = Bench::with(vec![spec(&TaskId::numbered(1))], None, settings).await?;
    for bench in [before_a_commit, asked] {
        let head = bench.run_head()?;
        for file in bench
            .repo
            .git(&["ls-tree", "-r", "--name-only", &head])?
            .lines()
        {
            let text = bench.repo.git(&["show", &format!("{head}:{file}")])?;
            bench.repo.write(file, &format!("{text}\n"))?;
        }
        let done = bench
            .run(async {
                let first = bench.next_merge_approval(&[]).await?;
                bench.answer(&first, AnswerAction::Merge)
            })
            .await?;
        assert_eq!(done.flow, Flow::Finish(RunOutcome::Merged));
        ensure!(bench.run_merged(), "the landing wasn't recorded");
        let waited = bench
            .merge_events()
            .iter()
            .any(|event| matches!(event, MergeEvent::WaitingToMerge { .. }));
        ensure!(!waited, "the run's own files were taken for yours");
    }
    Ok(())
}

/// A landing as files cut short partway, before a first commit: what it wrote holds the
/// run's work already, so only the rest is written, and nothing waits on you.
#[tokio::test]
async fn files_written_partway_are_not_in_the_way_of_the_rest() -> anyhow::Result<()> {
    let bench = started_before_your_first_commit().await?;
    bench.repo.write("README.md", "# From the run\n")?;
    let done = bench.run(merged(&bench)).await?;
    assert_eq!(done.flow, Flow::Finish(RunOutcome::Merged));
    let waited = bench
        .merge_events()
        .iter()
        .any(|event| matches!(event, MergeEvent::WaitingToMerge { .. }));
    assert!(!waited, "the run's own file was taken for yours");
    assert_eq!(
        std::fs::read_to_string(bench.repo.root().join("store.rs"))?,
        "fn store() {}\n"
    );
    assert!(
        bench.repo.head().is_err(),
        "a first commit was made for you"
    );
    Ok(())
}

/// Your first commit, made while the run worked, shares no history with the snapshot the run
/// started from. The run still lands as files over what it started from, and your commit and
/// branch stay as you made them.
#[tokio::test]
async fn a_first_commit_made_meanwhile_stays_yours() -> anyhow::Result<()> {
    let bench = started_before_your_first_commit().await?;
    let yours = bench.repo.commit_all("my first commit")?;
    let done = bench.run(merged(&bench)).await?;
    assert_eq!(done.flow, Flow::Finish(RunOutcome::Merged));
    assert_eq!(bench.repo.head()?, yours, "your branch moved");
    // The run's work, as changes of yours to commit: one file edited, one new.
    let unstaged = bench.repo.git(&["diff", "--name-only"])?;
    assert_eq!(unstaged, "README.md");
    let status = bench.repo.git(&["status", "--porcelain"])?;
    assert!(status.contains("?? store.rs"), "{status}");
    Ok(())
}

/// A run that started before your first commit, under `merge.unborn = "first_commit"`.
async fn asked_for_your_first_commit() -> anyhow::Result<Bench> {
    let mut settings = suite_settings();
    settings.merge.unborn = UnbornLanding::FirstCommit;
    settings.evolution.propose_every = 0;
    before_your_first_commit_with(settings).await
}

impl Bench {
    /// What merged, where, and whether a branch of yours moved to it.
    fn landing(&self) -> Option<(String, String, bool)> {
        self.merge_events()
            .into_iter()
            .find_map(|event| match event {
                MergeEvent::RunMerged {
                    target,
                    sha,
                    fast_forward,
                } => Some((target, sha, fast_forward)),
                _ => None,
            })
    }

    /// `main` holds one commit with no parent, made for the run, with the run's files.
    fn main_has_the_runs_first_commit(&self) -> anyhow::Result<String> {
        let first = self.repo.head()?;
        let with_parents = self
            .repo
            .git(&["rev-list", "--parents", "-n", "1", &first])?;
        ensure!(with_parents == first, "it has a parent: {with_parents}");
        let tree = |rev: &str| self.repo.git(&["rev-parse", &format!("{rev}^{{tree}}")]);
        ensure!(
            tree(&first)? == tree(&self.run_head()?)?,
            "it doesn't hold the run's files"
        );
        let message = self.repo.git(&["log", "-1", "--format=%B", &first])?;
        ensure!(
            message.contains(&format!("Argonauts-Run: {TEST_RUN}")),
            "it doesn't name the run: {message}"
        );
        Ok(first)
    }
}

/// `merge.unborn = "first_commit"`: the run gives your branch its first commit, one commit
/// with no parent that holds your files as the run found them and its work, and the approval
/// says so before you answer. Afterwards git shows only what you did since the run started.
#[tokio::test]
async fn a_run_can_make_your_branchs_first_commit() -> anyhow::Result<()> {
    let bench = asked_for_your_first_commit().await?;
    // Yours since the run started, beside what the run changes: a new file, staged.
    bench.repo.write("notes.txt", "mine\n")?;
    bench.repo.git(&["add", "notes.txt"])?;
    let done = bench
        .run(async {
            let opened = bench.next_merge_approval(&[]).await?;
            ensure!(
                opened.title == "Merge this run into main?",
                "{}",
                opened.title
            );
            let evidence = approvals::evidence_of(&bench.host, &opened)?;
            ensure!(
                evidence.agent_outputs.iter().any(|line| line
                    .starts_with("main has no commits yet, so merging makes its first one")),
                "{:?}",
                evidence.agent_outputs
            );
            bench.answer(&opened, AnswerAction::Merge)
        })
        .await?;
    assert_eq!(done.flow, Flow::Finish(RunOutcome::Merged));
    let first = bench.main_has_the_runs_first_commit()?;
    assert_eq!(
        bench.landing(),
        Some(("main".to_owned(), first.clone(), true))
    );

    let root = bench.repo.root();
    assert_eq!(
        std::fs::read_to_string(root.join("README.md"))?,
        "# From the run\n"
    );
    assert_eq!(
        std::fs::read_to_string(root.join("store.rs"))?,
        "fn store() {}\n"
    );
    assert_eq!(
        bench.repo.git(&["status", "--porcelain"])?,
        "A  notes.txt",
        "only what you did since the run started is a change"
    );
    assert!(
        bench
            .notes()
            .iter()
            .any(|note| note.starts_with("main had no commits, so the run made its first one")),
        "{:?}",
        bench.notes()
    );
    for scratch in ["landing.index", "in-the-way.index"] {
        assert!(
            !bench.host.repo().run_dir.join(scratch).exists(),
            "{scratch}"
        );
    }

    // The run review finds its notes in that commit already, and adds nothing to your branch.
    run_review::run(&bench.host).await?;
    assert_eq!(bench.repo.head()?, first);
    Ok(())
}

/// A file you changed since the run started holds the first commit back like any landing:
/// nothing is committed or written until it's out of the way.
#[tokio::test]
async fn your_edit_in_the_way_holds_the_first_commit_back() -> anyhow::Result<()> {
    let bench = asked_for_your_first_commit().await?;
    bench.repo.write("README.md", "# Mine now\n")?;
    let done = bench
        .run(async {
            let first = bench.next_merge_approval(&[]).await?;
            bench.answer(&first, AnswerAction::Merge)?;
            let asked = bench
                .in_the_way_approval(std::slice::from_ref(&first.id))
                .await?;
            ensure!(bench.repo.head().is_err(), "committed over your edit");
            ensure!(
                !bench.repo.root().join("store.rs").exists(),
                "half of the run was written"
            );
            bench.repo.write("README.md", "# Test repo\n")?;
            drop(asked);
            Ok(())
        })
        .await?;
    assert_eq!(done.flow, Flow::Finish(RunOutcome::Merged));
    bench.main_has_the_runs_first_commit()?;
    assert_eq!(bench.repo.git(&["status", "--porcelain"])?, "");
    Ok(())
}

/// Your own first commit, made while the run worked, is never replaced or built on: the run
/// lands as files, as it would without the setting, and says why it made no commit.
#[tokio::test]
async fn your_own_first_commit_wins_over_the_runs() -> anyhow::Result<()> {
    let bench = asked_for_your_first_commit().await?;
    let yours = bench.repo.commit_all("my first commit")?;
    let done = bench
        .run(async {
            let opened = bench.next_merge_approval(&[]).await?;
            ensure!(
                opened.title == "Merge this run into your files?",
                "{}",
                opened.title
            );
            bench.answer(&opened, AnswerAction::Merge)
        })
        .await?;
    assert_eq!(done.flow, Flow::Finish(RunOutcome::Merged));
    assert_eq!(bench.repo.head()?, yours, "your branch moved");
    let landing = bench.landing().context("nothing merged")?;
    assert_eq!((landing.0.as_str(), landing.2), ("your files", false));
    assert_eq!(bench.repo.git(&["diff", "--name-only"])?, "README.md");
    assert!(
        bench
            .notes()
            .iter()
            .any(|note| note.contains("you made that one while the run worked")),
        "{:?}",
        bench.notes()
    );
    Ok(())
}

/// A first commit cut short is taken up where it stopped, and never read as edits of yours
/// in the run's way: after the files were written, the commit is still made, and after your
/// branch got the commit, only your index is left to follow it.
#[tokio::test]
async fn a_first_commit_cut_short_is_taken_up_where_it_stopped() -> anyhow::Result<()> {
    for branch_started in [false, true] {
        let bench = asked_for_your_first_commit().await?;
        let head = bench.run_head()?;
        write_out(&bench, &head)?;
        let made = if branch_started {
            let tree = bench
                .repo
                .git(&["rev-parse", &format!("{head}^{{tree}}")])?;
            let message = format!("Work\n\nArgonauts-Run: {TEST_RUN}");
            let commit = bench.repo.git(&["commit-tree", &tree, "-m", &message])?;
            bench
                .repo
                .git(&["update-ref", "refs/heads/main", &commit, ""])?;
            Some(commit)
        } else {
            None
        };
        let done = bench.run(merged(&bench)).await?;
        assert_eq!(done.flow, Flow::Finish(RunOutcome::Merged));
        let first = bench.main_has_the_runs_first_commit()?;
        if let Some(made) = made {
            assert_eq!(first, made, "a second first commit was made");
        }
        assert_eq!(bench.landing(), Some(("main".to_owned(), first, true)));
        assert_eq!(bench.repo.git(&["status", "--porcelain"])?, "");
        let waited = bench
            .merge_events()
            .iter()
            .any(|event| matches!(event, MergeEvent::WaitingToMerge { .. }));
        ensure!(!waited, "the run's own files were taken for yours");
    }
    Ok(())
}

/// Your files as the run's `head` has them, written the way a landing writes them.
fn write_out(bench: &Bench, head: &str) -> anyhow::Result<()> {
    for file in bench
        .repo
        .git(&["ls-tree", "-r", "--name-only", head])?
        .lines()
    {
        let text = bench.repo.git(&["show", &format!("{head}:{file}")])?;
        bench.repo.write(file, &format!("{text}\n"))?;
    }
    Ok(())
}

/// The sample run's failure: your checkout had an untracked file where the run adds one, and
/// git refused the fast-forward. Now the run waits to merge and asks you, and moving the file
/// aside lands it, with nothing of yours deleted.
#[tokio::test]
async fn files_in_the_way_wait_to_merge_and_move_aside() -> anyhow::Result<()> {
    let bench = Bench::new(None).await?;
    bench.repo.write("store.rs", "// my own draft\n")?;
    bench
        .repo
        .write("__pycache__/store.cpython-312.pyc", "not in the way")?;
    let done = bench
        .run(async {
            let first = bench.next_merge_approval(&[]).await?;
            bench.answer(&first, AnswerAction::Merge)?;
            let asked = bench
                .in_the_way_approval(std::slice::from_ref(&first.id))
                .await?;
            assert_eq!(asked.title, "Waiting to merge into main");
            assert_eq!(keys(&asked), ['m', 't', 'l', 'p', 'k']);
            assert!(asked.needs_you, "only you can move your files");
            assert_eq!(asked.mode, ApprovalMode::Supervised);
            assert_eq!(
                asked.free_text,
                FreeText::None,
                "you choose, you don't type"
            );
            let ApprovalPacket::Merge(packet) = &asked.packet else {
                bail!("not a merge packet");
            };
            assert_eq!(packet.in_the_way, ["store.rs"]);
            assert_eq!(packet.rows.len(), 1, "the merge's evidence, shown again");
            let evidence = approvals::evidence_of(&bench.host, &asked)?;
            assert_eq!(
                evidence.agent_outputs,
                [
                    "1 file of yours is in the way of main: store.rs (not committed, where the \
                  run adds a file). Nothing of yours was changed."
                ]
            );
            ensure!(
                !bench.run_merged(),
                "nothing merged while the files were in the way"
            );
            ensure!(bench.merge_events().contains(&MergeEvent::WaitingToMerge {
                target: "main".to_owned(),
                in_the_way: vec!["store.rs".to_owned()],
            }));
            bench.answer(&asked, choose(MOVE_ASIDE))
        })
        .await?;
    assert_eq!(done.flow, Flow::Finish(RunOutcome::Merged));
    bench.merged_into("HEAD")?;
    assert_eq!(
        std::fs::read_to_string(bench.repo.root().join("store.rs"))?,
        "fn store() {}\n"
    );
    let set_aside = bench.host.repo().run_dir.join("set-aside/store.rs");
    assert_eq!(std::fs::read_to_string(set_aside)?, "// my own draft\n");
    assert!(
        bench
            .repo
            .root()
            .join("__pycache__/store.cpython-312.pyc")
            .exists()
    );
    let notes = bench.notes();
    assert!(
        notes
            .iter()
            .any(|note| note.starts_with("Moved store.rs out of the way, into ")),
        "{notes:?}"
    );
    Ok(())
}

/// The sample case with the built-in list off, as it is by default: the tests left bytecode
/// beside the work, the run's commit took it, and your checkout has the same file untracked.
/// Git won't overwrite yours, so the run waits to merge on a question with choices instead of
/// failing, and its work stays on its branch.
#[tokio::test]
async fn bytecode_the_run_committed_and_you_have_waits_to_merge() -> anyhow::Result<()> {
    const BYTECODE: &str = "__pycache__/store.cpython-312.pyc";
    let settings = suite_settings();
    ensure!(
        !settings.merge.leave_out_builtin,
        "the built-in list is opt-in"
    );
    let bench = Bench::with_work(
        vec![spec(&TaskId::numbered(1))],
        None,
        settings,
        &[(BYTECODE, "bytecode the tests left")],
    )
    .await?;
    let work = bench.run_head()?;
    let committed = git_in(
        &bench.repo,
        &bench.run_tree,
        &["show", "--name-only", "--format=", "HEAD"],
    )?;
    assert_eq!(
        committed.lines().collect::<Vec<_>>(),
        [BYTECODE, "store.rs"]
    );
    bench.repo.write(BYTECODE, "your own bytecode")?;
    let main = bench.repo.head()?;

    let call = step_call()?;
    let step = merge_step(&bench.host, &call);
    let answering = async {
        let first = bench.next_merge_approval(&[]).await?;
        bench.answer(&first, AnswerAction::Merge)?;
        let asked = bench
            .in_the_way_approval(std::slice::from_ref(&first.id))
            .await?;
        assert_eq!(asked.title, "Waiting to merge into main");
        let labels: Vec<&str> = asked
            .options
            .iter()
            .map(|option| option.label.as_str())
            .collect();
        assert_eq!(
            labels,
            [
                MOVE_ASIDE,
                TRY_AGAIN,
                MERGE_LATER,
                "Open a pull request",
                "Keep the branch"
            ]
        );
        assert!(asked.needs_you);
        let ApprovalPacket::Merge(packet) = &asked.packet else {
            bail!("not a merge packet");
        };
        assert_eq!(packet.in_the_way, [BYTECODE]);
        ensure!(!bench.run_merged(), "nothing merged over your file");
        ensure!(bench.merge_events().contains(&MergeEvent::WaitingToMerge {
            target: "main".to_owned(),
            in_the_way: vec![BYTECODE.to_owned()],
        }));
        bench.answer(&asked, choose(MERGE_LATER))
    };
    let (done, answered) = tokio::join!(
        tokio::time::timeout(deadline::scaled(Duration::from_secs(20)), step),
        answering
    );
    answered?;
    match done.context("the merge step timed out")? {
        Err(EngineError::Interrupted) => {}
        other => bail!("expected the run to wait to merge, got {other:?}"),
    }
    let finished = bench
        .sink
        .events()
        .into_iter()
        .any(|event| matches!(event.kind, RunEventKind::Run(RunLifecycle::Finished { .. })));
    assert!(!finished, "the run waits to merge, it hasn't failed");
    assert!(bench.host.read(|state| state.run.paused));
    assert_eq!(
        bench
            .host
            .read(|state| state.merge.waiting_to_merge.clone()),
        [BYTECODE]
    );
    // The work stays on the run's branch, and your checkout and your file are as they were.
    let run_branch = branch::run_branch(bench.host.run_id());
    assert_eq!(
        git_in(&bench.repo, bench.repo.root(), &["rev-parse", &run_branch])?,
        work
    );
    assert_eq!(bench.run_head()?, work);
    assert_eq!(bench.repo.head()?, main);
    assert_eq!(
        std::fs::read_to_string(bench.repo.root().join(BYTECODE))?,
        "your own bytecode"
    );
    Ok(())
}

#[tokio::test]
async fn try_again_asks_while_files_are_still_in_the_way() -> anyhow::Result<()> {
    let bench = Bench::new(None).await?;
    bench.repo.write("store.rs", "// mine\n")?;
    let done = bench
        .run(async {
            let first = bench.next_merge_approval(&[]).await?;
            bench.answer(&first, AnswerAction::Merge)?;
            let asked = bench
                .in_the_way_approval(std::slice::from_ref(&first.id))
                .await?;
            bench.answer(&asked, choose(TRY_AGAIN))?;
            let again = bench
                .in_the_way_approval(&[first.id.clone(), asked.id.clone()])
                .await?;
            std::fs::remove_file(bench.repo.root().join("store.rs"))?;
            bench.answer(&again, choose(TRY_AGAIN))
        })
        .await?;
    assert_eq!(done.flow, Flow::Finish(RunOutcome::Merged));
    bench.merged_into("HEAD")?;
    Ok(())
}

/// A merge waiting on a file of yours can end without merging, as when it was ready: the run
/// keeps its work on its branch, and nothing of yours moves.
#[tokio::test]
async fn a_waiting_merge_can_keep_the_branch_instead() -> anyhow::Result<()> {
    let bench = Bench::new(None).await?;
    let base = bench.repo.head()?;
    bench.repo.write("store.rs", "// mine\n")?;
    let done = bench
        .run(async {
            let first = bench.next_merge_approval(&[]).await?;
            bench.answer(&first, AnswerAction::Merge)?;
            let asked = bench
                .in_the_way_approval(std::slice::from_ref(&first.id))
                .await?;
            let offered: Vec<&AnswerAction> =
                asked.options.iter().map(|option| &option.action).collect();
            ensure!(
                offered.contains(&&AnswerAction::PullRequest)
                    && offered.contains(&&AnswerAction::KeepBranch),
                "{offered:?}"
            );
            bench.answer(&asked, AnswerAction::KeepBranch)
        })
        .await?;
    assert_eq!(done.flow, Flow::Finish(RunOutcome::BranchLeft));
    assert!(!bench.run_merged());
    assert_eq!(bench.repo.head()?, base, "your branch didn't move");
    assert_eq!(
        std::fs::read_to_string(bench.repo.root().join("store.rs"))?,
        "// mine\n"
    );
    Ok(())
}

/// Git failing while the run lands never fails the run: it waits with git's words, and
/// trying again once the trouble is dealt with lands it.
#[tokio::test]
async fn git_trouble_while_landing_waits_instead_of_failing() -> anyhow::Result<()> {
    let bench = Bench::new(None).await?;
    let index = bench.repo.root().join(".git/index");
    let done = bench
        .run(async {
            let first = bench.next_merge_approval(&[]).await?;
            std::fs::write(&index, "not an index")?;
            bench.answer(&first, AnswerAction::Merge)?;
            let asked = bench
                .next_merge_approval(std::slice::from_ref(&first.id))
                .await?;
            let ApprovalPacket::Merge(packet) = &asked.packet else {
                bail!("not a merge packet");
            };
            ensure!(
                packet
                    .refused
                    .as_deref()
                    .is_some_and(|said| said.contains("index")),
                "{:?}",
                packet.refused
            );
            ensure!(!bench.run_merged(), "the run merged with a broken index");
            std::fs::remove_file(&index)?;
            bench.repo.git(&["reset", "--quiet"])?;
            bench.answer(&asked, choose(TRY_AGAIN))
        })
        .await?;
    assert_eq!(done.flow, Flow::Finish(RunOutcome::Merged));
    bench.merged_into("HEAD")?;
    Ok(())
}

/// A merge your rules approved, drawn for a spot check, joins your queue once it is in, and
/// not when they approved it: until then there is no merge of theirs to check.
#[tokio::test]
async fn an_auto_merge_drawn_for_a_spot_check_joins_your_queue_once_it_is_in() -> anyhow::Result<()>
{
    let bench = Bench::new(None).await?;
    bench.services.policy.decide_next(PolicyOutcome::Decided {
        verdict: Verdict::Approve,
        by: DecideWith::Rules,
        judge: None,
        reasons: vec!["within your rules".to_owned()],
        spot_check: true,
    });
    let done = bench.run(async { Ok(()) }).await?;
    assert_eq!(done.flow, Flow::Finish(RunOutcome::Merged));
    let events = bench.sink.events();
    let at = |wanted: &dyn Fn(&RunEventKind) -> bool| {
        events.iter().position(|event| wanted(&event.kind))
    };
    let approved = at(&|kind| matches!(kind, RunEventKind::Approval(ApprovalEvent::Decided(_))));
    let merged = at(&|kind| matches!(kind, RunEventKind::Merge(MergeEvent::Merged { .. })));
    let queued = at(&|kind| {
        matches!(
            kind,
            RunEventKind::Approval(ApprovalEvent::SpotCheckQueued { .. })
        )
    });
    assert!(
        approved.is_some() && approved < merged,
        "{approved:?} {merged:?}"
    );
    assert!(merged < queued, "{merged:?} {queued:?}");
    Ok(())
}

/// A landing your rules approved and that was refused merged nothing, so the auto-merge
/// they took goes back, and the spot check drawn for it never joins your queue; the question
/// you answer yourself takes none.
#[tokio::test]
async fn a_refused_auto_landing_gives_back_its_auto_merge() -> anyhow::Result<()> {
    let bench = Bench::new(None).await?;
    bench.services.policy.decide_next_after(
        PolicyOutcome::Decided {
            verdict: Verdict::Approve,
            by: DecideWith::Rules,
            judge: None,
            reasons: vec!["within your rules".to_owned()],
            spot_check: true,
        },
        Duration::from_millis(300),
    );
    let done = bench
        .run(async {
            let first = bench.next_merge_approval(&[]).await?;
            // The branch moves before your rules' approval lands.
            commit_in(
                &bench.repo,
                &bench.run_tree,
                "late.rs",
                "late\n",
                "late change",
            )?;
            let second = bench
                .next_merge_approval(std::slice::from_ref(&first.id))
                .await?;
            bench.answer(&second, AnswerAction::Merge)
        })
        .await?;
    assert_eq!(done.flow, Flow::Finish(RunOutcome::Merged));
    let refused: Vec<ApprovalId> = bench
        .services
        .policy
        .refused_landings()
        .into_iter()
        .map(|point| point.approval)
        .collect();
    assert_eq!(refused.len(), 1, "{refused:?}");
    let queued = bench.sink.events().into_iter().any(|event| {
        matches!(
            event.kind,
            RunEventKind::Approval(ApprovalEvent::SpotCheckQueued { .. })
        )
    });
    assert!(!queued, "a merge that never happened has nothing to check");
    Ok(())
}

/// The review's first probe: you switch branches while the question waits, then choose Try
/// again. Landing reads the checkout again, so main lands and your branch is left alone,
/// instead of a "Merged" that moved the branch you were on.
#[tokio::test]
async fn a_branch_switched_while_the_question_waits_leaves_it_alone() -> anyhow::Result<()> {
    let bench = Bench::new(None).await?;
    let base = bench.repo.head()?;
    bench.repo.write("store.rs", "// mine\n")?;
    let done = bench
        .run(async {
            let first = bench.next_merge_approval(&[]).await?;
            bench.answer(&first, AnswerAction::Merge)?;
            let asked = bench
                .in_the_way_approval(std::slice::from_ref(&first.id))
                .await?;
            std::fs::remove_file(bench.repo.root().join("store.rs"))?;
            git_in(
                &bench.repo,
                bench.repo.root(),
                &["checkout", "-qb", "experiment"],
            )?;
            bench.answer(&asked, choose(TRY_AGAIN))
        })
        .await?;
    assert_eq!(done.flow, Flow::Finish(RunOutcome::Merged));
    bench.merged_into("main")?;
    assert_eq!(bench.repo.head()?, base, "your branch didn't move");
    assert!(bench.merge_events().iter().any(|event| matches!(
        event,
        MergeEvent::RunMerged { target, .. } if target == "main"
    )));
    Ok(())
}

/// The review's second probe: while the question waits you clear the file in the way and edit
/// one the run changes. Try again asks about the edit, in plain words, instead of failing the
/// run with git's "Aborting", and lands once it's committed or undone.
#[tokio::test]
async fn an_edit_made_while_the_question_waits_asks_instead_of_failing() -> anyhow::Result<()> {
    let bench = Bench::new(None).await?;
    commit_in(
        &bench.repo,
        &bench.run_tree,
        "README.md",
        "# Test repo\n\nRun `store` to save.\n",
        "Document the store",
    )?;
    bench.repo.write("store.rs", "// mine\n")?;
    let done = bench
        .run(async {
            let first = bench.next_merge_approval(&[]).await?;
            bench.answer(&first, AnswerAction::Merge)?;
            let asked = bench
                .in_the_way_approval(std::slice::from_ref(&first.id))
                .await?;
            std::fs::remove_file(bench.repo.root().join("store.rs"))?;
            bench.repo.write("README.md", "# Test repo, my notes\n")?;
            bench.answer(&asked, choose(TRY_AGAIN))?;

            let edited = bench
                .next_merge_approval(&[first.id.clone(), asked.id.clone()])
                .await?;
            let ApprovalPacket::Merge(packet) = &edited.packet else {
                bail!("not a merge packet");
            };
            assert_eq!(edited.title, "Waiting to merge into main");
            assert_eq!(packet.edited, ["README.md"]);
            assert!(packet.in_the_way.is_empty());
            assert_eq!(keys(&edited), ['t', 'l', 'p', 'k'], "nothing to move aside");
            assert_eq!(edited.mode, ApprovalMode::Supervised);
            ensure!(!bench.run_merged());
            git_in(
                &bench.repo,
                bench.repo.root(),
                &["checkout", "--", "README.md"],
            )?;
            bench.answer(&edited, choose(TRY_AGAIN))
        })
        .await?;
    assert_eq!(done.flow, Flow::Finish(RunOutcome::Merged));
    bench.merged_into("HEAD")?;
    Ok(())
}

/// An uncommitted edit to a file the run leaves alone rides along with the fast-forward, so
/// it neither stops the merge nor sends the question back to your rules again and again.
#[tokio::test]
async fn an_edit_to_a_file_the_run_leaves_alone_rides_along() -> anyhow::Result<()> {
    let bench = Bench::new(None).await?;
    bench.repo.write("README.md", "# Test repo, my notes\n")?;
    let done = bench
        .run(async {
            let first = bench.next_merge_approval(&[]).await?;
            bench.answer(&first, AnswerAction::Merge)
        })
        .await?;
    assert_eq!(done.flow, Flow::Finish(RunOutcome::Merged));
    bench.merged_into("HEAD")?;
    assert_eq!(
        std::fs::read_to_string(bench.repo.root().join("README.md"))?,
        "# Test repo, my notes\n"
    );
    let merge_tasks = bench.host.read(|state| {
        state
            .approvals
            .by_id
            .values()
            .filter(|record| record.opened.kind == ApprovalKind::Merge)
            .count()
    });
    assert_eq!(merge_tasks, 1, "asked once");
    Ok(())
}

/// Merge later pauses the run at the merge, waiting to merge rather than failed, and resuming
/// acts on the same approval once the way is clear.
#[tokio::test]
async fn merge_later_pauses_and_resuming_merges() -> anyhow::Result<()> {
    let bench = Bench::new(None).await?;
    bench.repo.write("store.rs", "// mine\n")?;
    let call = step_call()?;
    let step = merge_step(&bench.host, &call);
    let answering = async {
        let first = bench.next_merge_approval(&[]).await?;
        bench.answer(&first, AnswerAction::Merge)?;
        let asked = bench
            .in_the_way_approval(std::slice::from_ref(&first.id))
            .await?;
        bench.answer(&asked, choose(MERGE_LATER))
    };
    let (done, answered) = tokio::join!(
        tokio::time::timeout(deadline::scaled(Duration::from_secs(20)), step),
        answering
    );
    answered?;
    match done.context("the merge step timed out")? {
        Err(EngineError::Interrupted) => {}
        other => bail!("expected the run to pause, got {other:?}"),
    }
    assert!(bench.host.read(|state| state.run.paused));
    assert!(!bench.run_merged());
    assert_eq!(
        bench
            .host
            .read(|state| state.merge.waiting_to_merge.clone()),
        ["store.rs"]
    );
    assert!(
        bench
            .notes()
            .iter()
            .any(|note| note.contains("/resume run-") && note.contains("waits to merge"))
    );

    std::fs::remove_file(bench.repo.root().join("store.rs"))?;
    let done = bench.run(async { Ok(()) }).await?;
    assert_eq!(done.flow, Flow::Finish(RunOutcome::Merged));
    assert!(
        bench
            .host
            .read(|state| state.merge.waiting_to_merge.is_empty())
    );
    Ok(())
}

/// Under `merge_rule = "none"` a fallback's debt is shown, but nothing counts as
/// unreviewed, so it never holds up the merge.
#[tokio::test]
async fn debt_under_merge_rule_none_is_not_unreviewed() -> anyhow::Result<()> {
    let debt = QualityDebt {
        since: 1,
        by: worker(CODEX),
        needs: worker(OPUS),
    };
    let mut settings = suite_settings();
    settings.fallback.merge_rule = Some(MergeRule::None);
    let bench = Bench::with(
        vec![spec(&TaskId::numbered(1))],
        Some(debt.clone()),
        settings,
    )
    .await?;
    let done = bench
        .run(async {
            let question = bench.next_merge_approval(&[]).await?;
            let ApprovalPacket::Merge(packet) = &question.packet else {
                bail!("not a merge packet");
            };
            ensure!(packet.rows[0].debt == vec![debt.clone()]);
            let evidence = approvals::evidence_of(&bench.host, &question)?;
            ensure!(
                evidence.facts.unreviewed_debt == Some(Vec::new()),
                "{:?}",
                evidence.facts.unreviewed_debt
            );
            ensure!(evidence.facts.irreversible_actions == Some(Vec::new()));
            bench.answer(&question, AnswerAction::Merge)
        })
        .await?;
    assert_eq!(done.flow, Flow::Finish(RunOutcome::Merged));
    Ok(())
}

/// Only tasks that passed merge. When none did, the merge never opens, nothing lands, and the
/// run ends saying what stopped each task.
#[tokio::test]
async fn with_no_task_passed_the_merge_never_opens() -> anyhow::Result<()> {
    let task = TaskId::numbered(1);
    let bench = Bench::with(vec![spec(&task)], None, suite_settings()).await?;
    let before = bench.repo.head()?;
    bench.host.record(
        Some(&task),
        None,
        RunEventKind::Task(TaskEvent::State {
            from: Some(TaskState::Merged),
            to: TaskState::Blocked,
            reason: "waiting on your answer about the key prefix".to_owned(),
        }),
    )?;

    let done = bench.run(async { Ok(()) }).await?;
    assert_eq!(done.flow, Flow::Finish(RunOutcome::Failed));
    let why = done.summary.unwrap_or_default();
    assert_eq!(
        why,
        "Nothing was merged, since no task passed: \"Redis store\" is blocked on a question \
         (waiting on your answer about the key prefix)."
    );
    let events = bench.sink.events();
    assert!(!events.iter().any(|event| matches!(
        &event.kind,
        RunEventKind::Approval(ApprovalEvent::Opened(opened))
            if opened.kind == ApprovalKind::Merge
    )));
    assert!(events.iter().any(|event| matches!(
        &event.kind,
        RunEventKind::Note(note) if note.text == why
    )));
    assert_eq!(bench.repo.head()?, before, "nothing merged");
    Ok(())
}

/// A worker's note is kept under Argonauts Code's home before the merge approval opens. It is
/// never part of what lands, and the run review after the landing commits nothing.
#[tokio::test]
async fn run_review_notes_stay_out_of_the_merge() -> anyhow::Result<()> {
    let mut settings = suite_settings();
    settings.evolution.propose_every = 0;
    let bench = Bench::with(vec![spec(&TaskId::numbered(1))], None, settings).await?;
    let task = TaskId::numbered(1);
    let report = TaskReport {
        status: ReportStatus::Done,
        summary: "Built the store.".to_owned(),
        files_changed: vec!["store.rs".to_owned()],
        tests_run: Vec::new(),
        concerns: Vec::new(),
        questions: Vec::new(),
        change_requests: Vec::new(),
        memory_proposals: vec![MemoryProposal {
            topic: "store".to_owned(),
            text: "The store keeps keys for an hour.".to_owned(),
        }],
        answer: String::new(),
    };
    bench.host.record(
        Some(&task),
        None,
        RunEventKind::Task(TaskEvent::Reported(Box::new(report))),
    )?;
    let note = ".argonauts/notes/store.md";

    let done = bench
        .run(async {
            let opened = bench.next_merge_approval(&[]).await?;
            let evidence = approvals::evidence_of(&bench.host, &opened)?;
            ensure!(
                evidence.facts.conductor_paths.is_empty(),
                "{:?}",
                evidence.facts.conductor_paths
            );
            ensure!(!evidence.facts.agent_paths.contains(&note.to_owned()));
            bench.answer(&opened, AnswerAction::Merge)
        })
        .await?;
    assert_eq!(done.flow, Flow::Finish(RunOutcome::Merged));
    let merged = bench.repo.head()?;
    let tracked = bench.repo.git(&["ls-tree", "-r", "--name-only", &merged])?;
    assert!(!tracked.contains(".argonauts/"), "{tracked}");
    let kept = argonauts_crew::divide::repo_state_dir(&bench.host).join("notes/store.md");
    assert!(std::fs::read_to_string(kept)?.contains("keeps keys for an hour"));

    run_review::run(&bench.host).await?;
    let events = bench.sink.events();
    assert!(!events.iter().any(|event| matches!(
        event.kind,
        RunEventKind::RunReview(RunReviewEvent::Committed { .. })
    )));
    assert_eq!(bench.repo.head()?, merged, "nothing merged after the merge");
    Ok(())
}

#[tokio::test]
async fn the_final_check_skips_a_main_that_did_not_move() -> anyhow::Result<()> {
    let bench = Bench::new(None).await?;
    let check = final_check::run(&bench.host).await?;
    assert!(check.passed && check.suite_ran());
    assert!(
        !bench
            .merge_events()
            .iter()
            .any(|event| matches!(event, MergeEvent::MainMoved { .. }))
    );
    Ok(())
}

/// The suite's result as its command leaves it: failing, with the counts it printed.
fn suite_failed(summary: &str) -> CheckOutcome {
    CheckOutcome {
        name: "suite".to_owned(),
        kind: CheckKind::Command,
        passed: false,
        command: Some(vec!["true".to_owned()]),
        exit_code: Some(1),
        duration_ms: 1,
        summary: summary.to_owned(),
        output_tail: String::new(),
        violations: Vec::new(),
        sandboxed: true,
        skipped: false,
    }
}

/// A suite that fails once the run's work is in is run on the starting code too. Failing there
/// the same way, the run didn't break it: the final check passes and says so. Before, a test
/// that already failed kept every run's work out.
#[tokio::test]
async fn the_final_check_passes_what_already_failed_at_the_start() -> anyhow::Result<()> {
    let bench = Bench::new(None).await?;
    let checks = &bench.services.checks;
    checks.push(suite_failed("15 passed, 1 failed"));
    checks.push_at_start(suite_failed("15 passed, 1 failed"));
    let check = final_check::run(&bench.host).await?;
    assert_eq!(
        check.checks[0].summary,
        "15 passed, 1 failed, as at the start"
    );
    assert!(check.passed, "{:?}", check.checks);
    assert!(
        bench
            .merge_events()
            .iter()
            .any(|event| matches!(event, MergeEvent::FinalCheck { passed: true }))
    );
    // The starting code ran the suite's own command, in a copy of its own.
    let asked = checks.start_requests();
    assert_eq!(asked.len(), 1, "{asked:?}");
    assert_eq!(asked[0].name, "suite");
    assert_ne!(asked[0].workdir, bench.run_tree);
    Ok(())
}

/// A suite that fails worse than it did on the starting code fails the final check, and its
/// result says how the start compares.
#[tokio::test]
async fn the_final_check_fails_what_the_run_broke() -> anyhow::Result<()> {
    let bench = Bench::new(None).await?;
    let checks = &bench.services.checks;
    checks.push(suite_failed("14 passed, 2 failed"));
    checks.push_at_start(suite_failed("15 passed, 1 failed"));
    let check = final_check::run(&bench.host).await?;
    assert!(!check.passed);
    assert_eq!(
        check.checks[0].summary,
        "14 passed, 2 failed (at the start: 15 passed, 1 failed)"
    );
    // One that passed on the starting code is the run's to answer for, as it was.
    let bench = Bench::new(None).await?;
    bench
        .services
        .checks
        .push(suite_failed("15 passed, 1 failed"));
    let check = final_check::run(&bench.host).await?;
    assert!(!check.passed);
    assert_eq!(check.checks[0].summary, "15 passed, 1 failed");
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn request_changes_sends_back_only_the_named_tasks() -> anyhow::Result<()> {
    let (one, two) = (TaskId::numbered(1), TaskId::numbered(2));
    let tasks = [&one, &two]
        .map(|task| TaskSpec {
            reviewers: vec![worker(CODEX)],
            ..spec(task)
        })
        .to_vec();
    let bench = Bench::with(tasks, None, suite_settings()).await?;
    let worker = bench
        .services
        .workers
        .add(fixing_worker(OPUS, "anthropic", "renamed.rs"));
    bench.services.workers.add(reviewer(CODEX, "codex", &[]));

    let done = bench
        .run(async {
            let first = bench.next_merge_approval(&[]).await?;
            bench.answer_with(
                &first,
                AnswerAction::RequestChanges,
                Some("Rename the store"),
                vec![two.clone()],
            )?;
            let second = bench
                .next_merge_approval(std::slice::from_ref(&first.id))
                .await?;
            bench.answer(&second, AnswerAction::KeepBranch)
        })
        .await?;
    assert_eq!(done.flow, Flow::Finish(RunOutcome::BranchLeft));

    // Only the named task went back, and its worker got your note.
    assert_eq!(bench.sent_back(), std::slice::from_ref(&two));
    let specs = worker.specs();
    assert_eq!(specs.len(), 1);
    assert_eq!(specs[0].task.as_ref(), Some(&two));
    assert!(
        specs[0]
            .prompt
            .iter()
            .any(|part| part.text.contains("Rename the store"))
    );
    // It merged again, and the final check ran on the new head.
    let merges = bench
        .merge_events()
        .into_iter()
        .filter(|event| matches!(event, MergeEvent::Merged { task, .. } if *task == two))
        .count();
    assert_eq!(merges, 2);
    assert!(bench.run_tree.join("renamed.rs").exists());
    assert_eq!(bench.final_checks(), 2);
    let state = bench
        .host
        .read(|state| [&one, &two].map(|task| state.tasks.get(task).map(|record| record.state)));
    assert_eq!(state, [Some(TaskState::Merged); 2]);
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn an_upgrade_review_that_asks_for_changes_sends_the_task_back() -> anyhow::Result<()> {
    let debt = QualityDebt {
        since: 1,
        by: worker(CODEX),
        needs: worker(OPUS),
    };
    let mut settings = suite_settings();
    settings.crew = vec![worker(OPUS), worker(CODEX), worker(GEMINI)].into();
    settings.roles.clear();
    settings.fallback.unknown_tier = Some(UnknownTier::Equal);
    let task = TaskSpec {
        reviewers: vec![worker(GEMINI)],
        ..spec(&TaskId::numbered(1))
    };
    let bench = Bench::with(vec![task], Some(debt), settings).await?;
    bench.services.health.set(
        &worker(OPUS).agent_id(),
        HealthState::Down,
        Some("usage limit"),
    );
    let worker = bench
        .services
        .workers
        .add(fixing_worker(CODEX, "codex", "reviewed.rs"));
    bench
        .services
        .workers
        .add(reviewer(GEMINI, "google", &["request_changes"]));

    let done = bench
        .run(async {
            let first = bench.next_merge_approval(&[]).await?;
            ensure!(keys(&first).contains(&'a'), "{:?}", keys(&first));
            bench.answer(&first, AnswerAction::UpgradeReview)?;
            let second = bench
                .next_merge_approval(std::slice::from_ref(&first.id))
                .await?;
            let ApprovalPacket::Merge(packet) = &second.packet else {
                bail!("not a merge packet");
            };
            ensure!(packet.rows[0].debt.is_empty(), "{:?}", packet.rows[0].debt);
            bench.answer(&second, AnswerAction::KeepBranch)
        })
        .await?;
    assert_eq!(done.flow, Flow::Finish(RunOutcome::BranchLeft));
    assert_eq!(bench.sent_back(), [TaskId::numbered(1)]);
    let specs = worker.specs();
    assert_eq!(specs.len(), 1);
    assert!(
        specs[0]
            .prompt
            .iter()
            .any(|part| part.text.contains("the upgrade review asked for changes"))
    );
    assert!(bench.run_tree.join("reviewed.rs").exists());
    Ok(())
}

#[tokio::test]
async fn a_woken_run_acts_on_your_answer_without_checking_again() -> anyhow::Result<()> {
    let bench = Bench::new(None).await?;
    // The run waits at the approval, and you answer while it's waiting.
    let call = step_call()?;
    let waiting =
        tokio::time::timeout(Duration::from_secs(2), merge_step(&bench.host, &call)).await;
    ensure!(waiting.is_err(), "the step ended without your answer");
    let opened = bench.next_merge_approval(&[]).await?;
    bench.answer(&opened, AnswerAction::KeepBranch)?;
    let phases = bench.phase_events();

    // A suite that would fail now changes nothing: the answer stands on the same heads.
    bench.services.checks.fail_next("flaky");
    let done = bench.run(async { Ok(()) }).await?;
    assert_eq!(done.flow, Flow::Finish(RunOutcome::BranchLeft));
    assert_eq!(bench.final_checks(), 1);
    assert_eq!(bench.services.checks.requests().len(), 1);
    let merge_tasks = bench.host.read(|state| {
        state
            .approvals
            .by_id
            .values()
            .filter(|record| record.opened.kind == ApprovalKind::Merge)
            .count()
    });
    assert_eq!(merge_tasks, 1);
    // Only the merge phase's end is new.
    assert_eq!(bench.phase_events(), phases + 1);
    Ok(())
}
