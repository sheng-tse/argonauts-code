//! Review scenarios: reviewers and the adjudicator on a card, and an endpoint reviewer's
//! acceptance tests, on a host over fake workers and a temp repo.

use std::sync::Arc;

use serde::de::DeserializeOwned;
use serde_json::json;

use argonauts_core::engine::RepoContext;
use argonauts_core::events::card::{CardBudget, CardEvent, CardKind, CardSpec, ReviewEvent};
use argonauts_core::events::{RunEvent, RunEventKind};
use argonauts_core::names::WorkerRef;
use argonauts_core::names::WorkflowId;
use argonauts_core::settings::Settings;
use argonauts_core::steps::StepName;
use argonauts_core::steps::card::AcceptanceTestsInput;
use argonauts_core::worker::{Access, SessionSpec, Worker};
use argonauts_core::workflow::{StandardPhase, Step, StepRef};
use argonauts_core::{CardId, RepoId, RunId};
use argonauts_crew::card::worktree;
use argonauts_crew::engine::host::Host;
use argonauts_crew::engine::shared::Shared;
use argonauts_crew::review::{self, Verdict, endpoint_tests};
use argonauts_crew::state::RunState;
use argonauts_crew::workflow::{StepAt, StepCall};
use argonauts_testkit::repo::TempRepo;
use argonauts_testkit::services::{FakeServices, MemorySink, engine_context_in};
use argonauts_testkit::worker::{FakeSession, FakeWorker};

const CLAUDE: &str = "claude-code/claude-opus";
const CODEX: &str = "codex/gpt-5";
const AGY: &str = "antigravity-cli/gemini-3";
const LLAMA: &str = "opencode/llama";
const ENDPOINT: &str = "openrouter/glm-5";

struct Bench {
    services: FakeServices,
    _repo: TempRepo,
    host: Host,
    sink: Arc<MemorySink>,
}

impl Bench {
    fn new(
        workers: Vec<FakeWorker>,
        settings: impl FnOnce(&mut Settings),
        history: Vec<RunEvent>,
    ) -> anyhow::Result<Self> {
        let services = FakeServices::new();
        let mut all = Settings {
            default_agent: CLAUDE.parse()?,
            crew: Vec::new(),
            ..Settings::default()
        };
        for worker in workers {
            let added = services.workers.add(worker);
            all.crew.push(added.worker_ref().clone());
        }
        settings(&mut all);
        let repo = TempRepo::new()?;
        let history: Vec<RunEvent> = std::iter::once(started(&repo)?)
            .chain(history)
            .enumerate()
            .map(|(seq, event)| RunEvent {
                seq: seq as u64,
                ..event
            })
            .collect();
        let context = RepoContext {
            id: RepoId::from_canonical_path(repo.root()),
            root: repo.root().to_path_buf(),
            run_dir: repo.home().join("state/repo-test/runs/run-01"),
            worktrees: repo.home().join("worktrees/repo-test/run-01"),
            base_worktree: repo.root().to_path_buf(),
        };
        let (context, sink, _commands) =
            engine_context_in(&services, all, context, history.clone());
        let shared = Shared::new(&context, RunState::fold(&history))?;
        Ok(Self {
            services,
            _repo: repo,
            host: Host::new(Arc::new(shared)),
            sink,
        })
    }

    fn specs_of(&self, worker: &str, step: StepName) -> Vec<SessionSpec> {
        self.services
            .workers
            .fake(worker)
            .map(|fake| fake.specs())
            .unwrap_or_default()
            .into_iter()
            .filter(|spec| spec.step == Some(step))
            .collect()
    }

    fn reviews(&self) -> Vec<ReviewEvent> {
        self.sink
            .events()
            .into_iter()
            .filter_map(|event| match event.kind {
                RunEventKind::Review(review) => Some(review),
                _ => None,
            })
            .collect()
    }
}

/// The run's first event, as the supervisor writes it, based on the repo's first commit.
fn started(repo: &TempRepo) -> anyhow::Result<RunEvent> {
    let head = repo.git(&["rev-parse", "HEAD"])?;
    let started: argonauts_core::events::run::RunStarted = serde_json::from_value(json!({
        "request": "Rate limit the API",
        "title": "Rate limit the API",
        "mode": "crew",
        "forced": true,
        "preset": "peer-crew",
        "estimate": null,
        "triage": null,
        "chat_turn": null,
        "continues": null,
        "repo_root": repo.root(),
        "base_branch": "main",
        "base_commit": head.trim(),
        "uncommitted": [],
        "uncommitted_included": false,
        "settings_sha256": "",
        "settings": {},
        "overrides": [],
        "harness_versions": {},
        "version": "0.0.1"
    }))?;
    Ok(RunEvent {
        seq: 0,
        ts: time::OffsetDateTime::UNIX_EPOCH,
        run_id: RunId::new("run-01")?,
        card_id: None,
        session: None,
        source: "supervisor".to_owned(),
        kind: RunEventKind::Run(argonauts_core::events::run::RunLifecycle::Started(
            Box::new(started),
        )),
    })
}

fn input_of<T: DeserializeOwned>(spec: &SessionSpec) -> Option<T> {
    spec.prompt
        .iter()
        .find_map(|part| serde_json::from_str(&part.text).ok())
}

fn prompt_text(spec: &SessionSpec) -> String {
    spec.prompt
        .iter()
        .map(|part| part.text.as_str())
        .collect::<Vec<_>>()
        .join("\n")
}

fn created(seq: u64, spec: CardSpec) -> anyhow::Result<RunEvent> {
    Ok(RunEvent {
        seq,
        ts: time::OffsetDateTime::UNIX_EPOCH,
        run_id: RunId::new("run-01")?,
        card_id: Some(spec.id.clone()),
        session: None,
        source: "conductor".to_owned(),
        kind: RunEventKind::Card(CardEvent::Created(Box::new(spec))),
    })
}

fn card(id: &str, kind: CardKind, owner: &str, reviewers: &[&str], owned: &[&str]) -> CardSpec {
    CardSpec {
        id: CardId::new(id).expect("card id"),
        title: format!("{id} title"),
        task: "Rate limit the API with a token bucket.".to_owned(),
        kind,
        owned_paths: owned.iter().map(|p| (*p).to_owned()).collect(),
        read_only_paths: Vec::new(),
        forbidden_paths: Vec::new(),
        hidden_paths: Vec::new(),
        depends_on: Vec::new(),
        criteria: vec!["G1".to_owned()],
        owner: Some(owner.parse().expect("worker")),
        reviewers: reviewers
            .iter()
            .map(|r| r.parse().expect("worker"))
            .collect(),
        budget: CardBudget::default(),
        predictions: None,
        item: None,
    }
}

fn reviewer(name: &str, family: &str, verdict: &'static str, summary: &'static str) -> FakeWorker {
    FakeWorker::harness(name, family).respond_with(move |spec| {
        let value = match spec.step {
            Some(StepName::Review) => json!({
                "verdict": verdict,
                "summary": summary,
                "comments": [{ "path": "src/limit.rs", "line": 12, "severity": "medium",
                               "text": "Reset the bucket on a new window." }],
                "tamper_suspected": false
            }),
            Some(StepName::Adjudicate) => json!({
                "verdict": "approve",
                "reasons": ["The window reset is covered by the second test."],
                "required_changes": []
            }),
            _ => json!({}),
        };
        FakeSession::structured(value)
    })
}

#[tokio::test]
async fn review_disagreement_goes_to_a_fresh_adjudicator() -> anyhow::Result<()> {
    let workers = vec![
        FakeWorker::harness(CLAUDE, "claude"),
        reviewer(CODEX, "gpt", "approve", "APPROVE-SUMMARY"),
        reviewer(AGY, "gemini", "request_changes", "CHANGES-SUMMARY"),
        reviewer(LLAMA, "llama", "approve", "unused"),
    ];
    let build = card(
        "card-001",
        CardKind::Build,
        CLAUDE,
        &[CODEX, AGY],
        &["src/limit.rs"],
    );
    let bench = Bench::new(
        workers,
        |settings| {
            settings.review.per_card = 2;
            settings.review.blind = true;
        },
        vec![created(0, build)?],
    )?;
    let card = CardId::new("card-001")?;
    let verdict = review::review_card(&bench.host, &card, 1).await?;
    assert_eq!(verdict, Verdict::Approved);

    let reviews = bench.reviews();
    let reviewed: Vec<(WorkerRef, bool)> = reviews
        .iter()
        .filter_map(|r| match r {
            ReviewEvent::Reviewed {
                reviewer, blind, ..
            } => Some((reviewer.clone(), *blind)),
            _ => None,
        })
        .collect();
    assert_eq!(reviewed.len(), 2);
    assert!(reviewed.iter().all(|(_, blind)| *blind));
    let adjudicator = reviews.iter().find_map(|r| match r {
        ReviewEvent::Adjudicated { adjudicator, .. } => Some(adjudicator.to_string()),
        _ => None,
    });
    assert_eq!(adjudicator.as_deref(), Some(LLAMA));

    // Blind reviewers never saw each other; the adjudicator ran fresh without tools.
    let codex = prompt_text(&bench.specs_of(CODEX, StepName::Review)[0]);
    let agy = prompt_text(&bench.specs_of(AGY, StepName::Review)[0]);
    assert!(!codex.contains("CHANGES-SUMMARY"));
    assert!(!agy.contains("APPROVE-SUMMARY"));
    let ruling = &bench.specs_of(LLAMA, StepName::Adjudicate)[0];
    assert_eq!(ruling.access, Access::NoTools);
    assert_eq!(ruling.resume, None);
    assert!(prompt_text(ruling).contains("CHANGES-SUMMARY"));
    assert!(
        bench.specs_of(CLAUDE, StepName::Review).is_empty(),
        "the owner never reviews"
    );
    Ok(())
}

#[tokio::test]
async fn endpoint_tests_outside_owned_paths_rejected() -> anyhow::Result<()> {
    let endpoint = FakeWorker::endpoint(ENDPOINT, "glm").respond_with(|spec| {
        let value = match spec.step {
            Some(StepName::AcceptanceTests) => json!({
                "files": [
                    { "path": "tests/limit/bucket_test.rs", "content": "#[test] fn limits() {}\n" },
                    { "path": "src/lib.rs", "content": "// overwritten\n" },
                    { "path": "tests/limit/../../escape.rs", "content": "// escaped\n" }
                ],
                "criteria_covered": ["G1"]
            }),
            _ => json!({}),
        };
        FakeSession::structured(value)
    });
    let workers = vec![
        FakeWorker::harness(CLAUDE, "claude"),
        FakeWorker::harness(CODEX, "gpt"),
        endpoint,
    ];
    let build = card(
        "card-001",
        CardKind::Build,
        CODEX,
        &[ENDPOINT],
        &["src/limit.rs"],
    );
    let tests = card(
        "test-001",
        CardKind::AcceptanceTests {
            for_card: CardId::new("card-001")?,
        },
        ENDPOINT,
        &[],
        &["tests/limit/"],
    );
    let bench = Bench::new(
        workers,
        |_| {},
        vec![created(0, build)?, created(1, tests)?],
    )?;
    let test_card = CardId::new("test-001")?;
    let written = endpoint_tests::write_tests(&bench.host, &test_card).await?;
    assert_eq!(written, ["tests/limit/bucket_test.rs"]);

    let ask = &bench.specs_of(ENDPOINT, StepName::AcceptanceTests)[0];
    assert_eq!(ask.access, Access::NoTools);
    let input: AcceptanceTestsInput = input_of(ask).expect("acceptance tests input");
    assert_eq!(input.test_dir, "tests/limit/");
    assert_eq!(input.criteria, ["G1"]);

    let tree = worktree::ensure(&bench.host, &test_card).await?;
    assert!(tree.path.join("tests/limit/bucket_test.rs").is_file());
    let lib = std::fs::read_to_string(tree.path.join("src/lib.rs")).unwrap_or_default();
    assert!(!lib.contains("overwritten"));
    assert!(!tree.path.join("escape.rs").exists());
    assert!(
        !tree
            .path
            .parent()
            .map(|p| p.join("escape.rs").exists())
            .unwrap_or(false)
    );

    let rejected = bench.sink.events().into_iter().find_map(|e| match e.kind {
        RunEventKind::Note(note) if note.text.contains("Rejected acceptance tests") => {
            Some(note.text)
        }
        _ => None,
    });
    let rejected = rejected.expect("a note naming the rejected files");
    assert!(rejected.contains("src/lib.rs") && rejected.contains("escape.rs"));
    for review in bench.reviews() {
        if let ReviewEvent::TestsWritten {
            files, for_card, ..
        } = review
        {
            assert_eq!(files, ["tests/limit/bucket_test.rs"]);
            assert_eq!(for_card.as_str(), "card-001");
        }
    }
    Ok(())
}

#[tokio::test]
async fn a_reviewer_of_the_owners_family_is_skipped() -> anyhow::Result<()> {
    let workers = vec![
        reviewer(CLAUDE, "claude", "approve", "fine"),
        reviewer(CODEX, "gpt", "reject", "never asked"),
        FakeWorker::harness("opencode/gpt-oss", "gpt"),
    ];
    let build = card(
        "card-001",
        CardKind::Build,
        "opencode/gpt-oss",
        &[CODEX, CLAUDE],
        &["src/limit.rs"],
    );
    let bench = Bench::new(workers, |_| {}, vec![created(0, build)?])?;
    let verdict = review::review_card(&bench.host, &CardId::new("card-001")?, 1).await?;
    assert_eq!(verdict, Verdict::Approved);
    assert!(bench.specs_of(CODEX, StepName::Review).is_empty());
    assert_eq!(bench.specs_of(CLAUDE, StepName::Review).len(), 1);
    Ok(())
}

fn step_call(step: serde_json::Value, index: u32) -> anyhow::Result<StepCall> {
    let step: Step = serde_json::from_value(step)?;
    Ok(StepCall {
        at: StepAt {
            step: StepRef {
                workflow: WorkflowId::numbered(1),
                phase: index,
                path: vec![0],
            },
            round: 1,
        },
        depth: 0,
        standard: StandardPhase::Check,
        step,
        item: None,
    })
}

#[tokio::test]
async fn review_panel_adjudicates_disagreeing_reviewers() -> anyhow::Result<()> {
    let workers = vec![
        reviewer(CLAUDE, "claude", "approve", "unused"),
        reviewer(CODEX, "gpt", "approve", "APPROVE-SUMMARY"),
        reviewer(AGY, "gemini", "request_changes", "CHANGES-SUMMARY"),
    ];
    let bench = Bench::new(workers, |_| {}, Vec::new())?;
    let review = step_call(
        json!({ "kind": "review", "role": "reviewer", "count": 2, "blind": true,
                "different_family_from": "author", "input": "goal" }),
        0,
    )?;
    let done = review::review_step(&bench.host, &review).await?;
    assert_eq!(done.summary.as_deref(), Some("The reviewers disagree"));
    assert!(
        bench.specs_of(CLAUDE, StepName::Review).is_empty(),
        "the author's family"
    );

    let adjudicate = step_call(
        json!({ "kind": "judge", "role": "adjudicator", "when": "reviewers_disagree" }),
        1,
    )?;
    let done = argonauts_crew::plan::judge_step(&bench.host, &adjudicate).await?;
    assert_eq!(done.summary.as_deref(), Some("Approved"));
    let ruling = &bench.specs_of(CLAUDE, StepName::Adjudicate)[0];
    let text = prompt_text(ruling);
    assert!(text.contains("APPROVE-SUMMARY") && text.contains("CHANGES-SUMMARY"));
    assert!(
        bench
            .reviews()
            .iter()
            .any(|r| matches!(r, ReviewEvent::Adjudicated { .. }))
    );
    Ok(())
}
