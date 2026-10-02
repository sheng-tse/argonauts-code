//! Integration: approved cards merge into `argonauts/<run>/integration` in dependency order
//! with conductor merge commits, conflicts go to the interface's owner, and every commit is the
//! conductor's. Each scenario seeds an approved goal, plan, and cards, then runs `build`.
mod support;

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use serde_json::{Value, json};

use argonauts_core::events::card::{
    CardBudget, CardEvent, CardKind, CardSpec, CardState, GateKind, GateOutcome, IntegrationEvent,
};
use argonauts_core::events::checkpoint::{
    AnswerAction, CheckpointDecided, CheckpointEvent, CheckpointKind, CheckpointOpened,
    CheckpointPacket, FreeText, PlanPacket,
};
use argonauts_core::events::goal::{Criterion, GoalEvent, GoalFile, GoalSource};
use argonauts_core::events::plan::{BudgetPlan, TaskGraph};
use argonauts_core::events::run::{Party, RunLifecycle, RunMode, RunStarted};
use argonauts_core::events::{RunEvent, RunEventKind};
use argonauts_core::future::BoxFuture;
use argonauts_core::limits::{GateRequest, GateRunner};
use argonauts_core::names::{CheckpointId, WorkerRef, WorkflowId};
use argonauts_core::settings::approvals::{ApprovalMode, DecideWith};
use argonauts_core::settings::budget::{BudgetAmount, BudgetUnit};
use argonauts_core::settings::protocol::GateCommand;
use argonauts_core::steps::StepName;
use argonauts_core::worker::SessionSpec;
use argonauts_core::workflow::{StandardPhase, StepRef};
use argonauts_core::{CardId, RunId};
use argonauts_crew::card::{self, gates};
use argonauts_crew::engine::host::Host;
use argonauts_crew::engine::shared::Shared;
use argonauts_crew::git::Git;
use argonauts_crew::integrate::final_check::FinalCheck;
use argonauts_crew::integrate::{branch, conductor, merge_packet};
use argonauts_crew::state::RunState;
use argonauts_crew::workflow::{StepAt, StepCall, StepDone};
use argonauts_testkit::services::MemorySink;
use argonauts_testkit::worker::{FakeSession, FakeWorker};

use support::Crew;

const INTERFACE_OWNER: &str = "claude-code/claude-x";
const OWNER_B: &str = "codex/gpt-x";
const OWNER_C: &str = "codex/gpt-y";
const REVIEWER: &str = "agy/gemini-x";

#[tokio::test(flavor = "multi_thread")]
async fn integrate_in_dependency_order() -> anyhow::Result<()> {
    let crew = Crew::new()?;
    crew.services
        .workers
        .add(owner(OWNER_B, &[("src/one.rs", "one\n")]));
    crew.services
        .workers
        .add(owner(INTERFACE_OWNER, &[("src/two.rs", "two\n")]));
    crew.services
        .workers
        .add(owner(OWNER_C, &[("src/three.rs", "three\n")]));
    crew.services.workers.add(reviewer(REVIEWER));
    // Listed out of order on purpose: the graph decides, not the list.
    let cards = vec![
        spec("card-003", CardKind::Build, &["card-002"], OWNER_C),
        spec("card-002", CardKind::Build, &["card-001"], INTERFACE_OWNER),
        spec("card-001", CardKind::Build, &[], OWNER_B),
    ];
    let bench = Bench::new(&crew, &cards).await?;
    let done = bench.build().await?;
    assert_eq!(done.summary.as_deref(), Some("3 cards integrated"));

    let events = bench.sink.events();
    let merged: Vec<(String, String)> = events
        .iter()
        .filter_map(|e| match &e.kind {
            RunEventKind::Integration(IntegrationEvent::Merged { card, sha }) => {
                Some((card.to_string(), sha.clone()))
            }
            _ => None,
        })
        .collect();
    let order: Vec<&str> = merged.iter().map(|(card, _)| card.as_str()).collect();
    assert_eq!(order, ["card-001", "card-002", "card-003"]);

    let root = crew.repo.root();
    let integration = branch::integration_branch(&RunId::new("run-01")?);
    let subjects = crew
        .repo
        .git(&["log", "--merges", "--reverse", "--format=%s", &integration])?;
    let subjects: Vec<&str> = subjects.lines().collect();
    assert_eq!(
        subjects,
        [
            "Merge card-001: Build card-001",
            "Merge card-002: Build card-002",
            "Merge card-003: Build card-003",
        ]
    );
    let git = Git::new();
    for (card, sha) in &merged {
        let parents = git.parents(root, sha).await?;
        assert_eq!(parents.len(), 2, "{card} was fast-forwarded");
        let card_head = git
            .branch_head(root, &format!("argonauts/run-01/{card}"))
            .await?
            .context("no card branch")?;
        assert_eq!(parents[1], card_head, "{card}'s merge brings in its branch");
    }
    // A dependent card starts from the integration branch with its dependency in.
    let bases = worktree_bases(&events);
    let base_two = &bases[&CardId::new("card-002")?];
    assert!(
        crew.repo
            .git(&["cat-file", "-e", &format!("{base_two}:src/one.rs")])
            .is_ok()
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn conflict_goes_to_interface_owner() -> anyhow::Result<()> {
    let crew = Crew::new()?;
    let interface =
        crew.services
            .workers
            .add(
                FakeWorker::harness(INTERFACE_OWNER, "claude").respond_with(|spec| {
                    if spec.step == Some(StepName::ResolveConflict) {
                        work(spec, &[("shared.txt", "both sides\n")])
                    } else {
                        work(spec, &[("src/api.rs", "pub trait Api {}\n")])
                    }
                }),
            );
    crew.services
        .workers
        .add(owner(OWNER_B, &[("shared.txt", "from card-002\n")]));
    crew.services
        .workers
        .add(owner(OWNER_C, &[("shared.txt", "from card-003\n")]));
    crew.services.workers.add(reviewer(REVIEWER));
    let cards = vec![
        spec("card-001", CardKind::Interface, &[], INTERFACE_OWNER),
        spec("card-002", CardKind::Build, &["card-001"], OWNER_B),
        spec("card-003", CardKind::Build, &["card-001"], OWNER_C),
    ];
    let bench = Bench::new(&crew, &cards).await?;
    let done = bench.build().await?;
    assert_eq!(done.summary.as_deref(), Some("3 cards integrated"));

    let events = bench.sink.events();
    let conflict = events
        .iter()
        .find_map(|e| match &e.kind {
            RunEventKind::Integration(IntegrationEvent::Conflict {
                card,
                files,
                assigned_to,
            }) => Some((card.clone(), files.clone(), assigned_to.clone())),
            _ => None,
        })
        .context("no conflict recorded")?;
    assert_eq!(conflict.1, ["shared.txt"]);
    assert_eq!(conflict.2, worker(INTERFACE_OWNER));

    let resolver: Vec<SessionSpec> = interface
        .specs()
        .into_iter()
        .filter(|s| s.step == Some(StepName::ResolveConflict))
        .collect();
    assert_eq!(resolver.len(), 1);
    assert_eq!(resolver[0].card.as_ref(), Some(&conflict.0));
    let trees = worktree_paths(&events);
    assert_eq!(resolver[0].workdir, trees[&conflict.0]);

    let integration = branch::integration_branch(&RunId::new("run-01")?);
    let merged = crew
        .repo
        .git(&["show", &format!("{integration}:shared.txt")])?;
    assert!(merged.starts_with("both sides"), "{merged}");
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn only_the_conductor_commits() -> anyhow::Result<()> {
    let crew = Crew::new()?;
    crew.services
        .workers
        .add(owner(OWNER_B, &[("src/b.rs", "b\n")]));
    crew.services
        .workers
        .add(owner(INTERFACE_OWNER, &[("src/c.rs", "c\n")]));
    crew.services.workers.add(reviewer(REVIEWER));
    let cards = vec![
        spec("card-001", CardKind::Build, &[], OWNER_B),
        spec("card-002", CardKind::Build, &[], INTERFACE_OWNER),
    ];
    let bench = Bench::new(&crew, &cards).await?;
    let base = crew.repo.head()?;

    // The conductor records the goal on the integration branch before any card starts.
    let integration_tree = branch::ensure_integration(&bench.host).await?;
    std::fs::create_dir_all(integration_tree.join(".argonauts/goal"))?;
    std::fs::write(
        integration_tree.join(".argonauts/goal/run-01.md"),
        "# Goal\n",
    )?;
    let goal = conductor::commit(
        &bench.host,
        &integration_tree,
        &[".argonauts/goal/run-01.md"],
        "Record the approved goal",
    )
    .await?;
    bench.build().await?;

    let git = Git::new();
    let run = RunId::new("run-01")?;
    let head = git
        .branch_head(crew.repo.root(), &branch::integration_branch(&run))
        .await?
        .context("no integration branch")?;
    let log = git
        .log_messages(crew.repo.root(), &format!("{base}..{head}"))
        .await?;
    assert!(log.len() >= 5, "{log:?}");
    for (sha, message) in &log {
        let by_conductor = conductor::is_conductor_message(&run, message);
        let session_commit = message
            .lines()
            .any(|line| line.starts_with("Argonauts-Session: s-"));
        assert!(
            by_conductor || session_commit,
            "{sha} wasn't made by the conductor: {message}"
        );
        if git.parents(crew.repo.root(), sha).await?.len() > 1 {
            assert!(
                by_conductor,
                "a merge without the conductor trailer: {message}"
            );
            assert!(message.contains("Argonauts-Card: card-00"), "{message}");
        }
    }
    let conductor_only =
        conductor::conductor_paths(&bench.host, crew.repo.root(), &base, &head).await?;
    assert_eq!(conductor_only, [".argonauts/goal/run-01.md"]);
    // Walked along the integration branch itself, every card merge counts in full.
    let history = gates::CardHistory {
        dir: crew.repo.root(),
        conductor: [goal].into_iter().collect(),
        integration: "no-integration".to_owned(),
    };
    let agent = gates::agent_diff(&git, &history, &base, &head).await?;
    assert!(agent.contains("src/b.rs") && agent.contains("src/c.rs"));
    assert!(
        !agent.contains(".argonauts/goal"),
        "the conductor's goal commit was checked"
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn acceptance_tests_fail_until_their_build_card_is_in() -> anyhow::Result<()> {
    let mut crew = Crew::new()?;
    // Each tests/*.want names a file that must say "done"; the stubs say "stub".
    crew.repo.write(
        "run-tests.sh",
        "status=0\nfor want in tests/*.want; do\n  [ -e \"$want\" ] || continue\n  \
         file=$(cat \"$want\")\n  if [ \"$(cat \"$file\" 2>/dev/null)\" != done ]; then \
         echo \"FAIL $want\"; status=1; fi\ndone\nexit $status\n",
    )?;
    crew.repo.commit_all("Add the test runner")?;
    crew.settings.gates.run = vec![GateCommand {
        name: "tests".into(),
        command: vec!["sh".into(), "run-tests.sh".into()],
    }];
    for name in [INTERFACE_OWNER, OWNER_B, OWNER_C] {
        crew.services.workers.add(contract_owner(name));
    }
    crew.services.workers.add(reviewer(REVIEWER));
    let tests_for = |card: &str| CardKind::AcceptanceTests {
        for_card: CardId::new(card).unwrap(),
    };
    let cards = vec![
        spec("card-001", CardKind::Interface, &[], INTERFACE_OWNER),
        spec("card-002", tests_for("card-004"), &["card-001"], OWNER_B),
        spec("card-003", tests_for("card-005"), &["card-001"], OWNER_C),
        spec("card-004", CardKind::Build, &["card-002"], OWNER_B),
        spec("card-005", CardKind::Build, &["card-003"], OWNER_C),
    ];
    let bench = Bench::with_gates(&crew, &cards, Some(Arc::new(Commands))).await?;
    let done = bench.build().await?;
    assert_eq!(done.summary.as_deref(), Some("5 cards integrated"));

    // Every card went through once: no failure sent one back.
    let events = bench.sink.events();
    let mut attempts: BTreeMap<String, u32> = BTreeMap::new();
    for event in &events {
        if let (Some(card), RunEventKind::Card(CardEvent::Attempt { .. })) =
            (&event.card_id, &event.kind)
        {
            *attempts.entry(card.to_string()).or_default() += 1;
        }
    }
    assert!(attempts.values().all(|n| *n == 1), "{attempts:?}");

    // The test cards' merges failed as expected; the last build card's merge ran them all.
    let affected: Vec<(String, GateOutcome)> = events
        .iter()
        .filter_map(|event| match (&event.card_id, &event.kind) {
            (Some(card), RunEventKind::Gate(gate)) if gate.kind == GateKind::AffectedTests => {
                Some((card.to_string(), gate.clone()))
            }
            _ => None,
        })
        .collect();
    for (card, gate) in &affected {
        assert!(gate.passed, "{card}: {gate:?}");
    }
    for card in ["card-002", "card-003"] {
        let (_, gate) = affected
            .iter()
            .find(|(c, _)| c == card)
            .context("a test card's merge")?;
        assert_eq!(gate.exit_code, Some(1));
        assert!(
            gate.summary.contains("failed as expected"),
            "{}",
            gate.summary
        );
    }
    let (_, last) = affected.last().context("merges")?;
    assert_eq!(last.exit_code, Some(0), "{last:?}");

    // The merge packet counts every card's gates as passed, test cards by their stub check.
    let check = FinalCheck {
        passed: true,
        gates: Vec::new(),
        problem: None,
    };
    let evidence = merge_packet::build(&bench.host, &check).await?;
    for row in &evidence.packet.rows {
        assert!(row.gates_passed, "{}", row.card);
    }
    assert_eq!(evidence.facts.gates_passed, Some(true));
    Ok(())
}

// Scenario helpers.

/// Runs each gate's command for real in its workdir, without a sandbox.
struct Commands;

impl GateRunner for Commands {
    fn run(&self, request: GateRequest) -> BoxFuture<'_, GateOutcome> {
        Box::pin(async move {
            let ran = match request.command.split_first() {
                Some((program, args)) => tokio::process::Command::new(program)
                    .args(args)
                    .current_dir(&request.workdir)
                    .output()
                    .await
                    .map_err(|err| err.to_string()),
                None => Err("no command".to_owned()),
            };
            let (passed, exit_code, output_tail) = match ran {
                Ok(out) => (
                    out.status.success(),
                    out.status.code(),
                    String::from_utf8_lossy(&out.stdout).into_owned(),
                ),
                Err(err) => (false, None, err),
            };
            GateOutcome {
                name: request.name,
                kind: GateKind::Command,
                passed,
                command: Some(request.command),
                exit_code,
                duration_ms: 1,
                summary: if passed { "passed" } else { "failed" }.to_owned(),
                output_tail,
                violations: Vec::new(),
                sandboxed: false,
                skipped: false,
            }
        })
    }
}

/// Writes what each card of the contracts scenario needs: the interface's stubs, a test that
/// wants one file done, or that file done.
fn contract_owner(name: &str) -> FakeWorker {
    let family = name.split_once('/').map_or("", |(_, m)| m);
    let family = family.split('-').next().unwrap_or("unknown").to_owned();
    FakeWorker::harness(name, &family).respond_with(|spec| {
        let files: &[(&str, &str)] = match spec.card.as_ref().map(CardId::as_str) {
            Some("card-001") => &[("src/one.rs", "stub\n"), ("src/two.rs", "stub\n")],
            Some("card-002") => &[("tests/one.want", "src/one.rs\n")],
            Some("card-003") => &[("tests/two.want", "src/two.rs\n")],
            Some("card-004") => &[("src/one.rs", "done\n")],
            Some("card-005") => &[("src/two.rs", "done\n")],
            _ => &[],
        };
        work(spec, files)
    })
}

struct Bench {
    host: Host,
    sink: Arc<MemorySink>,
}

impl Bench {
    async fn new(crew: &Crew, cards: &[CardSpec]) -> anyhow::Result<Self> {
        Self::with_gates(crew, cards, None).await
    }

    async fn with_gates(
        crew: &Crew,
        cards: &[CardSpec],
        gates: Option<Arc<dyn GateRunner>>,
    ) -> anyhow::Result<Self> {
        let base = crew.repo.head()?;
        let (mut context, sink, _commands) = crew.context(history(crew, &base, cards)?)?;
        if let Some(gates) = gates {
            context.services.gates = gates;
        }
        Git::new()
            .add_detached_worktree(crew.repo.root(), &context.repo.base_worktree, &base)
            .await?;
        let shared = Shared::new(&context, RunState::fold(&context.history))?;
        Ok(Self {
            host: Host::new(Arc::new(shared)),
            sink,
        })
    }

    async fn build(&self) -> anyhow::Result<StepDone> {
        let host = self.host.clone();
        let call = StepCall {
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
            step: serde_json::from_value(json!({"kind": "build", "input": "plan"}))?,
            item: None,
        };
        let build = tokio::spawn(async move { card::build_step(&host, &call).await });
        let joined = tokio::time::timeout(Duration::from_secs(60), build)
            .await
            .context("the build step didn't finish")?;
        Ok(joined??)
    }
}

/// A crew run with its goal and plan approved and `cards` created.
fn history(crew: &Crew, base: &str, cards: &[CardSpec]) -> anyhow::Result<Vec<RunEvent>> {
    let run = RunId::new("run-01")?;
    let started = RunStarted {
        request: "Build it".into(),
        title: "Build it".into(),
        mode: RunMode::Crew,
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
    let evidence = "0".repeat(64);
    let plan = CheckpointOpened {
        id: CheckpointId::numbered(1),
        kind: CheckpointKind::Plan,
        title: "Approve this plan?".into(),
        card: None,
        packet: CheckpointPacket::Plan(Box::new(PlanPacket {
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
        })),
        evidence_sha256: evidence.clone(),
        mode: ApprovalMode::Supervised,
        decide_with: DecideWith::Rules,
        options: Vec::new(),
        free_text: FreeText::None,
        needs_you: true,
    };
    let approved = CheckpointDecided {
        id: CheckpointId::numbered(1),
        action: AnswerAction::Approve,
        text: None,
        by: Party::User,
        evidence_sha256: evidence,
        shadow: None,
        cards: Vec::new(),
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
        (
            None,
            RunEventKind::Checkpoint(CheckpointEvent::Opened(Box::new(plan))),
        ),
        (
            None,
            RunEventKind::Checkpoint(CheckpointEvent::Decided(approved)),
        ),
    ];
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

fn spec(id: &str, kind: CardKind, deps: &[&str], owner: &str) -> CardSpec {
    CardSpec {
        id: CardId::new(id).unwrap(),
        title: format!("Build {id}"),
        task: format!("Do {id}."),
        kind,
        owned_paths: Vec::new(),
        read_only_paths: Vec::new(),
        forbidden_paths: Vec::new(),
        hidden_paths: Vec::new(),
        depends_on: deps.iter().map(|d| CardId::new(*d).unwrap()).collect(),
        criteria: vec!["G1".into()],
        owner: Some(worker(owner)),
        reviewers: vec![worker(REVIEWER)],
        budget: CardBudget::default(),
        predictions: None,
    }
}

fn worker(name: &str) -> WorkerRef {
    name.parse().unwrap()
}

fn owner(name: &str, files: &'static [(&'static str, &'static str)]) -> FakeWorker {
    let family = name.split_once('/').map_or("", |(_, m)| m);
    let family = family.split('-').next().unwrap_or("unknown").to_owned();
    FakeWorker::harness(name, &family).respond_with(move |spec| work(spec, files))
}

/// Writes `files` into the session's worktree and reports done.
fn work(spec: &SessionSpec, files: &[(&str, &str)]) -> FakeSession {
    for (path, text) in files {
        let path = spec.workdir.join(path);
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let _ = std::fs::write(path, text);
    }
    FakeSession::structured(report()).with_vendor_session(&format!("v-{}", spec.key))
}

fn reviewer(name: &str) -> FakeWorker {
    FakeWorker::harness(name, "gemini").respond_with(|_| {
        FakeSession::structured(json!({
            "verdict": "approve", "summary": "fine", "comments": [], "tamper_suspected": false
        }))
    })
}

fn report() -> Value {
    json!({
        "status": "done", "summary": "did the card", "files_changed": [],
        "tests_run": [], "concerns": [], "questions": [], "change_requests": [],
        "memory_proposals": []
    })
}

fn worktree_paths(events: &[RunEvent]) -> BTreeMap<CardId, PathBuf> {
    events
        .iter()
        .filter_map(|e| match (&e.card_id, &e.kind) {
            (Some(card), RunEventKind::Card(CardEvent::Worktree { path, .. })) => {
                Some((card.clone(), path.clone()))
            }
            _ => None,
        })
        .collect()
}

fn worktree_bases(events: &[RunEvent]) -> BTreeMap<CardId, String> {
    events
        .iter()
        .filter_map(|e| match (&e.card_id, &e.kind) {
            (Some(card), RunEventKind::Card(CardEvent::Worktree { base, .. })) => {
                Some((card.clone(), base.clone()))
            }
            _ => None,
        })
        .collect()
}
