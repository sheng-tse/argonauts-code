//! The card pipeline: parallel worktrees, gates before review, the gate and retry ladders,
//! escalation answers, card session lifetimes, and the status part for workers without a
//! status tool. Each scenario seeds an approved goal and its cards, then runs the `build` step.
mod support;

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, bail};
use serde_json::{Value, json};

use argonauts_core::api::CheckpointAnswer;
use argonauts_core::events::card::{
    AttemptCause, CardBudget, CardEvent, CardKind, CardSpec, CardState, GateKind,
};
use argonauts_core::events::checkpoint::{
    AnswerAction, CheckpointDecided, CheckpointEvent, CheckpointKind, CheckpointOpened,
    CheckpointPacket, FreeText, PlanPacket,
};
use argonauts_core::events::goal::{Criterion, GoalEvent, GoalFile, GoalSource};
use argonauts_core::events::plan::{BudgetPlan, TaskGraph};
use argonauts_core::events::run::{Party, RunLifecycle, RunMode, RunStarted};
use argonauts_core::events::session::{InputPartKind, SessionEvent};
use argonauts_core::events::vendor::{BudgetEvent, RunBudget};
use argonauts_core::events::{RunEvent, RunEventKind};
use argonauts_core::limits::{Candidate, CostEstimate};
use argonauts_core::names::{CheckpointId, WorkerRef, WorkflowId};
use argonauts_core::policy::EscalationFacts;
use argonauts_core::settings::approvals::{ApprovalMode, DecideWith};
use argonauts_core::settings::budget::{BudgetAmount, BudgetUnit};
use argonauts_core::settings::protocol::GateCommand;
use argonauts_core::steps::StepName;
use argonauts_core::worker::{SessionSpec, Worker, WorkerCapabilities};
use argonauts_core::workflow::{StandardPhase, StepRef};
use argonauts_core::{CardId, RunId, Spend};
use argonauts_crew::card;
use argonauts_crew::card::gates::{self, NO_COMMAND_GATES};
use argonauts_crew::checkpoints;
use argonauts_crew::engine::host::Host;
use argonauts_crew::engine::shared::Shared;
use argonauts_crew::git::Git;
use argonauts_crew::state::RunState;
use argonauts_crew::workflow::{StepAt, StepCall, StepDone};
use argonauts_testkit::services::MemorySink;
use argonauts_testkit::worker::{FakeSession, FakeWorker};

use support::Crew;

const OWNER_A: &str = "claude-code/claude-x";
const OWNER_B: &str = "codex/gpt-x";
const REVIEWER: &str = "agy/gemini-x";

#[tokio::test(flavor = "multi_thread")]
async fn cards_run_in_parallel_worktrees() -> anyhow::Result<()> {
    let crew = Crew::new()?;
    let slow = Duration::from_millis(200);
    let a = crew.services.workers.add(slow_owner(
        OWNER_A,
        &[("src/a.rs", "pub fn a() {}\n")],
        slow,
    ));
    let b = crew.services.workers.add(slow_owner(
        OWNER_B,
        &[("src/b.rs", "pub fn b() {}\n")],
        slow,
    ));
    crew.services.workers.add(reviewer(REVIEWER));
    let cards = vec![
        spec("card-001", &[], OWNER_A, REVIEWER),
        spec("card-002", &[], OWNER_B, REVIEWER),
    ];
    let bench = Bench::new(&crew, RunMode::Crew, &cards).await?;

    let done = bench.build().await?;
    assert_eq!(done.summary.as_deref(), Some("2 cards integrated"));
    let events = bench.sink.events();

    // Both owner sessions started before either ended.
    let owner_sessions: Vec<(u64, bool)> = events
        .iter()
        .filter(|e| e.card_id.is_some())
        .filter_map(|e| match &e.kind {
            RunEventKind::Session(SessionEvent::Started(s)) if s.step == Some(StepName::Build) => {
                Some((e.seq, true))
            }
            RunEventKind::Session(SessionEvent::Ended(_)) => Some((e.seq, false)),
            _ => None,
        })
        .collect();
    let starts: Vec<u64> = owner_sessions
        .iter()
        .filter(|(_, s)| *s)
        .map(|(q, _)| *q)
        .collect();
    assert_eq!(starts.len(), 2, "{owner_sessions:?}");
    let first_end = owner_sessions
        .iter()
        .find(|(_, s)| !*s)
        .map(|(q, _)| *q)
        .unwrap();
    assert!(starts.iter().all(|s| *s < first_end), "{owner_sessions:?}");

    // Each card in its own worktree on its own branch.
    let trees = worktrees(&events);
    assert_eq!(trees.len(), 2);
    let run = RunId::new("run-01")?;
    for (id, worker) in [("card-001", &a), ("card-002", &b)] {
        let (path, branch) = &trees[&CardId::new(id)?];
        assert_eq!(branch, &format!("argonauts/{run}/{id}"));
        assert!(path.ends_with(id), "{path:?}");
        assert_eq!(worker.specs()[0].workdir, *path);
    }
    assert_ne!(
        trees[&CardId::new("card-001")?].0,
        trees[&CardId::new("card-002")?].0
    );
    assert!(state_of(&bench, "card-001") == CardState::Integrated);
    assert!(state_of(&bench, "card-002") == CardState::Integrated);
    Ok(())
}

/// An agent that commits part of its work keeps its commit, the conductor commits only what it
/// left, and git notes name the agent as the author of both.
#[tokio::test(flavor = "multi_thread")]
async fn an_agents_own_commits_are_kept_and_noted() -> anyhow::Result<()> {
    let crew = Crew::new()?;
    crew.services.workers.add(
        FakeWorker::harness(OWNER_B, &family_of(OWNER_B)).respond_with(|spec| {
            let dir = &spec.workdir;
            if !dir.join("committed.rs").exists() {
                let _ = std::fs::write(dir.join("committed.rs"), "fn a() {}\n");
                git_in(dir, &["add", "committed.rs"]);
                git_in(dir, &["commit", "-qm", "Add a"]);
                let _ = std::fs::write(dir.join("left.rs"), "fn b() {}\n");
            }
            FakeSession::structured(report("done"))
        }),
    );
    crew.services.workers.add(reviewer(REVIEWER));
    let bench = Bench::new(
        &crew,
        RunMode::Crew,
        &[spec("card-001", &[], OWNER_B, REVIEWER)],
    )
    .await?;
    bench.build().await?;

    let commits: Vec<_> = card_events(&bench.sink.events(), "card-001")
        .into_iter()
        .filter_map(|event| match event {
            CardEvent::Committed(info) if info.sha.is_some() => Some(info),
            _ => None,
        })
        .collect();
    let [own, left] = commits.as_slice() else {
        bail!("expected the agent's commit and the conductor's, got {commits:?}");
    };
    assert_eq!(own.message.trim(), "Add a");
    assert!(
        left.message.starts_with("card-001: work from"),
        "{}",
        left.message
    );
    for commit in [own, left] {
        assert_eq!(commit.by, Some(worker(OWNER_B)));
        let sha = commit.sha.as_deref().context("no sha")?;
        let note = crew
            .repo
            .git(&["notes", "--ref=refs/notes/argonauts", "show", sha])?;
        assert_eq!(note.trim(), format!("author: {OWNER_B}"));
    }
    let left_sha = left.sha.as_deref().context("no sha")?;
    let files = crew
        .repo
        .git(&["show", "--name-only", "--format=", left_sha])?;
    assert_eq!(files.trim(), "left.rs");
    assert_eq!(left.stat.files_changed, 1);
    Ok(())
}

/// Runs git in `dir` as an agent would, with its own identity.
fn git_in(dir: &std::path::Path, args: &[&str]) {
    let status = std::process::Command::new("git")
        .args(["-c", "user.name=agent", "-c", "user.email=agent@local"])
        .args(args)
        .current_dir(dir)
        .status();
    assert!(status.is_ok_and(|status| status.success()), "git {args:?}");
}

#[tokio::test(flavor = "multi_thread")]
async fn gates_before_review() -> anyhow::Result<()> {
    let mut crew = Crew::new()?;
    crew.settings.gates.run = vec![test_gate()];
    let owner = crew
        .services
        .workers
        .add(owner(OWNER_B, &[("src/a.rs", "fn a() {}\n")]));
    let reviewer = crew.services.workers.add(reviewer(REVIEWER));
    // The first gate run fails, the resumed session fixes it.
    crew.services.gates.fail_next("1 failed");
    let bench = Bench::new(
        &crew,
        RunMode::Crew,
        &[spec("card-001", &[], OWNER_B, REVIEWER)],
    )
    .await?;

    bench.build().await?;
    let events = bench.sink.events();
    let gate_seqs: Vec<u64> = events
        .iter()
        .filter(|e| matches!(&e.kind, RunEventKind::Gate(g) if g.kind == GateKind::Command))
        .map(|e| e.seq)
        .collect();
    let first_review = events
        .iter()
        .find(|e| matches!(e.kind, RunEventKind::Review(_)))
        .map(|e| e.seq)
        .context("no review recorded")?;
    assert_eq!(gate_seqs.len(), 2, "one failed run and one passing run");
    assert!(gate_seqs.iter().all(|g| *g < first_review));
    assert_eq!(
        owner.specs().len(),
        2,
        "the owner resumed once for the gate output"
    );
    assert_eq!(
        reviewer.specs().len(),
        1,
        "a card failing its gates never reaches review"
    );
    assert_eq!(
        states(&events, "card-001"),
        [
            CardState::Waiting,
            CardState::Ready,
            CardState::Running,
            CardState::Gating,
            CardState::Running,
            CardState::Gating,
            CardState::InReview,
            CardState::Integrating,
            CardState::Integrated,
        ]
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn no_command_gates_blocks_auto_merge() -> anyhow::Result<()> {
    let crew = Crew::new()?;
    assert!(crew.settings.gates.run.is_empty());
    crew.services
        .workers
        .add(owner(OWNER_B, &[("src/a.rs", "fn a() {}\n")]));
    crew.services.workers.add(reviewer(REVIEWER));
    let bench = Bench::new(
        &crew,
        RunMode::Crew,
        &[spec("card-001", &[], OWNER_B, REVIEWER)],
    )
    .await?;

    bench.build().await?;
    let record = bench
        .host
        .read(|state| state.cards.get(&CardId::new("card-001").unwrap()).cloned())
        .context("no card")?;
    let marker = record
        .gates
        .iter()
        .find(|g| gates::is_no_command_gates(g))
        .context("no gates cell note")?;
    assert_eq!(marker.summary, NO_COMMAND_GATES);
    assert!(
        marker.skipped,
        "an empty gates.run is recorded as skipped, not as a failure"
    );
    // Merge rules that need the suite see it as not run.
    assert_eq!(gates::gates_passed(&record.gates), None);
    // The built-in checks still ran on the agent's commits.
    for kind in [
        GateKind::ProtectedPaths,
        GateKind::SecretScan,
        GateKind::TestTamper,
    ] {
        assert!(
            record.gates.iter().any(|g| g.kind == kind && g.passed),
            "{kind:?}"
        );
    }
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn a_done_report_needs_its_changes_on_the_branch() -> anyhow::Result<()> {
    let crew = Crew::new()?;
    let resumed_with = Arc::new(std::sync::Mutex::new(String::new()));
    let seen = resumed_with.clone();
    // The first session claims a change it never made. Told so, its resume makes it.
    crew.services.workers.add(
        FakeWorker::harness(OWNER_B, &family_of(OWNER_B)).respond_with(move |spec| {
            if spec.resume.is_some() {
                *seen.lock().unwrap() = format!("{:?}", spec.prompt);
                return work(spec, &[("src/a.rs", "fn a() {}\n")], Duration::ZERO);
            }
            let mut claimed = report("done");
            claimed["files_changed"] = json!(["src/a.rs"]);
            FakeSession::structured(claimed).with_vendor_session(&format!("v-{}", spec.key))
        }),
    );
    crew.services.workers.add(reviewer(REVIEWER));
    let bench = Bench::new(
        &crew,
        RunMode::Crew,
        &[spec("card-001", &[], OWNER_B, REVIEWER)],
    )
    .await?;

    bench.build().await?;
    let events = bench.sink.events();
    assert_eq!(
        attempts(&events, "card-001"),
        [
            (AttemptCause::First, worker(OWNER_B)),
            (AttemptCause::GateRetry, worker(OWNER_B)),
        ]
    );
    assert!(
        resumed_with
            .lock()
            .unwrap()
            .contains("the report lists src/a.rs, but the branch has no changes"),
        "{}",
        resumed_with.lock().unwrap()
    );
    let record = bench
        .host
        .read(|state| state.cards.get(&CardId::new("card-001").unwrap()).cloned())
        .context("no card")?;
    let checks: Vec<bool> = record
        .gates
        .iter()
        .filter(|g| g.kind == GateKind::ReportedChanges)
        .map(|g| g.passed)
        .collect();
    assert_eq!(checks, [false], "the resume's changes need no check");
    assert_eq!(state_of(&bench, "card-001"), CardState::Integrated);
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn an_owner_that_changed_nothing_says_so() -> anyhow::Result<()> {
    let crew = Crew::new()?;
    crew.services.workers.add(owner(OWNER_B, &[]));
    crew.services.workers.add(reviewer(REVIEWER));
    let bench = Bench::new(
        &crew,
        RunMode::Crew,
        &[spec("card-001", &[], OWNER_B, REVIEWER)],
    )
    .await?;

    bench.build().await?;
    let record = bench
        .host
        .read(|state| state.cards.get(&CardId::new("card-001").unwrap()).cloned())
        .context("no card")?;
    let check = record
        .gates
        .iter()
        .find(|g| g.kind == GateKind::ReportedChanges)
        .context("no check of the empty branch")?;
    assert!(check.passed);
    assert_eq!(
        check.summary,
        "no changes on the branch, as the report says"
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn retry_ladder_same_then_other_then_escalate() -> anyhow::Result<()> {
    let mut crew = Crew::new()?;
    crew.settings.gates.run = vec![test_gate()];
    // The run's meter records the budget from the settings as sessions spend.
    crew.settings.budget.enabled = false;
    crew.services
        .workers
        .add(owner(OWNER_B, &[("src/a.rs", "fn a() {}\n")]));
    crew.services
        .workers
        .add(owner(OWNER_A, &[("src/a.rs", "fn a() { 1 }\n")]));
    crew.services.workers.add(reviewer(REVIEWER));
    crew.services
        .handoff
        .set_candidates(vec![candidate(OWNER_A)]);
    for _ in 0..4 {
        crew.services.gates.fail_next("still failing");
    }
    let bench = Bench::new(
        &crew,
        RunMode::Crew,
        &[spec("card-001", &[], OWNER_B, REVIEWER)],
    )
    .await?;
    // Budgets off, so one more attempt always fits.
    let budgets_off = RunBudget {
        spent: Spend::default(),
        limit: None,
        lines: Vec::new(),
        reserve_left: None,
    };
    bench.host.record(
        None,
        None,
        RunEventKind::Budget(BudgetEvent::Run(budgets_off)),
    )?;

    let build = bench.spawn_build();
    let checkpoint = bench
        .answer(CheckpointKind::Escalation, AnswerAction::Cancel, None)
        .await?;
    let done = finish(build).await?;
    assert_eq!(
        done.summary.as_deref(),
        Some("0 cards integrated, 1 cancelled")
    );

    let events = bench.sink.events();
    let attempts = attempts(&events, "card-001");
    assert_eq!(
        attempts,
        [
            (AttemptCause::First, worker(OWNER_B)),
            (AttemptCause::GateRetry, worker(OWNER_B)),
            (AttemptCause::AfterFailure, worker(OWNER_A)),
            (AttemptCause::GateRetry, worker(OWNER_A)),
        ]
    );
    let reassigned: Vec<_> = card_events(&events, "card-001")
        .into_iter()
        .filter_map(|e| match e {
            CardEvent::Reassigned { to, by, .. } => Some((to, by)),
            _ => None,
        })
        .collect();
    assert_eq!(reassigned, [(worker(OWNER_A), Party::Conductor)]);
    let states = states(&events, "card-001");
    let failed = states.iter().filter(|s| **s == CardState::Failed).count();
    assert_eq!(failed, 2, "{states:?}");
    assert_eq!(states[states.len() - 2], CardState::Escalated);
    assert_eq!(states.last(), Some(&CardState::Cancelled));
    assert!(opened(&events).iter().any(|(id, _)| *id == checkpoint.0));

    // The policy saw what it needs to grant one more attempt: both families tried, no
    // fallback of a third family left, and none granted yet.
    let evaluated = crew.services.policy.evaluated();
    let (_, evidence) = evaluated
        .iter()
        .find(|(point, _)| point.checkpoint == checkpoint.0)
        .context("the policy never saw the escalation")?;
    let facts = evidence
        .facts
        .escalation
        .clone()
        .context("no escalation facts")?;
    assert_eq!(
        facts,
        EscalationFacts {
            handoff_mode: crew.settings.handoff.mode,
            families_tried: vec!["claude".to_owned(), "gpt".to_owned()],
            next_family: None,
            extra_attempts: 0,
            fits_reserve: true,
        }
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn escalation_defer_and_split() -> anyhow::Result<()> {
    // Deferred: the card waits and the run goes on without it, and without the card that
    // depends on it.
    let mut crew = Crew::new()?;
    crew.settings.gates.run = vec![test_gate()];
    crew.services
        .workers
        .add(owner(OWNER_B, &[("src/a.rs", "fn a() {}\n")]));
    crew.services.workers.add(reviewer(REVIEWER));
    crew.services.gates.fail_next("1 failed");
    crew.services.gates.fail_next("1 failed");
    let bench = Bench::new(
        &crew,
        RunMode::Crew,
        &[
            spec("card-001", &[], OWNER_B, REVIEWER),
            spec("card-002", &["card-001"], OWNER_B, REVIEWER),
        ],
    )
    .await?;
    let build = bench.spawn_build();
    bench
        .answer(CheckpointKind::Escalation, AnswerAction::Defer, None)
        .await?;
    let done = finish(build).await?;
    assert_eq!(
        done.summary.as_deref(),
        Some("0 cards integrated, 1 deferred, 1 waiting on deferred cards")
    );
    assert_eq!(state_of(&bench, "card-001"), CardState::Deferred);
    assert_eq!(state_of(&bench, "card-002"), CardState::Waiting);

    // Split: your note becomes new cards through PlanToCards and the graph validator.
    let mut crew = Crew::new()?;
    crew.settings.gates.run = vec![test_gate()];
    // The build's phase lead owns its critical-path card, so this card's owner splits it.
    let proposal = json!({
        "cards": [
            {"key": "store", "title": "Store", "task": "The store.", "kind": "build",
             "owned_paths": ["src/store/**"], "read_only_paths": [], "depends_on": [],
             "criteria": ["G1"]},
            {"key": "api", "title": "API", "task": "The API.", "kind": "build",
             "owned_paths": ["src/api/**"], "read_only_paths": [], "depends_on": ["store"],
             "criteria": ["G1"]}
        ]
    });
    let splitter = FakeWorker::harness(OWNER_B, "gpt").respond_with(move |spec| {
        if spec.step == Some(StepName::PlanToCards) {
            return FakeSession::structured(proposal.clone());
        }
        work(spec, &[("src/part.rs", "fn part() {}\n")], Duration::ZERO)
    });
    crew.services.workers.add(splitter);
    crew.services.workers.add(reviewer(REVIEWER));
    crew.services.gates.fail_next("1 failed");
    crew.services.gates.fail_next("1 failed");
    let bench = Bench::new(
        &crew,
        RunMode::Crew,
        &[spec("card-001", &[], OWNER_B, REVIEWER)],
    )
    .await?;
    let build = bench.spawn_build();
    bench
        .answer(
            CheckpointKind::Escalation,
            AnswerAction::Split,
            Some("one card for the store, one for the API"),
        )
        .await?;
    let done = finish(build).await?;
    assert_eq!(
        done.summary.as_deref(),
        Some("2 cards integrated, 1 cancelled")
    );
    let events = bench.sink.events();
    let last = card_events(&events, "card-001")
        .into_iter()
        .rev()
        .find_map(|e| match e {
            CardEvent::State { to, reason, .. } => Some((to, reason)),
            _ => None,
        })
        .context("no state")?;
    assert_eq!(last.0, CardState::Cancelled);
    assert_eq!(last.1, "split into card-002, card-003");
    let created: Vec<CardSpec> = events
        .iter()
        .filter_map(|e| match &e.kind {
            RunEventKind::Card(CardEvent::Created(spec)) => Some((**spec).clone()),
            _ => None,
        })
        .filter(|spec| spec.id.as_str() != "card-001")
        .collect();
    let ids: Vec<&str> = created.iter().map(|s| s.id.as_str()).collect();
    assert_eq!(ids, ["card-002", "card-003"]);
    assert_eq!(created[1].depends_on, [CardId::new("card-002")?]);
    assert_eq!(created[1].owner, Some(worker(OWNER_B)));
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn lifetime_card_resumes_on_retry() -> anyhow::Result<()> {
    let mut crew = Crew::new()?;
    crew.settings.gates.run = vec![test_gate()];
    let owner = crew
        .services
        .workers
        .add(owner(OWNER_B, &[("src/a.rs", "fn a() {}\n")]));
    crew.services.workers.add(reviewer(REVIEWER));
    crew.services.gates.fail_next("1 failed");
    let bench = Bench::new(
        &crew,
        RunMode::Crew,
        &[spec("card-001", &[], OWNER_B, REVIEWER)],
    )
    .await?;

    bench.build().await?;
    let specs = owner.specs();
    assert_eq!(specs.len(), 2);
    assert_eq!(specs[0].resume, None, "the card's first session is fresh");
    assert_eq!(
        specs[1].resume.as_deref(),
        Some(format!("v-{}", specs[0].key).as_str()),
        "the gate retry resumes the card's own session"
    );
    let kinds: Vec<InputPartKind> = specs[1].prompt.iter().map(|p| p.kind).collect();
    assert!(kinds.contains(&InputPartKind::GateOutput), "{kinds:?}");
    assert!(
        !kinds.contains(&InputPartKind::Prompt),
        "a resume sends only what's new"
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn status_part_on_each_delivery_without_tool() -> anyhow::Result<()> {
    for status_tool in [false, true] {
        let mut crew = Crew::new()?;
        crew.settings.gates.run = vec![test_gate()];
        let capabilities = WorkerCapabilities {
            status_tool,
            ..FakeWorker::harness(OWNER_B, "gpt").capabilities()
        };
        let owner = crew
            .services
            .workers
            .add(owner(OWNER_B, &[("src/a.rs", "fn a() {}\n")]).with_capabilities(capabilities));
        crew.services.workers.add(reviewer(REVIEWER));
        crew.services.gates.fail_next("1 failed");
        let bench = Bench::new(
            &crew,
            RunMode::Crew,
            &[spec("card-001", &[], OWNER_B, REVIEWER)],
        )
        .await?;
        bench.build().await?;

        let specs = owner.specs();
        let status_parts = |i: usize| {
            specs[i]
                .prompt
                .iter()
                .filter(|p| p.kind == InputPartKind::Status)
                .count()
        };
        assert_eq!(
            status_parts(0),
            1,
            "the first prompt always has the status summary"
        );
        let expected = usize::from(!status_tool);
        assert_eq!(status_parts(1), expected, "status_tool = {status_tool}");
    }
    Ok(())
}

// Scenario helpers.

struct Bench {
    host: Host,
    sink: Arc<MemorySink>,
}

impl Bench {
    /// A run with an approved goal and `cards` created, as `divide` leaves it.
    async fn new(crew: &Crew, mode: RunMode, cards: &[CardSpec]) -> anyhow::Result<Self> {
        let base = crew.repo.head()?;
        let history = history(crew, mode, &base, cards)?;
        let (context, sink, _commands) = crew.context(history)?;
        Git::new()
            .add_detached_worktree(crew.repo.root(), &context.repo.base_worktree, &base)
            .await?;
        let state = RunState::fold(&context.history);
        let shared = Shared::new(&context, state)?;
        Ok(Self {
            host: Host::new(Arc::new(shared)),
            sink,
        })
    }

    async fn build(&self) -> anyhow::Result<StepDone> {
        finish(self.spawn_build()).await
    }

    fn spawn_build(&self) -> tokio::task::JoinHandle<argonauts_crew::EngineResult<StepDone>> {
        let host = self.host.clone();
        tokio::spawn(async move { card::build_step(&host, &build_call()).await })
    }

    /// Answers the newest open checkpoint of `kind` once it opens.
    async fn answer(
        &self,
        kind: CheckpointKind,
        action: AnswerAction,
        text: Option<&str>,
    ) -> anyhow::Result<(CheckpointId, String)> {
        for _ in 0..600 {
            let events = self.sink.events();
            let decided: Vec<_> = events
                .iter()
                .filter_map(|e| match &e.kind {
                    RunEventKind::Checkpoint(CheckpointEvent::Decided(d)) => Some(d.id.clone()),
                    _ => None,
                })
                .collect();
            let open = events.iter().rev().find_map(|e| match &e.kind {
                RunEventKind::Checkpoint(CheckpointEvent::Opened(o))
                    if o.kind == kind && !decided.contains(&o.id) =>
                {
                    Some((o.id.clone(), o.evidence_sha256.clone(), o.title.clone()))
                }
                _ => None,
            });
            if let Some((id, evidence, title)) = open {
                let answer = CheckpointAnswer {
                    id: id.clone(),
                    action: action.clone(),
                    text: text.map(str::to_owned),
                    evidence_sha256: evidence,
                    cards: Vec::new(),
                };
                checkpoints::answer(&self.host, answer).map_err(anyhow::Error::msg)?;
                return Ok((id, title));
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        bail!("no {kind:?} checkpoint opened")
    }
}

async fn finish(
    build: tokio::task::JoinHandle<argonauts_crew::EngineResult<StepDone>>,
) -> anyhow::Result<StepDone> {
    let joined = tokio::time::timeout(Duration::from_secs(60), build)
        .await
        .context("the build step didn't finish")?;
    Ok(joined??)
}

fn build_call() -> StepCall {
    StepCall {
        at: StepAt {
            step: StepRef {
                workflow: WorkflowId::numbered(1),
                phase: 3,
                path: vec![0],
            },
            round: 1,
        },
        depth: 0,
        standard: StandardPhase::Do,
        step: serde_json::from_value(json!({"kind": "build", "input": "plan"}))
            .expect("a build step"),
        item: None,
    }
}

fn history(
    crew: &Crew,
    mode: RunMode,
    base: &str,
    cards: &[CardSpec],
) -> anyhow::Result<Vec<RunEvent>> {
    let run = RunId::new("run-01")?;
    let started = RunStarted {
        request: "Build it".into(),
        title: "Build it".into(),
        mode,
        forced: true,
        preset: "default".into(),
        estimate: None,
        triage: None,
        chat_turn: None,
        continues: None,
        repo_root: crew.repo.root().to_path_buf(),
        base_branch: Some("main".into()),
        base_commit: base.to_owned(),
        uncommitted: Vec::new(),
        uncommitted_included: false,
        settings_sha256: String::new(),
        settings: serde_json::to_value(&crew.settings)?,
        overrides: Vec::new(),
        harness_versions: BTreeMap::new(),
        version: "0.0.1".into(),
        front_end: None,
    };
    let goal = GoalFile {
        run: run.clone(),
        version: 1,
        summary: "Build it".into(),
        criteria: vec![Criterion {
            id: "G1".into(),
            text: "It works.".into(),
        }],
        in_scope: Vec::new(),
        out_of_scope: Vec::new(),
        assumptions: Vec::new(),
        answers: Vec::new(),
        source: GoalSource::Restatements,
        path: PathBuf::from(".argonauts/goal/run-01.md"),
        sha256: "0".repeat(64),
    };
    let mut kinds = vec![
        (
            None,
            RunEventKind::Run(RunLifecycle::Started(Box::new(started))),
        ),
        (
            None,
            RunEventKind::Goal(GoalEvent::Approved {
                file: goal,
                by: Party::User,
            }),
        ),
    ];
    kinds.extend(approved_plan(cards));
    for spec in cards {
        let id = Some(spec.id.clone());
        kinds.push((
            id.clone(),
            RunEventKind::Card(CardEvent::Created(Box::new(spec.clone()))),
        ));
        kinds.push((
            id,
            RunEventKind::Card(CardEvent::State {
                from: None,
                to: CardState::Waiting,
                reason: "created".into(),
            }),
        ));
    }
    Ok(kinds
        .into_iter()
        .enumerate()
        .map(|(seq, (card_id, kind))| RunEvent {
            seq: seq as u64,
            ts: time::OffsetDateTime::UNIX_EPOCH,
            run_id: run.clone(),
            card_id,
            session: None,
            source: "conductor".into(),
            kind,
        })
        .collect())
}

/// The plan checkpoint, opened and approved, which crew build sessions wait for.
fn approved_plan(cards: &[CardSpec]) -> Vec<(Option<CardId>, RunEventKind)> {
    let id = CheckpointId::numbered(1);
    let evidence = "0".repeat(64);
    let packet = PlanPacket {
        summary: "The plan".into(),
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
    };
    let opened = CheckpointOpened {
        id: id.clone(),
        kind: CheckpointKind::Plan,
        title: "Approve this plan?".into(),
        card: None,
        packet: CheckpointPacket::Plan(Box::new(packet)),
        evidence_sha256: evidence.clone(),
        mode: ApprovalMode::Supervised,
        decide_with: DecideWith::Rules,
        options: Vec::new(),
        free_text: FreeText::None,
        needs_you: true,
    };
    let decided = CheckpointDecided {
        id,
        action: AnswerAction::Approve,
        text: None,
        by: Party::User,
        evidence_sha256: evidence,
        shadow: None,
        cards: Vec::new(),
    };
    vec![
        (
            None,
            RunEventKind::Checkpoint(CheckpointEvent::Opened(Box::new(opened))),
        ),
        (
            None,
            RunEventKind::Checkpoint(CheckpointEvent::Decided(decided)),
        ),
    ]
}

fn spec(id: &str, deps: &[&str], owner: &str, reviewer: &str) -> CardSpec {
    CardSpec {
        id: CardId::new(id).unwrap(),
        title: format!("Build {id}"),
        task: format!("Do {id}."),
        kind: CardKind::Build,
        owned_paths: Vec::new(),
        read_only_paths: Vec::new(),
        forbidden_paths: Vec::new(),
        hidden_paths: Vec::new(),
        depends_on: deps.iter().map(|d| CardId::new(*d).unwrap()).collect(),
        criteria: vec!["G1".into()],
        owner: Some(worker(owner)),
        reviewers: vec![worker(reviewer)],
        budget: CardBudget::default(),
        predictions: None,
    }
}

fn worker(name: &str) -> WorkerRef {
    name.parse().unwrap()
}

fn test_gate() -> GateCommand {
    GateCommand {
        name: "test".into(),
        command: vec!["cargo".into(), "test".into()],
    }
}

fn report(status: &str) -> Value {
    json!({
        "status": status, "summary": "did the card", "files_changed": [],
        "tests_run": [], "concerns": [], "questions": [], "change_requests": [],
        "memory_proposals": []
    })
}

/// A harness owner that writes `files` into its worktree and reports done. Its vendor session
/// id is `v-<session key>`, so resumes can be traced.
fn owner(name: &str, files: &'static [(&'static str, &'static str)]) -> FakeWorker {
    slow_owner(name, files, Duration::ZERO)
}

/// An owner whose sessions take `delay` per event, so two can be seen running at once.
fn slow_owner(
    name: &str,
    files: &'static [(&'static str, &'static str)],
    delay: Duration,
) -> FakeWorker {
    FakeWorker::harness(name, &family_of(name)).respond_with(move |spec| work(spec, files, delay))
}

/// Writes `files` into the session's worktree and reports done.
fn work(spec: &SessionSpec, files: &[(&str, &str)], delay: Duration) -> FakeSession {
    for (path, text) in files {
        let path = spec.workdir.join(path);
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let _ = std::fs::write(path, format!("{text}// {}\n", spec.key));
    }
    FakeSession::structured(report("done"))
        .with_vendor_session(&format!("v-{}", spec.key))
        .with_delay(delay)
}

fn reviewer(name: &str) -> FakeWorker {
    FakeWorker::harness(name, &family_of(name)).respond_with(|_| {
        FakeSession::structured(json!({
            "verdict": "approve", "summary": "fine", "comments": [], "tamper_suspected": false
        }))
    })
}

fn family_of(name: &str) -> String {
    let model = name.split_once('/').map_or("", |(_, m)| m);
    model.split('-').next().unwrap_or("unknown").to_owned()
}

fn candidate(name: &str) -> Candidate {
    Candidate {
        worker: worker(name),
        quota_left: None,
        quota_text: String::new(),
        track_record: None,
        cost: CostEstimate::Unknown,
        needs_confirm: false,
        may_run_out: false,
    }
}

fn card_events(events: &[RunEvent], card: &str) -> Vec<CardEvent> {
    events
        .iter()
        .filter(|e| e.card_id.as_ref().is_some_and(|c| c.as_str() == card))
        .filter_map(|e| match &e.kind {
            RunEventKind::Card(event) => Some(event.clone()),
            _ => None,
        })
        .collect()
}

fn states(events: &[RunEvent], card: &str) -> Vec<CardState> {
    card_events(events, card)
        .into_iter()
        .filter_map(|e| match e {
            CardEvent::State { to, .. } => Some(to),
            _ => None,
        })
        .collect()
}

fn attempts(events: &[RunEvent], card: &str) -> Vec<(AttemptCause, WorkerRef)> {
    card_events(events, card)
        .into_iter()
        .filter_map(|e| match e {
            CardEvent::Attempt { cause, worker, .. } => Some((cause, worker)),
            _ => None,
        })
        .collect()
}

fn worktrees(events: &[RunEvent]) -> BTreeMap<CardId, (PathBuf, String)> {
    events
        .iter()
        .filter_map(|e| match (&e.card_id, &e.kind) {
            (Some(card), RunEventKind::Card(CardEvent::Worktree { path, branch, .. })) => {
                Some((card.clone(), (path.clone(), branch.clone())))
            }
            _ => None,
        })
        .collect()
}

fn opened(events: &[RunEvent]) -> Vec<(CheckpointId, CheckpointKind)> {
    events
        .iter()
        .filter_map(|e| match &e.kind {
            RunEventKind::Checkpoint(CheckpointEvent::Opened(o)) => Some((o.id.clone(), o.kind)),
            _ => None,
        })
        .collect()
}

fn state_of(bench: &Bench, card: &str) -> CardState {
    let id = CardId::new(card).unwrap();
    bench
        .host
        .read(|state| state.cards.get(&id).map(|r| r.state))
        .unwrap_or(CardState::Waiting)
}
