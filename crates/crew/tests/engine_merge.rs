//! The merge step: main merged in when it moved, the final check, the merge checkpoint, landing
//! by your answer, and the merge rule for cards with quality debt. Each test runs the step on a
//! real temp repo through the engine's `Host` and answers its checkpoint as you would.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use anyhow::{Context, bail, ensure};
use serde_json::json;
use time::macros::datetime;
use tokio::sync::mpsc;

use argonauts_core::api::CheckpointAnswer;
use argonauts_core::engine::{EngineCommand, RepoContext};
use argonauts_core::events::card::{
    CardBudget, CardEvent, CardKind, CardSpec, CardState, IntegrationEvent,
};
use argonauts_core::events::checkpoint::{
    AnswerAction, CheckpointDecided, CheckpointEvent, CheckpointKind, CheckpointOpened,
    CheckpointPacket, FreeText, PlanPacket,
};
use argonauts_core::events::goal::{Criterion, GoalEvent, GoalFile, GoalSource};
use argonauts_core::events::plan::{BudgetPlan, TaskGraph};
use argonauts_core::events::run::{Party, RunLifecycle, RunMode, RunOutcome, RunStarted};
use argonauts_core::events::shared::RetroEvent;
use argonauts_core::events::vendor::VendorEvent;
use argonauts_core::events::workflow::{StepStarted, WorkflowEvent, WorkflowStarted};
use argonauts_core::events::{RunEvent, RunEventKind};
use argonauts_core::handoff::QualityDebt;
use argonauts_core::limits::{HealthState, VendorHealth};
use argonauts_core::names::{CheckpointId, RoleName, WorkerRef, WorkflowId};
use argonauts_core::policy::{HardLimit, PolicyOutcome, Verdict};
use argonauts_core::settings::Settings;
use argonauts_core::settings::approvals::{ApprovalMode, DecideWith};
use argonauts_core::settings::budget::{BudgetAmount, BudgetUnit};
use argonauts_core::settings::delivery::{DeliveryCommits, DeliveryWrite, UnbornLanding};
use argonauts_core::settings::handoff::{MergeRule, UnknownTier};
use argonauts_core::settings::protocol::GateCommand;
use argonauts_core::steps::StepName;
use argonauts_core::steps::card::{CardReport, MemoryProposal, ReportStatus};
use argonauts_core::worker::Access;
use argonauts_core::workflow::{StandardPhase, Step, StepKind, StepRef, Workflow};
use argonauts_core::{CardId, RepoId, RunId, unborn};
use argonauts_crew::checkpoints;
use argonauts_crew::engine::host::Host;
use argonauts_crew::engine::shared::Shared;
use argonauts_crew::error::EngineError;
use argonauts_crew::integrate::in_the_way::{MERGE_LATER, MOVE_ASIDE, TRY_AGAIN};
use argonauts_crew::integrate::{branch, final_check, merge};
use argonauts_crew::retro;
use argonauts_crew::state::RunState;
use argonauts_crew::workflow::{Flow, StepAt, StepCall, StepDone};
use argonauts_testkit::repo::TempRepo;
use argonauts_testkit::services::{FakeServices, MemorySink, TEST_RUN, engine_context_in};
use argonauts_testkit::worker::{FakeSession, FakeWorker};

const OPUS: &str = "claude-code/opus";
const CODEX: &str = "codex/gpt-5.6";
const GEMINI: &str = "agy/gemini-3";
/// The card's ring reviewer, of another family than its planned owner.
const AGY: &str = "agy/gemini-3.6";
/// The merge agent. Each test that wants its answers adds it, so the other agents' answers
/// go to the sessions they were written for.
const MERGER: &str = "opencode/merger";

fn worker(name: &str) -> WorkerRef {
    name.parse().expect("a valid worker")
}

/// A crew run whose one card is integrated: the integration branch holds its work, and main is
/// checked out in the repository.
struct Bench {
    services: FakeServices,
    repo: TempRepo,
    host: Host,
    sink: Arc<MemorySink>,
    integration: PathBuf,
    _commands: mpsc::Sender<EngineCommand>,
}

impl Bench {
    async fn new(debt: Option<QualityDebt>) -> anyhow::Result<Self> {
        Self::with(vec![spec(&CardId::numbered(1))], debt, suite_settings()).await
    }

    /// `cards` all integrated, in order, the first one with `debt`.
    async fn with(
        cards: Vec<CardSpec>,
        debt: Option<QualityDebt>,
        settings: Settings,
    ) -> anyhow::Result<Self> {
        Self::with_work(cards, debt, settings, &[]).await
    }

    /// As [`Bench::with`], with `files` beside the first card's work. The engine's own git
    /// commits them, so the leave-out settings decide what goes in.
    async fn with_work(
        cards: Vec<CardSpec>,
        debt: Option<QualityDebt>,
        settings: Settings,
        files: &[(&str, &str)],
    ) -> anyhow::Result<Self> {
        let repo = TempRepo::new()?;
        let base = repo.head()?;
        Self::in_repo(repo, &base, cards, debt, settings, files).await
    }

    /// As [`Bench::with_work`], for a run that started from `base` in `repo`.
    async fn in_repo(
        repo: TempRepo,
        base: &str,
        cards: Vec<CardSpec>,
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
            (None, goal_approved()?),
            (None, plan_opened(&cards)),
            (None, plan_approved()),
            (None, workflow_started()?),
            (None, merge_step_started()),
        ];
        for card in &cards {
            let id = Some(card.id.clone());
            history.push((
                id.clone(),
                RunEventKind::Card(CardEvent::Created(Box::new(card.clone()))),
            ));
            history.push((
                id,
                RunEventKind::Card(CardEvent::State {
                    from: None,
                    to: CardState::Integrated,
                    reason: "merged and tested".to_owned(),
                }),
            ));
        }
        if let (Some(debt), Some(card)) = (debt, cards.first()) {
            let card = card.id.clone();
            history.push((
                Some(card.clone()),
                RunEventKind::Card(CardEvent::Reassigned {
                    from: Some(worker(OPUS)),
                    to: debt.by.clone(),
                    by: Party::Rules,
                    rule_override: None,
                }),
            ));
            history.push((Some(card), RunEventKind::Card(CardEvent::QualityDebt(debt))));
        }
        let history: Vec<RunEvent> = history
            .into_iter()
            .zip(0..)
            .map(|((card, kind), seq)| event(seq, card, kind))
            .collect();
        let (context, sink, commands) =
            engine_context_in(&services, settings, layout, history.clone());
        let shared = Shared::new(&context, RunState::fold(&history))?;
        let host = Host::new(Arc::new(shared));

        // Each card's work, merged into the integration branch as the conductor does.
        let integration = branch::ensure_integration(&host).await?;
        for (i, card) in cards.iter().enumerate() {
            let (file, text) = if i == 0 {
                ("store.rs".to_owned(), "fn store() {}\n".to_owned())
            } else {
                (format!("{}.rs", card.id), format!("// {}\n", card.id))
            };
            let message = format!("Merge {}", card.id);
            if i == 0 && !files.is_empty() {
                std::fs::write(integration.join(&file), &text)?;
                for (path, text) in files {
                    let path = integration.join(path);
                    if let Some(folder) = path.parent() {
                        std::fs::create_dir_all(folder)?;
                    }
                    std::fs::write(path, text)?;
                }
                host.git()
                    .commit_work(&integration, &message)
                    .await?
                    .sha
                    .context("nothing to commit")?;
            } else {
                commit_in(&repo, &integration, &file, &text, &message)?;
            }
            host.record(
                None,
                None,
                RunEventKind::Integration(IntegrationEvent::Merged {
                    card: card.id.clone(),
                    sha: git_in(&repo, &integration, &["rev-parse", "HEAD"])?,
                }),
            )?;
        }
        Ok(Self {
            services,
            repo,
            host,
            sink,
            integration,
            _commands: commands,
        })
    }

    fn integration_head(&self) -> anyhow::Result<String> {
        git_in(&self.repo, &self.integration, &["rev-parse", "HEAD"])
    }

    /// `rev` holds the run as it lands by default: the run's files, in one commit of its own
    /// with one parent, and none of the run's own commits or merges.
    fn landed_on(&self, rev: &str) -> anyhow::Result<()> {
        let root = self.repo.root();
        let tree = |rev: &str| git_in(&self.repo, root, &["rev-parse", &format!("{rev}^{{tree}}")]);
        let head = self.integration_head()?;
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

    fn integration_events(&self) -> Vec<IntegrationEvent> {
        self.sink
            .events()
            .into_iter()
            .filter_map(|event| match event.kind {
                RunEventKind::Integration(event) => Some(event),
                _ => None,
            })
            .collect()
    }

    /// Waits for a merge checkpoint other than the ones in `seen`.
    async fn next_merge_card(&self, seen: &[CheckpointId]) -> anyhow::Result<CheckpointOpened> {
        for _ in 0..400 {
            let found = self.host.read(|state| {
                state
                    .checkpoints
                    .open()
                    .find(|record| {
                        record.opened.kind == CheckpointKind::Merge
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

    /// Waits for an open "original is back" card.
    async fn return_card(&self) -> anyhow::Result<CheckpointOpened> {
        for _ in 0..400 {
            let found = self.host.read(|state| {
                state
                    .checkpoints
                    .open_for(CheckpointKind::OriginalReturned, Some(&CardId::numbered(1)))
                    .map(|record| record.opened.clone())
            });
            if let Some(opened) = found {
                return Ok(opened);
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        bail!("no \"original is back\" question opened")
    }

    /// The run saw `name`'s vendor go down, and the board has it healthy again.
    fn out_and_back(&self, name: &str) -> anyhow::Result<()> {
        let health = VendorHealth {
            vendor: worker(name).vendor(),
            state: HealthState::Down,
            since: datetime!(2026-09-30 10:00 UTC),
            reason: Some("usage limit".to_owned()),
            next_test_at: None,
            disabled: None,
        };
        self.host.record(
            None,
            None,
            RunEventKind::Vendor(VendorEvent::Health(health)),
        )?;
        Ok(())
    }

    fn answer(&self, opened: &CheckpointOpened, action: AnswerAction) -> anyhow::Result<()> {
        self.answer_with(opened, action, None, Vec::new())
    }

    fn answer_with(
        &self,
        opened: &CheckpointOpened,
        action: AnswerAction,
        text: Option<&str>,
        cards: Vec<CardId>,
    ) -> anyhow::Result<()> {
        checkpoints::answer(
            &self.host,
            CheckpointAnswer {
                id: opened.id.clone(),
                action,
                text: text.map(str::to_owned),
                evidence_sha256: opened.evidence_sha256.clone(),
                cards,
            },
        )
        .map_err(anyhow::Error::msg)
    }

    /// Cards moved out of `Integrated`, in order.
    fn sent_back(&self) -> Vec<CardId> {
        self.sink
            .events()
            .into_iter()
            .filter_map(|event| match (event.card_id, event.kind) {
                (
                    Some(card),
                    RunEventKind::Card(CardEvent::State {
                        from: Some(CardState::Integrated),
                        ..
                    }),
                ) => Some(card),
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
        self.integration_events()
            .iter()
            .filter(|event| matches!(event, IntegrationEvent::FinalCheck { .. }))
            .count()
    }

    /// Runs the merge step while `answering` plays your part.
    async fn run(
        &self,
        answering: impl std::future::Future<Output = anyhow::Result<()>>,
    ) -> anyhow::Result<StepDone> {
        let call = step_call()?;
        let step = merge::merge_step(&self.host, &call);
        let (done, answered) = tokio::join!(
            async { tokio::time::timeout(Duration::from_secs(20), step).await },
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
    settings.checks.full_suite = vec![GateCommand {
        name: "suite".to_owned(),
        command: vec!["true".to_owned()],
    }];
    settings.crew.merger = vec![worker(MERGER)];
    settings
}

/// An owner that writes `file` in its worktree and reports done.
fn fixing_owner(name: &str, family: &str, file: &'static str) -> FakeWorker {
    FakeWorker::harness(name, family).respond_with(move |spec| {
        let _ = std::fs::write(spec.workdir.join(file), "fixed\n");
        FakeSession::structured(json!({
            "status": "done", "summary": "made the change", "files_changed": [file],
            "tests_run": [], "concerns": [], "questions": [], "change_requests": [],
            "memory_proposals": []
        }))
    })
}

/// A reviewer that gives `verdicts` in turn, then approves.
fn reviewer(name: &str, family: &str, verdicts: &'static [&'static str]) -> FakeWorker {
    let given = AtomicUsize::new(0);
    FakeWorker::harness(name, family).respond_with(move |_| {
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

fn event(seq: u64, card: Option<CardId>, kind: RunEventKind) -> RunEvent {
    RunEvent {
        seq,
        ts: datetime!(2026-09-30 10:00 UTC),
        run_id: RunId::new(TEST_RUN).expect("the test run id"),
        card_id: card,
        session: None,
        source: "conductor".to_owned(),
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
        triage: None,
        chat_turn: None,
        continues: None,
        repo_root: repo.root().to_path_buf(),
        base_branch: Some("main".to_owned()),
        base_commit: base.to_owned(),
        uncommitted: Vec::new(),
        uncommitted_included: false,
        settings_sha256: String::new(),
        settings: serde_json::Value::Null,
        overrides: Vec::new(),
        harness_versions: Default::default(),
        version: "0.0.1".to_owned(),
        front_end: None,
        instructions: Vec::new(),
        required_steps: Vec::new(),
    })))
}

/// Owner sessions of cards sent back need the approved goal.
fn goal_approved() -> anyhow::Result<RunEventKind> {
    let file = GoalFile {
        run: RunId::new(TEST_RUN)?,
        version: 1,
        summary: "Add a Redis store".to_owned(),
        criteria: vec![Criterion::new("G1", "It stores.")],
        in_scope: Vec::new(),
        out_of_scope: Vec::new(),
        assumptions: Vec::new(),
        answers: Vec::new(),
        source: GoalSource::Restatements,
        path: PathBuf::from(".argonauts/goal/run-01.md"),
        sha256: "0".repeat(64),
    };
    Ok(RunEventKind::Goal(GoalEvent::Approved {
        file,
        by: Party::User,
    }))
}

fn plan_opened(cards: &[CardSpec]) -> RunEventKind {
    let plan = CheckpointOpened {
        id: CheckpointId::numbered(1),
        kind: CheckpointKind::Plan,
        title: "Approve this plan?".to_owned(),
        card: None,
        packet: CheckpointPacket::Plan(Box::new(PlanPacket {
            summary: "The plan".to_owned(),
            judging: None,
            near_tie: false,
            decisions: Vec::new(),
            graph: TaskGraph {
                cards: cards.to_vec(),
                critical_path: Vec::new(),
            },
            acceptance_tests: Vec::new(),
            budget: BudgetPlan {
                lines: Vec::new(),
                total: BudgetAmount {
                    unit: BudgetUnit::Usd,
                    amount: 10.0,
                },
                estimate_low: 0.0,
                estimate_high: 0.0,
            },
            new_dependencies: Vec::new(),
            flags: Vec::new(),
            workflow: None,
        })),
        evidence_sha256: "0".repeat(64),
        mode: ApprovalMode::Supervised,
        decide_with: DecideWith::Rules,
        options: Vec::new(),
        free_text: FreeText::None,
        needs_you: true,
    };
    RunEventKind::Checkpoint(CheckpointEvent::Opened(Box::new(plan)))
}

fn plan_approved() -> RunEventKind {
    RunEventKind::Checkpoint(CheckpointEvent::Decided(CheckpointDecided {
        id: CheckpointId::numbered(1),
        action: AnswerAction::Approve,
        text: None,
        by: Party::User,
        evidence_sha256: "0".repeat(64),
        shadow: None,
        cards: Vec::new(),
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

fn spec(card: &CardId) -> CardSpec {
    CardSpec {
        id: card.clone(),
        title: "Redis store".to_owned(),
        task: "Build the Redis store".to_owned(),
        kind: CardKind::Build,
        owned_paths: Vec::new(),
        read_only_paths: Vec::new(),
        forbidden_paths: Vec::new(),
        hidden_paths: Vec::new(),
        depends_on: Vec::new(),
        criteria: Vec::new(),
        owner: Some(worker(OPUS)),
        reviewers: vec![worker(AGY)],
        budget: CardBudget::default(),
        predictions: None,
        item: None,
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

fn keys(opened: &CheckpointOpened) -> Vec<char> {
    opened.options.iter().map(|option| option.key).collect()
}

#[tokio::test]
async fn main_moved_merged_in_and_rechecked() -> anyhow::Result<()> {
    let bench = Bench::new(None).await?;
    let moved_before = bench.move_main("before.txt")?;

    let done = bench
        .run(async {
            let opened = bench.next_merge_card(&[]).await?;
            // The card asks its question by name; the run's id stays in the detail views.
            assert_eq!(opened.title, "Merge this run into main?");
            // Main moves again while the merge waits for you.
            bench.move_main("while-waiting.txt")?;
            bench.answer(&opened, AnswerAction::Merge)
        })
        .await?;
    assert_eq!(done.flow, Flow::Finish(RunOutcome::Merged));

    let events = bench.integration_events();
    let moved: Vec<&String> = events
        .iter()
        .filter_map(|event| match event {
            IntegrationEvent::MainMoved { to, .. } => Some(to),
            _ => None,
        })
        .collect();
    assert_eq!(moved.len(), 2, "{events:?}");
    assert_eq!(moved[0], &moved_before);
    let merged_in = events
        .iter()
        .filter(|event| matches!(event, IntegrationEvent::MainMergedIn { .. }))
        .count();
    assert_eq!(merged_in, 2);
    let checks: Vec<bool> = events
        .iter()
        .filter_map(|event| match event {
            IntegrationEvent::FinalCheck { passed } => Some(*passed),
            _ => None,
        })
        .collect();
    assert_eq!(checks, [true, true], "the suite ran again on the new main");

    // The run's own diff didn't change, so the approval carried over and main fast-forwarded.
    bench.landed_on("HEAD")?;
    for file in ["store.rs", "before.txt", "while-waiting.txt"] {
        assert!(bench.repo.root().join(file).exists(), "{file}");
    }
    let requests = bench.services.gates.requests();
    assert_eq!(requests.len(), 2);
    assert!(
        requests
            .iter()
            .all(|request| request.workdir == bench.integration)
    );
    Ok(())
}

/// A merge agent that resolves the conflict in `store.rs` with `resolution`, or reports
/// `status` without touching it, and writes the message the change lands with.
fn merging(status: &'static str, resolution: Option<&'static str>) -> FakeWorker {
    FakeWorker::harness(MERGER, "opencode").respond_with(move |spec| {
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

/// Your branch moved and changed the file the run wrote too: the merge agent resolves the
/// conflict on the run's branch, the suite runs on its merge, and the run lands with both.
#[tokio::test]
async fn a_main_that_conflicts_goes_to_the_merge_agent() -> anyhow::Result<()> {
    let bench = Bench::new(None).await?;
    bench.repo.write("store.rs", "fn main_store() {}\n")?;
    bench.repo.commit_all("a store on main")?;
    let both = "fn store() {}\nfn main_store() {}\n";
    let merger = bench.services.workers.add(merging("done", Some(both)));

    let done = bench
        .run(async {
            let opened = bench.next_merge_card(&[]).await?;
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
    assert_eq!(specs[0].card, None);
    assert_eq!(specs[0].access, Access::Build);
    assert_eq!(specs[0].workdir, bench.integration);
    assert_eq!(
        std::fs::read_to_string(bench.repo.root().join("store.rs"))?,
        both
    );
    let events = bench.integration_events();
    assert!(
        events
            .iter()
            .any(|event| matches!(event, IntegrationEvent::MainMergedIn { .. })),
        "{events:?}"
    );
    assert!(
        events
            .iter()
            .any(|event| matches!(event, IntegrationEvent::FinalCheck { passed: true })),
        "the suite ran on the merge agent's merge"
    );
    Ok(())
}

/// The merge agent writes the message the change lands with while the approval waits, and
/// the run's trailer follows it.
#[tokio::test]
async fn the_merge_agent_writes_the_landed_message() -> anyhow::Result<()> {
    let bench = Bench::new(None).await?;
    let merger = bench.services.workers.add(merging("done", None));

    let done = bench
        .run(async {
            let opened = bench.next_merge_card(&[]).await?;
            bench.answer(&opened, AnswerAction::Merge)
        })
        .await?;
    assert_eq!(done.flow, Flow::Finish(RunOutcome::Merged));
    bench.landed_on("HEAD")?;
    let message = bench.repo.git(&["log", "-1", "--format=%B", "HEAD"])?;
    assert_eq!(
        message.trim_end(),
        format!("Add a store\n\nKeeps what the app saves.\n\nArgonauts-Run: {TEST_RUN}")
    );
    let specs = merger.specs();
    assert_eq!(specs.len(), 1, "one message, written once");
    assert_eq!(specs[0].access, Access::ReadOnly);
    assert_eq!(specs[0].card, None);
    assert_eq!(specs[0].workdir, bench.integration);
    // It's the Merge step's agent, so the run's view shows it there, on its own model.
    let started = bench
        .host
        .read(|state| state.sessions.get(&specs[0].key).cloned())
        .context("the merge agent's session")?
        .started;
    assert_eq!(started.workflow_step, Some(step_ref()));
    assert_eq!(started.worker, worker(MERGER));
    assert_eq!(started.label.as_deref(), Some("merge:main"));
    Ok(())
}

/// The suite passed, then your branch moved and its new commits broke it: the merge agent
/// fixes what the merge broke on the run's branch, and the suite runs again. The run's own
/// change grew with the fix, so you approve again before it lands.
#[tokio::test]
async fn the_merge_agent_fixes_what_a_moved_main_broke() -> anyhow::Result<()> {
    let bench = Bench::new(None).await?;
    let merger =
        bench.services.workers.add(
            FakeWorker::harness(MERGER, "opencode").respond_with(|spec| {
                if spec.access != Access::Build {
                    return FakeSession::structured(
                        json!({ "subject": "Add a store", "body": "" }),
                    );
                }
                let _ = std::fs::write(spec.workdir.join("fixed.txt"), "fixed\n");
                FakeSession::structured(json!({
                    "status": "done", "summary": "fixed the store for the new main",
                    "files_changed": ["fixed.txt"], "tests_run": [], "concerns": [],
                    "questions": [], "change_requests": [], "memory_proposals": []
                }))
            }),
        );

    let done = bench
        .run(async {
            let first = bench.next_merge_card(&[]).await?;
            bench.move_main("while-waiting.txt")?;
            bench
                .services
                .gates
                .fail_next("the store breaks with the new main");
            bench.answer(&first, AnswerAction::Merge)?;
            let again = bench
                .next_merge_card(std::slice::from_ref(&first.id))
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
    assert_eq!(fixes[0].workdir, bench.integration);
    for file in ["store.rs", "while-waiting.txt", "fixed.txt"] {
        assert!(bench.repo.root().join(file).exists(), "{file}");
    }
    assert_eq!(
        bench.services.gates.requests().len(),
        3,
        "the suite, then after main came in, then after the fix"
    );
    let checks: Vec<bool> = bench
        .integration_events()
        .iter()
        .filter_map(|event| match event {
            IntegrationEvent::FinalCheck { passed } => Some(*passed),
            _ => None,
        })
        .collect();
    assert_eq!(checks, [true, true]);
    Ok(())
}

/// A merge agent that can't answer leaves the message plain code writes.
#[tokio::test]
async fn without_the_merge_agents_message_code_writes_it() -> anyhow::Result<()> {
    let bench = Bench::new(None).await?;
    bench.services.workers.add(
        FakeWorker::harness(MERGER, "opencode")
            .respond_with(|_| FakeSession::structured(json!({ "subject": " " }))),
    );

    let done = bench
        .run(async {
            let opened = bench.next_merge_card(&[]).await?;
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

/// When the merge agent says the two sides can't both hold, the merge is undone and the
/// approval says why, in the merge agent's words.
#[tokio::test]
async fn a_main_the_merge_agent_cannot_reconcile_is_left_to_you() -> anyhow::Result<()> {
    let bench = Bench::new(None).await?;
    let before = bench.integration_head()?;
    bench.repo.write("store.rs", "fn main_store() {}\n")?;
    bench.repo.commit_all("a store on main")?;
    bench.services.workers.add(merging("needs_decision", None));

    let call = step_call()?;
    let step = merge::merge_step(&bench.host, &call);
    let watching = async {
        let opened = bench.next_merge_card(&[]).await?;
        let evidence = checkpoints::evidence_of(&bench.host, &opened)?;
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
    assert_eq!(bench.integration_head()?, before, "the merge was undone");
    let merging = git_in(
        &bench.repo,
        &bench.integration,
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
            let opened = bench.next_merge_card(&[]).await?;
            assert_eq!(keys(&opened), ['y', 'p', 'b', 'k', 'x', 'd']);
            let CheckpointPacket::Merge(packet) = &opened.packet else {
                bail!("not a merge packet");
            };
            assert_eq!(
                packet.branch,
                branch::integration_branch(bench.host.run_id())
            );
            assert_eq!(packet.rows.len(), 1);
            assert!(packet.suite_passed_on_latest_main);
            assert!(packet.diff_path.exists());
            bench.answer(&opened, AnswerAction::Merge)
        })
        .await?;
    assert_eq!(done.flow, Flow::Finish(RunOutcome::Merged));
    bench.landed_on("HEAD")?;
    assert!(
        bench
            .integration_events()
            .contains(&IntegrationEvent::Landed {
                target: "main".to_owned(),
                sha: bench.repo.head()?,
                fast_forward: true,
            })
    );

    // Main not checked out: only its branch moves.
    let bench = Bench::new(None).await?;
    bench
        .repo
        .git(&["checkout", "--quiet", "-b", "elsewhere"])?;
    let done = bench
        .run(async {
            let opened = bench.next_merge_card(&[]).await?;
            bench.answer(&opened, AnswerAction::Merge)
        })
        .await?;
    assert_eq!(done.flow, Flow::Finish(RunOutcome::Merged));
    bench.landed_on("main")?;
    assert_eq!(bench.repo.git(&["branch", "--show-current"])?, "elsewhere");

    // A pull request: the command is recorded, and with no remote it isn't run.
    let bench = Bench::new(None).await?;
    let main_before = bench.repo.head()?;
    let done = bench
        .run(async {
            let opened = bench.next_merge_card(&[]).await?;
            bench.answer(&opened, AnswerAction::PullRequest)
        })
        .await?;
    assert_eq!(done.flow, Flow::Finish(RunOutcome::PullRequest));
    assert_eq!(bench.repo.head()?, main_before);
    let pull_request = bench
        .integration_events()
        .into_iter()
        .find_map(|event| match event {
            IntegrationEvent::PullRequest {
                branch,
                url,
                command,
            } => Some((branch, url, command)),
            _ => None,
        });
    let (pr_branch, url, command) = pull_request.context("a pull request event")?;
    assert_eq!(pr_branch, branch::integration_branch(bench.host.run_id()));
    assert_eq!(url, None);
    let command = command.context("the recorded command")?;
    assert_eq!(&command[..3], ["gh", "pr", "create"]);
    assert!(command.contains(&"Redis store".to_owned()));
    Ok(())
}

#[tokio::test]
async fn merge_refused_when_head_moved_after_approval() -> anyhow::Result<()> {
    let bench = Bench::new(None).await?;
    let first_id = std::sync::Mutex::new(None);
    let done = bench
        .run(async {
            let first = bench.next_merge_card(&[]).await?;
            *first_id.lock().expect("lock") = Some(first.id.clone());
            // Something moves the integration branch after the card was shown.
            commit_in(
                &bench.repo,
                &bench.integration,
                "late.rs",
                "late\n",
                "late change",
            )?;
            bench.answer(&first, AnswerAction::Merge)?;
            // Landing is refused and the checkpoint opens again with the new head.
            let second = bench
                .next_merge_card(std::slice::from_ref(&first.id))
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
            RunEventKind::Checkpoint(CheckpointEvent::Checked(check)) => {
                check.id == first_id
                    && check
                        .violations
                        .iter()
                        .any(|violation| violation.limit == HardLimit::HeadMoved)
            }
            _ => false,
        });
    assert!(refused, "no HeadMoved refusal recorded");
    // What landed is what you approved the second time, late change included.
    bench.landed_on("HEAD")?;
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
    let claude = worker(OPUS).vendor();
    bench
        .services
        .health
        .set(&claude, HealthState::Down, Some("usage limit"));
    bench.services.workers.add(
        FakeWorker::harness(OPUS, "claude").then(FakeSession::structured(json!({
            "verdict": "approve",
            "summary": "Checked Codex's work.",
            "comments": [],
            "tamper_suspected": false,
        }))),
    );

    let done = bench
        .run(async {
            // The original is out: the card lists the debt and offers to wait for it.
            let first = bench.next_merge_card(&[]).await?;
            let CheckpointPacket::Merge(packet) = &first.packet else {
                bail!("not a merge packet");
            };
            assert_eq!(packet.rows[0].debt, vec![debt.clone()]);
            let evidence = checkpoints::evidence_of(&bench.host, &first)?;
            // The card goes by its label, as the merge table names it.
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
                .next_merge_card(std::slice::from_ref(&first.id))
                .await?;
            let CheckpointPacket::Merge(packet) = &second.packet else {
                bail!("not a merge packet");
            };
            ensure!(packet.rows[0].debt.is_empty());
            let evidence = checkpoints::evidence_of(&bench.host, &second)?;
            ensure!(evidence.facts.unreviewed_debt == Some(Vec::new()));
            ensure!(!keys(&second).contains(&'w'));
            bench.answer(&second, AnswerAction::Merge)
        })
        .await?;
    assert_eq!(done.flow, Flow::Finish(RunOutcome::Merged));
    let cleared = bench.sink.events().into_iter().any(|event| {
        matches!(
            event.kind,
            RunEventKind::Card(CardEvent::DebtCleared { ref by }) if *by == worker(OPUS)
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

/// The original back while its card waits at the merge card: with `on_return = "ask"` its card
/// opens in place of the merge card, and its review clears the debt.
#[tokio::test]
async fn the_original_back_at_the_merge_card_asks_first() -> anyhow::Result<()> {
    let bench = Bench::new(Some(codex_debt())).await?;
    bench.out_and_back(OPUS)?;
    bench.services.workers.add(reviewer(OPUS, "claude", &[]));

    let done = bench
        .run(async {
            let back = bench.return_card().await?;
            ensure!(
                keys(&back) == ['r', 'o', 'u', 'y', 't', 'd'],
                "{:?}",
                keys(&back)
            );
            let merge_cards = bench.host.read(|state| {
                state
                    .checkpoints
                    .open_for(CheckpointKind::Merge, None)
                    .is_some()
            });
            ensure!(!merge_cards, "the merge approval opened beside it");
            bench.answer(&back, AnswerAction::UpgradeReview)?;
            let merge = bench.next_merge_card(&[]).await?;
            let CheckpointPacket::Merge(packet) = &merge.packet else {
                bail!("not a merge packet");
            };
            ensure!(packet.rows[0].debt.is_empty(), "{:?}", packet.rows[0].debt);
            bench.answer(&merge, AnswerAction::Merge)
        })
        .await?;
    assert_eq!(done.flow, Flow::Finish(RunOutcome::Merged));
    assert!(bench.sink.events().iter().any(|event| matches!(
        &event.kind,
        RunEventKind::Card(CardEvent::DebtCleared { by }) if *by == worker(OPUS)
    )));
    Ok(())
}

/// Keeping the replacement's work from the "original is back" card skips the original's
/// review, and the merge card lists the debt for you.
#[tokio::test]
async fn keeping_the_replacement_at_the_merge_card_leaves_the_debt() -> anyhow::Result<()> {
    let bench = Bench::new(Some(codex_debt())).await?;
    bench.out_and_back(OPUS)?;
    let original = bench.services.workers.add(reviewer(OPUS, "claude", &[]));

    let done = bench
        .run(async {
            let back = bench.return_card().await?;
            bench.answer(&back, AnswerAction::KeepReplacement)?;
            let merge = bench.next_merge_card(&[]).await?;
            let CheckpointPacket::Merge(packet) = &merge.packet else {
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

/// Giving the card back from the merge card: the original works on it from the run's branch,
/// it's integrated again, and its own review then clears the debt.
#[tokio::test]
async fn the_original_takes_its_card_back_from_the_merge_card() -> anyhow::Result<()> {
    let bench = Bench::new(Some(codex_debt())).await?;
    bench.out_and_back(OPUS)?;
    let original = bench
        .services
        .workers
        .add(FakeWorker::harness(OPUS, "claude").respond_with(|spec| {
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
    bench.services.workers.add(reviewer(AGY, "gemini", &[]));

    let done = bench
        .run(async {
            let back = bench.return_card().await?;
            bench.answer(&back, AnswerAction::HandBack)?;
            let merge = bench.next_merge_card(&[]).await?;
            let CheckpointPacket::Merge(packet) = &merge.packet else {
                bail!("not a merge packet");
            };
            ensure!(packet.rows[0].debt.is_empty(), "{:?}", packet.rows[0].debt);
            bench.answer(&merge, AnswerAction::KeepBranch)
        })
        .await?;
    assert_eq!(done.flow, Flow::Finish(RunOutcome::BranchLeft));
    assert_eq!(bench.sent_back(), [CardId::numbered(1)]);
    let owner = bench
        .host
        .read(|state| state.cards.get(&CardId::numbered(1))?.owner.clone());
    assert_eq!(owner, Some(worker(OPUS)));
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
    assert!(bench.integration.join("back.rs").exists());
    Ok(())
}

/// A merge card withdrawn while it waits, as `/rewind` does once the run's branch moves, opens
/// again on what's true now.
#[tokio::test]
async fn a_withdrawn_merge_card_opens_again() -> anyhow::Result<()> {
    let bench = Bench::new(None).await?;
    let done = bench
        .run(async {
            let first = bench.next_merge_card(&[]).await?;
            checkpoints::withdraw(
                &bench.host,
                &first.id,
                "you reverted task-001 to fallback-1",
            )?;
            let second = bench
                .next_merge_card(std::slice::from_ref(&first.id))
                .await?;
            let evidence = checkpoints::evidence_of(&bench.host, &second)?;
            ensure!(
                evidence
                    .agent_outputs
                    .iter()
                    .any(|line| line.contains("you reverted task-001")),
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
    /// The card asking about your files in the way, other than the ones in `seen`.
    async fn in_the_way_card(&self, seen: &[CheckpointId]) -> anyhow::Result<CheckpointOpened> {
        let opened = self.next_merge_card(seen).await?;
        let CheckpointPacket::Merge(packet) = &opened.packet else {
            bail!("not a merge packet");
        };
        ensure!(
            packet.waits_on_you(),
            "a merge approval without files in the way: {}",
            opened.title
        );
        Ok(opened)
    }

    fn landed(&self) -> bool {
        self.integration_events()
            .iter()
            .any(|event| matches!(event, IntegrationEvent::Landed { .. }))
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
            let first = bench.next_merge_card(&[]).await?;
            bench.answer(&first, AnswerAction::Merge)?;
            let asked = bench
                .in_the_way_card(std::slice::from_ref(&first.id))
                .await?;
            ensure!(!bench.landed(), "merged over your file");
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
    bench.landed_on("HEAD")?;
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
        vec![spec(&CardId::numbered(1))],
        None,
        suite_settings(),
        &[("README.md", "# From the run\n")],
    )
    .await?;
    bench.repo.write("store.rs", "fn store() {}\n")?;
    bench.repo.write("README.md", "# From the run\n")?;
    let done = bench.run(merged(&bench)).await?;
    assert_eq!(done.flow, Flow::Finish(RunOutcome::Merged));
    bench.landed_on("HEAD")?;
    let waited = bench
        .integration_events()
        .iter()
        .any(|event| matches!(event, IntegrationEvent::WaitingToMerge { .. }));
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
        vec![spec(&CardId::numbered(1))],
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
    let opened = bench.next_merge_card(&[]).await?;
    bench.answer(&opened, AnswerAction::Merge)
}

/// `delivery.commits = "as_run"` lands the run's history as it happened, and `"per_task"` one
/// commit for each step the run took.
#[tokio::test]
async fn delivery_can_keep_the_runs_history_or_a_commit_per_task() -> anyhow::Result<()> {
    let mut as_run = suite_settings();
    as_run.delivery.commits = DeliveryCommits::AsRun;
    let bench = Bench::with(vec![spec(&CardId::numbered(1))], None, as_run).await?;
    let done = bench.run(merged(&bench)).await?;
    assert_eq!(done.flow, Flow::Finish(RunOutcome::Merged));
    assert_eq!(bench.repo.head()?, bench.integration_head()?);

    let mut per_task = suite_settings();
    per_task.delivery.commits = DeliveryCommits::PerTask;
    let bench = Bench::with(vec![spec(&CardId::numbered(1))], None, per_task).await?;
    let base = bench.repo.head()?;
    let steps = git_in(
        &bench.repo,
        &bench.integration,
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
    let landed = git_in(
        &bench.repo,
        root,
        &["rev-list", "--count", &format!("{base}..main")],
    )?;
    assert_eq!(landed, steps, "one commit for each step of the run");
    let merges = git_in(
        &bench.repo,
        root,
        &["rev-list", "--merges", &format!("{base}..main")],
    )?;
    assert!(merges.is_empty(), "a merge commit landed: {merges}");
    assert_eq!(
        git_in(&bench.repo, root, &["rev-parse", "main^{tree}"])?,
        git_in(
            &bench.repo,
            &bench.integration,
            &["rev-parse", "HEAD^{tree}"]
        )?
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
    let bench = Bench::with(vec![spec(&CardId::numbered(1))], None, settings).await?;
    let base = bench.repo.head()?;
    // Yours: a staged edit to a file the run leaves alone.
    bench.repo.write("README.md", "# Mine\n")?;
    bench.repo.git(&["add", "README.md"])?;
    let done = bench
        .run(async {
            let opened = bench.next_merge_card(&[]).await?;
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
    let bench = Bench::with(vec![spec(&CardId::numbered(1))], None, settings).await?;
    let done = bench
        .run(async {
            let opened = bench.next_merge_card(&[]).await?;
            let head = bench.integration_head()?;
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
    let fast_forward = bench
        .integration_events()
        .iter()
        .find_map(|event| match event {
            IntegrationEvent::Landed { fast_forward, .. } => Some(*fast_forward),
            _ => None,
        });
    assert_eq!(
        fast_forward,
        Some(false),
        "your branch was taken as the run's"
    );
    retro::run(&bench.host).await?;
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
        vec![spec(&CardId::numbered(1))],
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
            let opened = bench.next_merge_card(&[]).await?;
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
    let landed_on = bench
        .integration_events()
        .into_iter()
        .find_map(|event| match event {
            IntegrationEvent::Landed {
                target,
                fast_forward,
                ..
            } => Some((target, fast_forward)),
            _ => None,
        });
    assert_eq!(landed_on, Some(("your files".to_owned(), false)));

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
        .integration_events()
        .iter()
        .any(|event| matches!(event, IntegrationEvent::MainMoved { .. }));
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
            let first = bench.next_merge_card(&[]).await?;
            bench.answer(&first, AnswerAction::Merge)?;
            let asked = bench
                .in_the_way_card(std::slice::from_ref(&first.id))
                .await?;
            ensure!(!bench.landed(), "wrote over your edit");
            let waiting = bench
                .host
                .read(|state| state.integration.waiting_to_merge.clone());
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
/// files hold the run's work already. Taken up again, that reads as landed, and never as
/// edits of yours in the run's way.
#[tokio::test]
async fn files_written_before_a_crash_are_not_in_their_own_way() -> anyhow::Result<()> {
    let before_a_commit = started_before_your_first_commit().await?;
    let mut settings = suite_settings();
    settings.delivery.write = DeliveryWrite::Files;
    let asked = Bench::with(vec![spec(&CardId::numbered(1))], None, settings).await?;
    for bench in [before_a_commit, asked] {
        let head = bench.integration_head()?;
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
                let first = bench.next_merge_card(&[]).await?;
                bench.answer(&first, AnswerAction::Merge)
            })
            .await?;
        assert_eq!(done.flow, Flow::Finish(RunOutcome::Merged));
        ensure!(bench.landed(), "the landing wasn't recorded");
        let waited = bench
            .integration_events()
            .iter()
            .any(|event| matches!(event, IntegrationEvent::WaitingToMerge { .. }));
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
        .integration_events()
        .iter()
        .any(|event| matches!(event, IntegrationEvent::WaitingToMerge { .. }));
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
    /// What landed, where, and whether a branch of yours moved to it.
    fn landing(&self) -> Option<(String, String, bool)> {
        self.integration_events()
            .into_iter()
            .find_map(|event| match event {
                IntegrationEvent::Landed {
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
            tree(&first)? == tree(&self.integration_head()?)?,
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
            let opened = bench.next_merge_card(&[]).await?;
            ensure!(
                opened.title == "Merge this run into main?",
                "{}",
                opened.title
            );
            let evidence = checkpoints::evidence_of(&bench.host, &opened)?;
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
    retro::run(&bench.host).await?;
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
            let first = bench.next_merge_card(&[]).await?;
            bench.answer(&first, AnswerAction::Merge)?;
            let asked = bench
                .in_the_way_card(std::slice::from_ref(&first.id))
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
            let opened = bench.next_merge_card(&[]).await?;
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
    let landing = bench.landing().context("nothing landed")?;
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
        let head = bench.integration_head()?;
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
            .integration_events()
            .iter()
            .any(|event| matches!(event, IntegrationEvent::WaitingToMerge { .. }));
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
            let first = bench.next_merge_card(&[]).await?;
            bench.answer(&first, AnswerAction::Merge)?;
            let asked = bench
                .in_the_way_card(std::slice::from_ref(&first.id))
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
            let CheckpointPacket::Merge(packet) = &asked.packet else {
                bail!("not a merge packet");
            };
            assert_eq!(packet.in_the_way, ["store.rs"]);
            assert_eq!(packet.rows.len(), 1, "the merge's evidence, shown again");
            let evidence = checkpoints::evidence_of(&bench.host, &asked)?;
            assert_eq!(
                evidence.agent_outputs,
                [
                    "1 file of yours is in the way of main: store.rs (not committed, where the \
                  run adds a file). Nothing of yours was changed."
                ]
            );
            ensure!(
                !bench.landed(),
                "nothing merged while the files were in the way"
            );
            ensure!(
                bench
                    .integration_events()
                    .contains(&IntegrationEvent::WaitingToMerge {
                        target: "main".to_owned(),
                        in_the_way: vec!["store.rs".to_owned()],
                    })
            );
            bench.answer(&asked, choose(MOVE_ASIDE))
        })
        .await?;
    assert_eq!(done.flow, Flow::Finish(RunOutcome::Merged));
    bench.landed_on("HEAD")?;
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
/// Git won't overwrite yours, so the run waits to merge on a card with choices instead of
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
        vec![spec(&CardId::numbered(1))],
        None,
        settings,
        &[(BYTECODE, "bytecode the tests left")],
    )
    .await?;
    let work = bench.integration_head()?;
    let committed = git_in(
        &bench.repo,
        &bench.integration,
        &["show", "--name-only", "--format=", "HEAD"],
    )?;
    assert_eq!(
        committed.lines().collect::<Vec<_>>(),
        [BYTECODE, "store.rs"]
    );
    bench.repo.write(BYTECODE, "your own bytecode")?;
    let main = bench.repo.head()?;

    let call = step_call()?;
    let step = merge::merge_step(&bench.host, &call);
    let answering = async {
        let first = bench.next_merge_card(&[]).await?;
        bench.answer(&first, AnswerAction::Merge)?;
        let asked = bench
            .in_the_way_card(std::slice::from_ref(&first.id))
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
        let CheckpointPacket::Merge(packet) = &asked.packet else {
            bail!("not a merge packet");
        };
        assert_eq!(packet.in_the_way, [BYTECODE]);
        ensure!(!bench.landed(), "nothing merged over your file");
        ensure!(
            bench
                .integration_events()
                .contains(&IntegrationEvent::WaitingToMerge {
                    target: "main".to_owned(),
                    in_the_way: vec![BYTECODE.to_owned()],
                })
        );
        bench.answer(&asked, choose(MERGE_LATER))
    };
    let (done, answered) = tokio::join!(
        tokio::time::timeout(Duration::from_secs(20), step),
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
            .read(|state| state.integration.waiting_to_merge.clone()),
        [BYTECODE]
    );
    // The work stays on the run's branch, and your checkout and your file are as they were.
    let run_branch = branch::integration_branch(bench.host.run_id());
    assert_eq!(
        git_in(&bench.repo, bench.repo.root(), &["rev-parse", &run_branch])?,
        work
    );
    assert_eq!(bench.integration_head()?, work);
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
            let first = bench.next_merge_card(&[]).await?;
            bench.answer(&first, AnswerAction::Merge)?;
            let asked = bench
                .in_the_way_card(std::slice::from_ref(&first.id))
                .await?;
            bench.answer(&asked, choose(TRY_AGAIN))?;
            let again = bench
                .in_the_way_card(&[first.id.clone(), asked.id.clone()])
                .await?;
            std::fs::remove_file(bench.repo.root().join("store.rs"))?;
            bench.answer(&again, choose(TRY_AGAIN))
        })
        .await?;
    assert_eq!(done.flow, Flow::Finish(RunOutcome::Merged));
    bench.landed_on("HEAD")?;
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
            let first = bench.next_merge_card(&[]).await?;
            bench.answer(&first, AnswerAction::Merge)?;
            let asked = bench
                .in_the_way_card(std::slice::from_ref(&first.id))
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
    assert!(!bench.landed());
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
            let first = bench.next_merge_card(&[]).await?;
            std::fs::write(&index, "not an index")?;
            bench.answer(&first, AnswerAction::Merge)?;
            let asked = bench
                .next_merge_card(std::slice::from_ref(&first.id))
                .await?;
            let CheckpointPacket::Merge(packet) = &asked.packet else {
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
            ensure!(!bench.landed(), "landed with a broken index");
            std::fs::remove_file(&index)?;
            bench.repo.git(&["reset", "--quiet"])?;
            bench.answer(&asked, choose(TRY_AGAIN))
        })
        .await?;
    assert_eq!(done.flow, Flow::Finish(RunOutcome::Merged));
    bench.landed_on("HEAD")?;
    Ok(())
}

/// A landing your rules approved and that was refused merged nothing, so the auto-merge
/// they took goes back; the card you answer yourself takes none.
#[tokio::test]
async fn a_refused_auto_landing_gives_back_its_auto_merge() -> anyhow::Result<()> {
    let bench = Bench::new(None).await?;
    bench.services.policy.decide_next_after(
        PolicyOutcome::Decided {
            verdict: Verdict::Approve,
            by: DecideWith::Rules,
            approver: None,
            reasons: vec!["within your rules".to_owned()],
            spot_check: false,
        },
        Duration::from_millis(300),
    );
    let done = bench
        .run(async {
            let first = bench.next_merge_card(&[]).await?;
            // The branch moves before your rules' approval lands.
            commit_in(
                &bench.repo,
                &bench.integration,
                "late.rs",
                "late\n",
                "late change",
            )?;
            let second = bench
                .next_merge_card(std::slice::from_ref(&first.id))
                .await?;
            bench.answer(&second, AnswerAction::Merge)
        })
        .await?;
    assert_eq!(done.flow, Flow::Finish(RunOutcome::Merged));
    let refused: Vec<CheckpointId> = bench
        .services
        .policy
        .refused_landings()
        .into_iter()
        .map(|point| point.checkpoint)
        .collect();
    assert_eq!(refused.len(), 1, "{refused:?}");
    Ok(())
}

/// The review's first probe: you switch branches while the card waits, then choose Try
/// again. Landing reads the checkout again, so main lands and your branch is left alone,
/// instead of a "Merged" that moved the branch you were on.
#[tokio::test]
async fn a_branch_switched_while_the_card_waits_leaves_it_alone() -> anyhow::Result<()> {
    let bench = Bench::new(None).await?;
    let base = bench.repo.head()?;
    bench.repo.write("store.rs", "// mine\n")?;
    let done = bench
        .run(async {
            let first = bench.next_merge_card(&[]).await?;
            bench.answer(&first, AnswerAction::Merge)?;
            let asked = bench
                .in_the_way_card(std::slice::from_ref(&first.id))
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
    bench.landed_on("main")?;
    assert_eq!(bench.repo.head()?, base, "your branch didn't move");
    assert!(bench.integration_events().iter().any(|event| matches!(
        event,
        IntegrationEvent::Landed { target, .. } if target == "main"
    )));
    Ok(())
}

/// The review's second probe: while the card waits you clear the file in the way and edit
/// one the run changes. Try again asks about the edit, in plain words, instead of failing the
/// run with git's "Aborting", and lands once it's committed or undone.
#[tokio::test]
async fn an_edit_made_while_the_card_waits_asks_instead_of_failing() -> anyhow::Result<()> {
    let bench = Bench::new(None).await?;
    commit_in(
        &bench.repo,
        &bench.integration,
        "README.md",
        "# Test repo\n\nRun `store` to save.\n",
        "Document the store",
    )?;
    bench.repo.write("store.rs", "// mine\n")?;
    let done = bench
        .run(async {
            let first = bench.next_merge_card(&[]).await?;
            bench.answer(&first, AnswerAction::Merge)?;
            let asked = bench
                .in_the_way_card(std::slice::from_ref(&first.id))
                .await?;
            std::fs::remove_file(bench.repo.root().join("store.rs"))?;
            bench.repo.write("README.md", "# Test repo, my notes\n")?;
            bench.answer(&asked, choose(TRY_AGAIN))?;

            let edited = bench
                .next_merge_card(&[first.id.clone(), asked.id.clone()])
                .await?;
            let CheckpointPacket::Merge(packet) = &edited.packet else {
                bail!("not a merge packet");
            };
            assert_eq!(edited.title, "Waiting to merge into main");
            assert_eq!(packet.edited, ["README.md"]);
            assert!(packet.in_the_way.is_empty());
            assert_eq!(keys(&edited), ['t', 'l', 'p', 'k'], "nothing to move aside");
            assert_eq!(edited.mode, ApprovalMode::Supervised);
            ensure!(!bench.landed());
            git_in(
                &bench.repo,
                bench.repo.root(),
                &["checkout", "--", "README.md"],
            )?;
            bench.answer(&edited, choose(TRY_AGAIN))
        })
        .await?;
    assert_eq!(done.flow, Flow::Finish(RunOutcome::Merged));
    bench.landed_on("HEAD")?;
    Ok(())
}

/// An uncommitted edit to a file the run leaves alone rides along with the fast-forward, so
/// it neither stops the merge nor sends the card back to your rules again and again.
#[tokio::test]
async fn an_edit_to_a_file_the_run_leaves_alone_rides_along() -> anyhow::Result<()> {
    let bench = Bench::new(None).await?;
    bench.repo.write("README.md", "# Test repo, my notes\n")?;
    let done = bench
        .run(async {
            let first = bench.next_merge_card(&[]).await?;
            bench.answer(&first, AnswerAction::Merge)
        })
        .await?;
    assert_eq!(done.flow, Flow::Finish(RunOutcome::Merged));
    bench.landed_on("HEAD")?;
    assert_eq!(
        std::fs::read_to_string(bench.repo.root().join("README.md"))?,
        "# Test repo, my notes\n"
    );
    let merge_cards = bench.host.read(|state| {
        state
            .checkpoints
            .by_id
            .values()
            .filter(|record| record.opened.kind == CheckpointKind::Merge)
            .count()
    });
    assert_eq!(merge_cards, 1, "asked once");
    Ok(())
}

/// Merge later pauses the run at the merge, waiting to merge rather than failed, and resuming
/// acts on the same approval once the way is clear.
#[tokio::test]
async fn merge_later_pauses_and_resuming_merges() -> anyhow::Result<()> {
    let bench = Bench::new(None).await?;
    bench.repo.write("store.rs", "// mine\n")?;
    let call = step_call()?;
    let step = merge::merge_step(&bench.host, &call);
    let answering = async {
        let first = bench.next_merge_card(&[]).await?;
        bench.answer(&first, AnswerAction::Merge)?;
        let asked = bench
            .in_the_way_card(std::slice::from_ref(&first.id))
            .await?;
        bench.answer(&asked, choose(MERGE_LATER))
    };
    let (done, answered) = tokio::join!(
        tokio::time::timeout(Duration::from_secs(20), step),
        answering
    );
    answered?;
    match done.context("the merge step timed out")? {
        Err(EngineError::Interrupted) => {}
        other => bail!("expected the run to pause, got {other:?}"),
    }
    assert!(bench.host.read(|state| state.run.paused));
    assert!(!bench.landed());
    assert_eq!(
        bench
            .host
            .read(|state| state.integration.waiting_to_merge.clone()),
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
            .read(|state| state.integration.waiting_to_merge.is_empty())
    );
    Ok(())
}

/// Under `merge_rule = "none"` a replacement's debt is shown, but nothing counts as
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
        vec![spec(&CardId::numbered(1))],
        Some(debt.clone()),
        settings,
    )
    .await?;
    let done = bench
        .run(async {
            let card = bench.next_merge_card(&[]).await?;
            let CheckpointPacket::Merge(packet) = &card.packet else {
                bail!("not a merge packet");
            };
            ensure!(packet.rows[0].debt == vec![debt.clone()]);
            let evidence = checkpoints::evidence_of(&bench.host, &card)?;
            ensure!(
                evidence.facts.unreviewed_debt == Some(Vec::new()),
                "{:?}",
                evidence.facts.unreviewed_debt
            );
            ensure!(evidence.facts.irreversible_actions == Some(Vec::new()));
            bench.answer(&card, AnswerAction::Merge)
        })
        .await?;
    assert_eq!(done.flow, Flow::Finish(RunOutcome::Merged));
    Ok(())
}

/// Only tasks that passed merge. When none did, the merge never opens, nothing lands, and the
/// run ends saying what stopped each task.
#[tokio::test]
async fn with_no_task_passed_the_merge_never_opens() -> anyhow::Result<()> {
    let card = CardId::numbered(1);
    let bench = Bench::with(vec![spec(&card)], None, suite_settings()).await?;
    let before = bench.repo.head()?;
    bench.host.record(
        Some(&card),
        None,
        RunEventKind::Card(CardEvent::State {
            from: Some(CardState::Integrated),
            to: CardState::Blocked,
            reason: "waiting on your answer about the key prefix".to_owned(),
        }),
    )?;

    let done = bench.run(async { Ok(()) }).await?;
    assert_eq!(done.flow, Flow::Finish(RunOutcome::Failed));
    let why = done.summary.unwrap_or_default();
    assert_eq!(
        why,
        "Nothing was merged, since no task passed: task-001 is blocked on a question \
         (waiting on your answer about the key prefix)."
    );
    let events = bench.sink.events();
    assert!(!events.iter().any(|event| matches!(
        &event.kind,
        RunEventKind::Checkpoint(CheckpointEvent::Opened(opened))
            if opened.kind == CheckpointKind::Merge
    )));
    assert!(events.iter().any(|event| matches!(
        &event.kind,
        RunEventKind::Note(note) if note.text == why
    )));
    assert_eq!(bench.repo.head()?, before, "nothing landed");
    Ok(())
}

/// An owner's note is kept under Argonauts' home before the merge checkpoint opens. It is
/// never part of what lands, and the retro after the landing commits nothing.
#[tokio::test]
async fn retro_notes_stay_out_of_the_merge() -> anyhow::Result<()> {
    let mut settings = suite_settings();
    settings.evolution.propose_every = 0;
    let bench = Bench::with(vec![spec(&CardId::numbered(1))], None, settings).await?;
    let card = CardId::numbered(1);
    let report = CardReport {
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
        Some(&card),
        None,
        RunEventKind::Card(CardEvent::Reported(Box::new(report))),
    )?;
    let note = ".argonauts/notes/store.md";

    let done = bench
        .run(async {
            let opened = bench.next_merge_card(&[]).await?;
            let evidence = checkpoints::evidence_of(&bench.host, &opened)?;
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
    let landed = bench.repo.head()?;
    let tracked = bench.repo.git(&["ls-tree", "-r", "--name-only", &landed])?;
    assert!(!tracked.contains(".argonauts/"), "{tracked}");
    let kept = argonauts_crew::divide::repo_state_dir(&bench.host).join("notes/store.md");
    assert!(std::fs::read_to_string(kept)?.contains("keeps keys for an hour"));

    retro::run(&bench.host).await?;
    let events = bench.sink.events();
    assert!(!events.iter().any(|event| matches!(
        event.kind,
        RunEventKind::Retro(RetroEvent::Committed { .. })
    )));
    assert_eq!(bench.repo.head()?, landed, "nothing merged after the merge");
    Ok(())
}

#[tokio::test]
async fn the_final_check_skips_a_main_that_did_not_move() -> anyhow::Result<()> {
    let bench = Bench::new(None).await?;
    let check = final_check::run(&bench.host).await?;
    assert!(check.passed && check.suite_ran());
    assert!(
        !bench
            .integration_events()
            .iter()
            .any(|event| matches!(event, IntegrationEvent::MainMoved { .. }))
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn request_changes_sends_back_only_the_named_cards() -> anyhow::Result<()> {
    let (one, two) = (CardId::numbered(1), CardId::numbered(2));
    let cards = [&one, &two]
        .map(|card| CardSpec {
            reviewers: vec![worker(CODEX)],
            ..spec(card)
        })
        .to_vec();
    let bench = Bench::with(cards, None, suite_settings()).await?;
    let owner = bench
        .services
        .workers
        .add(fixing_owner(OPUS, "claude", "renamed.rs"));
    bench.services.workers.add(reviewer(CODEX, "codex", &[]));

    let done = bench
        .run(async {
            let first = bench.next_merge_card(&[]).await?;
            bench.answer_with(
                &first,
                AnswerAction::RequestChanges,
                Some("Rename the store"),
                vec![two.clone()],
            )?;
            let second = bench
                .next_merge_card(std::slice::from_ref(&first.id))
                .await?;
            bench.answer(&second, AnswerAction::KeepBranch)
        })
        .await?;
    assert_eq!(done.flow, Flow::Finish(RunOutcome::BranchLeft));

    // Only the named card went back, and its owner got your note.
    assert_eq!(bench.sent_back(), std::slice::from_ref(&two));
    let specs = owner.specs();
    assert_eq!(specs.len(), 1);
    assert_eq!(specs[0].card.as_ref(), Some(&two));
    assert!(
        specs[0]
            .prompt
            .iter()
            .any(|part| part.text.contains("Rename the store"))
    );
    // It merged again, and the final check ran on the new head.
    let merges = bench
        .integration_events()
        .into_iter()
        .filter(|event| matches!(event, IntegrationEvent::Merged { card, .. } if *card == two))
        .count();
    assert_eq!(merges, 2);
    assert!(bench.integration.join("renamed.rs").exists());
    assert_eq!(bench.final_checks(), 2);
    let state = bench
        .host
        .read(|state| [&one, &two].map(|card| state.cards.get(card).map(|record| record.state)));
    assert_eq!(state, [Some(CardState::Integrated); 2]);
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn an_upgrade_review_that_asks_for_changes_sends_the_card_back() -> anyhow::Result<()> {
    let debt = QualityDebt {
        since: 1,
        by: worker(CODEX),
        needs: worker(OPUS),
    };
    let mut settings = suite_settings();
    settings.crew = vec![worker(OPUS), worker(CODEX), worker(GEMINI)].into();
    settings.roles.clear();
    settings.fallback.unknown_tier = Some(UnknownTier::Equal);
    let card = CardSpec {
        reviewers: vec![worker(GEMINI)],
        ..spec(&CardId::numbered(1))
    };
    let bench = Bench::with(vec![card], Some(debt), settings).await?;
    bench.services.health.set(
        &worker(OPUS).vendor(),
        HealthState::Down,
        Some("usage limit"),
    );
    let owner = bench
        .services
        .workers
        .add(fixing_owner(CODEX, "codex", "reviewed.rs"));
    bench
        .services
        .workers
        .add(reviewer(GEMINI, "gemini", &["request_changes"]));

    let done = bench
        .run(async {
            let first = bench.next_merge_card(&[]).await?;
            ensure!(keys(&first).contains(&'a'), "{:?}", keys(&first));
            bench.answer(&first, AnswerAction::UpgradeReview)?;
            let second = bench
                .next_merge_card(std::slice::from_ref(&first.id))
                .await?;
            let CheckpointPacket::Merge(packet) = &second.packet else {
                bail!("not a merge packet");
            };
            ensure!(packet.rows[0].debt.is_empty(), "{:?}", packet.rows[0].debt);
            bench.answer(&second, AnswerAction::KeepBranch)
        })
        .await?;
    assert_eq!(done.flow, Flow::Finish(RunOutcome::BranchLeft));
    assert_eq!(bench.sent_back(), [CardId::numbered(1)]);
    let specs = owner.specs();
    assert_eq!(specs.len(), 1);
    assert!(
        specs[0]
            .prompt
            .iter()
            .any(|part| part.text.contains("the upgrade review asked for changes"))
    );
    assert!(bench.integration.join("reviewed.rs").exists());
    Ok(())
}

#[tokio::test]
async fn a_woken_run_acts_on_your_answer_without_checking_again() -> anyhow::Result<()> {
    let bench = Bench::new(None).await?;
    // The run parks at the checkpoint, and you answer while it's parked.
    let call = step_call()?;
    let parked = tokio::time::timeout(
        Duration::from_secs(2),
        merge::merge_step(&bench.host, &call),
    )
    .await;
    ensure!(parked.is_err(), "the step ended without your answer");
    let opened = bench.next_merge_card(&[]).await?;
    bench.answer(&opened, AnswerAction::KeepBranch)?;
    let phases = bench.phase_events();

    // A suite that would fail now changes nothing: the answer stands on the same heads.
    bench.services.gates.fail_next("flaky");
    let done = bench.run(async { Ok(()) }).await?;
    assert_eq!(done.flow, Flow::Finish(RunOutcome::BranchLeft));
    assert_eq!(bench.final_checks(), 1);
    assert_eq!(bench.services.gates.requests().len(), 1);
    let merge_cards = bench.host.read(|state| {
        state
            .checkpoints
            .by_id
            .values()
            .filter(|record| record.opened.kind == CheckpointKind::Merge)
            .count()
    });
    assert_eq!(merge_cards, 1);
    // Only the merge phase's end is new.
    assert_eq!(bench.phase_events(), phases + 1);
    Ok(())
}
