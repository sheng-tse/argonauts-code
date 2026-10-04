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
use argonauts_core::settings::handoff::{MergeRule, UnknownTier};
use argonauts_core::settings::protocol::GateCommand;
use argonauts_core::steps::card::{CardReport, MemoryProposal, ReportStatus};
use argonauts_core::workflow::{StandardPhase, Step, StepKind, StepRef, Workflow};
use argonauts_core::{CardId, RepoId, RunId};
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
        let mut history = vec![
            (None, started(&repo, &base)),
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
        bail!("no merge checkpoint opened")
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
        bail!("no \"original is back\" card opened")
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
    settings.gates.full_suite = vec![GateCommand {
        name: "suite".to_owned(),
        command: vec!["true".to_owned()],
    }];
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
        preset: "default".to_owned(),
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
    let head = bench.integration_head()?;
    assert_eq!(bench.repo.head()?, head);
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
    let head = bench.integration_head()?;
    assert_eq!(bench.repo.head()?, head);
    assert!(
        bench
            .integration_events()
            .contains(&IntegrationEvent::Landed {
                target: "main".to_owned(),
                sha: head,
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
    assert_eq!(
        bench.repo.git(&["rev-parse", "main"])?,
        bench.integration_head()?
    );
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
    let head = bench.integration_head()?;
    assert_eq!(bench.repo.head()?, head);
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
            ensure!(
                evidence.facts.unreviewed_debt == Some(vec!["card-001".to_owned()]),
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
            ensure!(!merge_cards, "the merge card opened beside it");
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
        .find(|spec| spec.role == RoleName::Owner)
        .context("the original never worked on it")?;
    assert!(
        worked
            .prompt
            .iter()
            .any(|part| part.text.contains("is back and continues the card"))
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
            checkpoints::withdraw(&bench.host, &first.id, "you reverted card-001 to handoff-1")?;
            let second = bench
                .next_merge_card(std::slice::from_ref(&first.id))
                .await?;
            let evidence = checkpoints::evidence_of(&bench.host, &second)?;
            ensure!(
                evidence
                    .agent_outputs
                    .iter()
                    .any(|line| line.contains("you reverted card-001")),
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
            "a merge card without files in the way: {}",
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
            assert_eq!(keys(&asked), ['m', 't', 'l']);
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
            ensure!(
                !bench.landed(),
                "nothing landed while the files were in the way"
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
    assert_eq!(bench.repo.head()?, bench.integration_head()?);
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
        assert_eq!(labels, [MOVE_ASIDE, TRY_AGAIN, MERGE_LATER]);
        assert!(asked.needs_you);
        let CheckpointPacket::Merge(packet) = &asked.packet else {
            bail!("not a merge packet");
        };
        assert_eq!(packet.in_the_way, [BYTECODE]);
        ensure!(!bench.landed(), "nothing landed over your file");
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
    assert_eq!(bench.repo.head()?, bench.integration_head()?);
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
    let main = git_in(&bench.repo, bench.repo.root(), &["rev-parse", "main"])?;
    assert_eq!(main, bench.integration_head()?, "main has the run");
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
            assert_eq!(keys(&edited), ['t', 'l'], "nothing to move aside");
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
    assert_eq!(bench.repo.head()?, bench.integration_head()?);
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
    assert_eq!(bench.repo.head()?, bench.integration_head()?);
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
    settings.handoff.merge_rule = Some(MergeRule::None);
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

/// An owner's note is committed before the merge checkpoint opens, so your approval covers it
/// and it lands with the run. The retro after the landing has nothing left to land.
#[tokio::test]
async fn retro_notes_land_as_part_of_the_merge() -> anyhow::Result<()> {
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
                evidence.facts.conductor_paths.contains(&note.to_owned()),
                "{:?}",
                evidence.facts.conductor_paths
            );
            ensure!(!evidence.facts.agent_paths.contains(&note.to_owned()));
            let shown = evidence.facts.subject.context("no subject")?;
            ensure!(
                git_in(
                    &bench.repo,
                    &bench.integration,
                    &["show", &format!("{}:{note}", shown.head_commit)]
                )?
                .contains("keeps keys for an hour")
            );
            bench.answer(&opened, AnswerAction::Merge)
        })
        .await?;
    assert_eq!(done.flow, Flow::Finish(RunOutcome::Merged));
    let landed = bench.repo.head()?;
    assert!(
        bench
            .repo
            .git(&["show", &format!("{landed}:{note}")])?
            .contains("keeps keys for an hour")
    );

    retro::run(&bench.host).await?;
    let events = bench.sink.events();
    let commits = events
        .iter()
        .filter(|event| {
            matches!(
                event.kind,
                RunEventKind::Retro(RetroEvent::Committed { .. })
            )
        })
        .count();
    assert_eq!(
        commits, 1,
        "the notes were committed once, before the merge"
    );
    assert!(!events.iter().any(|event| matches!(
        &event.kind,
        RunEventKind::Note(note) if note.text.contains("retro's notes")
    )));
    assert_eq!(bench.repo.head()?, landed, "nothing landed after the merge");
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
    settings.crew = vec![worker(OPUS), worker(CODEX), worker(GEMINI)];
    settings.roles.clear();
    settings.handoff.unknown_tier = Some(UnknownTier::Equal);
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
