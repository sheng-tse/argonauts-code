//! Review scenarios: reviewers and the judge on a task, and an endpoint reviewer's
//! acceptance tests, on a host over fake workers and a temp repo.

use std::sync::Arc;

use serde::de::DeserializeOwned;
use serde_json::json;

use argonauts_core::engine::RepoContext;
use argonauts_core::events::task::{ReviewEvent, TaskEvent, TaskKind, TaskLimit, TaskSpec};
use argonauts_core::events::{RunEvent, RunEventKind};
use argonauts_core::names::ModelRef;
use argonauts_core::names::WorkflowId;
use argonauts_core::session_runner::{Access, SessionRunner, SessionSpec};
use argonauts_core::settings::Settings;
use argonauts_core::settings::crew::CrewSettings;
use argonauts_core::steps::StepName;
use argonauts_core::steps::task::AcceptanceTestsInput;
use argonauts_core::workflow::{StandardPhase, Step, StepRef};
use argonauts_core::{RepoId, RunId, TaskId};
use argonauts_crew::engine::host::Host;
use argonauts_crew::review::{self, Verdict, endpoint_tests};
use argonauts_crew::state::RunState;
use argonauts_crew::task::worktree;
use argonauts_crew::workflow::{StepAt, StepCall};
use argonauts_testkit::repo::TempRepo;
use argonauts_testkit::runner::{FakeRunner, FakeSession};
use argonauts_testkit::services::{FakeServices, MemorySink, engine_context_in};

use crate::support::shared_in;

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
        workers: Vec<FakeRunner>,
        settings: impl FnOnce(&mut Settings),
        history: Vec<RunEvent>,
    ) -> anyhow::Result<Self> {
        let services = FakeServices::new();
        let mut all = Settings {
            crew: CrewSettings {
                main: CLAUDE.parse()?,
                ..CrewSettings::of(Vec::new())
            },
            ..Settings::default()
        };
        for worker in workers {
            let added = services.workers.add(worker);
            all.crew.add(added.model_ref().clone());
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
        let shared = shared_in(&repo, &context, RunState::fold(&history))?;
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

/// The run's first event, as the run host writes it, based on the repo's first commit.
fn started(repo: &TempRepo) -> anyhow::Result<RunEvent> {
    let head = repo.git(&["rev-parse", "HEAD"])?;
    let started: argonauts_core::events::run::RunStarted = serde_json::from_value(json!({
        "request": "Rate limit the API",
        "title": "Rate limit the API",
        "mode": "crew",
        "forced": true,
        "workflow": "council",
        "estimate": null,
        "captain_or_crew": null,
        "conversation_turn": null,
        "continues": null,
        "repo_root": repo.root(),
        "base_branch": "main",
        "base_commit": head.trim(),
        "uncommitted": [],
        "uncommitted_included": false,
        "settings_sha256": "",
        "settings": {},
        "overrides": [],
        "cli_versions": {},
        "version": "0.0.1"
    }))?;
    Ok(RunEvent {
        seq: 0,
        ts: time::OffsetDateTime::UNIX_EPOCH,
        run_id: RunId::new("run-01")?,
        task_id: None,
        session: None,
        source: "run-host".to_owned(),
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

fn created(seq: u64, spec: TaskSpec) -> anyhow::Result<RunEvent> {
    Ok(RunEvent {
        seq,
        ts: time::OffsetDateTime::UNIX_EPOCH,
        run_id: RunId::new("run-01")?,
        task_id: Some(spec.id.clone()),
        session: None,
        source: "argonauts-code".to_owned(),
        kind: RunEventKind::Task(TaskEvent::Created(Box::new(spec))),
    })
}

fn task(id: &str, kind: TaskKind, worker: &str, reviewers: &[&str], owned: &[&str]) -> TaskSpec {
    TaskSpec {
        id: TaskId::new(id).expect("task id"),
        title: format!("{id} title"),
        task: "Rate limit the API with a token bucket.".to_owned(),
        kind,
        owned_paths: owned.iter().map(|p| (*p).to_owned()).collect(),
        read_only_paths: Vec::new(),
        forbidden_paths: Vec::new(),
        hidden_paths: Vec::new(),
        depends_on: Vec::new(),
        criteria: vec!["G1".to_owned()],
        worker: Some(worker.parse().expect("worker")),
        reviewers: reviewers
            .iter()
            .map(|r| r.parse().expect("worker"))
            .collect(),
        limit: TaskLimit::default(),
        predictions: None,
        item: None,
        way: None,
    }
}

fn reviewer(
    name: &str,
    provider: &str,
    verdict: &'static str,
    summary: &'static str,
) -> FakeRunner {
    FakeRunner::cli(name, provider).respond_with(move |spec| {
        let value = match spec.step {
            Some(StepName::Review) => json!({
                "verdict": verdict,
                "summary": summary,
                "comments": [{ "path": "src/limit.rs", "line": 12, "severity": "medium",
                               "text": "Reset the bucket on a new window." }],
                "tamper_suspected": false
            }),
            Some(StepName::Settle) => json!({
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
async fn review_disagreement_goes_to_a_fresh_judge() -> anyhow::Result<()> {
    let workers = vec![
        FakeRunner::cli(CLAUDE, "anthropic"),
        reviewer(CODEX, "openai", "approve", "APPROVE-SUMMARY"),
        reviewer(AGY, "google", "request_changes", "CHANGES-SUMMARY"),
        reviewer(LLAMA, "meta", "approve", "unused"),
    ];
    let build = task(
        "task-001",
        TaskKind::Build,
        CLAUDE,
        &[CODEX, AGY],
        &["src/limit.rs"],
    );
    let bench = Bench::new(
        workers,
        |settings| {
            settings.review.per_task = 2;
            settings.review.blind = true;
        },
        vec![created(0, build)?],
    )?;
    let task = TaskId::new("task-001")?;
    let verdict = review::review_task(&bench.host, &task, 1).await?;
    assert_eq!(verdict, Verdict::Approved);

    let reviews = bench.reviews();
    let reviewed: Vec<(ModelRef, bool)> = reviews
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
    let judge = reviews.iter().find_map(|r| match r {
        ReviewEvent::Settled { judge, .. } => Some(judge.to_string()),
        _ => None,
    });
    assert_eq!(judge.as_deref(), Some(LLAMA));

    // Blind reviewers never saw each other; the judge ran fresh without tools.
    let codex = prompt_text(&bench.specs_of(CODEX, StepName::Review)[0]);
    let agy = prompt_text(&bench.specs_of(AGY, StepName::Review)[0]);
    assert!(!codex.contains("CHANGES-SUMMARY"));
    assert!(!agy.contains("APPROVE-SUMMARY"));
    let ruling = &bench.specs_of(LLAMA, StepName::Settle)[0];
    assert_eq!(ruling.access, Access::NoTools);
    assert_eq!(ruling.resume, None);
    assert!(prompt_text(ruling).contains("CHANGES-SUMMARY"));
    assert!(
        bench.specs_of(CLAUDE, StepName::Review).is_empty(),
        "the worker never reviews"
    );
    Ok(())
}

#[tokio::test]
async fn endpoint_tests_outside_owned_paths_rejected() -> anyhow::Result<()> {
    let endpoint = FakeRunner::endpoint(ENDPOINT, "zhipu").respond_with(|spec| {
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
        FakeRunner::cli(CLAUDE, "anthropic"),
        FakeRunner::cli(CODEX, "openai"),
        endpoint,
    ];
    let build = task(
        "task-001",
        TaskKind::Build,
        CODEX,
        &[ENDPOINT],
        &["src/limit.rs"],
    );
    let tests = task(
        "test-001",
        TaskKind::AcceptanceTests {
            for_task: TaskId::new("task-001")?,
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
    let test_task = TaskId::new("test-001")?;
    let written = endpoint_tests::write_tests(&bench.host, &test_task).await?;
    assert_eq!(written, ["tests/limit/bucket_test.rs"]);

    let ask = &bench.specs_of(ENDPOINT, StepName::AcceptanceTests)[0];
    assert_eq!(ask.access, Access::NoTools);
    let input: AcceptanceTestsInput = input_of(ask).expect("acceptance tests input");
    assert_eq!(input.test_dir, "tests/limit/");
    assert_eq!(input.criteria, ["G1"]);

    let tree = worktree::ensure(&bench.host, &test_task).await?;
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
            files, for_task, ..
        } = review
        {
            assert_eq!(files, ["tests/limit/bucket_test.rs"]);
            assert_eq!(for_task.as_str(), "task-001");
        }
    }
    Ok(())
}

#[tokio::test]
async fn a_reviewer_of_the_workers_provider_is_skipped() -> anyhow::Result<()> {
    let workers = vec![
        reviewer(CLAUDE, "anthropic", "approve", "fine"),
        reviewer(CODEX, "openai", "reject", "never asked"),
        FakeRunner::cli("opencode/gpt-oss", "openai"),
    ];
    let build = task(
        "task-001",
        TaskKind::Build,
        "opencode/gpt-oss",
        &[CODEX, CLAUDE],
        &["src/limit.rs"],
    );
    let bench = Bench::new(workers, |_| {}, vec![created(0, build)?])?;
    let verdict = review::review_task(&bench.host, &TaskId::new("task-001")?, 1).await?;
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
async fn review_panel_settles_disagreeing_reviewers() -> anyhow::Result<()> {
    let workers = vec![
        reviewer(CLAUDE, "anthropic", "approve", "unused"),
        reviewer(CODEX, "openai", "approve", "APPROVE-SUMMARY"),
        reviewer(AGY, "google", "request_changes", "CHANGES-SUMMARY"),
    ];
    let bench = Bench::new(workers, |_| {}, Vec::new())?;
    let review = step_call(
        json!({ "kind": "review", "role": "reviewer", "count": 2, "blind": true,
                "different_provider_from": "author", "input": "goal" }),
        0,
    )?;
    let done = review::review_step(&bench.host, &review).await?;
    assert_eq!(done.summary.as_deref(), Some("The reviewers disagree"));
    assert!(
        bench.specs_of(CLAUDE, StepName::Review).is_empty(),
        "the author's provider"
    );

    let settle = step_call(
        json!({ "kind": "judge", "role": "judge", "when": "reviewers_disagree" }),
        1,
    )?;
    let done = argonauts_crew::plan::judge_step(&bench.host, &settle).await?;
    assert_eq!(done.summary.as_deref(), Some("Approved"));
    let ruling = &bench.specs_of(CLAUDE, StepName::Settle)[0];
    let text = prompt_text(ruling);
    assert!(text.contains("APPROVE-SUMMARY") && text.contains("CHANGES-SUMMARY"));
    assert!(
        bench
            .reviews()
            .iter()
            .any(|r| matches!(r, ReviewEvent::Settled { .. }))
    );
    Ok(())
}
