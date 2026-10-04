//! Handoffs with checkpoints: the strategies, which replacement list applies,
//! checkpoint tags, commit authors in git notes, `/revert`, and what happens when the original
//! is back. Each test drives the handoff code on a real temp repo through the engine's `Host`.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, bail, ensure};
use serde_json::json;
use time::macros::datetime;
use tokio::sync::mpsc;

use argonauts_core::api::CheckpointAnswer;
use argonauts_core::engine::{EngineCommand, RepoContext};
use argonauts_core::events::card::{
    CardBudget, CardEvent, CardKind, CardSpec, CardState, IntegrationEvent, ReviewEvent,
};
use argonauts_core::events::checkpoint::{
    AnswerAction, CheckpointDecided, CheckpointEvent, CheckpointKind, CheckpointOpened,
    CheckpointPacket, FreeText, MergePacket,
};
use argonauts_core::events::goal::{GoalEvent, GoalFile, GoalSource};
use argonauts_core::events::run::{Party, RunLifecycle, RunMode, RunStarted};
use argonauts_core::events::session::EndReason;
use argonauts_core::events::vendor::{HandoffEvent, HandoffPlan};
use argonauts_core::events::workflow::{StepStarted, WorkflowEvent, WorkflowStarted};
use argonauts_core::events::{RunEvent, RunEventKind};
use argonauts_core::handoff::CheckpointTag;
use argonauts_core::limits::{
    Candidate, CandidateQuery, CostEstimate, HandoffPacket, HandoffPlanner, HealthState,
    QuotaBoard, QuotaReading, QuotaSource, QuotaStatus, RecoveryAction, Shortlist, WorkSoFar,
};
use argonauts_core::names::{CheckpointId, RoleName, WorkerRef, WorkflowId};
use argonauts_core::settings::Settings;
use argonauts_core::settings::approvals::{ApprovalMode, DecideWith};
use argonauts_core::settings::handoff::{
    FallbackEntry, HandoffMode, HandoffSettings, HandoffStrategy, OnReturn, UnknownTier,
    VendorReplacement,
};
use argonauts_core::settings::roles::RoleSettings;
use argonauts_core::workflow::{StandardPhase, StepKind, StepRef, Workflow};
use argonauts_core::{CardId, CommitInfo, DiffStat, RepoId, RunId, Spend};
use argonauts_crew::checkpoints;
use argonauts_crew::engine::commands;
use argonauts_crew::engine::host::Host;
use argonauts_crew::engine::shared::Shared;
use argonauts_crew::handoff::on_return::Applied;
use argonauts_crew::handoff::{self, on_return, revert};
use argonauts_crew::integrate::branch::ensure_integration;
use argonauts_crew::state::RunState;
use argonauts_testkit::repo::TempRepo;
use argonauts_testkit::services::{FakeServices, MemorySink, TEST_RUN, engine_context_in};
use argonauts_testkit::worker::{FakeSession, FakeWorker};

const OPUS: &str = "claude-code/opus";
const CODEX: &str = "codex/gpt-5.6";
const AGY: &str = "agy/gemini-3.6";
/// The card's ring reviewers, of other families than its planned owner.
const GPT_REVIEWER: &str = "codex/gpt-5.6-mini";
const GEMINI_REVIEWER: &str = "agy/gemini-3.6-flash";

fn worker(name: &str) -> WorkerRef {
    name.parse().expect("a valid worker")
}

fn card(n: u32) -> CardId {
    CardId::numbered(n)
}

/// A run with a stopped card: `card-001`, planned for claude-code/opus, in its own worktree
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
    planner: Option<Arc<dyn HandoffPlanner>>,
    /// Extra cards planned for claude-code/opus, with no worktree yet.
    more_cards: u32,
}

impl Setup {
    fn new(settings: Settings) -> Self {
        Self {
            settings,
            planner: None,
            more_cards: 0,
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
        let tree = layout.worktrees.join(card(1).as_str());
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
            (None, goal_approved(&repo)),
            (None, workflow_started()?),
            (None, build_step()?),
            (
                Some(card(1)),
                RunEventKind::Card(CardEvent::Created(Box::new(spec(1)))),
            ),
            (
                Some(card(1)),
                RunEventKind::Card(CardEvent::Worktree {
                    path: tree.clone(),
                    branch: branch(1),
                    base: base.clone(),
                }),
            ),
            (Some(card(1)), stopped_state(None)),
            (Some(card(1)), committed(1, &first, Some(OPUS))),
        ];
        for n in 2..2 + setup.more_cards {
            history.push((
                Some(card(n)),
                RunEventKind::Card(CardEvent::Created(Box::new(spec(n)))),
            ));
            history.push((Some(card(n)), stopped_state(None)));
        }
        let history: Vec<RunEvent> = history
            .into_iter()
            .zip(0..)
            .map(|((card, kind), seq)| event(seq, card, kind))
            .collect();

        let (mut context, sink, commands) =
            engine_context_in(&services, setup.settings, layout.clone(), history.clone());
        if let Some(planner) = setup.planner {
            context.services.handoff = planner;
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
        self.layout.worktrees.join(card(1).as_str())
    }

    fn owner(&self, card: &CardId) -> Option<WorkerRef> {
        self.host.read(|state| {
            state
                .cards
                .get(card)
                .and_then(|record| record.owner.clone())
        })
    }

    fn debt(&self, card: &CardId) -> Option<argonauts_core::handoff::QualityDebt> {
        self.host
            .read(|state| state.cards.get(card).and_then(|record| record.debt.clone()))
    }

    fn card_events(&self) -> Vec<CardEvent> {
        self.sink
            .events()
            .into_iter()
            .filter_map(|event| match event.kind {
                RunEventKind::Card(card) => Some(card),
                _ => None,
            })
            .collect()
    }

    fn handoff_events(&self) -> Vec<HandoffEvent> {
        self.sink
            .events()
            .into_iter()
            .filter_map(|event| match event.kind {
                RunEventKind::Handoff(handoff) => Some(handoff),
                _ => None,
            })
            .collect()
    }

    fn record(&self, card: &CardId, kind: RunEventKind) -> anyhow::Result<()> {
        self.host.record(Some(card), None, kind)?;
        Ok(())
    }

    /// The card's agent stopped again: back to `NeedsDecision`, as recovery leaves it.
    fn stop_again(&self, card: &CardId) -> anyhow::Result<()> {
        let from = self
            .host
            .read(|state| state.cards.get(card).map(|record| record.state));
        self.record(card, stopped_state(from))
    }

    /// Moves the card through `path`, as its pipeline would.
    fn move_through(&self, card: &CardId, path: &[CardState]) -> anyhow::Result<()> {
        for to in path {
            let from = self.state(card);
            self.record(
                card,
                RunEventKind::Card(CardEvent::State {
                    from,
                    to: *to,
                    reason: "moved by the test".to_owned(),
                }),
            )?;
        }
        Ok(())
    }

    fn state(&self, card: &CardId) -> Option<CardState> {
        self.host
            .read(|state| state.cards.get(card).map(|record| record.state))
    }

    /// Reviews recorded on the card, by reviewer.
    fn reviewed_by(&self, card: &CardId) -> Vec<WorkerRef> {
        self.host.read(|state| {
            state
                .cards
                .get(card)
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

    /// A commit by `by` in card-001's worktree, recorded as the conductor records it.
    fn work(&self, file: &str, by: Option<&str>) -> anyhow::Result<String> {
        let sha = commit_in(&self.repo, &self.tree(), file, "work\n", file)?;
        self.record(&card(1), committed(1, &sha, by))?;
        Ok(sha)
    }

    async fn hand_off(&self, card: &CardId, to: &str) -> anyhow::Result<()> {
        let action = RecoveryAction::HandOff {
            to: worker(to),
            packet: Box::new(packet(card)),
            checkpoint: Some(tag(card, 0)),
        };
        handoff::apply(&self.host, card, action).await?;
        Ok(())
    }

    fn tag_target(&self, n: u32) -> anyhow::Result<String> {
        self.repo.git(&[
            "rev-parse",
            &format!("{}^{{commit}}", tag(&card(1), n).tag_name()),
        ])
    }

    /// Waits for a checkpoint of `kind` that isn't decided yet.
    async fn opened(&self, kind: CheckpointKind) -> anyhow::Result<CheckpointOpened> {
        for _ in 0..200 {
            let found = self.host.read(|state| {
                state
                    .checkpoints
                    .open()
                    .find(|record| record.opened.kind == kind)
                    .map(|record| record.opened.clone())
            });
            if let Some(opened) = found {
                return Ok(opened);
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        bail!("no {kind:?} checkpoint opened")
    }

    fn answer(&self, opened: &CheckpointOpened, action: AnswerAction) -> anyhow::Result<()> {
        checkpoints::answer(
            &self.host,
            CheckpointAnswer {
                id: opened.id.clone(),
                action,
                text: None,
                evidence_sha256: opened.evidence_sha256.clone(),
                cards: Vec::new(),
            },
        )
        .map_err(anyhow::Error::msg)
    }
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
        // Solo, so build sessions need only the approved goal, not a plan as well.
        mode: RunMode::Solo,
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
        instructions: Vec::new(),
    })))
}

/// Build sessions start only under an approved goal.
fn goal_approved(repo: &TempRepo) -> RunEventKind {
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

fn build_step() -> anyhow::Result<RunEventKind> {
    Ok(RunEventKind::Workflow(WorkflowEvent::StepStarted(
        StepStarted {
            step: StepRef {
                workflow: WorkflowId::numbered(1),
                phase: 0,
                path: vec![0],
            },
            kind: StepKind::Build,
            standard: StandardPhase::Do,
            round: 1,
        },
    )))
}

fn spec(n: u32) -> CardSpec {
    CardSpec {
        id: card(n),
        title: format!("Card {n}"),
        task: "Build the Redis store".to_owned(),
        kind: CardKind::Build,
        owned_paths: Vec::new(),
        read_only_paths: Vec::new(),
        forbidden_paths: Vec::new(),
        hidden_paths: Vec::new(),
        depends_on: Vec::new(),
        criteria: Vec::new(),
        owner: Some(worker(OPUS)),
        reviewers: vec![worker(GPT_REVIEWER), worker(GEMINI_REVIEWER)],
        budget: CardBudget::default(),
        predictions: None,
        item: None,
    }
}

fn branch(n: u32) -> String {
    format!("argonauts/{TEST_RUN}/{}", card(n))
}

fn stopped_state(from: Option<CardState>) -> RunEventKind {
    RunEventKind::Card(CardEvent::State {
        from,
        to: CardState::NeedsDecision,
        reason: "usage limit".to_owned(),
    })
}

fn committed(n: u32, sha: &str, by: Option<&str>) -> RunEventKind {
    RunEventKind::Card(CardEvent::Committed(CommitInfo {
        branch: branch(n),
        sha: Some(sha.to_owned()),
        message: "work".to_owned(),
        stat: DiffStat::default(),
        by: by.map(worker),
    }))
}

fn tag(card: &CardId, n: u32) -> CheckpointTag {
    CheckpointTag {
        run: RunId::new(TEST_RUN).expect("the test run id"),
        card: card.clone(),
        n,
    }
}

fn packet(card: &CardId) -> HandoffPacket {
    let spec = spec(
        card.as_str()
            .trim_start_matches("card-")
            .parse()
            .unwrap_or(1),
    );
    HandoffPacket {
        card: spec,
        goal_sha256: None,
        notes: Vec::new(),
        work: WorkSoFar {
            branch: format!("argonauts/{TEST_RUN}/{card}"),
            commit: None,
            stat: DiffStat::default(),
        },
        happened: Vec::new(),
        last_todo: Vec::new(),
        gates: Vec::new(),
        why_stopped: "usage limit, resets at 3:45 PM".to_owned(),
        last_words: None,
        last_message: None,
        budget_left: None,
        transcript: None,
        summary: None,
    }
}

fn candidate(name: &str) -> Candidate {
    Candidate {
        worker: worker(name),
        quota_left: Some(0.5),
        quota_text: "50%".to_owned(),
        track_record: None,
        cost: CostEstimate::Quota(0.1),
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

/// A planner that walks the handoff tables the way the limits service does, with a fixed
/// strategy when one is given.
struct WalkingPlanner {
    settings: Settings,
    strategy: Option<HandoffStrategy>,
}

impl WalkingPlanner {
    fn applied(&self, query: &CandidateQuery) -> argonauts_core::settings::handoff::AppliedHandoff {
        let role = self
            .settings
            .roles
            .get(&query.role)
            .map(|role| &role.handoff);
        HandoffSettings::applied(&self.settings.handoff, role, &query.stopped.vendor())
    }
}

impl HandoffPlanner for WalkingPlanner {
    fn candidates(&self, query: &CandidateQuery) -> Vec<Candidate> {
        self.applied(query)
            .candidates
            .iter()
            .map(|entry| candidate(&entry.worker().to_string()))
            .collect()
    }

    fn packet(&self, card: &CardSpec, _events: &[RunEvent]) -> HandoffPacket {
        packet(&card.id)
    }

    fn shortlist(&self, query: &CandidateQuery) -> Shortlist {
        let applied = self.applied(query);
        Shortlist {
            strategy: self.strategy.unwrap_or(applied.strategy),
            race_count: applied.race_count,
            candidates: self.candidates(query),
        }
    }
}

fn entries(workers: &[&str]) -> Vec<FallbackEntry> {
    workers
        .iter()
        .map(|name| FallbackEntry::Worker(worker(name)))
        .collect()
}

fn owner_table(handoff: HandoffSettings) -> RoleSettings {
    RoleSettings {
        handoff,
        ..RoleSettings::default()
    }
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
fn slow_candidate(name: &str, family: &str, file: &'static str) -> FakeWorker {
    FakeWorker::harness(name, family).respond_with(move |spec| {
        let _ = std::fs::write(spec.workdir.join(file), "// half done\n");
        FakeSession::until_stopped(Vec::new())
    })
}

#[tokio::test]
async fn handoff_strategy_first() -> anyhow::Result<()> {
    let bench = Bench::new().await?;
    let c1 = card(1);
    bench.hand_off(&c1, CODEX).await?;
    assert_eq!(bench.owner(&c1), Some(worker(CODEX)));
    assert!(bench.handoff_events().iter().any(|event| matches!(
        event,
        HandoffEvent::HandedOff { to, by: Party::Rules, .. } if *to == worker(CODEX)
    )));
    assert!(bench.card_events().iter().any(|event| matches!(
        event,
        CardEvent::State {
            to: CardState::Ready,
            ..
        }
    )));
    // An unknown tier counts as weaker, so the card owes a review by its original.
    let debt = bench.debt(&c1).context("debt after the first handoff")?;
    assert_eq!(
        (debt.since, &debt.by, &debt.needs),
        (1, &worker(CODEX), &worker(OPUS))
    );

    // The replacement runs out too: the next candidate takes over at handoff-2, and the debt
    // still names the strongest owner before it.
    bench.work("codex.rs", Some(CODEX))?;
    bench.stop_again(&c1)?;
    bench.hand_off(&c1, AGY).await?;
    assert_eq!(bench.owner(&c1), Some(worker(AGY)));
    let debt = bench.debt(&c1).context("debt after the second handoff")?;
    assert_eq!(
        (debt.since, &debt.by, &debt.needs),
        (1, &worker(AGY), &worker(OPUS))
    );
    assert!(bench.tag_target(2).is_ok());
    Ok(())
}

#[tokio::test]
async fn handoff_strategy_spread() -> anyhow::Result<()> {
    let settings = Settings {
        handoff: HandoffSettings {
            fallback: entries(&[CODEX, AGY]),
            ..HandoffSettings::built_in()
        },
        ..Settings::default()
    };
    let mut setup = Setup::new(settings.clone());
    setup.more_cards = 1;
    setup.planner = Some(Arc::new(WalkingPlanner {
        settings,
        strategy: Some(HandoffStrategy::Spread),
    }));
    let bench = Bench::with(setup).await?;

    // The limits service proposes Codex for both of Claude Code's cards; spread balances them.
    bench.hand_off(&card(1), CODEX).await?;
    bench.hand_off(&card(2), CODEX).await?;
    assert_eq!(bench.owner(&card(1)), Some(worker(CODEX)));
    assert_eq!(bench.owner(&card(2)), Some(worker(AGY)));
    Ok(())
}

#[tokio::test]
async fn handoff_strategy_race() -> anyhow::Result<()> {
    let bench = Bench::new().await?;
    let codex = FakeWorker::harness(CODEX, "gpt").then(FakeSession::ended(EndReason::Crashed {
        message: "segfault".to_owned(),
    }));
    let agy = FakeWorker::harness(AGY, "gemini").then(report("Store done"));
    let reviewer = FakeWorker::harness(GPT_REVIEWER, "gpt").then(approval());
    bench.services.workers.add(codex);
    bench.services.workers.add(agy);
    bench.services.workers.add(reviewer);

    let c1 = card(1);
    let action = RecoveryAction::Race {
        to: vec![worker(CODEX), worker(AGY)],
        packet: Box::new(packet(&c1)),
        checkpoint: Some(tag(&c1, 1)),
    };
    handoff::apply(&bench.host, &c1, action).await?;

    assert_eq!(bench.owner(&c1), Some(worker(AGY)));
    // Agy's attempt won only once a reviewer of another family approved it.
    assert_eq!(bench.reviewed_by(&c1), vec![worker(GPT_REVIEWER)]);
    let tagged = bench
        .card_events()
        .into_iter()
        .find_map(|event| match event {
            CardEvent::CheckpointTagged { n, to, .. } => Some((n, to)),
            _ => None,
        });
    assert_eq!(tagged, Some((1, vec![worker(CODEX), worker(AGY)])));
    Ok(())
}

#[tokio::test]
async fn handoff_race_winner_passes_gates_and_review() -> anyhow::Result<()> {
    let bench = Bench::new().await?;
    // Codex is done first, but its review rejects the approach; Agy's slower attempt wins.
    let codex = FakeWorker::harness(CODEX, "gpt").then(report("Store done fast"));
    let agy = FakeWorker::harness(AGY, "gemini")
        .then(report("Store done").with_delay(Duration::from_millis(300)));
    let gemini = FakeWorker::harness(GEMINI_REVIEWER, "gemini")
        .then(verdict("reject", "This store loses writes."));
    let gpt = FakeWorker::harness(GPT_REVIEWER, "gpt").then(approval());
    bench.services.workers.add(codex);
    bench.services.workers.add(agy);
    bench.services.workers.add(gemini);
    bench.services.workers.add(gpt);

    let c1 = card(1);
    let action = RecoveryAction::Race {
        to: vec![worker(CODEX), worker(AGY)],
        packet: Box::new(packet(&c1)),
        checkpoint: None,
    };
    handoff::apply(&bench.host, &c1, action).await?;

    assert_eq!(bench.owner(&c1), Some(worker(AGY)));
    assert_eq!(
        bench.reviewed_by(&c1),
        vec![worker(GEMINI_REVIEWER), worker(GPT_REVIEWER)]
    );
    Ok(())
}

#[tokio::test]
async fn handoff_race_keeps_loser_work() -> anyhow::Result<()> {
    let bench = Bench::new().await?;
    let c1 = card(1);
    // Codex's attempt is slow, and has work in its worktree when Agy's attempt wins.
    bench
        .services
        .workers
        .add(slow_candidate(CODEX, "gpt", "codex-notes.rs"));
    let agy = FakeWorker::harness(AGY, "gemini").then(report("Store done"));
    let reviewer = FakeWorker::harness(GPT_REVIEWER, "gpt").then(approval());
    bench.services.workers.add(agy);
    bench.services.workers.add(reviewer);

    let action = RecoveryAction::Race {
        to: vec![worker(CODEX), worker(AGY)],
        packet: Box::new(packet(&c1)),
        checkpoint: None,
    };
    handoff::apply(&bench.host, &c1, action).await?;

    assert_eq!(bench.owner(&c1), Some(worker(AGY)));
    // The loser's session ended before its tree was committed, as its own recorded work.
    let loser_branch = format!("{}-attempt-1", branch(1));
    let kept = bench
        .repo
        .git(&["show", &format!("{loser_branch}:codex-notes.rs")])?;
    assert_eq!(kept, "// half done");
    let commit = bench
        .card_events()
        .into_iter()
        .find_map(|event| match event {
            CardEvent::Committed(commit) if commit.branch == loser_branch => Some(commit),
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
async fn a_second_race_never_reuses_attempt_trees() -> anyhow::Result<()> {
    let bench = Bench::new().await?;
    let c1 = card(1);
    bench
        .services
        .workers
        .add(slow_candidate(CODEX, "gpt", "codex-notes.rs"));
    let agy = FakeWorker::harness(AGY, "gemini")
        .then(report("Store done"))
        .then(report("Store done again"));
    let reviewer = FakeWorker::harness(GPT_REVIEWER, "gpt")
        .then(approval())
        .then(approval());
    bench.services.workers.add(agy);
    bench.services.workers.add(reviewer);
    let race = || RecoveryAction::Race {
        to: vec![worker(CODEX), worker(AGY)],
        packet: Box::new(packet(&c1)),
        checkpoint: None,
    };

    handoff::apply(&bench.host, &c1, race()).await?;
    let first_loser = format!("{}-attempt-1", branch(1));
    let kept = bench.repo.git(&["rev-parse", &first_loser])?;
    bench.stop_again(&c1)?;
    handoff::apply(&bench.host, &c1, race()).await?;

    // The second race took attempts 3 and 4, and the first loser's branch didn't move.
    assert_eq!(bench.repo.git(&["rev-parse", &first_loser])?, kept);
    for n in [3, 4] {
        let tree = bench.layout.worktrees.join(format!("{c1}-attempt-{n}"));
        assert!(tree.exists(), "{}", tree.display());
    }
    Ok(())
}

#[tokio::test]
async fn handoff_precedence_every_level_set() -> anyhow::Result<()> {
    let level = |n: u32| format!("codex/level-{n}");
    let claude = worker(OPUS).vendor();
    let every_level = || {
        let mut settings = Settings::default();
        let mut top = HandoffSettings::built_in();
        top.fallback = entries(&[&level(5)]);
        top.replace.insert(
            claude.clone(),
            VendorReplacement {
                with: entries(&[&level(4)]),
                ..VendorReplacement::default()
            },
        );
        let mut role = HandoffSettings {
            fallback: entries(&[&level(3)]),
            ..HandoffSettings::default()
        };
        role.replace.insert(
            claude.clone(),
            VendorReplacement {
                with: entries(&[&level(2)]),
                ..VendorReplacement::default()
            },
        );
        settings.handoff = top;
        settings.roles.insert(RoleName::Owner, owner_table(role));
        settings
    };

    // Levels 2 to 5: the most specific list that's set gives the proposal, and the engine
    // applies it. Each round clears the level that won the one before.
    for expected in 2..=5 {
        let mut settings = every_level();
        if expected > 2
            && let Some(role) = settings.roles.get_mut(&RoleName::Owner)
        {
            role.handoff.replace.clear();
        }
        if expected > 3 {
            settings.roles.remove(&RoleName::Owner);
        }
        if expected > 4 {
            settings.handoff.replace.clear();
        }
        let planner = Arc::new(WalkingPlanner {
            settings: settings.clone(),
            strategy: None,
        });
        let mut setup = Setup::new(settings);
        setup.planner = Some(planner.clone());
        let bench = Bench::with(setup).await?;
        let record = bench.host.read(|state| state.cards.get(&card(1)).cloned());
        let record = record.context("card-001")?;
        let query =
            argonauts_crew::handoff::choose::query(&bench.host, &record, &worker(OPUS), false);
        let proposed = planner.shortlist(&query).candidates;
        let proposed = proposed.first().context("a proposal")?.worker.to_string();
        assert_eq!(proposed, level(expected), "level {expected}");
        bench.hand_off(&card(1), &proposed).await?;
        assert_eq!(bench.owner(&card(1)), Some(worker(&level(expected))));
    }

    // Level 1: your pick on the card's handoff card wins over every list.
    let settings = every_level();
    let mut setup = Setup::new(settings.clone());
    setup.planner = Some(Arc::new(WalkingPlanner {
        settings,
        strategy: None,
    }));
    let bench = Bench::with(setup).await?;
    let pick = worker("agy/your-pick");
    let id = CheckpointId::numbered(900);
    let plan = HandoffPlan {
        card: card(1),
        title: "Card 1".to_owned(),
        stopped: worker(OPUS),
        reason: "usage limit".to_owned(),
        resets_at: None,
        work: DiffStat::default(),
        gates_ran: false,
        candidates: vec![candidate(&level(2)), candidate("agy/your-pick")],
        mode: HandoffMode::Ask,
        strategy: HandoffStrategy::First,
        race_count: 1,
        also_moving: Vec::new(),
    };
    bench.record(
        &card(1),
        RunEventKind::Checkpoint(CheckpointEvent::Opened(Box::new(CheckpointOpened {
            id: id.clone(),
            kind: CheckpointKind::Handoff,
            title: "claude-code/opus stopped · card-001".to_owned(),
            card: Some(card(1)),
            packet: CheckpointPacket::Handoff(plan),
            evidence_sha256: "e".repeat(64),
            mode: ApprovalMode::Supervised,
            decide_with: DecideWith::Rules,
            options: Vec::new(),
            free_text: FreeText::None,
            needs_you: true,
        }))),
    )?;
    bench.record(
        &card(1),
        RunEventKind::Checkpoint(CheckpointEvent::Decided(CheckpointDecided {
            id,
            action: AnswerAction::Candidate {
                worker: pick.clone(),
            },
            text: None,
            by: Party::User,
            evidence_sha256: "e".repeat(64),
            shadow: None,
            cards: Vec::new(),
        })),
    )?;
    bench.hand_off(&card(1), &level(2)).await?;
    assert_eq!(bench.owner(&card(1)), Some(pick.clone()));

    // A pick whose vendor is down doesn't hold; the lists decide again.
    bench.stop_again(&card(1))?;
    bench
        .services
        .health
        .set(&pick.vendor(), HealthState::Down, Some("outage"));
    bench.hand_off(&card(1), &level(2)).await?;
    assert_eq!(bench.owner(&card(1)), Some(worker(&level(2))));
    Ok(())
}

#[tokio::test]
async fn checkpoint_tag_at_every_handoff() -> anyhow::Result<()> {
    let bench = Bench::new().await?;
    let c1 = card(1);
    let first = bench.repo.git(&["rev-parse", &branch(1)])?;
    bench.hand_off(&c1, CODEX).await?;
    let second = bench.work("codex.rs", Some(CODEX))?;
    bench.stop_again(&c1)?;
    bench.hand_off(&c1, AGY).await?;
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
        .card_events()
        .into_iter()
        .filter_map(|event| match event {
            CardEvent::CheckpointTagged { n, commit, .. } => Some((n, commit)),
            _ => None,
        })
        .collect();
    assert_eq!(tagged, vec![(1, first), (2, second)]);
    let listed = handoff::tags::list(&bench.host, &c1).await?;
    assert_eq!(listed.len(), 2);
    Ok(())
}

#[tokio::test]
async fn commit_authors_recorded_in_git_notes() -> anyhow::Result<()> {
    let bench = Bench::new().await?;
    let c1 = card(1);
    let note = |sha: &str| {
        bench
            .repo
            .git(&["notes", "--ref=refs/notes/argonauts", "show", sha])
    };
    let by_opus = bench.repo.git(&["rev-parse", &branch(1)])?;
    // Your own commit, after taking the card over, gets no note.
    let yours = bench.work("yours.rs", None)?;
    bench.hand_off(&c1, CODEX).await?;
    let by_codex = bench.work("codex.rs", Some(CODEX))?;
    bench.stop_again(&c1)?;
    bench.hand_off(&c1, AGY).await?;

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
async fn revert_keeps_replaced_work() -> anyhow::Result<()> {
    let bench = Bench::new().await?;
    let c1 = card(1);
    bench.hand_off(&c1, CODEX).await?;
    let checkpoint = bench.tag_target(1)?;
    let replaced = bench.work("codex.rs", Some(CODEX))?;
    bench.stop_again(&c1)?;

    // `/revert` reaches the engine as a command over the control socket.
    assert!(revert_command(&bench, &c1, 2).await?.is_err());
    revert_command(&bench, &c1, 1)
        .await?
        .map_err(anyhow::Error::msg)?;

    assert_eq!(
        git_in(&bench.repo, &bench.tree(), &["rev-parse", "HEAD"])?,
        checkpoint
    );
    assert!(!bench.tree().join("codex.rs").exists());
    let kept_on = tag(&c1, 1).replaced_branch();
    assert_eq!(bench.repo.git(&["rev-parse", &kept_on])?, replaced);
    assert!(bench.card_events().iter().any(|event| matches!(
        event,
        CardEvent::Reverted { to: 1, kept_on: branch } if *branch == kept_on
    )));
    // The debt was for work that is off the branch now, and the card is back in line.
    assert_eq!(bench.debt(&c1), None);
    assert_eq!(bench.state(&c1), Some(CardState::Ready));

    // Work that landed is never reverted; a new change undoes it.
    for event in [
        IntegrationEvent::Merged {
            card: c1.clone(),
            sha: checkpoint.clone(),
        },
        IntegrationEvent::Landed {
            target: "main".to_owned(),
            sha: checkpoint,
            fast_forward: true,
        },
    ] {
        bench
            .host
            .record(None, None, RunEventKind::Integration(event))?;
    }
    let refused = revert_command(&bench, &c1, 1).await?;
    assert!(refused.is_err_and(|why| why.contains("merged")));
    Ok(())
}

/// At the merge card, `/rewind` takes an integrated card back to its checkpoint on both
/// branches: the run's branch gets one commit undoing the work after the tag, the card stays
/// integrated as it was there, and the merge card opens again on the new branch.
#[tokio::test]
async fn revert_undoes_an_integrated_card_on_the_run_branch() -> anyhow::Result<()> {
    let bench = Bench::new().await?;
    let c1 = card(1);
    bench.hand_off(&c1, CODEX).await?;
    let checkpoint = bench.tag_target(1)?;
    let replaced = bench.work("codex.rs", Some(CODEX))?;
    let integration = integrate(&bench).await?;
    let merged = git_in(&bench.repo, &integration, &["rev-parse", "HEAD"])?;
    let merge_card = open_merge_card(&bench)?;

    revert_command(&bench, &c1, 1)
        .await?
        .map_err(anyhow::Error::msg)?;

    assert_eq!(bench.repo.git(&["rev-parse", &branch(1)])?, checkpoint);
    let kept_on = tag(&c1, 1).replaced_branch();
    assert_eq!(bench.repo.git(&["rev-parse", &kept_on])?, replaced);
    assert!(integration.join("store.rs").exists());
    assert!(!integration.join("codex.rs").exists());
    let undo = git_in(&bench.repo, &integration, &["rev-parse", "HEAD"])?;
    assert_eq!(
        git_in(&bench.repo, &integration, &["rev-parse", "HEAD^"])?,
        merged
    );
    let message = git_in(
        &bench.repo,
        &integration,
        &["log", "-1", "--format=%B", &undo],
    )?;
    assert!(
        message.starts_with("Revert card-001 to handoff-1") && message.contains("Conductor"),
        "{message}"
    );
    assert_eq!(bench.state(&c1), Some(CardState::Integrated));
    assert!(bench.sink.events().iter().any(|event| matches!(
        &event.kind,
        RunEventKind::Checkpoint(CheckpointEvent::Withdrawn { id, .. }) if *id == merge_card
    )));
    Ok(())
}

/// Undoing an integrated card's work that later work on the run's branch built on would
/// conflict, so nothing changes on either branch. Nothing changes while another card merges
/// into the run's branch either.
#[tokio::test]
async fn an_integrated_revert_that_cant_apply_changes_nothing() -> anyhow::Result<()> {
    let mut setup = Setup::new(Settings::default());
    setup.more_cards = 1;
    let bench = Bench::with(setup).await?;
    let c1 = card(1);
    bench.hand_off(&c1, CODEX).await?;
    bench.work("codex.rs", Some(CODEX))?;
    let integration = integrate(&bench).await?;
    commit_in(
        &bench.repo,
        &integration,
        "codex.rs",
        "built on\n",
        "Later work",
    )?;
    let heads = || -> anyhow::Result<[String; 2]> {
        Ok([
            bench.repo.git(&["rev-parse", &branch(1)])?,
            git_in(&bench.repo, &integration, &["rev-parse", "HEAD"])?,
        ])
    };
    let before = heads()?;

    let refused = revert_command(&bench, &c1, 1).await?;
    assert!(
        refused.as_ref().is_err_and(|why| why.contains("conflicts")),
        "{refused:?}"
    );
    assert_eq!(heads()?, before);

    bench.move_through(&card(2), &[CardState::Integrating])?;
    let refused = revert_command(&bench, &c1, 1).await?;
    assert!(
        refused
            .as_ref()
            .is_err_and(|why| why.contains("being merged")),
        "{refused:?}"
    );
    assert_eq!(heads()?, before);
    assert!(
        !bench
            .card_events()
            .iter()
            .any(|event| matches!(event, CardEvent::Reverted { .. }))
    );
    Ok(())
}

/// Merges card-001's branch into the integration branch as its merge does, and marks the card
/// integrated. Returns the integration worktree.
async fn integrate(bench: &Bench) -> anyhow::Result<PathBuf> {
    let integration = ensure_integration(&bench.host).await?;
    let merge = [
        "merge",
        "--no-ff",
        "--quiet",
        "-m",
        "Merge card-001",
        &branch(1),
    ];
    git_in(&bench.repo, &integration, &merge)?;
    let sha = git_in(&bench.repo, &integration, &["rev-parse", "HEAD"])?;
    bench.record(
        &card(1),
        RunEventKind::Integration(IntegrationEvent::Merged { card: card(1), sha }),
    )?;
    bench.move_through(&card(1), &[CardState::Integrated])?;
    Ok(integration)
}

/// A merge card waiting for you, as the merge step opens it.
fn open_merge_card(bench: &Bench) -> anyhow::Result<CheckpointId> {
    let id = CheckpointId::numbered(800);
    let packet = MergePacket {
        target: Some("main".to_owned()),
        branch: format!("argonauts/{TEST_RUN}/integration"),
        rows: Vec::new(),
        suite_passed_on_latest_main: true,
        gates: Vec::new(),
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
    };
    bench.host.record(
        None,
        None,
        RunEventKind::Checkpoint(CheckpointEvent::Opened(Box::new(CheckpointOpened {
            id: id.clone(),
            kind: CheckpointKind::Merge,
            title: "Merge this run into main?".to_owned(),
            card: None,
            packet: CheckpointPacket::Merge(packet),
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

async fn revert_command(
    bench: &Bench,
    card: &CardId,
    checkpoint: u32,
) -> anyhow::Result<Result<(), String>> {
    let (reply, receiver) = tokio::sync::oneshot::channel();
    let command = EngineCommand::Revert {
        card: card.clone(),
        checkpoint,
        reply,
    };
    commands::handle(&bench.host, command).await?;
    Ok(receiver.await?)
}

#[tokio::test]
async fn revert_refused_unless_the_card_is_at_rest() -> anyhow::Result<()> {
    let bench = Bench::new().await?;
    let c1 = card(1);
    bench.hand_off(&c1, CODEX).await?;
    let head = bench.repo.git(&["rev-parse", &branch(1)])?;
    // Ready, then the gates and review: the branch is about to be worked on or judged.
    for path in [
        &[][..],
        &[CardState::Running, CardState::Gating][..],
        &[CardState::InReview][..],
        &[CardState::Integrating][..],
    ] {
        bench.move_through(&c1, path)?;
        let refused = revert::revert(&bench.host, &c1, 1).await;
        assert!(refused.is_err(), "{:?}", bench.state(&c1));
    }
    assert_eq!(bench.repo.git(&["rev-parse", &branch(1)])?, head);
    Ok(())
}

/// Hands card-001 to Codex (with debt), and sets the return rule.
async fn handed_to_codex(on_return: Option<OnReturn>) -> anyhow::Result<Bench> {
    let mut settings = Settings {
        handoff: HandoffSettings::built_in(),
        ..Settings::default()
    };
    settings.handoff.on_return = on_return.or(settings.handoff.on_return);
    let bench = Bench::with(Setup::new(settings)).await?;
    bench.hand_off(&card(1), CODEX).await?;
    bench.work("codex.rs", Some(CODEX))?;
    ensure!(bench.debt(&card(1)).is_some(), "the handoff left no debt");
    Ok(bench)
}

/// Without debt the card stays with its replacement when the original is back. A quota that
/// ran out counts until its reset passes; then the notice names the vendor the way the screen
/// does and says how to move the card back.
#[tokio::test]
async fn original_back_after_its_quota_reset_leaves_a_notice() -> anyhow::Result<()> {
    let mut settings = Settings {
        handoff: HandoffSettings::built_in(),
        ..Settings::default()
    };
    settings.handoff.unknown_tier = Some(UnknownTier::Equal);
    let bench = Bench::with(Setup::new(settings)).await?;
    let c1 = card(1);
    bench.hand_off(&c1, CODEX).await?;
    ensure!(bench.debt(&c1).is_none(), "a handoff to an equal left debt");
    let claude = worker(OPUS).vendor();
    let rejected = |resets_at| QuotaReading {
        vendor: claude.clone(),
        status: QuotaStatus::Rejected,
        used: Some(1.0),
        resets_at: Some(resets_at),
        window: Some("five_hour".to_owned()),
        source: QuotaSource::RateLimitEvent,
        observed_at: bench.host.now(),
        detail: None,
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
        .quota
        .observe(rejected(bench.host.now() + time::Duration::hours(1)));
    on_return::vendor_back(&bench.host, &claude).await?;
    assert!(notes().is_empty(), "{:?}", notes());

    bench
        .services
        .quota
        .observe(rejected(bench.host.now() - time::Duration::seconds(1)));
    on_return::vendor_back(&bench.host, &claude).await?;
    assert_eq!(
        notes(),
        [format!(
            "Claude Code is back · card-001 stays with {CODEX} · /reassign card-001 {OPUS} moves it back"
        )]
    );
    assert_eq!(bench.owner(&c1), Some(worker(CODEX)));
    Ok(())
}

#[tokio::test]
async fn on_return_ask() -> anyhow::Result<()> {
    let bench = handed_to_codex(Some(OnReturn::Ask)).await?;
    let c1 = card(1);
    on_return::vendor_back(&bench.host, &worker(OPUS).vendor()).await?;
    let opened = bench.opened(CheckpointKind::OriginalReturned).await?;
    let CheckpointPacket::OriginalReturned(packet) = &opened.packet else {
        bail!("not an original-returned packet");
    };
    assert_eq!((packet.since, packet.commits), (1, 1));
    assert_eq!(packet.replacement, worker(CODEX));
    let keys: Vec<char> = opened.options.iter().map(|option| option.key).collect();
    assert_eq!(keys, ['r', 'o', 'u', 'y', 't', 'd']);

    bench.answer(&opened, AnswerAction::HandBack)?;
    // The card isn't at rest, so its pipeline applies the choice at its next safe point.
    let mut changed = false;
    for _ in 0..200 {
        let applied = on_return::apply_pending(&bench.host, &c1).await?;
        changed = matches!(applied, Applied::NewOwner { .. });
        if changed {
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    assert!(changed);
    assert_eq!(bench.owner(&c1), Some(worker(OPUS)));
    Ok(())
}

#[tokio::test]
async fn on_return_keep() -> anyhow::Result<()> {
    let bench = handed_to_codex(Some(OnReturn::Keep)).await?;
    let c1 = card(1);
    on_return::vendor_back(&bench.host, &worker(OPUS).vendor()).await?;
    assert_eq!(
        on_return::apply_pending(&bench.host, &c1).await?,
        Applied::Nothing
    );
    assert_eq!(bench.owner(&c1), Some(worker(CODEX)));
    assert!(
        bench.debt(&c1).is_some(),
        "the debt stays for the merge rule"
    );
    Ok(())
}

#[tokio::test]
async fn on_return_review() -> anyhow::Result<()> {
    let bench = handed_to_codex(Some(OnReturn::Review)).await?;
    bench
        .services
        .workers
        .add(FakeWorker::harness(OPUS, "claude").then(approval()));
    let c1 = card(1);
    // Before the gates pass, the original has nothing finished to review.
    bench.move_through(&c1, &[CardState::Running, CardState::Gating])?;
    assert_eq!(
        on_return::apply_pending(&bench.host, &c1).await?,
        Applied::Nothing
    );
    assert!(bench.reviewed_by(&c1).is_empty());

    // The original reviews once, though the ring's reviewers are other families and Codex owns
    // the card; its approval clears the debt, and the card's own review follows.
    bench.move_through(&c1, &[CardState::InReview])?;
    assert_eq!(
        on_return::apply_pending(&bench.host, &c1).await?,
        Applied::Nothing
    );
    assert_eq!(bench.owner(&c1), Some(worker(CODEX)));
    assert_eq!(bench.debt(&c1), None);
    assert!(bench.card_events().iter().any(|event| matches!(
        event,
        CardEvent::DebtCleared { by } if *by == worker(OPUS)
    )));
    assert_eq!(bench.state(&c1), Some(CardState::InReview));
    assert_eq!(
        on_return::apply_pending(&bench.host, &c1).await?,
        Applied::Nothing
    );
    assert_eq!(bench.reviewed_by(&c1), vec![worker(OPUS)]);
    Ok(())
}

#[tokio::test]
async fn on_return_review_sends_changes_back() -> anyhow::Result<()> {
    let bench = handed_to_codex(Some(OnReturn::Review)).await?;
    bench.services.workers.add(
        FakeWorker::harness(OPUS, "claude")
            .then(verdict("request_changes", "Writes aren't retried.")),
    );
    let c1 = card(1);
    bench.move_through(
        &c1,
        &[CardState::Running, CardState::Gating, CardState::InReview],
    )?;
    let applied = on_return::apply_pending(&bench.host, &c1).await?;
    let Applied::Changes { comments, .. } = applied else {
        bail!("the original's changes weren't sent back: {applied:?}");
    };
    assert!(comments.iter().any(|line| line.contains("Handle errors")));
    assert_eq!(bench.state(&c1), Some(CardState::Running));
    assert_eq!(bench.owner(&c1), Some(worker(CODEX)));
    // Its review happened, so the debt is paid, and it isn't asked again.
    assert_eq!(bench.debt(&c1), None);
    assert_eq!(
        on_return::apply_pending(&bench.host, &c1).await?,
        Applied::Nothing
    );
    Ok(())
}

#[tokio::test]
async fn on_return_hand_back() -> anyhow::Result<()> {
    let bench = handed_to_codex(Some(OnReturn::HandBack)).await?;
    let c1 = card(1);
    // At rest, so the choice applies as soon as the vendor is back.
    bench.stop_again(&c1)?;
    on_return::vendor_back(&bench.host, &worker(OPUS).vendor()).await?;
    assert_eq!(bench.owner(&c1), Some(worker(OPUS)));
    assert!(bench.card_events().iter().any(|event| matches!(
        event,
        CardEvent::Reassigned { from: Some(from), to, .. }
            if *from == worker(CODEX) && *to == worker(OPUS)
    )));
    // It's told what changed since the checkpoint.
    assert!(
        bench
            .handoff_events()
            .iter()
            .any(|event| matches!(event, HandoffEvent::Packet { .. }))
    );
    Ok(())
}

#[tokio::test]
async fn on_return_redo() -> anyhow::Result<()> {
    let bench = handed_to_codex(Some(OnReturn::Redo)).await?;
    let c1 = card(1);
    let checkpoint = bench.tag_target(1)?;
    let replaced = bench.repo.git(&["rev-parse", &branch(1)])?;
    let applied = on_return::apply_pending(&bench.host, &c1).await?;
    assert!(matches!(applied, Applied::NewOwner { .. }), "{applied:?}");

    assert_eq!(bench.owner(&c1), Some(worker(OPUS)));
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
async fn on_return_redo_while_gating() -> anyhow::Result<()> {
    let bench = handed_to_codex(Some(OnReturn::Redo)).await?;
    let c1 = card(1);
    let checkpoint = bench.tag_target(1)?;
    // The original came back while Codex's session ran; the choice applies once it ended.
    bench.move_through(&c1, &[CardState::Running, CardState::Gating])?;
    let applied = on_return::apply_pending(&bench.host, &c1).await?;
    assert!(matches!(applied, Applied::NewOwner { .. }), "{applied:?}");

    // The gates never run on the checkpoint's half-done work: the original redoes it.
    assert_eq!(bench.state(&c1), Some(CardState::Running));
    assert_eq!(bench.owner(&c1), Some(worker(OPUS)));
    assert_eq!(
        git_in(&bench.repo, &bench.tree(), &["rev-parse", "HEAD"])?,
        checkpoint
    );
    assert_eq!(bench.debt(&c1), None);
    Ok(())
}

#[tokio::test]
async fn on_return_hand_back_while_in_review() -> anyhow::Result<()> {
    let bench = handed_to_codex(Some(OnReturn::HandBack)).await?;
    let c1 = card(1);
    bench.move_through(
        &c1,
        &[CardState::Running, CardState::Gating, CardState::InReview],
    )?;
    let applied = on_return::apply_pending(&bench.host, &c1).await?;
    assert!(matches!(applied, Applied::NewOwner { .. }), "{applied:?}");
    assert_eq!(bench.state(&c1), Some(CardState::Running));
    assert_eq!(bench.owner(&c1), Some(worker(OPUS)));
    Ok(())
}
