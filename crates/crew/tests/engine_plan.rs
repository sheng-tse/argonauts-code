//! Planning scenarios: each planning step runs on a host over fake workers and a temp repo,
//! and the recorded events and sealed files are checked.

use std::os::unix::fs::PermissionsExt;
use std::sync::Arc;
use std::time::Duration;

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
use argonauts_core::events::run::{Party, PhaseChange, RunPhase};
use argonauts_core::events::workflow::StepEnd;
use argonauts_core::events::{RunEvent, RunEventKind};
use argonauts_core::names::{PlanLabel, RoleName, WorkerRef, WorkflowId};
use argonauts_core::settings::Settings;
use argonauts_core::settings::protocol::RoutingAlgorithm;
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
            default_agent: CLAUDE.parse()?,
            crew: Vec::new(),
            ..Settings::default()
        };
        for worker in workers {
            let added = services.workers.add(worker);
            all.crew.push(added_ref(&added));
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
            "coordinator",
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
                    anyhow::bail!("the step ended before the plan checkpoint opened: {ended:?}");
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
    ["CLAUDE", "CODEX", "AGY", "OTHER", "MERGED"]
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
    FakeWorker::harness(name, family).respond_with(move |spec| {
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
    })
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
        criteria: vec![Criterion {
            id: "G1".to_owned(),
            text: "The 101st call in a minute gets 429".to_owned(),
        }],
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
            "{judge} scored its own family"
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
        "the seat goes to the non-drafting family"
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
        assert_eq!(input.spikes.len(), 2, "judges see the spike reports");
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

#[tokio::test]
async fn phase_lead_rotates_by_phase() -> anyhow::Result<()> {
    use argonauts_core::events::card::{CardBudget, CardKind, CardSpec, CardState};
    use lead::CardView;

    let spec = |id: &str, kind: CardKind, deps: &[&str]| CardSpec {
        id: id.parse().expect("card id"),
        title: id.to_owned(),
        task: String::new(),
        kind,
        owned_paths: Vec::new(),
        read_only_paths: Vec::new(),
        forbidden_paths: Vec::new(),
        hidden_paths: Vec::new(),
        depends_on: deps.iter().map(|d| d.parse().expect("card id")).collect(),
        criteria: Vec::new(),
        owner: None,
        reviewers: Vec::new(),
        budget: CardBudget::default(),
        predictions: None,
    };
    let specs = [
        spec("card-001", CardKind::Interface, &[]),
        spec("card-002", CardKind::Build, &["card-001"]),
        spec("card-003", CardKind::Build, &["card-002"]),
        spec("card-004", CardKind::Build, &["card-001"]),
        spec("card-005", CardKind::Chore, &[]),
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
            settings.review.per_card = 2;
            settings.divide.predictions = false;
        },
        Vec::new(),
    )
}

#[tokio::test]
async fn coordinated_crew_structured_delegation() -> anyhow::Result<()> {
    let bench = coordinated_bench(vec![coordinator_output(&[CLAUDE, AGY])])?;
    let done = bench
        .step("draft", "coordinator", json!({ "workers": [CLAUDE] }))
        .await?;
    assert!(done.summary.unwrap_or_default().contains("3 cards"));

    let events = bench.plan_events();
    assert!(events.iter().any(|e| matches!(
        e,
        PlanEvent::Coordinated {
            rejection: None,
            ..
        }
    )));
    let state = bench.state();
    let chosen = state.plan.chosen().expect("the coordinator's plan");
    assert_eq!(chosen.worker.to_string(), CLAUDE);
    let cards = coordinated::load_cards(chosen)?;
    assert_eq!(cards.cards.len(), 3);
    assert_eq!(cards.assignments[1].owner, CODEX);

    let spec = &bench.specs_of(CLAUDE, StepName::Coordinate)[0];
    let input: CoordinateInput = input_of(spec).expect("coordinate input");
    assert!(
        input
            .rules
            .iter()
            .any(|r| r.contains("another model family"))
    );
    assert!(input.crew.iter().any(|c| c.starts_with(CODEX)));
    assert_eq!(input.rejection, None);

    // Dividing the work takes the coordinator's cards, owners, and reviewers as they are.
    let (step, opened) = bench.plan_checkpoint().await?;
    let graph = &plan_packet(&opened)?.graph;
    let owned = |id: &str| -> anyhow::Result<(String, Vec<String>)> {
        let card = graph
            .cards
            .iter()
            .find(|card| card.id.as_str() == id)
            .ok_or_else(|| anyhow::anyhow!("no card {id}"))?;
        Ok((
            card.owner
                .as_ref()
                .map(ToString::to_string)
                .unwrap_or_default(),
            card.reviewers.iter().map(ToString::to_string).collect(),
        ))
    };
    assert_eq!(graph.cards.len(), 5, "three cards and two test cards");
    assert_eq!(
        owned("card-001")?,
        (CLAUDE.to_owned(), vec![CODEX.to_owned(), AGY.to_owned()])
    );
    assert_eq!(
        owned("card-002")?,
        (CODEX.to_owned(), vec![CLAUDE.to_owned(), AGY.to_owned()])
    );
    assert_eq!(owned("card-003")?.0, AGY);
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
            ("card-001".to_owned(), CODEX.to_owned()),
            ("card-002".to_owned(), CLAUDE.to_owned())
        ],
        "each build card's first reviewer writes its tests"
    );
    let algorithms: Vec<RoutingAlgorithm> = bench
        .divide_events()
        .into_iter()
        .filter_map(|event| match event {
            DivideEvent::Owner { algorithm, .. } => Some(algorithm),
            _ => None,
        })
        .collect();
    assert_eq!(algorithms, [RoutingAlgorithm::Coordinator; 5]);
    for worker in [CLAUDE, CODEX, AGY] {
        assert!(
            bench.specs_of(worker, StepName::PlanToCards).is_empty(),
            "nobody turns the coordinator's plan into cards again"
        );
    }
    bench.answer(&opened, AnswerAction::Abandon, None)?;
    step.await??;
    Ok(())
}

#[tokio::test]
async fn coordinator_output_rejected_and_retried() -> anyhow::Result<()> {
    // First answer: a gpt reviewer on a gpt owner's card. Second answer keeps the rules.
    let bench = coordinated_bench(vec![
        coordinator_output(&["opencode/gpt-oss", AGY]),
        coordinator_output(&[CLAUDE, AGY]),
    ])?;
    bench
        .step("draft", "coordinator", json!({ "workers": [CLAUDE] }))
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
            .contains("owner's family")
    );
    assert_eq!(rejections[1], None);
    let specs = bench.specs_of(CLAUDE, StepName::Coordinate);
    assert_eq!(specs.len(), 2);
    let retry: CoordinateInput = input_of(&specs[1]).expect("coordinate input");
    assert!(
        retry
            .rejection
            .unwrap_or_default()
            .contains("owner's family")
    );
    assert!(
        specs[1].resume.is_some(),
        "the retry continues the coordinator's session"
    );

    // Broken twice: the plan checkpoint asks you, with nothing to approve.
    let bench = coordinated_bench(vec![
        coordinator_output(&["opencode/gpt-oss", AGY]),
        coordinator_output(&[CODEX, AGY]),
        coordinator_output(&[CLAUDE, AGY]),
    ])?;
    let done = bench
        .step("draft", "coordinator", json!({ "workers": [CLAUDE] }))
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
            .any(|f| f.starts_with("Try 1:") && f.contains("owner's family"))
    );
    assert!(
        flags
            .iter()
            .any(|f| f.starts_with("Try 2:") && f.contains("its own card api"))
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
        .step("draft", "coordinator", json!({ "workers": [CLAUDE] }))
        .await?;
    assert!(done.summary.unwrap_or_default().contains("3 cards"));
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
description: Blind drafts, a map, judging, and the plan checkpoint
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
                anyhow::bail!("the run ended before plan card {n} opened: {ended:?}");
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .map_err(|_| anyhow::anyhow!("plan card {n} never opened"))??;
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
    const EVEN: &[(&str, f64)] = &[("CLAUDE", 7.0), ("CODEX", 7.2), ("AGY", 6.9)];
    let bench = Bench::new(three(true, EVEN), |_| {}, vec![approved_goal()?])?;
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
