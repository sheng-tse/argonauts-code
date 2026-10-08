//! Planning scenarios: each planning step runs on a host over fake workers and a temp repo,
//! and the recorded events and sealed files are checked.

use std::os::unix::fs::PermissionsExt;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};

use argonauts_core::api::CheckpointAnswer;
use argonauts_core::engine::RepoContext;
use argonauts_core::events::card::CardKind;
use argonauts_core::events::checkpoint::{
    AnswerAction, CheckpointEvent, CheckpointKind, CheckpointOpened, CheckpointPacket, PlanPacket,
};
use argonauts_core::events::goal::{Criterion, GoalEvent, GoalFile, GoalSource};
use argonauts_core::events::plan::{DivideEvent, JudgingResult, PlanEvent};
use argonauts_core::events::run::{Party, PhaseChange, RunLifecycle, RunPhase};
use argonauts_core::events::session::{EndReason, SessionEvent};
use argonauts_core::events::workflow::StepEnd;
use argonauts_core::events::{RunEvent, RunEventKind};
use argonauts_core::limits::HealthState;
use argonauts_core::names::{PlanLabel, RoleName, WorkerRef, WorkflowId};
use argonauts_core::settings::approvals::{Answerer, ApprovalMode};
use argonauts_core::settings::handoff::CrewRole;
use argonauts_core::settings::protocol::RoutingAlgorithm;
use argonauts_core::settings::{Settings, Span};
use argonauts_core::steps::StepName;
use argonauts_core::steps::plan::{CoordinateInput, JudgeInput, LabeledPlan, MapInput};
use argonauts_core::worker::{Access, SessionSpec};
use argonauts_core::workflow::{StandardPhase, Step, StepRef};
use argonauts_core::{PhaseStatus, RepoId, RunId, sha256_hex};
use argonauts_crew::engine::host::Host;
use argonauts_crew::engine::shared::Shared;
use argonauts_crew::state::RunState;
use argonauts_crew::workflow::{Flow, StepAt, StepCall, StepDone, runner, yaml};
use argonauts_crew::{checkpoints, coordinated, divide, lead, plan};
use argonauts_testkit::repo::TempRepo;
use argonauts_testkit::services::{FakeServices, MemorySink, engine_context_in};
use argonauts_testkit::worker::{FakeSession, FakeWorker};

const CLAUDE: &str = "claude-code/claude-opus";
const CODEX: &str = "codex/gpt-5";
const AGY: &str = "antigravity-cli/gemini-3";

/// A host over fake workers and a temp repo, as the engine hands it to a step.
struct Bench {
    services: FakeServices,
    repo: TempRepo,
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
            main_agent: CLAUDE.parse()?,
            crew: Vec::new().into(),
            ..Settings::default()
        };
        for worker in workers {
            let added = services.workers.add(worker);
            all.crew.add(added_ref(&added));
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
        let host = Host::new(Arc::new(shared));
        Ok(Self {
            services,
            repo,
            host,
            sink,
        })
    }

    async fn step(&self, kind: &str, role: &str, extra: Value) -> anyhow::Result<StepDone> {
        let call = call(kind, role, extra, self.steps_run())?;
        let host = &self.host;
        Ok(match kind {
            "draft" => plan::draft_step(host, &call).await?,
            "map" => plan::map_step(host, &call).await?,
            "refute" => plan::refute_step(host, &call).await?,
            "synthesize" => plan::synthesize_step(host, &call).await?,
            "judge" => plan::judge_step(host, &call).await?,
            other => anyhow::bail!("no step kind {other}"),
        })
    }

    fn steps_run(&self) -> u32 {
        self.sink.events().len() as u32
    }

    fn plan_events(&self) -> Vec<PlanEvent> {
        self.sink
            .events()
            .into_iter()
            .filter_map(|event| match event.kind {
                RunEventKind::Plan(event) => Some(event),
                _ => None,
            })
            .collect()
    }

    fn state(&self) -> RunState {
        self.host.read(Clone::clone)
    }

    fn specs(&self, worker: &str) -> Vec<SessionSpec> {
        self.services
            .workers
            .fake(worker)
            .map(|fake| fake.specs())
            .unwrap_or_default()
    }

    fn specs_of(&self, worker: &str, step: StepName) -> Vec<SessionSpec> {
        self.specs(worker)
            .into_iter()
            .filter(|spec| spec.step == Some(step))
            .collect()
    }

    /// Starts the plan checkpoint step and waits for the checkpoint it opens.
    async fn plan_checkpoint(&self) -> anyhow::Result<(PlanStep, CheckpointOpened)> {
        let opened_before = plan_checkpoints(&self.sink.events()).len();
        let host = self.host.clone();
        let call = call(
            "checkpoint",
            "orchestrator",
            json!({ "checkpoint": "plan" }),
            self.steps_run(),
        )?;
        let mut step: PlanStep =
            tokio::spawn(async move { Ok(divide::plan_checkpoint_step(&host, &call).await?) });
        let opened = tokio::time::timeout(Duration::from_secs(20), async {
            loop {
                if let Some(opened) = plan_checkpoints(&self.sink.events())
                    .into_iter()
                    .nth(opened_before)
                {
                    return Ok(opened);
                }
                if step.is_finished() {
                    let ended = (&mut step).await?;
                    anyhow::bail!("the step ended before the plan approval opened: {ended:?}");
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await??;
        Ok((step, opened))
    }

    fn answer(
        &self,
        opened: &CheckpointOpened,
        action: AnswerAction,
        text: Option<&str>,
    ) -> anyhow::Result<()> {
        checkpoints::answer(
            &self.host,
            CheckpointAnswer {
                id: opened.id.clone(),
                action,
                text: text.map(str::to_owned),
                evidence_sha256: opened.evidence_sha256.clone(),
                cards: Vec::new(),
            },
        )
        .map_err(anyhow::Error::msg)
    }

    fn divide_events(&self) -> Vec<DivideEvent> {
        self.sink
            .events()
            .into_iter()
            .filter_map(|event| match event.kind {
                RunEventKind::Divide(event) => Some(event),
                _ => None,
            })
            .collect()
    }
}

type PlanStep = tokio::task::JoinHandle<anyhow::Result<StepDone>>;

fn plan_checkpoints(events: &[RunEvent]) -> Vec<CheckpointOpened> {
    events
        .iter()
        .filter_map(|event| match &event.kind {
            RunEventKind::Checkpoint(CheckpointEvent::Opened(opened))
                if opened.kind == CheckpointKind::Plan =>
            {
                Some((**opened).clone())
            }
            _ => None,
        })
        .collect()
}

fn plan_packet(opened: &CheckpointOpened) -> anyhow::Result<&PlanPacket> {
    match &opened.packet {
        CheckpointPacket::Plan(packet) => Ok(packet),
        other => anyhow::bail!("not a plan packet: {other:?}"),
    }
}

fn added_ref(worker: &FakeWorker) -> WorkerRef {
    use argonauts_core::worker::Worker;
    worker.worker_ref().clone()
}

/// A step call at its own place in a workflow, so asks of different steps never share an
/// identity.
fn call(kind: &str, role: &str, extra: Value, index: u32) -> anyhow::Result<StepCall> {
    let mut step = json!({ "kind": kind, "role": role });
    if let (Some(step), Value::Object(extra)) = (step.as_object_mut(), extra) {
        step.extend(extra);
    }
    let step: Step = serde_json::from_value(step)?;
    Ok(StepCall {
        at: StepAt {
            step: StepRef {
                workflow: WorkflowId::numbered(1),
                phase: 0,
                path: vec![index],
            },
            round: 1,
        },
        depth: 0,
        standard: StandardPhase::Plan,
        step,
        item: None,
    })
}

/// The first prompt part that parses as `T`: the step's input, as JSON.
/// The run's first event, as the supervisor writes it, based on the repo's first commit.
fn started(repo: &TempRepo) -> anyhow::Result<RunEvent> {
    let head = repo.git(&["rev-parse", "HEAD"])?;
    let started: argonauts_core::events::run::RunStarted = serde_json::from_value(json!({
        "request": "Rate limit the API",
        "title": "Rate limit the API",
        "mode": "crew",
        "forced": true,
        "workflow": "council",
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

/// A plan whose text carries `marker`, choosing `counters` for its one disputed topic.
fn plan_json(marker: &str, counters: &str) -> Value {
    json!({
        "summary": format!("{marker} rate limits the API."),
        "approach": format!("{marker} adds a middleware after auth."),
        "pieces": [{
            "name": "limiter",
            "description": "The token bucket.",
            "paths": ["src/limit/"],
            "interfaces": ["Limiter::check"],
            "depends_on": []
        }],
        "choices": [{
            "topic": "counters",
            "choice": counters,
            "rationale": format!("Keep counters in {counters}.")
        }],
        "risks": ["Bursts at the window edge."],
        "test_strategy": "Unit tests for the bucket.",
        "new_dependencies": []
    })
}

/// The plan's marker, from its sealed text.
fn marker(text: &str) -> &str {
    ["CLAUDE", "CODEX", "AGY", "OTHER", "MERGED", "QWEN", "OSS"]
        .into_iter()
        .find(|m| text.contains(&format!("{m} rate limits")))
        .unwrap_or("NONE")
}

/// A mapper answer: one disputed topic when `disputed`, quoting each plan's own choice.
fn map_json(plans: &[LabeledPlan], disputed: bool, good_quotes: bool) -> Value {
    let positions: Vec<Value> = plans
        .iter()
        .map(|plan| {
            let choice = if plan.text.contains("Keep counters in Redis") {
                "Redis"
            } else {
                "memory"
            };
            let quote = if good_quotes {
                format!("Keep counters in {choice}.")
            } else {
                format!("Counters live in {choice}, as agreed.")
            };
            json!({ "label": plan.label, "summary": choice, "quote": quote })
        })
        .collect();
    json!({ "topics": [{ "name": "counters", "disputed": disputed, "positions": positions }] })
}

/// Scores every plan shown by its marker: `scores` maps a marker to its value.
fn judge_json(input: &JudgeInput, scores: &[(&str, f64)]) -> Value {
    let entries: Vec<Value> = input
        .plans
        .iter()
        .map(|plan| {
            let value = scores
                .iter()
                .find(|(m, _)| *m == marker(&plan.text))
                .map_or(5.0, |(_, v)| *v);
            json!({
                "plan": plan.label,
                "criteria": [value, value, value, value, value],
                "rationale": format!("{} scores {value}", marker(&plan.text)),
                "addressed_to_judges": false
            })
        })
        .collect();
    json!({ "scores": entries })
}

/// A worker that drafts with `marker`, maps, critiques, judges, and revises, by step.
fn crew_worker(
    name: &str,
    family: &str,
    marker: &'static str,
    counters: &'static str,
    disputed: bool,
    scores: &'static [(&'static str, f64)],
) -> FakeWorker {
    FakeWorker::harness(name, family).respond_with(answers(marker, counters, disputed, scores))
}

/// A crew worker's answers, as [`crew_worker`] gives them.
fn answers(
    marker: &'static str,
    counters: &'static str,
    disputed: bool,
    scores: &'static [(&'static str, f64)],
) -> impl Fn(&SessionSpec) -> FakeSession + Send + Sync + 'static {
    move |spec| {
        let value = match spec.step {
            Some(StepName::Draft) if spec.role == RoleName::PhaseLead => {
                plan_json("MERGED", counters)
            }
            Some(StepName::Draft) => plan_json(marker, counters),
            Some(StepName::Revise) => plan_json(marker, "Redis"),
            Some(StepName::Map) => {
                let input: MapInput = input_of(spec).unwrap_or(MapInput { plans: Vec::new() });
                map_json(&input.plans, disputed, true)
            }
            Some(StepName::Critique) => json!({ "points": [
                { "plan": 1, "topic": "counters", "claim": "Memory loses counts on restart.",
                  "severity": "high", "suggestion": "Use Redis." },
                { "plan": 2, "topic": "counters", "claim": "Redis adds a dependency.",
                  "severity": "low", "suggestion": "Say how it fails open." }
            ]}),
            Some(StepName::Judge) => match input_of::<JudgeInput>(spec) {
                Some(input) => judge_json(&input, scores),
                None => json!({ "scores": [] }),
            },
            Some(StepName::ProposeSpike) => json!({ "proposals": [] }),
            Some(StepName::SpikeReport) => spike_report(spec),
            Some(StepName::PlanToCards) => cards_json(),
            Some(StepName::Predict) => json!({
                "p_pass_gates_first_try": 0.7,
                "p_within_time_cap": 0.8,
                "p_at_most_two_attempts": 0.9,
            }),
            _ => json!({}),
        };
        FakeSession::structured(value).with_vendor_session(&format!("{marker}-session"))
    }
}

/// `answer`, except that the first `times` sessions of `step` that `fails` picks end on a 401
/// the CLI's own login check contradicts, counted in `failed` with any other worker's.
fn failing_first(
    step: StepName,
    times: usize,
    failed: &Arc<std::sync::atomic::AtomicUsize>,
    fails: impl Fn(&SessionSpec) -> bool + Send + Sync + 'static,
    answer: impl Fn(&SessionSpec) -> FakeSession + Send + Sync + 'static,
) -> impl Fn(&SessionSpec) -> FakeSession + Send + Sync + 'static {
    let failed = failed.clone();
    move |spec| {
        if spec.step == Some(step)
            && fails(spec)
            && failed.fetch_add(1, std::sync::atomic::Ordering::SeqCst) < times
        {
            return FakeSession::ended(EndReason::Auth {
                message: "workspace routing discovery unauthorized (401)".to_owned(),
            });
        }
        answer(spec)
    }
}

/// The run in full auto, as Shift+Tab sets it.
fn autopilot() -> RunEvent {
    RunEvent {
        seq: 0,
        ts: time::OffsetDateTime::UNIX_EPOCH,
        run_id: RunId::new("run-01").expect("the run id"),
        card_id: None,
        session: None,
        source: "app".to_owned(),
        kind: RunEventKind::Run(RunLifecycle::ApprovalModeChanged {
            mode: ApprovalMode::Autopilot,
            by: Party::User,
        }),
    }
}

/// Two cards: the interface contracts first needs, and the limiter.
fn cards_json() -> Value {
    let card = |key: &str, kind: &str, owned: &str, depends: &[&str]| {
        json!({
            "key": key,
            "title": key,
            "task": format!("Build the {key}."),
            "kind": kind,
            "owned_paths": [owned],
            "read_only_paths": [],
            "depends_on": depends,
            "criteria": ["G1"],
        })
    };
    json!({ "cards": [
        card("api", "interface", "src/limit/api.rs", &[]),
        card("limiter", "build", "src/limit/bucket/**", &["api"]),
    ]})
}

fn spike_report(spec: &SessionSpec) -> Value {
    let option = if prompt_text(spec).contains("a Redis counter") {
        "Redis"
    } else {
        "memory"
    };
    json!({ "topic": "counters", "option": option,
            "measurements": [{ "metric": "p50_ms", "value": 2.0, "unit": "ms" }],
            "finding": format!("{option} works"), "works": true })
}

const CLAUDE_WINS: &[(&str, f64)] = &[("CLAUDE", 9.0), ("CODEX", 5.0), ("AGY", 5.0)];

fn three(disputed: bool, scores: &'static [(&'static str, f64)]) -> Vec<FakeWorker> {
    vec![
        crew_worker(CLAUDE, "claude", "CLAUDE", "Redis", disputed, scores),
        crew_worker(CODEX, "gpt", "CODEX", "memory", disputed, scores),
        crew_worker(AGY, "gemini", "AGY", "Redis", disputed, scores),
    ]
}

fn approved_goal() -> anyhow::Result<RunEvent> {
    let file = GoalFile {
        run: RunId::new("run-01")?,
        version: 1,
        summary: "Rate limit the API".to_owned(),
        criteria: vec![Criterion::new("G1", "The 101st call in a minute gets 429")],
        in_scope: Vec::new(),
        out_of_scope: Vec::new(),
        assumptions: Vec::new(),
        answers: Vec::new(),
        source: GoalSource::Restatements,
        path: ".argonauts/goal.md".into(),
        sha256: sha256_hex(b"goal"),
    };
    Ok(RunEvent {
        seq: 0,
        ts: time::OffsetDateTime::UNIX_EPOCH,
        run_id: RunId::new("run-01")?,
        card_id: None,
        session: None,
        source: "conductor".to_owned(),
        kind: RunEventKind::Goal(GoalEvent::Approved {
            file,
            by: Party::User,
        }),
    })
}

fn sealed_labels(events: &[PlanEvent]) -> Vec<(PlanLabel, WorkerRef, u8)> {
    events
        .iter()
        .filter_map(|event| match event {
            PlanEvent::Sealed {
                label,
                worker,
                round,
                ..
            } => Some((*label, worker.clone(), *round)),
            _ => None,
        })
        .collect()
}

#[tokio::test]
async fn plan_drafts_blind_and_sealed() -> anyhow::Result<()> {
    let bench = Bench::new(three(true, CLAUDE_WINS), |_| {}, Vec::new())?;
    let done = bench
        .step("draft", "drafter", json!({ "blind": true }))
        .await?;
    assert_eq!(done.end, StepEnd::Done);

    let events = bench.plan_events();
    let mut labels: Vec<PlanLabel> = sealed_labels(&events).iter().map(|s| s.0).collect();
    labels.sort_unstable();
    assert_eq!(labels, [1, 2, 3]);
    for event in &events {
        let PlanEvent::Sealed { path, sha256, .. } = event else {
            continue;
        };
        let text = std::fs::read_to_string(path)?;
        assert_eq!(&sha256_hex(text.as_bytes()), sha256);
        assert_eq!(std::fs::metadata(path)?.permissions().mode() & 0o777, 0o600);
        assert!(path.starts_with(bench.repo.home().join("state/repo-test/runs/run-01/drafts")));
        assert!(!text.contains(CLAUDE) && !text.contains(CODEX), "{text}");
    }

    // Each drafter ran fresh and read-only, and saw no other draft.
    for (worker, own) in [(CLAUDE, "CLAUDE"), (CODEX, "CODEX"), (AGY, "AGY")] {
        let specs = bench.specs_of(worker, StepName::Draft);
        assert_eq!(specs.len(), 1, "{worker}");
        assert_eq!(specs[0].access, Access::ReadOnly);
        assert_eq!(specs[0].resume, None);
        let prompt = prompt_text(&specs[0]);
        for other in ["CLAUDE", "CODEX", "AGY"].into_iter().filter(|m| *m != own) {
            assert!(
                !prompt.contains(&format!("{other} rate limits")),
                "{worker} saw {other}"
            );
        }
    }

    // Running the step again seals nothing new.
    bench.step("draft", "drafter", json!({})).await?;
    assert_eq!(sealed_labels(&bench.plan_events()).len(), 3);
    Ok(())
}

#[tokio::test]
async fn plan_quote_check() -> anyhow::Result<()> {
    // The mapper misquotes once, then quotes exactly.
    let answers = std::sync::Mutex::new(0u32);
    let mapper = FakeWorker::harness("opencode/llama", "llama").respond_with(move |spec| {
        let input: MapInput = input_of(spec).unwrap_or(MapInput { plans: Vec::new() });
        let mut n = answers
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        *n += 1;
        FakeSession::structured(map_json(&input.plans, true, *n > 1))
    });
    let mut workers = three(true, CLAUDE_WINS);
    workers.push(mapper);
    let bench = Bench::new(workers, |_| {}, Vec::new())?;
    bench
        .step(
            "draft",
            "drafter",
            json!({ "workers": [CLAUDE, CODEX, AGY] }),
        )
        .await?;
    let done = bench
        .step("map", "mapper", json!({ "workers": ["opencode/llama"] }))
        .await?;
    assert!(done.summary.unwrap_or_default().contains("1 disputed"));

    let specs = bench.specs_of("opencode/llama", StepName::Map);
    assert_eq!(specs.len(), 2, "one more run after a failed quote");
    assert!(prompt_text(&specs[1]).contains("not in the plan word for word"));
    let state = bench.state();
    let map = state.plan.latest_map().expect("mapped");
    assert!(map.topics[0].positions.iter().all(|p| p.quote_verified));
    assert!(state.plan.unverified_topics().is_empty());
    Ok(())
}

#[tokio::test]
async fn plan_quote_check_flags_what_still_fails() -> anyhow::Result<()> {
    let mapper = FakeWorker::harness("opencode/llama", "llama").respond_with(|spec| {
        let input: MapInput = input_of(spec).unwrap_or(MapInput { plans: Vec::new() });
        FakeSession::structured(map_json(&input.plans, true, false))
    });
    let mut workers = three(true, CLAUDE_WINS);
    workers.push(mapper);
    let bench = Bench::new(workers, |_| {}, Vec::new())?;
    bench
        .step(
            "draft",
            "drafter",
            json!({ "workers": [CLAUDE, CODEX, AGY] }),
        )
        .await?;
    bench
        .step("map", "mapper", json!({ "workers": ["opencode/llama"] }))
        .await?;
    assert_eq!(bench.specs_of("opencode/llama", StepName::Map).len(), 2);
    assert_eq!(bench.state().plan.unverified_topics(), ["counters"]);
    Ok(())
}

#[tokio::test]
async fn plan_critique_and_revision() -> anyhow::Result<()> {
    let bench = Bench::new(three(true, CLAUDE_WINS), |_| {}, Vec::new())?;
    bench.step("draft", "drafter", json!({})).await?;
    bench
        .step("map", "mapper", json!({ "workers": [AGY] }))
        .await?;
    let done = bench.step("refute", "critic", json!({})).await?;
    assert_eq!(done.end, StepEnd::Done);

    let critiques: Vec<(WorkerRef, bool)> = bench
        .plan_events()
        .into_iter()
        .filter_map(|event| match event {
            PlanEvent::Critiqued {
                critic, designated, ..
            } => Some((critic, designated)),
            _ => None,
        })
        .collect();
    assert_eq!(critiques.len(), 3);
    assert_eq!(critiques.iter().filter(|(_, d)| *d).count(), 1);
    let crew_json = bench.repo.home().join("state/repo-test/crew.json");
    let rotation: Value = serde_json::from_slice(&std::fs::read(&crew_json)?)?;
    let designated = critiques
        .iter()
        .find(|(_, d)| *d)
        .map(|(c, _)| c.to_string());
    assert_eq!(
        rotation["critic"]["last"].as_str().map(str::to_owned),
        designated
    );
    // Every critic got the fixed checklist and only the disputed topic.
    let spec = &bench.specs_of(CODEX, StepName::Critique)[0];
    let prompt = prompt_text(spec);
    for item in plan::critique::CHECKLIST {
        assert!(prompt.contains(item), "{item}");
    }

    // Revision: each drafter resumes its own session.
    let done = bench.step("draft", "drafter", json!({})).await?;
    assert_eq!(done.end, StepEnd::Done);
    let rounds: Vec<u8> = sealed_labels(&bench.plan_events())
        .iter()
        .map(|s| s.2)
        .collect();
    assert_eq!(rounds.iter().filter(|r| **r == 2).count(), 3);
    for worker in [CLAUDE, CODEX, AGY] {
        let revisions = bench.specs_of(worker, StepName::Revise);
        assert_eq!(revisions.len(), 1, "{worker}");
        assert!(
            revisions[0].resume.is_some(),
            "{worker} resumes its own session"
        );
    }
    assert_eq!(bench.state().plan.current_round(), 2);
    Ok(())
}

#[tokio::test]
async fn ablation_no_critique_skips_round() -> anyhow::Result<()> {
    let bench = Bench::new(
        three(true, CLAUDE_WINS),
        |settings| settings.critique.rounds = 0,
        Vec::new(),
    )?;
    bench.step("draft", "drafter", json!({})).await?;
    bench
        .step("map", "mapper", json!({ "workers": [AGY] }))
        .await?;
    let refute = bench.step("refute", "critic", json!({})).await?;
    assert_eq!(refute.end, StepEnd::Skipped);
    let revise = bench.step("draft", "drafter", json!({})).await?;
    assert_eq!(revise.end, StepEnd::Skipped);
    let events = bench.plan_events();
    assert!(
        !events
            .iter()
            .any(|e| matches!(e, PlanEvent::Critiqued { .. }))
    );
    assert!(sealed_labels(&events).iter().all(|s| s.2 == 1));
    for worker in [CLAUDE, CODEX, AGY] {
        assert!(bench.specs_of(worker, StepName::Critique).is_empty());
        assert!(bench.specs_of(worker, StepName::Revise).is_empty());
    }
    Ok(())
}

#[tokio::test]
async fn plan_fast_path_merges_undisputed_drafts() -> anyhow::Result<()> {
    const COMPLETE: &[(&str, f64)] = &[("MERGED", 9.0)];
    let bench = Bench::new(three(false, COMPLETE), |_| {}, Vec::new())?;
    bench.step("draft", "drafter", json!({})).await?;
    bench
        .step("map", "mapper", json!({ "workers": [AGY] }))
        .await?;
    assert_eq!(
        bench.step("refute", "critic", json!({})).await?.end,
        StepEnd::Skipped
    );
    let done = bench.step("synthesize", "phase-lead", json!({})).await?;
    assert_eq!(done.end, StepEnd::Done);
    assert_eq!(
        bench.step("judge", "judge", json!({})).await?.end,
        StepEnd::Skipped
    );

    let events = bench.plan_events();
    assert!(events.iter().any(|e| matches!(
        e,
        PlanEvent::FastPath { merged_by } if merged_by.to_string() == CLAUDE
    )));
    let state = bench.state();
    let chosen = state.plan.chosen().expect("the merged plan");
    assert_eq!(chosen.label, argonauts_crew::state::plan::MERGED_LABEL);
    assert!(std::fs::read_to_string(&chosen.path)?.contains("MERGED rate limits"));
    // The phase lead merged once, and a judge of another family checked it.
    let merges: Vec<SessionSpec> = bench
        .specs_of(CLAUDE, StepName::Draft)
        .into_iter()
        .filter(|spec| spec.role == RoleName::PhaseLead)
        .collect();
    assert_eq!(merges.len(), 1);
    let checks =
        bench.specs_of(CODEX, StepName::Judge).len() + bench.specs_of(AGY, StepName::Judge).len();
    assert_eq!(checks, 1);
    assert_eq!(state.plan.decisions.len(), 1);
    Ok(())
}

/// Drafts, a map by `mapper` with the topic disputed, then the judge step.
async fn judged(bench: &Bench, mapper: &str) -> anyhow::Result<StepDone> {
    bench.step("draft", "drafter", json!({})).await?;
    bench
        .step("map", "mapper", json!({ "workers": [mapper] }))
        .await?;
    bench.step("judge", "judge", json!({})).await
}

#[tokio::test]
async fn plan_cross_judging_excludes_self_and_family() -> anyhow::Result<()> {
    // Two gpt-family drafters: neither may score the other's plan.
    let workers = vec![
        crew_worker(CLAUDE, "claude", "CLAUDE", "Redis", true, CLAUDE_WINS),
        crew_worker(CODEX, "gpt", "CODEX", "memory", true, CLAUDE_WINS),
        crew_worker(
            "opencode/gpt-oss",
            "gpt",
            "OTHER",
            "memory",
            true,
            CLAUDE_WINS,
        ),
    ];
    let bench = Bench::new(workers, |_| {}, Vec::new())?;
    judged(&bench, CLAUDE).await?;

    let events = bench.plan_events();
    let authors: Vec<(PlanLabel, WorkerRef, u8)> = sealed_labels(&events);
    let family = |w: &WorkerRef| {
        if w.to_string().starts_with("claude") {
            "claude"
        } else {
            "gpt"
        }
    };
    let mut scored = 0;
    for event in &events {
        let PlanEvent::Scored {
            judge,
            judge_family,
            plan,
            ..
        } = event
        else {
            continue;
        };
        scored += 1;
        let (_, author, _) = authors.iter().find(|a| a.0 == *plan).expect("author");
        assert_ne!(judge, author, "a judge scored its own plan");
        assert_ne!(
            judge_family,
            family(author),
            "{judge} scored its own provider"
        );
    }
    // The claude judge scores both gpt plans in both orders; each gpt judge scores the claude
    // plan in both orders.
    assert_eq!(scored, 8);
    for (worker, own) in [(CODEX, "CODEX"), ("opencode/gpt-oss", "OTHER")] {
        for spec in bench.specs_of(worker, StepName::Judge) {
            let input: JudgeInput = input_of(&spec).expect("judge input");
            assert!(
                input.plans.iter().all(|p| marker(&p.text) == "CLAUDE"),
                "{own}"
            );
            assert!(!prompt_text(&spec).contains(CLAUDE));
        }
    }
    let orders: Vec<Vec<PlanLabel>> = bench
        .specs_of(CLAUDE, StepName::Judge)
        .iter()
        .filter_map(input_of::<JudgeInput>)
        .map(|input| input.plans.iter().map(|p| p.label).collect())
        .collect();
    assert_eq!(orders.len(), 2);
    assert_eq!(
        orders[0].iter().rev().copied().collect::<Vec<_>>(),
        orders[1]
    );
    Ok(())
}

#[tokio::test]
async fn plan_near_tie_goes_to_checkpoint() -> anyhow::Result<()> {
    const EVEN: &[(&str, f64)] = &[("CLAUDE", 7.0), ("CODEX", 7.2), ("AGY", 6.9)];
    let bench = Bench::new(three(true, EVEN), |_| {}, Vec::new())?;
    let done = judged(&bench, AGY).await?;
    assert!(done.summary.unwrap_or_default().contains("near tie"));
    let events = bench.plan_events();
    let result = events
        .iter()
        .find_map(|e| match e {
            PlanEvent::Judged(result) => Some(result.clone()),
            _ => None,
        })
        .expect("judged");
    assert!(result.near_tie);
    assert_eq!(result.winner, None);
    assert!(!events.iter().any(|e| matches!(e, PlanEvent::Decided(_))));
    assert_eq!(bench.state().plan.chosen(), None);
    Ok(())
}

#[tokio::test]
async fn plan_decision_records() -> anyhow::Result<()> {
    let bench = Bench::new(three(true, CLAUDE_WINS), |_| {}, Vec::new())?;
    let done = judged(&bench, AGY).await?;
    let state = bench.state();
    let chosen = state.plan.chosen().expect("a clear winner");
    assert!(std::fs::read_to_string(&chosen.path)?.contains("CLAUDE rate limits"));
    assert!(
        done.summary
            .unwrap_or_default()
            .contains(&format!("Plan {} won", chosen.label))
    );

    let decisions = &state.plan.decisions;
    assert_eq!(decisions.len(), 1);
    let decision = &decisions[0];
    assert_eq!(decision.title, "counters");
    assert_eq!(decision.choice, "Redis");
    assert_eq!(decision.alternatives, ["memory"]);
    let path = decision.path.clone().expect("a file");
    assert_eq!(
        path.to_string_lossy(),
        ".argonauts/decisions/D-001-counters.md"
    );
    let branch = "argonauts/run-01/integration";
    let committed = bench
        .repo
        .git(&["show", &format!("{branch}:{}", path.display())])?;
    assert!(committed.starts_with("# D-001: counters"));
    let message = bench.repo.git(&["log", "-1", "--format=%B", branch])?;
    assert!(message.contains("Argonauts-Conductor: run-01"), "{message}");

    let finished = bench.sink.events().into_iter().any(|e| {
        matches!(
            e.kind,
            RunEventKind::Phase(PhaseChange {
                phase: RunPhase::Planning,
                status: PhaseStatus::Finished,
                ..
            })
        )
    });
    assert!(finished);
    Ok(())
}

#[tokio::test]
async fn reserved_judge_seat_non_drafting_family() -> anyhow::Result<()> {
    let seat = |reserve: bool| async move {
        let mut workers = three(true, CLAUDE_WINS);
        // Another family that drafts nothing, and a second gpt model that drafts nothing.
        workers.push(crew_worker(
            "opencode/llama",
            "llama",
            "OTHER",
            "Redis",
            true,
            CLAUDE_WINS,
        ));
        workers.push(crew_worker(
            "codex/gpt-mini",
            "gpt",
            "OTHER",
            "Redis",
            true,
            CLAUDE_WINS,
        ));
        let bench = Bench::new(
            workers,
            |settings| settings.judging.reserve_non_drafting_seat = reserve,
            Vec::new(),
        )?;
        bench
            .step(
                "draft",
                "drafter",
                json!({ "workers": [CLAUDE, CODEX, AGY] }),
            )
            .await?;
        bench
            .step("map", "mapper", json!({ "workers": [AGY] }))
            .await?;
        bench.step("judge", "judge", json!({})).await?;
        let llama = bench.specs_of("opencode/llama", StepName::Judge).len();
        let mini = bench.specs_of("codex/gpt-mini", StepName::Judge).len();
        anyhow::Ok((llama, mini))
    };
    assert_eq!(
        seat(true).await?,
        (2, 0),
        "the seat goes to the non-drafting provider"
    );
    assert_eq!(seat(false).await?, (0, 0));
    Ok(())
}

#[tokio::test]
async fn plan_spike_within_budget() -> anyhow::Result<()> {
    let proposals = json!({ "proposals": [
        { "topic": "counters", "question": "Redis or memory?",
          "options": [{ "label": "Redis", "build": "a Redis counter" },
                      { "label": "memory", "build": "a map counter" }],
          "measure": "p50 latency", "decides_by": "lower p50",
          "est_minutes": 10.0, "est_cost_usd": 1.0 },
        { "topic": "counters", "question": "A cluster?",
          "options": [{ "label": "cluster", "build": "three nodes" }],
          "measure": "p99", "decides_by": "lower p99",
          "est_minutes": 60.0, "est_cost_usd": 5.0 }
    ]});
    let lead = FakeWorker::harness(CLAUDE, "claude").respond_with(move |spec| {
        let value = match spec.step {
            Some(StepName::ProposeSpike) => proposals.clone(),
            Some(StepName::SpikeReport) => spike_report(spec),
            Some(StepName::Draft) => plan_json("CLAUDE", "Redis"),
            Some(StepName::Judge) => match input_of::<JudgeInput>(spec) {
                Some(input) => judge_json(&input, CLAUDE_WINS),
                None => json!({ "scores": [] }),
            },
            _ => json!({}),
        };
        FakeSession::structured(value)
    });
    let workers = vec![
        lead,
        crew_worker(CODEX, "gpt", "CODEX", "memory", true, CLAUDE_WINS),
        crew_worker(AGY, "gemini", "AGY", "Redis", true, CLAUDE_WINS),
    ];
    let bench = Bench::new(workers, |_| {}, vec![approved_goal()?])?;
    judged(&bench, AGY).await?;

    let events = bench.plan_events();
    let fits: Vec<bool> = events
        .iter()
        .filter_map(|e| match e {
            PlanEvent::SpikeProposed { fits_budget, .. } => Some(*fits_budget),
            _ => None,
        })
        .collect();
    assert_eq!(fits, [true, false]);
    let finished: Vec<String> = events
        .iter()
        .filter_map(|e| match e {
            PlanEvent::SpikeFinished { card, .. } => Some(card.to_string()),
            _ => None,
        })
        .collect();
    assert_eq!(finished.len(), 2);
    assert!(finished.contains(&"spike-001".to_owned()));
    assert!(finished.contains(&"spike-002".to_owned()));
    let spikes = bench.specs_of(CLAUDE, StepName::SpikeReport);
    let spikes: Vec<_> = spikes
        .into_iter()
        .chain(bench.specs_of(CODEX, StepName::SpikeReport))
        .chain(bench.specs_of(AGY, StepName::SpikeReport))
        .collect();
    assert_eq!(spikes.len(), 2);
    assert!(spikes.iter().all(|spec| spec.access == Access::Spike));
    for spec in bench.specs_of(CLAUDE, StepName::Judge) {
        let input: JudgeInput = input_of(&spec).expect("judge input");
        assert_eq!(input.spikes.len(), 2, "judges see the experiment reports");
    }

    // No spike branch is merged anywhere.
    let merged = bench.sink.events().into_iter().any(|e| {
        matches!(
            &e.kind,
            RunEventKind::Integration(argonauts_core::events::card::IntegrationEvent::Merged {
                card, ..
            }) if card.as_str().starts_with("spike-")
        )
    });
    assert!(!merged);
    // Whatever a spike committed stays on its own branch.
    let branches = bench.repo.git(&[
        "branch",
        "--list",
        "argonauts/*",
        "--format=%(refname:short)",
    ])?;
    let targets: Vec<&str> = branches
        .lines()
        .filter(|b| !b.contains("spike-"))
        .chain(["main"])
        .collect();
    let integration = "argonauts/run-01/integration";
    let base = if branches.lines().any(|b| b == integration) {
        integration
    } else {
        "main"
    };
    for spike in branches.lines().filter(|b| b.contains("spike-")) {
        let own = bench.repo.git(&["rev-list", &format!("{base}..{spike}")])?;
        for commit in own.lines() {
            for target in &targets {
                let merged = bench
                    .repo
                    .git(&["merge-base", "--is-ancestor", commit, target])
                    .is_ok();
                assert!(!merged, "{spike}'s {commit} reached {target}");
            }
        }
    }
    Ok(())
}

/// A spike whose session ends on a 401 goes again on its own session, and after a second 401
/// another agent runs it, in full auto, so both spikes still report.
#[tokio::test]
async fn a_401_on_a_spike_goes_again_then_to_another_agent() -> anyhow::Result<()> {
    for failures in [1, 2] {
        let proposals = json!({ "proposals": [
            { "topic": "counters", "question": "Redis or memory?",
              "options": [{ "label": "Redis", "build": "a Redis counter" },
                          { "label": "memory", "build": "a map counter" }],
              "measure": "p50 latency", "decides_by": "lower p50",
              "est_minutes": 10.0, "est_cost_usd": 1.0 }
        ]});
        let lead = move |spec: &SessionSpec| {
            let value = match spec.step {
                Some(StepName::ProposeSpike) => proposals.clone(),
                Some(StepName::SpikeReport) => spike_report(spec),
                Some(StepName::Draft) => plan_json("CLAUDE", "Redis"),
                Some(StepName::Judge) => match input_of::<JudgeInput>(spec) {
                    Some(input) => judge_json(&input, CLAUDE_WINS),
                    None => json!({ "scores": [] }),
                },
                _ => json!({}),
            };
            FakeSession::structured(value)
        };
        // The Redis option's spike sessions fail, the first `failures` times whoever runs them.
        let redis = |spec: &SessionSpec| prompt_text(spec).contains("a Redis counter");
        let failed = Arc::default();
        let spike = StepName::SpikeReport;
        let workers = vec![
            FakeWorker::harness(CLAUDE, "claude")
                .respond_with(failing_first(spike, failures, &failed, redis, lead)),
            FakeWorker::harness(CODEX, "gpt").respond_with(failing_first(
                spike,
                failures,
                &failed,
                redis,
                answers("CODEX", "memory", true, CLAUDE_WINS),
            )),
            FakeWorker::harness(AGY, "gemini").respond_with(failing_first(
                spike,
                failures,
                &failed,
                redis,
                answers("AGY", "Redis", true, CLAUDE_WINS),
            )),
        ];
        let bench = Bench::new(
            workers,
            |settings| settings.supervisor.retry_wait = Span::new("0s"),
            vec![approved_goal()?, autopilot()],
        )?;
        judged(&bench, AGY).await?;

        let finished: Vec<String> = bench
            .plan_events()
            .iter()
            .filter_map(|e| match e {
                PlanEvent::SpikeFinished { report, .. } => Some(report.option.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(finished.len(), 2, "after {failures} 401s: {finished:?}");
        assert!(finished.contains(&"Redis".to_owned()), "{finished:?}");
        let redis_spikes: Vec<(String, String)> = bench
            .sink
            .events()
            .iter()
            .filter_map(|event| match &event.kind {
                RunEventKind::Session(SessionEvent::Started(started))
                    if started.step == Some(StepName::SpikeReport) =>
                {
                    Some((
                        event
                            .session
                            .as_ref()
                            .map(ToString::to_string)
                            .unwrap_or_default(),
                        started.worker.to_string(),
                    ))
                }
                _ => None,
            })
            .collect();
        // Both options' sessions, the Redis one started again on its own key.
        let keys: std::collections::BTreeSet<&String> =
            redis_spikes.iter().map(|(key, _)| key).collect();
        assert_eq!(keys.len(), 2, "{redis_spikes:?}");
        let workers: std::collections::BTreeSet<&String> =
            redis_spikes.iter().map(|(_, worker)| worker).collect();
        if failures == 2 {
            assert!(workers.len() >= 2, "another agent ran it: {redis_spikes:?}");
        }
    }
    Ok(())
}

/// A judge's seat goes only to an agent from outside the companies of the plans it's shown,
/// and the stand-in's scores go under its own name and family. With none, the seat's scores
/// are dropped rather than given by a plan's own company.
#[tokio::test]
async fn a_judges_stand_in_comes_from_outside_the_companies_it_judges() -> anyhow::Result<()> {
    const GPT_OSS: &str = "opencode/gpt-oss";
    const QWEN: &str = "opencode/qwen3-coder";
    for with_qwen in [false, true] {
        let claude = FakeWorker::harness(CLAUDE, "claude").respond_with(failing_first(
            StepName::Judge,
            usize::MAX,
            &Arc::default(),
            |_| true,
            answers("CLAUDE", "Redis", false, CLAUDE_WINS),
        ));
        let mut workers = vec![
            claude,
            crew_worker(CODEX, "gpt", "CODEX", "memory", false, CLAUDE_WINS),
            crew_worker(AGY, "gemini", "AGY", "Redis", false, CLAUDE_WINS),
            crew_worker(GPT_OSS, "gpt", "OSS", "memory", false, CLAUDE_WINS),
        ];
        if with_qwen {
            workers.push(crew_worker(
                QWEN,
                "qwen",
                "QWEN",
                "memory",
                false,
                CLAUDE_WINS,
            ));
        }
        let drafters: Vec<WorkerRef> = [CLAUDE, CODEX, AGY]
            .iter()
            .map(|worker| worker.parse())
            .collect::<Result<_, _>>()?;
        let bench = Bench::new(
            workers,
            move |settings| {
                settings.supervisor.retry_wait = Span::new("0s");
                settings.roles.entry(RoleName::Drafter).or_default().workers = drafters;
            },
            vec![approved_goal()?, autopilot()],
        )?;
        // A judge is blind, so its stand-in's CLI must keep your plugins out, as OpenCode's
        // does here.
        bench.services.workers.isolating("opencode");
        judged(&bench, AGY).await?;

        let scored: Vec<(String, String)> = bench
            .plan_events()
            .iter()
            .filter_map(|e| match e {
                PlanEvent::Scored {
                    judge,
                    judge_family,
                    ..
                } => Some((judge.to_string(), judge_family.clone())),
                _ => None,
            })
            .collect();
        assert!(
            !scored
                .iter()
                .any(|(judge, _)| judge == CLAUDE || judge == GPT_OSS),
            "{scored:?}"
        );
        assert!(bench.specs_of(GPT_OSS, StepName::Judge).is_empty());
        if with_qwen {
            assert!(
                scored.contains(&(QWEN.to_owned(), "qwen".to_owned())),
                "{scored:?}"
            );
        } else {
            let dropped = bench.sink.events().iter().any(|event| {
                matches!(&event.kind, RunEventKind::Note(note)
                    if note.text.contains("scores are dropped"))
            });
            assert!(dropped);
        }
    }
    Ok(())
}

/// A drafter's seat goes to an agent that isn't drafting already, and the plan it seals goes
/// under its own name, in the drafter's place.
#[tokio::test]
async fn a_drafters_stand_in_seals_the_plan_under_its_own_name() -> anyhow::Result<()> {
    const QWEN: &str = "opencode/qwen3-coder";
    let claude = FakeWorker::harness(CLAUDE, "claude").respond_with(failing_first(
        StepName::Draft,
        usize::MAX,
        &Arc::default(),
        |_| true,
        answers("CLAUDE", "Redis", false, CLAUDE_WINS),
    ));
    let workers = vec![
        claude,
        crew_worker(CODEX, "gpt", "CODEX", "memory", false, CLAUDE_WINS),
        crew_worker(AGY, "gemini", "AGY", "Redis", false, CLAUDE_WINS),
        crew_worker(QWEN, "qwen", "QWEN", "memory", false, CLAUDE_WINS),
    ];
    let drafters: Vec<WorkerRef> = [CLAUDE, CODEX, AGY]
        .iter()
        .map(|worker| worker.parse())
        .collect::<Result<_, _>>()?;
    let bench = Bench::new(
        workers,
        move |settings| {
            settings.supervisor.retry_wait = Span::new("0s");
            settings.roles.entry(RoleName::Drafter).or_default().workers = drafters;
        },
        vec![approved_goal()?, autopilot()],
    )?;
    // Drafts are blind, so the stand-in's CLI must keep your plugins out, as OpenCode's does
    // here.
    bench.services.workers.isolating("opencode");
    bench.step("draft", "drafter", json!({})).await?;

    let sealed: Vec<String> = bench
        .plan_events()
        .iter()
        .filter_map(|e| match e {
            PlanEvent::Sealed { worker, .. } => Some(worker.to_string()),
            _ => None,
        })
        .collect();
    assert_eq!(sealed.len(), 3, "{sealed:?}");
    assert!(sealed.contains(&QWEN.to_owned()), "{sealed:?}");
    assert!(!sealed.contains(&CLAUDE.to_owned()), "{sealed:?}");
    // Codex and Antigravity draft once each, as themselves.
    assert_eq!(bench.specs_of(CODEX, StepName::Draft).len(), 1);
    assert_eq!(bench.specs_of(AGY, StepName::Draft).len(), 1);
    assert_eq!(bench.specs_of(QWEN, StepName::Draft).len(), 1);
    Ok(())
}

const QWEN: &str = "opencode/qwen3-coder";
const GPT_OSS: &str = "opencode/gpt-oss";
/// A judge that scores GPT OSS's draft over Qwen's.
const OSS_WINS: &[(&str, f64)] = &[("OSS", 9.0), ("QWEN", 4.0)];

/// Claude, Codex and Antigravity draft, with Qwen and GPT OSS as the fallbacks that may take a
/// drafter's seat, two at once. Claude answers its draft sessions as `claude` says.
fn bench_with_two_fallbacks(
    claude: impl Fn(&SessionSpec) -> FakeSession + Send + Sync + 'static,
) -> anyhow::Result<Bench> {
    let workers = vec![
        FakeWorker::harness(CLAUDE, "claude").respond_with(claude),
        crew_worker(CODEX, "gpt", "CODEX", "memory", false, OSS_WINS),
        crew_worker(AGY, "gemini", "AGY", "Redis", false, OSS_WINS),
        crew_worker(QWEN, "qwen", "QWEN", "memory", false, OSS_WINS),
        crew_worker(GPT_OSS, "gpt", "OSS", "Redis", false, OSS_WINS),
    ];
    let drafters: Vec<WorkerRef> = [CLAUDE, CODEX, AGY]
        .iter()
        .map(|worker| worker.parse())
        .collect::<Result<_, _>>()?;
    let bench = Bench::new(
        workers,
        move |settings| {
            settings.supervisor.retry_wait = Span::new("0s");
            settings.roles.entry(RoleName::Drafter).or_default().workers = drafters;
            // The drafters follow the workers' fallback table.
            let table = settings
                .fallback
                .roles
                .entry(CrewRole::Workers)
                .or_default();
            table.together = Some(2);
        },
        vec![approved_goal()?, autopilot()],
    )?;
    // Drafts are blind, so a fallback's CLI must keep your plugins out, as OpenCode's does here.
    bench.services.workers.isolating("opencode");
    Ok(bench)
}

/// Who sealed the plans, and what the transcript said.
fn sealed_and_said(bench: &Bench) -> (Vec<String>, Vec<String>) {
    let sealed = bench
        .plan_events()
        .iter()
        .filter_map(|event| match event {
            PlanEvent::Sealed { worker, .. } => Some(worker.to_string()),
            _ => None,
        })
        .collect();
    let said = bench
        .sink
        .events()
        .into_iter()
        .filter_map(|event| match event.kind {
            RunEventKind::Note(note) => Some(note.text),
            _ => None,
        })
        .collect();
    (sealed, said)
}

/// With `together = 2`, a drafter that's out as its draft starts has two fallbacks draft in
/// its place at once. A judge from another provider than both picks one, numbered and
/// unnamed, and that draft is the seat's plan.
#[tokio::test]
async fn two_fallbacks_draft_at_once_and_a_judge_picks_the_seats_plan() -> anyhow::Result<()> {
    let bench = bench_with_two_fallbacks(answers("CLAUDE", "Redis", false, OSS_WINS))?;
    let claude_code = argonauts_core::names::VendorId("claude-code".to_owned());
    bench
        .services
        .health
        .set(&claude_code, HealthState::Down, Some("server errors"));
    bench.step("draft", "drafter", json!({})).await?;

    let (sealed, said) = sealed_and_said(&bench);
    assert_eq!(sealed.len(), 3, "one plan a seat: {sealed:?}");
    assert!(sealed.contains(&GPT_OSS.to_owned()), "{sealed:?} {said:?}");
    assert!(!sealed.contains(&QWEN.to_owned()), "{sealed:?}");
    assert!(bench.specs_of(CLAUDE, StepName::Draft).is_empty());
    for drafted_once in [CODEX, AGY, QWEN, GPT_OSS] {
        assert_eq!(bench.specs_of(drafted_once, StepName::Draft).len(), 1);
    }
    // Antigravity judges: Codex shares a provider with GPT OSS.
    let picks = bench.specs_of(AGY, StepName::Judge);
    assert_eq!(picks.len(), 1, "{said:?}");
    let shown = prompt_text(&picks[0]);
    assert!(shown.contains("QWEN rate limits") && shown.contains("OSS rate limits"));
    assert!(
        !shown.contains("qwen3-coder") && !shown.contains("gpt-oss"),
        "unnamed"
    );
    assert!(bench.specs_of(CODEX, StepName::Judge).is_empty());
    let both = said
        .iter()
        .find(|note| note.contains("both draft the plan"))
        .with_context(|| format!("no line on who drafts: {said:?}"))?;
    assert!(
        both.ends_with("· the judge picks the best · /fallback changes this"),
        "{both}"
    );
    assert!(
        said.iter()
            .any(|note| note.starts_with("The judge, ") && note.contains("'s draft out of 2 · ")),
        "{said:?}"
    );
    Ok(())
}

/// A drafter that stops midway hands its seat to one fallback as ever, and with `together = 2`
/// a second joins it, so the judge still picks between two drafts.
#[tokio::test]
async fn a_second_fallback_joins_the_one_that_took_a_drafters_seat_midway() -> anyhow::Result<()> {
    let bench = bench_with_two_fallbacks(failing_first(
        StepName::Draft,
        usize::MAX,
        &Arc::default(),
        |_| true,
        answers("CLAUDE", "Redis", false, OSS_WINS),
    ))?;
    bench.step("draft", "drafter", json!({})).await?;

    let (sealed, said) = sealed_and_said(&bench);
    assert_eq!(sealed.len(), 3, "{sealed:?}");
    assert!(sealed.contains(&GPT_OSS.to_owned()), "{sealed:?} {said:?}");
    assert!(!sealed.contains(&CLAUDE.to_owned()), "{sealed:?}");
    for drafted_once in [CODEX, AGY, QWEN, GPT_OSS] {
        assert_eq!(bench.specs_of(drafted_once, StepName::Draft).len(), 1);
    }
    // Claude can't draft here, but it's reachable and wrote neither draft, so it judges.
    assert_eq!(bench.specs_of(CLAUDE, StepName::Judge).len(), 1, "{said:?}");
    assert_eq!(
        said.iter()
            .filter(|note| note.contains("both draft the plan"))
            .count(),
        1,
        "{said:?}"
    );
    Ok(())
}

/// With no judge apart from the fallbacks themselves, the first draft stands and a line says
/// why.
#[tokio::test]
async fn with_no_judge_to_ask_the_first_fallbacks_draft_is_kept() -> anyhow::Result<()> {
    let bench = bench_with_two_fallbacks(answers("CLAUDE", "Redis", false, OSS_WINS))?;
    for out in ["claude-code", "codex", "antigravity-cli"] {
        let vendor = argonauts_core::names::VendorId(out.to_owned());
        bench
            .services
            .health
            .set(&vendor, HealthState::Down, Some("server errors"));
    }
    bench.step("draft", "drafter", json!({})).await?;

    let (sealed, said) = sealed_and_said(&bench);
    assert!(
        sealed
            .iter()
            .any(|worker| worker == QWEN || worker == GPT_OSS),
        "{sealed:?} {said:?}"
    );
    assert!(
        said.iter().any(|note| note.ends_with(
            "'s draft came first and is kept, since no judge apart from them could be asked"
        )),
        "{said:?}"
    );
    Ok(())
}

#[tokio::test]
async fn phase_lead_rotates_by_phase() -> anyhow::Result<()> {
    use argonauts_core::events::card::{CardBudget, CardKind, CardSpec, CardState};
    use lead::CardView;

    let spec = |id: &str, kind: CardKind, deps: &[&str]| CardSpec {
        id: id.parse().expect("task id"),
        title: id.to_owned(),
        task: String::new(),
        kind,
        owned_paths: Vec::new(),
        read_only_paths: Vec::new(),
        forbidden_paths: Vec::new(),
        hidden_paths: Vec::new(),
        depends_on: deps.iter().map(|d| d.parse().expect("task id")).collect(),
        criteria: Vec::new(),
        owner: None,
        reviewers: Vec::new(),
        budget: CardBudget::default(),
        predictions: None,
        item: None,
    };
    let specs = [
        spec("task-001", CardKind::Interface, &[]),
        spec("task-002", CardKind::Build, &["task-001"]),
        spec("task-003", CardKind::Build, &["task-002"]),
        spec("task-004", CardKind::Build, &["task-001"]),
        spec("task-005", CardKind::Chore, &[]),
    ];
    let owners: Vec<WorkerRef> = [CLAUDE, CODEX, AGY, CLAUDE, AGY]
        .iter()
        .map(|w| w.parse())
        .collect::<Result<_, _>>()?;
    let states = [
        CardState::Integrated,
        CardState::Running,
        CardState::Waiting,
        CardState::Running,
        CardState::Waiting,
    ];
    let cards: Vec<CardView<'_>> = specs
        .iter()
        .zip(&owners)
        .zip(states)
        .map(|((spec, owner), state)| CardView {
            spec,
            state,
            owner: Some(owner),
        })
        .collect();
    let default: WorkerRef = CLAUDE.parse()?;
    let lead_in = |phase| lead::lead_for(phase, &cards, None, &default).to_string();
    assert_eq!(lead_in(RunPhase::Goal), CLAUDE);
    assert_eq!(lead_in(RunPhase::Planning), CLAUDE);
    // The critical path is card-001, card-002, card-003; card-002 is the first not in yet.
    assert_eq!(lead_in(RunPhase::Build), CODEX);
    assert_eq!(lead_in(RunPhase::FinalCheck), AGY);
    assert_eq!(lead_in(RunPhase::Merge), AGY);
    Ok(())
}

#[tokio::test]
async fn planning_phase_names_its_lead() -> anyhow::Result<()> {
    let bench = Bench::new(three(true, CLAUDE_WINS), |_| {}, Vec::new())?;
    bench.step("draft", "drafter", json!({})).await?;
    let started = bench.sink.events().into_iter().find_map(|e| match e.kind {
        RunEventKind::Phase(PhaseChange {
            phase: RunPhase::Planning,
            status: PhaseStatus::Started,
            summary,
        }) => summary,
        _ => None,
    });
    assert!(started.unwrap_or_default().contains(CLAUDE));
    Ok(())
}

fn coordinator_output(api_reviewers: &[&str]) -> Value {
    json!({
        "plan": plan_json("COORD", "Redis"),
        "cards": [
            { "key": "store", "title": "Counter store", "task": "Build the store.", "kind": "build",
              "owned_paths": ["src/store/"], "read_only_paths": [], "depends_on": [],
              "criteria": ["G1"] },
            { "key": "api", "title": "Middleware", "task": "Add the middleware.", "kind": "build",
              "owned_paths": ["src/api.rs"], "read_only_paths": [], "depends_on": ["store"],
              "criteria": ["G1"] },
            { "key": "contract", "title": "Limiter interface", "task": "Stub the limiter.",
              "kind": "interface", "owned_paths": ["src/limiter.rs"], "read_only_paths": [],
              "depends_on": [], "criteria": [] }
        ],
        "assignments": [
            { "card": "store", "owner": CLAUDE, "reviewers": [CODEX, AGY] },
            { "card": "api", "owner": CODEX, "reviewers": api_reviewers },
            { "card": "contract", "owner": AGY, "reviewers": [CLAUDE, CODEX] }
        ]
    })
}

fn coordinator(answers: Vec<Value>) -> FakeWorker {
    let answers = std::sync::Mutex::new(answers.into_iter());
    FakeWorker::harness(CLAUDE, "claude").respond_with(move |_| {
        let mut answers = answers
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let value = answers.next().unwrap_or_else(|| json!({}));
        FakeSession::structured(value).with_vendor_session("coordinator-session")
    })
}

fn coordinated_bench(answers: Vec<Value>) -> anyhow::Result<Bench> {
    Bench::new(
        vec![
            coordinator(answers),
            FakeWorker::harness(CODEX, "gpt"),
            FakeWorker::harness(AGY, "gemini"),
            FakeWorker::harness("opencode/gpt-oss", "gpt"),
        ],
        |settings| {
            settings.review.per_task = 2;
            settings.divide.predictions = false;
        },
        Vec::new(),
    )
}

#[tokio::test]
async fn coordinated_crew_structured_delegation() -> anyhow::Result<()> {
    let bench = coordinated_bench(vec![coordinator_output(&[CLAUDE, AGY])])?;
    let done = bench
        .step("draft", "orchestrator", json!({ "workers": [CLAUDE] }))
        .await?;
    assert!(done.summary.unwrap_or_default().contains("3 tasks"));

    let events = bench.plan_events();
    assert!(events.iter().any(|e| matches!(
        e,
        PlanEvent::Coordinated {
            rejection: None,
            ..
        }
    )));
    let state = bench.state();
    let chosen = state.plan.chosen().expect("the orchestrator's plan");
    assert_eq!(chosen.worker.to_string(), CLAUDE);
    let cards = coordinated::load_cards(chosen)?;
    assert_eq!(cards.cards.len(), 3);
    assert_eq!(cards.assignments[1].owner, CODEX);

    let spec = &bench.specs_of(CLAUDE, StepName::Coordinate)[0];
    let input: CoordinateInput = input_of(spec).expect("coordinate input");
    assert!(input.rules.iter().any(|r| r.contains("another provider")));
    assert!(
        input
            .crew
            .iter()
            .any(|c| c.starts_with(&format!("`{CODEX}`"))),
        "{:?}",
        input.crew
    );
    assert_eq!(input.rejection, None);

    // Dividing the work takes the coordinator's cards, owners, and reviewers as they are.
    let (step, opened) = bench.plan_checkpoint().await?;
    let graph = &plan_packet(&opened)?.graph;
    let owned = |id: &str| -> anyhow::Result<(String, Vec<String>)> {
        let card = graph
            .cards
            .iter()
            .find(|card| card.id.as_str() == id)
            .ok_or_else(|| anyhow::anyhow!("no task {id}"))?;
        Ok((
            card.owner
                .as_ref()
                .map(ToString::to_string)
                .unwrap_or_default(),
            card.reviewers.iter().map(ToString::to_string).collect(),
        ))
    };
    assert_eq!(graph.cards.len(), 5, "three tasks and two test tasks");
    assert_eq!(
        owned("task-001")?,
        (CLAUDE.to_owned(), vec![CODEX.to_owned(), AGY.to_owned()])
    );
    assert_eq!(
        owned("task-002")?,
        (CODEX.to_owned(), vec![CLAUDE.to_owned(), AGY.to_owned()])
    );
    assert_eq!(owned("task-003")?.0, AGY);
    let tests: Vec<(String, String)> = graph
        .cards
        .iter()
        .filter_map(|card| match &card.kind {
            CardKind::AcceptanceTests { for_card } => Some((
                for_card.to_string(),
                card.owner
                    .as_ref()
                    .map(ToString::to_string)
                    .unwrap_or_default(),
            )),
            _ => None,
        })
        .collect();
    assert_eq!(
        tests,
        [
            ("task-001".to_owned(), CODEX.to_owned()),
            ("task-002".to_owned(), CLAUDE.to_owned())
        ],
        "each build task's first reviewer writes its tests"
    );
    let algorithms: Vec<RoutingAlgorithm> = bench
        .divide_events()
        .into_iter()
        .filter_map(|event| match event {
            DivideEvent::Owner { algorithm, .. } => Some(algorithm),
            _ => None,
        })
        .collect();
    assert_eq!(algorithms, [RoutingAlgorithm::Orchestrator; 5]);
    for worker in [CLAUDE, CODEX, AGY] {
        assert!(
            bench.specs_of(worker, StepName::PlanToCards).is_empty(),
            "nobody turns the orchestrator's plan into tasks again"
        );
    }
    bench.answer(&opened, AnswerAction::Abandon, None)?;
    step.await??;
    Ok(())
}

/// "Codex coordinates and Gemini reviews": the settings the request layer sets for it (the
/// app's tests resolve the same keys from those words). Codex alone coordinates, Antigravity
/// CLI alone reviews, and a plan that gives the review to anyone else has it moved to
/// Antigravity CLI by code, with one note, instead of going back to Codex.
#[tokio::test]
async fn codex_coordinates_and_gemini_reviews_with_exactly_those_roles() -> anyhow::Result<()> {
    use argonauts_core::settings::protocol::{JudgingMethod, PlanningBy, ReviewAssignment};
    use argonauts_core::workflow::WorkflowRef;

    let output = |reviewer: &str| {
        json!({
            "plan": plan_json("CODEX", "Redis"),
            "cards": [
                { "key": "store", "title": "Counter store", "task": "Build the store.",
                  "kind": "build", "owned_paths": ["src/store/"], "read_only_paths": [],
                  "depends_on": [], "criteria": ["G1"] },
                { "key": "api", "title": "Middleware", "task": "Add the middleware.",
                  "kind": "build", "owned_paths": ["src/api.rs"], "read_only_paths": [],
                  "depends_on": ["store"], "criteria": ["G1"] }
            ],
            "assignments": [
                { "card": "store", "owner": CLAUDE, "reviewers": [reviewer] },
                { "card": "api", "owner": CODEX, "reviewers": [AGY] }
            ]
        })
    };
    let answers = std::sync::Mutex::new(vec![output(CODEX), output(AGY)].into_iter());
    let coordinator = FakeWorker::harness(CODEX, "gpt").respond_with(move |_| {
        let value = answers
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .next()
            .unwrap_or_else(|| json!({}));
        FakeSession::structured(value).with_vendor_session("codex-coordinator")
    });
    let bench = Bench::new(
        vec![
            FakeWorker::harness(CLAUDE, "claude"),
            coordinator,
            FakeWorker::harness(AGY, "gemini"),
        ],
        |settings| {
            let worker = |text: &str| text.parse::<WorkerRef>().unwrap();
            settings.main_agent = worker(CODEX);
            settings.phases = WorkflowRef::new("orchestrator");
            settings.drafting.by = PlanningBy::Orchestrator;
            settings.judging.method = JudgingMethod::Orchestrator;
            settings.routing.algorithm = RoutingAlgorithm::Orchestrator;
            settings.review.assign = ReviewAssignment::Orchestrator;
            settings.review.per_task = 1;
            settings.divide.contracts_first = false;
            settings.divide.predictions = false;
            let roles = &mut settings.roles;
            roles.entry(RoleName::Orchestrator).or_default().workers = vec![worker(CODEX)];
            roles.entry(RoleName::Reviewer).or_default().workers = vec![worker(AGY)];
            roles.entry(RoleName::Worker).or_default().workers =
                vec![worker(CLAUDE), worker(CODEX)];
        },
        Vec::new(),
    )?;
    bench.step("draft", "orchestrator", json!({})).await?;

    assert_eq!(bench.specs_of(CODEX, StepName::Coordinate).len(), 1);
    let input: CoordinateInput =
        input_of(&bench.specs_of(CODEX, StepName::Coordinate)[0]).expect("coordinate input");
    assert!(
        input
            .crew
            .iter()
            .any(|line| line.starts_with(&format!("`{AGY}`"))
                && line.ends_with("may review tasks, not own them.")),
        "{:?}",
        input.crew
    );
    for other in [CLAUDE, AGY] {
        assert!(
            bench.specs_of(other, StepName::Coordinate).is_empty(),
            "{other}"
        );
    }
    let rejections: Vec<Option<String>> = bench
        .plan_events()
        .into_iter()
        .filter_map(|e| match e {
            PlanEvent::Coordinated { rejection, .. } => Some(rejection),
            _ => None,
        })
        .collect();
    assert_eq!(rejections, [None]);
    let moved: Vec<String> = bench
        .sink
        .events()
        .into_iter()
        .filter_map(|event| match event.kind {
            RunEventKind::Note(note) if note.text.starts_with("Code moved") => Some(note.text),
            _ => None,
        })
        .collect();
    assert_eq!(moved.len(), 1, "{moved:?}");
    assert!(
        moved[0].starts_with(
            "Code moved who owns or reviews 1 of the orchestrator's tasks to agents your rules \
             allow.\n`store`: Gemini 3 on Antigravity CLI reviews it, not GPT 5 on Codex, since \
             only Gemini 3 on Antigravity CLI may review tasks."
        ),
        "{moved:?}"
    );

    let (step, opened) = bench.plan_checkpoint().await?;
    let graph = &plan_packet(&opened)?.graph;
    assert_eq!(graph.cards.len(), 2);
    for card in &graph.cards {
        let reviewers: Vec<String> = card.reviewers.iter().map(ToString::to_string).collect();
        assert_eq!(reviewers, [AGY], "{}", card.id);
        let owner = card
            .owner
            .as_ref()
            .map(ToString::to_string)
            .unwrap_or_default();
        assert!([CLAUDE, CODEX].contains(&owner.as_str()), "{owner}");
    }
    bench.answer(&opened, AnswerAction::Abandon, None)?;
    step.await??;
    Ok(())
}

/// The sample repo's plan with contracts first, as its preset has it: the coordinator wrote
/// families where models go and rotated every agent through every role. Code moves each card
/// to an owner and a reviewer the rules allow, the reviewer writes each build card's tests, and
/// those tests are reviewed by a builder other than the tested card's owner, since the only
/// reviewer allowed wrote them. Supervised, the plan checkpoint still asks you.
#[tokio::test]
async fn the_trial_plan_with_contracts_first_keeps_every_card_reviewed() -> anyhow::Result<()> {
    use argonauts_core::settings::protocol::{JudgingMethod, PlanningBy, ReviewAssignment};
    use argonauts_core::workflow::WorkflowRef;

    let card = |key: &str, kind: &str, path: &str| {
        json!({ "key": key, "title": key, "task": format!("Write {path}."), "kind": kind,
                "owned_paths": [path], "read_only_paths": [], "depends_on": [],
                "criteria": ["G1"] })
    };
    let output = json!({
        "plan": plan_json("CODEX", "Redis"),
        "cards": [
            card("iface", "interface", "src/types.js"),
            card("impl", "build", "src/store.js"),
            card("cli", "build", "src/cli.js"),
        ],
        "assignments": [
            { "card": "iface", "owner": "codex/gpt", "reviewers": ["claude-code/claude"] },
            { "card": "impl", "owner": "claude-code/claude", "reviewers": ["antigravity-cli/gemini"] },
            { "card": "cli", "owner": "antigravity-cli/gemini", "reviewers": ["codex/gpt"] }
        ]
    });
    let coordinator = FakeWorker::harness(CODEX, "gpt").respond_with(move |_| {
        FakeSession::structured(output.clone()).with_vendor_session("codex-coordinator")
    });
    let bench = Bench::new(
        vec![
            FakeWorker::harness(CLAUDE, "claude"),
            coordinator,
            FakeWorker::harness(AGY, "gemini"),
        ],
        |settings| {
            let worker = |text: &str| text.parse::<WorkerRef>().unwrap();
            settings.main_agent = worker(CODEX);
            settings.phases = WorkflowRef::new("orchestrator");
            settings.drafting.by = PlanningBy::Orchestrator;
            settings.judging.method = JudgingMethod::Orchestrator;
            settings.routing.algorithm = RoutingAlgorithm::Orchestrator;
            settings.review.assign = ReviewAssignment::Orchestrator;
            settings.review.per_task = 1;
            settings.divide.contracts_first = true;
            settings.divide.predictions = false;
            let roles = &mut settings.roles;
            roles.entry(RoleName::Orchestrator).or_default().workers = vec![worker(CODEX)];
            roles.entry(RoleName::Reviewer).or_default().workers = vec![worker(AGY)];
            roles.entry(RoleName::Worker).or_default().workers =
                vec![worker(CLAUDE), worker(CODEX)];
        },
        Vec::new(),
    )?;
    bench.step("draft", "orchestrator", json!({})).await?;
    assert_eq!(bench.specs_of(CODEX, StepName::Coordinate).len(), 1);

    let (step, opened) = bench.plan_checkpoint().await?;
    let packet = plan_packet(&opened)?;
    let name = |worker: Option<&WorkerRef>| worker.map(ToString::to_string).unwrap_or_default();
    let owner_of = |id: &argonauts_core::CardId| {
        packet
            .graph
            .cards
            .iter()
            .find(|card| &card.id == id)
            .and_then(|card| card.owner.clone())
    };
    assert_eq!(packet.graph.cards.len(), 5, "{:#?}", packet.graph.cards);
    for card in &packet.graph.cards {
        let owner = name(card.owner.as_ref());
        let reviewers: Vec<String> = card.reviewers.iter().map(ToString::to_string).collect();
        match &card.kind {
            CardKind::AcceptanceTests { for_card } => {
                assert_eq!(owner, AGY, "{}", card.id);
                let tested = name(owner_of(for_card).as_ref());
                assert_eq!(reviewers.len(), 1, "{}", card.id);
                assert!(
                    [CLAUDE, CODEX].contains(&reviewers[0].as_str()) && reviewers[0] != tested,
                    "{}: {reviewers:?}, tested {tested}",
                    card.id
                );
            }
            _ => {
                assert!(
                    [CLAUDE, CODEX].contains(&owner.as_str()),
                    "{}: {owner}",
                    card.id
                );
                assert_eq!(reviewers, [AGY], "{}", card.id);
            }
        }
    }
    assert!(
        !packet.flags.iter().any(|flag| flag.contains("reviewers")),
        "{:?}",
        packet.flags
    );
    bench.answer(&opened, AnswerAction::Abandon, None)?;
    step.await??;
    Ok(())
}

#[tokio::test]
async fn coordinator_output_rejected_and_retried() -> anyhow::Result<()> {
    // Cards that break the rules go back to the coordinator, whoever they went to.
    let overlapping = || {
        let mut output = coordinator_output(&[CLAUDE, AGY]);
        output["cards"][1]["owned_paths"] = json!(["src/store/redis.rs"]);
        output
    };
    let dangling = || {
        let mut output = coordinator_output(&[CLAUDE, AGY]);
        output["cards"][1]["depends_on"] = json!(["ghost"]);
        output
    };
    // First answer: two cards own one path. Second answer keeps the rules.
    let bench = coordinated_bench(vec![overlapping(), coordinator_output(&[CLAUDE, AGY])])?;
    bench
        .step("draft", "orchestrator", json!({ "workers": [CLAUDE] }))
        .await?;
    let rejections: Vec<Option<String>> = bench
        .plan_events()
        .into_iter()
        .filter_map(|e| match e {
            PlanEvent::Coordinated { rejection, .. } => Some(rejection),
            _ => None,
        })
        .collect();
    assert_eq!(rejections.len(), 2);
    assert!(
        rejections[0]
            .as_deref()
            .unwrap_or_default()
            .contains("both own"),
        "{rejections:?}"
    );
    assert_eq!(rejections[1], None);
    let specs = bench.specs_of(CLAUDE, StepName::Coordinate);
    assert_eq!(specs.len(), 2);
    let retry: CoordinateInput = input_of(&specs[1]).expect("coordinate input");
    assert!(retry.rejection.unwrap_or_default().contains("both own"));
    assert!(
        specs[1].resume.is_some(),
        "the retry continues the orchestrator's session"
    );

    // Broken twice: the plan checkpoint asks you, with nothing to approve.
    let bench = coordinated_bench(vec![
        overlapping(),
        dangling(),
        coordinator_output(&[CLAUDE, AGY]),
    ])?;
    let done = bench
        .step("draft", "orchestrator", json!({ "workers": [CLAUDE] }))
        .await?;
    assert!(done.summary.unwrap_or_default().contains("asks you"));
    let state = bench.state();
    assert_eq!(state.plan.coordinator_rejections().len(), 2);
    assert_eq!(state.plan.chosen(), None);

    let (step, opened) = bench.plan_checkpoint().await?;
    let flags = &plan_packet(&opened)?.flags;
    assert!(
        flags.iter().any(|f| f.contains("broke the rules twice")),
        "{flags:?}"
    );
    assert!(
        flags
            .iter()
            .any(|f| f.starts_with("Try 1:") && f.contains("both own")),
        "{flags:?}"
    );
    assert!(
        flags
            .iter()
            .any(|f| f.starts_with("Try 2:") && f.contains("unknown task ghost")),
        "{flags:?}"
    );
    assert!(
        opened.options.iter().all(|option| !matches!(
            option.action,
            AnswerAction::Approve | AnswerAction::ApproveWithEdits
        )),
        "{:?}",
        opened.options
    );
    assert!(
        opened
            .options
            .iter()
            .any(|option| option.action == AnswerAction::SendBack)
    );
    assert!(bench.divide_events().is_empty());

    // Sent back with a note, the coordinator gets two new tries and reads the note.
    bench.answer(
        &opened,
        AnswerAction::SendBack,
        Some("Keep api out of gpt's hands."),
    )?;
    step.await??;
    let done = bench
        .step("draft", "orchestrator", json!({ "workers": [CLAUDE] }))
        .await?;
    assert!(done.summary.unwrap_or_default().contains("3 tasks"));
    let specs = bench.specs_of(CLAUDE, StepName::Coordinate);
    assert_eq!(specs.len(), 3);
    let again: CoordinateInput = input_of(&specs[2]).expect("coordinate input");
    assert!(
        again
            .rejection
            .unwrap_or_default()
            .contains("Keep api out of gpt's hands.")
    );
    assert!(bench.state().plan.chosen().is_some());
    Ok(())
}

/// Planning as a crew workflow runs it: drafts, a map, judging, and the plan checkpoint, until
/// the plan is decided.
const PLAN_AND_DIVIDE: &str = r#"
name: plan-and-divide
description: Blind drafts, a map, judging, and the plan approval
phases:
  - name: Plan
    standard: plan
    steps:
      - kind: loop_until
        name: planning
        until: plan_decided
        max: 3
        steps:
          - kind: draft
            name: drafts
            role: drafter
            blind: true
            input: goal
          - kind: map
            name: map
            role: mapper
            input: drafts
          - kind: judge
            name: scores
            role: judge
            blind: true
            input: drafts
          - kind: checkpoint
            checkpoint: plan
"#;

type Running = tokio::task::JoinHandle<anyhow::Result<Flow>>;

fn run_planning(bench: &Bench) -> anyhow::Result<Running> {
    let workflow = yaml::parse(PLAN_AND_DIVIDE).map_err(anyhow::Error::msg)?;
    let id = runner::start(&bench.host, None, workflow, None)?;
    let host = bench.host.clone();
    Ok(tokio::spawn(
        async move { Ok(runner::run(&host, &id).await?) },
    ))
}

fn plan_cards(bench: &Bench) -> Vec<CheckpointOpened> {
    bench
        .sink
        .events()
        .into_iter()
        .filter_map(|event| match event.kind {
            RunEventKind::Checkpoint(CheckpointEvent::Opened(opened))
                if opened.kind == CheckpointKind::Plan =>
            {
                Some(*opened)
            }
            _ => None,
        })
        .collect()
}

/// Waits for the plan card numbered `n` (from 1), the pick card included, and answers it.
async fn answer_plan(
    bench: &Bench,
    running: &mut Running,
    n: usize,
    action: impl FnOnce(&CheckpointOpened) -> AnswerAction,
    text: Option<&str>,
) -> anyhow::Result<CheckpointOpened> {
    let opened = tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            if let Some(opened) = plan_cards(bench).into_iter().nth(n - 1) {
                return Ok(opened);
            }
            if running.is_finished() {
                let ended = (&mut *running).await?;
                anyhow::bail!("the run ended before plan approval {n} opened: {ended:?}");
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .map_err(|_| anyhow::anyhow!("plan approval {n} never opened"))??;
    checkpoints::answer(
        &bench.host,
        CheckpointAnswer {
            id: opened.id.clone(),
            action: action(&opened),
            text: text.map(str::to_owned),
            evidence_sha256: opened.evidence_sha256.clone(),
            cards: Vec::new(),
        },
    )
    .map_err(anyhow::Error::msg)?;
    Ok(opened)
}

fn packet(opened: &CheckpointOpened) -> anyhow::Result<&PlanPacket> {
    match &opened.packet {
        CheckpointPacket::Plan(packet) => Ok(packet),
        other => anyhow::bail!("not a plan packet: {other:?}"),
    }
}

fn judged_results(bench: &Bench) -> Vec<JudgingResult> {
    bench
        .plan_events()
        .into_iter()
        .filter_map(|event| match event {
            PlanEvent::Judged(result) => Some(result),
            _ => None,
        })
        .collect()
}

#[tokio::test]
async fn a_near_tie_is_yours_to_pick_before_dividing() -> anyhow::Result<()> {
    let yours = |settings: &mut Settings| settings.approvals.answers = Some(Answerer::You);
    let bench = Bench::new(three(true, EVEN), yours, vec![approved_goal()?])?;
    let mut running = run_planning(&bench)?;

    let mut picked = None;
    let pick = answer_plan(
        &bench,
        &mut running,
        1,
        |opened| {
            // Not the leader, so the pick is yours and not the scores'.
            let option = &opened.options[1];
            picked = Some(option.label.clone());
            option.action.clone()
        },
        None,
    )
    .await?;
    assert_eq!(pick.title, "Which plan should the run follow?");
    let keys: String = pick.options.iter().map(|option| option.key).collect();
    assert_eq!(keys, "abc");
    assert!(pick.options[0].recommended);
    assert!(pick.options.iter().all(|option| option.detail.is_some()));
    assert!(packet(&pick)?.near_tie);

    let checkpoint = answer_plan(&bench, &mut running, 2, |_| AnswerAction::Approve, None).await?;
    assert_eq!(checkpoint.title, "Approve this plan?");
    assert_eq!(running.await??, Flow::Continue);

    let state = bench.state();
    let chosen = state.plan.chosen().expect("your pick");
    assert_eq!(Some(format!("Plan {}", chosen.label)), picked);
    let judging = packet(&checkpoint)?.judging.clone().expect("judged");
    assert_eq!(judging.winner, Some(chosen.label));
    assert!(judging.near_tie);
    assert!(
        bench
            .plan_events()
            .iter()
            .any(|event| matches!(event, PlanEvent::Decided(_)))
    );
    assert_eq!(state.cards.by_id.len(), 3);
    Ok(())
}

const EVEN: &[(&str, f64)] = &[("CLAUDE", 7.0), ("CODEX", 7.2), ("AGY", 6.9)];

/// In auto the lead answers the crew's questions, so a near tie goes to the plan the judges
/// scored highest, and only the plan approval after it remains.
#[tokio::test]
async fn in_auto_a_near_tie_goes_to_the_top_scored_plan() -> anyhow::Result<()> {
    let bench = Bench::new(three(true, EVEN), |_| {}, vec![approved_goal()?])?;
    let mut running = run_planning(&bench)?;
    let checkpoint = answer_plan(&bench, &mut running, 1, |_| AnswerAction::Approve, None).await?;
    assert_eq!(checkpoint.title, "Approve this plan?");
    assert_eq!(running.await??, Flow::Continue);

    let state = bench.state();
    let chosen = state.plan.chosen().expect("the top-scored plan");
    assert_eq!(chosen.worker, CODEX.parse()?);
    let judging = packet(&checkpoint)?.judging.clone().expect("judged");
    assert_eq!(judging.winner, Some(chosen.label));
    let noted = bench.sink.events().into_iter().any(|event| {
        matches!(&event.kind, RunEventKind::Note(note)
            if note.text.contains("couldn't separate the plans")
                && note.text.contains(&format!("Plan {}", chosen.label)))
    });
    assert!(noted);
    Ok(())
}

/// A plan stops for you only in supervised mode. In auto, one your rules and
/// the judge weighed and turned down goes back for another round without a question, and a
/// line says so.
#[tokio::test]
async fn in_auto_a_plan_the_judge_turns_down_goes_another_round_without_asking()
-> anyhow::Result<()> {
    use argonauts_core::policy::{PolicyOutcome, ShadowRecord, Verdict};
    use argonauts_core::settings::approvals::DecideWith;

    let bench = Bench::new(three(true, CLAUDE_WINS), |_| {}, vec![approved_goal()?])?;
    bench.services.policy.decide_next(PolicyOutcome::AskUser {
        violations: Vec::new(),
        shadow: Some(ShadowRecord {
            would: Verdict::Reject,
            by: DecideWith::RulesAndModel,
            reasons: vec!["two tasks edit the same file".to_owned()],
        }),
    });
    let mut running = run_planning(&bench)?;
    // The second round's plan is one nothing weighed, so that approval still waits.
    answer_plan(&bench, &mut running, 2, |_| AnswerAction::Approve, None).await?;
    assert_eq!(running.await??, Flow::Continue);

    let first = plan_cards(&bench).remove(0);
    let decided = bench
        .sink
        .events()
        .into_iter()
        .find_map(|event| match event.kind {
            RunEventKind::Checkpoint(CheckpointEvent::Decided(decided))
                if decided.id == first.id =>
            {
                Some(decided)
            }
            _ => None,
        });
    let decided = decided.expect("the first plan approval was settled");
    assert_eq!(decided.action, AnswerAction::SendBack);
    assert_eq!(decided.by, Party::Autopilot);
    let said = bench.sink.events().into_iter().any(|event| {
        matches!(&event.kind, RunEventKind::Note(note)
            if note.text.starts_with("A plan waits for you only in supervised mode")
                && note.text.contains("two tasks edit the same file"))
    });
    assert!(said, "no line says the plan went on without you");
    let rounds: Vec<u8> = sealed_labels(&bench.plan_events())
        .iter()
        .map(|(_, _, round)| *round)
        .collect();
    assert_eq!(rounds, [1, 1, 1, 2, 2, 2]);
    Ok(())
}

#[tokio::test]
async fn a_plan_sent_back_is_drafted_and_judged_again() -> anyhow::Result<()> {
    let bench = Bench::new(three(true, CLAUDE_WINS), |_| {}, vec![approved_goal()?])?;
    let mut running = run_planning(&bench)?;
    let note = "Keep the counters in memory; no Redis.";
    answer_plan(
        &bench,
        &mut running,
        1,
        |_| AnswerAction::SendBack,
        Some(note),
    )
    .await?;
    answer_plan(&bench, &mut running, 2, |_| AnswerAction::Approve, None).await?;
    assert_eq!(running.await??, Flow::Continue);

    let sealed = sealed_labels(&bench.plan_events());
    let rounds: Vec<u8> = sealed.iter().map(|(_, _, round)| *round).collect();
    assert_eq!(rounds, [1, 1, 1, 2, 2, 2], "{sealed:?}");
    assert_eq!(judged_results(&bench).len(), 2);

    // The second round's drafters, and the second division, get your note.
    let drafts = bench.specs_of(CODEX, StepName::Draft);
    assert_eq!(drafts.len(), 2);
    assert!(!prompt_text(&drafts[0]).contains(note));
    assert!(prompt_text(&drafts[1]).contains(note));
    let divided: Vec<SessionSpec> = [CLAUDE, CODEX, AGY]
        .iter()
        .flat_map(|worker| bench.specs_of(worker, StepName::PlanToCards))
        .collect();
    assert_eq!(divided.len(), 2);
    assert_eq!(
        divided
            .iter()
            .filter(|spec| prompt_text(spec).contains(note))
            .count(),
        1
    );

    let state = bench.state();
    let chosen = state.plan.chosen().expect("the second round's winner");
    assert_eq!(chosen.round, 2);
    assert_eq!(state.cards.by_id.len(), 3);
    Ok(())
}
