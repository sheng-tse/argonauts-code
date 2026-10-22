//! Planning scenarios: each planning step runs on a host over fake workers and a temp repo,
//! and the recorded events and sealed files are checked.

use std::os::unix::fs::PermissionsExt;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};

use argonauts_core::api::ApprovalAnswer;
use argonauts_core::engine::RepoContext;
use argonauts_core::events::approval::{
    AnswerAction, ApprovalEvent, ApprovalKind, ApprovalOpened, ApprovalPacket, PlanPacket,
};
use argonauts_core::events::goal::{Criterion, GoalEvent, GoalFile, GoalSource};
use argonauts_core::events::plan::{DivideEvent, JudgingResult, PlanEvent};
use argonauts_core::events::run::{Party, PhaseChange, RunLifecycle, RunPhase};
use argonauts_core::events::session::{EndReason, SessionEvent};
use argonauts_core::events::task::TaskKind;
use argonauts_core::events::workflow::StepEnd;
use argonauts_core::events::{RunEvent, RunEventKind};
use argonauts_core::limits::HealthState;
use argonauts_core::names::{ModelRef, PlanLabel, RoleName, WorkflowId};
use argonauts_core::session_runner::{Access, SessionSpec};
use argonauts_core::settings::Settings;
use argonauts_core::settings::approvals::{Answerer, ApprovalMode};
use argonauts_core::settings::crew::CrewRole;
use argonauts_core::settings::crew::CrewSettings;
use argonauts_core::settings::fallback::Retry;
use argonauts_core::settings::protocol::RoutingAlgorithm;
use argonauts_core::steps::StepName;
use argonauts_core::steps::plan::{JudgeInput, LabeledPlan, MapInput, OrchestrateInput};
use argonauts_core::workflow::{StandardPhase, Step, StepRef};
use argonauts_core::{PhaseStatus, RepoId, RunId, sha256_hex};
use argonauts_crew::engine::host::Host;
use argonauts_crew::state::RunState;
use argonauts_crew::workflow::{Flow, StepAt, StepCall, StepDone, runner, yaml};
use argonauts_crew::{approvals, divide, orchestrated, plan, tie_breaker};
use argonauts_testkit::repo::TempRepo;
use argonauts_testkit::runner::{FakeRunner, FakeSession};
use argonauts_testkit::services::{FakeServices, MemorySink, engine_context_in};

use crate::support::shared_in;

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
        let shared = shared_in(&repo, &context, RunState::fold(&history))?;
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
            "critique" => plan::critique_step(host, &call).await?,
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

    /// Starts the plan approval step and waits for the approval it opens.
    async fn plan_approval(&self) -> anyhow::Result<(PlanStep, ApprovalOpened)> {
        let opened_before = plan_approvals(&self.sink.events()).len();
        let host = self.host.clone();
        let call = call(
            "approval",
            "orchestrator",
            json!({ "approval": "plan" }),
            self.steps_run(),
        )?;
        let mut step: PlanStep =
            tokio::spawn(async move { Ok(divide::plan_approval_step(&host, &call).await?) });
        let opened = tokio::time::timeout(Duration::from_secs(20), async {
            loop {
                if let Some(opened) = plan_approvals(&self.sink.events())
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
        opened: &ApprovalOpened,
        action: AnswerAction,
        text: Option<&str>,
    ) -> anyhow::Result<()> {
        approvals::answer(
            &self.host,
            ApprovalAnswer {
                id: opened.id.clone(),
                action,
                text: text.map(str::to_owned),
                evidence_sha256: opened.evidence_sha256.clone(),
                tasks: Vec::new(),
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

fn plan_approvals(events: &[RunEvent]) -> Vec<ApprovalOpened> {
    events
        .iter()
        .filter_map(|event| match &event.kind {
            RunEventKind::Approval(ApprovalEvent::Opened(opened))
                if opened.kind == ApprovalKind::Plan =>
            {
                Some((**opened).clone())
            }
            _ => None,
        })
        .collect()
}

fn plan_packet(opened: &ApprovalOpened) -> anyhow::Result<&PlanPacket> {
    match &opened.packet {
        ApprovalPacket::Plan(packet) => Ok(packet),
        other => anyhow::bail!("not a plan packet: {other:?}"),
    }
}

fn added_ref(worker: &FakeRunner) -> ModelRef {
    use argonauts_core::session_runner::SessionRunner;
    worker.model_ref().clone()
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

/// A mapping agent answer: one disputed topic when `disputed`, quoting each plan's own choice.
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
    provider: &str,
    marker: &'static str,
    counters: &'static str,
    disputed: bool,
    scores: &'static [(&'static str, f64)],
) -> FakeRunner {
    FakeRunner::cli(name, provider).respond_with(answers(marker, counters, disputed, scores))
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
            Some(StepName::Draft) if spec.role == RoleName::TieBreaker => {
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
            Some(StepName::ProposeExperiment) => json!({ "proposals": [] }),
            Some(StepName::ExperimentReport) => experiment_report(spec),
            Some(StepName::PlanToTasks) => tasks_json(),
            Some(StepName::Predict) => json!({
                "p_pass_checks_first_try": 0.7,
                "p_within_time_cap": 0.8,
                "p_at_most_two_tries": 0.9,
            }),
            _ => json!({}),
        };
        FakeSession::structured(value).with_cli_session(&format!("{marker}-session"))
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

/// The run in autopilot, as Shift+Tab sets it.
fn autopilot() -> RunEvent {
    RunEvent {
        seq: 0,
        ts: time::OffsetDateTime::UNIX_EPOCH,
        run_id: RunId::new("run-01").expect("the run id"),
        task_id: None,
        session: None,
        source: "app".to_owned(),
        kind: RunEventKind::Run(RunLifecycle::ApprovalModeChanged {
            mode: ApprovalMode::Autopilot,
            by: Party::User,
        }),
    }
}

/// Two tasks: the interface contracts first needs, and the limiter.
fn tasks_json() -> Value {
    let task = |key: &str, kind: &str, owned: &str, depends: &[&str]| {
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
    json!({ "tasks": [
        task("api", "interface", "src/limit/api.rs", &[]),
        task("limiter", "build", "src/limit/bucket/**", &["api"]),
    ]})
}

fn experiment_report(spec: &SessionSpec) -> Value {
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

fn three(disputed: bool, scores: &'static [(&'static str, f64)]) -> Vec<FakeRunner> {
    vec![
        crew_worker(CLAUDE, "anthropic", "CLAUDE", "Redis", disputed, scores),
        crew_worker(CODEX, "openai", "CODEX", "memory", disputed, scores),
        crew_worker(AGY, "google", "AGY", "Redis", disputed, scores),
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
        source: GoalSource::Readings,
        path: ".argonauts/goal.md".into(),
        sha256: sha256_hex(b"goal"),
    };
    Ok(RunEvent {
        seq: 0,
        ts: time::OffsetDateTime::UNIX_EPOCH,
        run_id: RunId::new("run-01")?,
        task_id: None,
        session: None,
        source: "argonauts-code".to_owned(),
        kind: RunEventKind::Goal(GoalEvent::Approved {
            file,
            by: Party::User,
        }),
    })
}

fn sealed_labels(events: &[PlanEvent]) -> Vec<(PlanLabel, ModelRef, u8)> {
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
        .step("draft", "draft", json!({ "blind": true }))
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

    // Each drafting agent ran fresh and read-only, and saw no other draft.
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
    bench.step("draft", "draft", json!({})).await?;
    assert_eq!(sealed_labels(&bench.plan_events()).len(), 3);
    Ok(())
}

#[tokio::test]
async fn plan_quote_check() -> anyhow::Result<()> {
    // The mapping agent misquotes once, then quotes exactly.
    let answers = std::sync::Mutex::new(0u32);
    let mapping_agent = FakeRunner::cli("opencode/llama", "meta").respond_with(move |spec| {
        let input: MapInput = input_of(spec).unwrap_or(MapInput { plans: Vec::new() });
        let mut n = answers
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        *n += 1;
        FakeSession::structured(map_json(&input.plans, true, *n > 1))
    });
    let mut workers = three(true, CLAUDE_WINS);
    workers.push(mapping_agent);
    let bench = Bench::new(workers, |_| {}, Vec::new())?;
    bench
        .step("draft", "draft", json!({ "models": [CLAUDE, CODEX, AGY] }))
        .await?;
    let done = bench
        .step("map", "map", json!({ "models": ["opencode/llama"] }))
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
    let mapping_agent = FakeRunner::cli("opencode/llama", "meta").respond_with(|spec| {
        let input: MapInput = input_of(spec).unwrap_or(MapInput { plans: Vec::new() });
        FakeSession::structured(map_json(&input.plans, true, false))
    });
    let mut workers = three(true, CLAUDE_WINS);
    workers.push(mapping_agent);
    let bench = Bench::new(workers, |_| {}, Vec::new())?;
    bench
        .step("draft", "draft", json!({ "models": [CLAUDE, CODEX, AGY] }))
        .await?;
    bench
        .step("map", "map", json!({ "models": ["opencode/llama"] }))
        .await?;
    assert_eq!(bench.specs_of("opencode/llama", StepName::Map).len(), 2);
    assert_eq!(bench.state().plan.unverified_topics(), ["counters"]);
    Ok(())
}

#[tokio::test]
async fn plan_critique_and_revision() -> anyhow::Result<()> {
    let bench = Bench::new(three(true, CLAUDE_WINS), |_| {}, Vec::new())?;
    bench.step("draft", "draft", json!({})).await?;
    bench.step("map", "map", json!({ "models": [AGY] })).await?;
    let done = bench.step("critique", "critic", json!({})).await?;
    assert_eq!(done.end, StepEnd::Done);

    let critiques: Vec<(ModelRef, bool)> = bench
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

    // Revision: each drafting agent resumes its own session.
    let done = bench.step("draft", "draft", json!({})).await?;
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
    bench.step("draft", "draft", json!({})).await?;
    bench.step("map", "map", json!({ "models": [AGY] })).await?;
    let critique = bench.step("critique", "critic", json!({})).await?;
    assert_eq!(critique.end, StepEnd::Skipped);
    let revise = bench.step("draft", "draft", json!({})).await?;
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
    bench.step("draft", "draft", json!({})).await?;
    bench.step("map", "map", json!({ "models": [AGY] })).await?;
    assert_eq!(
        bench.step("critique", "critic", json!({})).await?.end,
        StepEnd::Skipped
    );
    let done = bench.step("synthesize", "tie-breaker", json!({})).await?;
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
    // The tie-breaker merged once, and a judge of another provider checked it.
    let merges: Vec<SessionSpec> = bench
        .specs_of(CLAUDE, StepName::Draft)
        .into_iter()
        .filter(|spec| spec.role == RoleName::TieBreaker)
        .collect();
    assert_eq!(merges.len(), 1);
    let checks =
        bench.specs_of(CODEX, StepName::Judge).len() + bench.specs_of(AGY, StepName::Judge).len();
    assert_eq!(checks, 1);
    assert_eq!(state.plan.decisions.len(), 1);
    Ok(())
}

/// Drafts, a map by `mapping_agent` with the topic disputed, then the judge step.
async fn judged(bench: &Bench, mapping_agent: &str) -> anyhow::Result<StepDone> {
    bench.step("draft", "draft", json!({})).await?;
    bench
        .step("map", "map", json!({ "models": [mapping_agent] }))
        .await?;
    bench.step("judge", "judge", json!({})).await
}

#[tokio::test]
async fn plan_cross_judging_excludes_self_and_provider() -> anyhow::Result<()> {
    // Two drafting agents of one provider: neither may score the other's plan.
    let workers = vec![
        crew_worker(CLAUDE, "anthropic", "CLAUDE", "Redis", true, CLAUDE_WINS),
        crew_worker(CODEX, "openai", "CODEX", "memory", true, CLAUDE_WINS),
        crew_worker(
            "opencode/gpt-oss",
            "openai",
            "OTHER",
            "memory",
            true,
            CLAUDE_WINS,
        ),
    ];
    let bench = Bench::new(workers, |_| {}, Vec::new())?;
    judged(&bench, CLAUDE).await?;

    let events = bench.plan_events();
    let authors: Vec<(PlanLabel, ModelRef, u8)> = sealed_labels(&events);
    let provider = |w: &ModelRef| {
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
            judge_provider,
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
            judge_provider,
            provider(author),
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
async fn plan_near_tie_goes_to_approval() -> anyhow::Result<()> {
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
    assert_eq!(path.to_string_lossy(), "decisions/D-001-counters.md");
    let kept = argonauts_crew::divide::repo_state_dir(&bench.host).join(&path);
    assert!(std::fs::read_to_string(kept)?.starts_with("# D-001: counters"));
    let touched = bench.repo.git(&[
        "log",
        "--all",
        "--name-only",
        "--format=",
        "--",
        ".argonauts/decisions",
    ])?;
    assert!(touched.trim().is_empty(), "{touched}");

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
async fn reserved_judge_from_a_non_drafting_provider() -> anyhow::Result<()> {
    let reserved = |reserve: bool| async move {
        let mut workers = three(true, CLAUDE_WINS);
        // Another provider that drafts nothing, and a second gpt model that drafts nothing.
        workers.push(crew_worker(
            "opencode/llama",
            "meta",
            "OTHER",
            "Redis",
            true,
            CLAUDE_WINS,
        ));
        workers.push(crew_worker(
            "codex/gpt-mini",
            "openai",
            "OTHER",
            "Redis",
            true,
            CLAUDE_WINS,
        ));
        let bench = Bench::new(
            workers,
            |settings| settings.judging.reserve_non_drafting_judge = reserve,
            Vec::new(),
        )?;
        bench
            .step("draft", "draft", json!({ "models": [CLAUDE, CODEX, AGY] }))
            .await?;
        bench.step("map", "map", json!({ "models": [AGY] })).await?;
        bench.step("judge", "judge", json!({})).await?;
        let llama = bench.specs_of("opencode/llama", StepName::Judge).len();
        let mini = bench.specs_of("codex/gpt-mini", StepName::Judge).len();
        anyhow::Ok((llama, mini))
    };
    assert_eq!(
        reserved(true).await?,
        (2, 0),
        "the reserved judge is of the non-drafting provider"
    );
    assert_eq!(reserved(false).await?, (0, 0));
    Ok(())
}

#[tokio::test]
async fn plan_experiment_within_limit() -> anyhow::Result<()> {
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
    let main_agent = FakeRunner::cli(CLAUDE, "anthropic").respond_with(move |spec| {
        let value = match spec.step {
            Some(StepName::ProposeExperiment) => proposals.clone(),
            Some(StepName::ExperimentReport) => experiment_report(spec),
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
        main_agent,
        crew_worker(CODEX, "openai", "CODEX", "memory", true, CLAUDE_WINS),
        crew_worker(AGY, "google", "AGY", "Redis", true, CLAUDE_WINS),
    ];
    let bench = Bench::new(workers, |_| {}, vec![approved_goal()?])?;
    judged(&bench, AGY).await?;

    let events = bench.plan_events();
    let fits: Vec<bool> = events
        .iter()
        .filter_map(|e| match e {
            PlanEvent::ExperimentProposed { fits_limit, .. } => Some(*fits_limit),
            _ => None,
        })
        .collect();
    assert_eq!(fits, [true, false]);
    let finished: Vec<String> = events
        .iter()
        .filter_map(|e| match e {
            PlanEvent::ExperimentFinished { task, .. } => Some(task.to_string()),
            _ => None,
        })
        .collect();
    assert_eq!(finished.len(), 2);
    assert!(finished.contains(&"experiment-001".to_owned()));
    assert!(finished.contains(&"experiment-002".to_owned()));
    let experiments = bench.specs_of(CLAUDE, StepName::ExperimentReport);
    let experiments: Vec<_> = experiments
        .into_iter()
        .chain(bench.specs_of(CODEX, StepName::ExperimentReport))
        .chain(bench.specs_of(AGY, StepName::ExperimentReport))
        .collect();
    assert_eq!(experiments.len(), 2);
    assert!(
        experiments
            .iter()
            .all(|spec| spec.access == Access::Experiment)
    );
    for spec in bench.specs_of(CLAUDE, StepName::Judge) {
        let input: JudgeInput = input_of(&spec).expect("judge input");
        assert_eq!(
            input.experiments.len(),
            2,
            "judges see the experiment reports"
        );
    }

    // No experiment branch is merged anywhere.
    let merged = bench.sink.events().into_iter().any(|e| {
        matches!(
            &e.kind,
            RunEventKind::Merge(argonauts_core::events::task::MergeEvent::Merged {
                task, ..
            }) if task.as_str().starts_with("experiment-")
        )
    });
    assert!(!merged);
    // Whatever an experiment committed stays on its own branch.
    let branches = bench.repo.git(&[
        "branch",
        "--list",
        "argonauts/*",
        "--format=%(refname:short)",
    ])?;
    let targets: Vec<&str> = branches
        .lines()
        .filter(|b| !b.contains("experiment-"))
        .chain(["main"])
        .collect();
    let run_branch = "argonauts/run-01/merged";
    let base = if branches.lines().any(|b| b == run_branch) {
        run_branch
    } else {
        "main"
    };
    for experiment in branches.lines().filter(|b| b.contains("experiment-")) {
        let own = bench
            .repo
            .git(&["rev-list", &format!("{base}..{experiment}")])?;
        for commit in own.lines() {
            for target in &targets {
                let merged = bench
                    .repo
                    .git(&["merge-base", "--is-ancestor", commit, target])
                    .is_ok();
                assert!(!merged, "{experiment}'s {commit} reached {target}");
            }
        }
    }
    Ok(())
}

/// An experiment whose session ends on a 401 goes again on its own session, and after a second 401
/// another agent runs it, in autopilot, so both experiments still report.
#[tokio::test]
async fn a_401_on_an_experiment_goes_again_then_to_another_agent() -> anyhow::Result<()> {
    for failures in [1, 2] {
        let proposals = json!({ "proposals": [
            { "topic": "counters", "question": "Redis or memory?",
              "options": [{ "label": "Redis", "build": "a Redis counter" },
                          { "label": "memory", "build": "a map counter" }],
              "measure": "p50 latency", "decides_by": "lower p50",
              "est_minutes": 10.0, "est_cost_usd": 1.0 }
        ]});
        let main_agent = move |spec: &SessionSpec| {
            let value = match spec.step {
                Some(StepName::ProposeExperiment) => proposals.clone(),
                Some(StepName::ExperimentReport) => experiment_report(spec),
                Some(StepName::Draft) => plan_json("CLAUDE", "Redis"),
                Some(StepName::Judge) => match input_of::<JudgeInput>(spec) {
                    Some(input) => judge_json(&input, CLAUDE_WINS),
                    None => json!({ "scores": [] }),
                },
                _ => json!({}),
            };
            FakeSession::structured(value)
        };
        // The Redis option's experiment sessions fail, the first `failures` times whoever runs
        // them.
        let redis = |spec: &SessionSpec| prompt_text(spec).contains("a Redis counter");
        let failed = Arc::default();
        let experiment = StepName::ExperimentReport;
        let workers = vec![
            FakeRunner::cli(CLAUDE, "anthropic").respond_with(failing_first(
                experiment, failures, &failed, redis, main_agent,
            )),
            FakeRunner::cli(CODEX, "openai").respond_with(failing_first(
                experiment,
                failures,
                &failed,
                redis,
                answers("CODEX", "memory", true, CLAUDE_WINS),
            )),
            FakeRunner::cli(AGY, "google").respond_with(failing_first(
                experiment,
                failures,
                &failed,
                redis,
                answers("AGY", "Redis", true, CLAUDE_WINS),
            )),
        ];
        let bench = Bench::new(
            workers,
            |settings| settings.fallback.retry = Some(Retry::without_wait()),
            vec![approved_goal()?, autopilot()],
        )?;
        judged(&bench, AGY).await?;

        let finished: Vec<String> = bench
            .plan_events()
            .iter()
            .filter_map(|e| match e {
                PlanEvent::ExperimentFinished { report, .. } => Some(report.option.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(finished.len(), 2, "after {failures} 401s: {finished:?}");
        assert!(finished.contains(&"Redis".to_owned()), "{finished:?}");
        let redis_experiments: Vec<(String, String)> = bench
            .sink
            .events()
            .iter()
            .filter_map(|event| match &event.kind {
                RunEventKind::Session(SessionEvent::Started(started))
                    if started.step == Some(StepName::ExperimentReport) =>
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
            redis_experiments.iter().map(|(key, _)| key).collect();
        assert_eq!(keys.len(), 2, "{redis_experiments:?}");
        let workers: std::collections::BTreeSet<&String> =
            redis_experiments.iter().map(|(_, worker)| worker).collect();
        if failures == 2 {
            assert!(
                workers.len() >= 2,
                "another agent ran it: {redis_experiments:?}"
            );
        }
    }
    Ok(())
}

/// A judge's place goes only to an agent from outside the providers of the plans it's shown,
/// and the fallback's scores go under its own name and provider. With none, the judge's scores
/// are dropped rather than given by a plan's own provider.
#[tokio::test]
async fn a_judges_fallback_comes_from_outside_the_providers_it_judges() -> anyhow::Result<()> {
    const GPT_OSS: &str = "opencode/gpt-oss";
    const QWEN: &str = "opencode/qwen3-coder";
    for with_qwen in [false, true] {
        let claude = FakeRunner::cli(CLAUDE, "anthropic").respond_with(failing_first(
            StepName::Judge,
            usize::MAX,
            &Arc::default(),
            |_| true,
            answers("CLAUDE", "Redis", false, CLAUDE_WINS),
        ));
        let mut workers = vec![
            claude,
            crew_worker(CODEX, "openai", "CODEX", "memory", false, CLAUDE_WINS),
            crew_worker(AGY, "google", "AGY", "Redis", false, CLAUDE_WINS),
            crew_worker(GPT_OSS, "openai", "OSS", "memory", false, CLAUDE_WINS),
        ];
        if with_qwen {
            workers.push(crew_worker(
                QWEN,
                "alibaba",
                "QWEN",
                "memory",
                false,
                CLAUDE_WINS,
            ));
        }
        let drafting_agents: Vec<ModelRef> = [CLAUDE, CODEX, AGY]
            .iter()
            .map(|worker| worker.parse())
            .collect::<Result<_, _>>()?;
        let bench = Bench::new(
            workers,
            move |settings| {
                settings.fallback.retry = Some(Retry::without_wait());
                settings.roles.entry(RoleName::Draft).or_default().models = drafting_agents;
            },
            vec![approved_goal()?, autopilot()],
        )?;
        // A judge is blind, so its fallback's CLI must keep your plugins out, as OpenCode's
        // does here.
        bench.services.workers.isolating("opencode");
        judged(&bench, AGY).await?;

        let scored: Vec<(String, String)> = bench
            .plan_events()
            .iter()
            .filter_map(|e| match e {
                PlanEvent::Scored {
                    judge,
                    judge_provider,
                    ..
                } => Some((judge.to_string(), judge_provider.clone())),
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
                scored.contains(&(QWEN.to_owned(), "alibaba".to_owned())),
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

/// A drafting agent's place goes to an agent that isn't drafting already, and the plan it seals
/// goes under its own name, in the drafting agent's place.
#[tokio::test]
async fn a_drafting_agents_fallback_seals_the_plan_under_its_own_name() -> anyhow::Result<()> {
    const QWEN: &str = "opencode/qwen3-coder";
    let claude = FakeRunner::cli(CLAUDE, "anthropic").respond_with(failing_first(
        StepName::Draft,
        usize::MAX,
        &Arc::default(),
        |_| true,
        answers("CLAUDE", "Redis", false, CLAUDE_WINS),
    ));
    let workers = vec![
        claude,
        crew_worker(CODEX, "openai", "CODEX", "memory", false, CLAUDE_WINS),
        crew_worker(AGY, "google", "AGY", "Redis", false, CLAUDE_WINS),
        crew_worker(QWEN, "alibaba", "QWEN", "memory", false, CLAUDE_WINS),
    ];
    let drafting_agents: Vec<ModelRef> = [CLAUDE, CODEX, AGY]
        .iter()
        .map(|worker| worker.parse())
        .collect::<Result<_, _>>()?;
    let bench = Bench::new(
        workers,
        move |settings| {
            settings.fallback.retry = Some(Retry::without_wait());
            settings.roles.entry(RoleName::Draft).or_default().models = drafting_agents;
        },
        vec![approved_goal()?, autopilot()],
    )?;
    // Drafts are blind, so the fallback's CLI must keep your plugins out, as OpenCode's does
    // here.
    bench.services.workers.isolating("opencode");
    bench.step("draft", "draft", json!({})).await?;

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
    // Codex and agy draft once each, as themselves.
    assert_eq!(bench.specs_of(CODEX, StepName::Draft).len(), 1);
    assert_eq!(bench.specs_of(AGY, StepName::Draft).len(), 1);
    assert_eq!(bench.specs_of(QWEN, StepName::Draft).len(), 1);
    Ok(())
}

const QWEN: &str = "opencode/qwen3-coder";
const GPT_OSS: &str = "opencode/gpt-oss";
/// A judge that scores GPT OSS's draft over Qwen's.
const OSS_WINS: &[(&str, f64)] = &[("OSS", 9.0), ("QWEN", 4.0)];

/// Claude, Codex and agy draft, with Qwen and GPT OSS as the fallbacks that may take a
/// drafting agent's place, two at once. Claude answers its draft sessions as `claude` says.
fn bench_with_two_fallbacks(
    claude: impl Fn(&SessionSpec) -> FakeSession + Send + Sync + 'static,
) -> anyhow::Result<Bench> {
    let workers = vec![
        FakeRunner::cli(CLAUDE, "anthropic").respond_with(claude),
        crew_worker(CODEX, "openai", "CODEX", "memory", false, OSS_WINS),
        crew_worker(AGY, "google", "AGY", "Redis", false, OSS_WINS),
        crew_worker(QWEN, "alibaba", "QWEN", "memory", false, OSS_WINS),
        crew_worker(GPT_OSS, "openai", "OSS", "Redis", false, OSS_WINS),
    ];
    let drafting_agents: Vec<ModelRef> = [CLAUDE, CODEX, AGY]
        .iter()
        .map(|worker| worker.parse())
        .collect::<Result<_, _>>()?;
    let bench = Bench::new(
        workers,
        move |settings| {
            settings.fallback.retry = Some(Retry::without_wait());
            settings.roles.entry(RoleName::Draft).or_default().models = drafting_agents;
            // The drafting agents follow the workers' fallback table.
            let table = settings.fallback.roles.entry(CrewRole::Worker).or_default();
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

/// With `together = 2`, a drafting agent that's out as its draft starts has two fallbacks draft in
/// its place at once. A judge from another provider than both picks one, numbered and
/// unnamed, and that draft is the drafting agent's plan.
#[tokio::test]
async fn two_fallbacks_draft_at_once_and_a_judge_picks_the_drafting_agents_plan()
-> anyhow::Result<()> {
    let bench = bench_with_two_fallbacks(answers("CLAUDE", "Redis", false, OSS_WINS))?;
    let claude_code = argonauts_core::names::AgentId("claude-code".to_owned());
    bench
        .services
        .health
        .set(&claude_code, HealthState::Down, Some("server errors"));
    bench.step("draft", "draft", json!({})).await?;

    let (sealed, said) = sealed_and_said(&bench);
    assert_eq!(sealed.len(), 3, "one plan a drafting agent: {sealed:?}");
    assert!(sealed.contains(&GPT_OSS.to_owned()), "{sealed:?} {said:?}");
    assert!(!sealed.contains(&QWEN.to_owned()), "{sealed:?}");
    assert!(bench.specs_of(CLAUDE, StepName::Draft).is_empty());
    for drafted_once in [CODEX, AGY, QWEN, GPT_OSS] {
        assert_eq!(bench.specs_of(drafted_once, StepName::Draft).len(), 1);
    }
    // agy judges: Codex shares a provider with GPT OSS.
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

/// A drafting agent that stops midway hands its place to one fallback as ever, and with
/// `together = 2` a second joins it, so the judge still picks between two drafts.
#[tokio::test]
async fn a_second_fallback_joins_the_one_that_took_a_drafting_agents_place_midway()
-> anyhow::Result<()> {
    let bench = bench_with_two_fallbacks(failing_first(
        StepName::Draft,
        usize::MAX,
        &Arc::default(),
        |_| true,
        answers("CLAUDE", "Redis", false, OSS_WINS),
    ))?;
    bench.step("draft", "draft", json!({})).await?;

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
        let agent_id = argonauts_core::names::AgentId(out.to_owned());
        bench
            .services
            .health
            .set(&agent_id, HealthState::Down, Some("server errors"));
    }
    bench.step("draft", "draft", json!({})).await?;

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
async fn tie_breaker_rotates_by_phase() -> anyhow::Result<()> {
    use argonauts_core::events::task::{TaskKind, TaskLimit, TaskSpec, TaskState};
    use tie_breaker::TaskView;

    let spec = |id: &str, kind: TaskKind, deps: &[&str]| TaskSpec {
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
        worker: None,
        reviewers: Vec::new(),
        limit: TaskLimit::default(),
        predictions: None,
        item: None,
    };
    let specs = [
        spec("task-001", TaskKind::Interface, &[]),
        spec("task-002", TaskKind::Build, &["task-001"]),
        spec("task-003", TaskKind::Build, &["task-002"]),
        spec("task-004", TaskKind::Build, &["task-001"]),
        spec("task-005", TaskKind::Chore, &[]),
    ];
    let workers: Vec<ModelRef> = [CLAUDE, CODEX, AGY, CLAUDE, AGY]
        .iter()
        .map(|w| w.parse())
        .collect::<Result<_, _>>()?;
    let states = [
        TaskState::Merged,
        TaskState::Running,
        TaskState::Waiting,
        TaskState::Running,
        TaskState::Waiting,
    ];
    let tasks: Vec<TaskView<'_>> = specs
        .iter()
        .zip(&workers)
        .zip(states)
        .map(|((spec, worker), state)| TaskView {
            spec,
            state,
            worker: Some(worker),
        })
        .collect();
    let default: ModelRef = CLAUDE.parse()?;
    let tie_breaker_in = |phase| tie_breaker::for_phase(phase, &tasks, None, &default).to_string();
    assert_eq!(tie_breaker_in(RunPhase::Understand), CLAUDE);
    assert_eq!(tie_breaker_in(RunPhase::Planning), CLAUDE);
    // The critical path is task-001, task-002, task-003; task-002 is the first not in yet.
    assert_eq!(tie_breaker_in(RunPhase::Build), CODEX);
    assert_eq!(tie_breaker_in(RunPhase::FinalCheck), AGY);
    assert_eq!(tie_breaker_in(RunPhase::Merge), AGY);
    Ok(())
}

#[tokio::test]
async fn planning_phase_names_its_main_agent() -> anyhow::Result<()> {
    let bench = Bench::new(three(true, CLAUDE_WINS), |_| {}, Vec::new())?;
    bench.step("draft", "draft", json!({})).await?;
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

fn orchestrator_output(api_reviewers: &[&str]) -> Value {
    json!({
        "plan": plan_json("ORCH", "Redis"),
        "tasks": [
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
            { "task": "store", "worker": CLAUDE, "reviewers": [CODEX, AGY] },
            { "task": "api", "worker": CODEX, "reviewers": api_reviewers },
            { "task": "contract", "worker": AGY, "reviewers": [CLAUDE, CODEX] }
        ]
    })
}

fn orchestrator(answers: Vec<Value>) -> FakeRunner {
    let answers = std::sync::Mutex::new(answers.into_iter());
    FakeRunner::cli(CLAUDE, "anthropic").respond_with(move |_| {
        let mut answers = answers
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let value = answers.next().unwrap_or_else(|| json!({}));
        FakeSession::structured(value).with_cli_session("orchestrator-session")
    })
}

fn orchestrated_bench(answers: Vec<Value>) -> anyhow::Result<Bench> {
    Bench::new(
        vec![
            orchestrator(answers),
            FakeRunner::cli(CODEX, "openai"),
            FakeRunner::cli(AGY, "google"),
            FakeRunner::cli("opencode/gpt-oss", "openai"),
        ],
        |settings| {
            settings.review.per_task = 2;
            settings.divide.predictions = false;
        },
        Vec::new(),
    )
}

#[tokio::test]
async fn orchestrator_assigns_tasks_in_its_answer() -> anyhow::Result<()> {
    let bench = orchestrated_bench(vec![orchestrator_output(&[CLAUDE, AGY])])?;
    let done = bench
        .step("draft", "orchestrator", json!({ "models": [CLAUDE] }))
        .await?;
    assert!(done.summary.unwrap_or_default().contains("3 tasks"));

    let events = bench.plan_events();
    assert!(events.iter().any(|e| matches!(
        e,
        PlanEvent::Orchestrated {
            rejection: None,
            ..
        }
    )));
    let state = bench.state();
    let chosen = state.plan.chosen().expect("the orchestrator's plan");
    assert_eq!(chosen.worker.to_string(), CLAUDE);
    let tasks = orchestrated::load_tasks(chosen)?;
    assert_eq!(tasks.tasks.len(), 3);
    assert_eq!(tasks.assignments[1].worker, CODEX);

    let spec = &bench.specs_of(CLAUDE, StepName::Orchestrate)[0];
    let input: OrchestrateInput = input_of(spec).expect("orchestrate input");
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

    // Dividing the work takes the orchestrator's tasks, workers, and reviewers as they are.
    let (step, opened) = bench.plan_approval().await?;
    let graph = &plan_packet(&opened)?.graph;
    let owned = |id: &str| -> anyhow::Result<(String, Vec<String>)> {
        let task = graph
            .tasks
            .iter()
            .find(|task| task.id.as_str() == id)
            .ok_or_else(|| anyhow::anyhow!("no task {id}"))?;
        Ok((
            task.worker
                .as_ref()
                .map(ToString::to_string)
                .unwrap_or_default(),
            task.reviewers.iter().map(ToString::to_string).collect(),
        ))
    };
    assert_eq!(graph.tasks.len(), 5, "three tasks and two test tasks");
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
        .tasks
        .iter()
        .filter_map(|task| match &task.kind {
            TaskKind::AcceptanceTests { for_task } => Some((
                for_task.to_string(),
                task.worker
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
            DivideEvent::Worker { algorithm, .. } => Some(algorithm),
            _ => None,
        })
        .collect();
    assert_eq!(algorithms, [RoutingAlgorithm::Orchestrator; 5]);
    for worker in [CLAUDE, CODEX, AGY] {
        assert!(
            bench.specs_of(worker, StepName::PlanToTasks).is_empty(),
            "nobody turns the orchestrator's plan into tasks again"
        );
    }
    bench.answer(&opened, AnswerAction::Abandon, None)?;
    step.await??;
    Ok(())
}

/// "Codex orchestrates and Gemini reviews": the settings the request layer sets for it (the
/// app's tests resolve the same keys from those words). Codex alone orchestrates, agy alone
/// reviews, and a plan that gives the review to anyone else has it moved to agy by code,
/// with one note, instead of going back to Codex.
#[tokio::test]
async fn codex_orchestrates_and_gemini_reviews_with_exactly_those_roles() -> anyhow::Result<()> {
    use argonauts_core::settings::protocol::{JudgingMethod, PlanningBy, ReviewAssignment};

    let output = |reviewer: &str| {
        json!({
            "plan": plan_json("CODEX", "Redis"),
            "tasks": [
                { "key": "store", "title": "Counter store", "task": "Build the store.",
                  "kind": "build", "owned_paths": ["src/store/"], "read_only_paths": [],
                  "depends_on": [], "criteria": ["G1"] },
                { "key": "api", "title": "Middleware", "task": "Add the middleware.",
                  "kind": "build", "owned_paths": ["src/api.rs"], "read_only_paths": [],
                  "depends_on": ["store"], "criteria": ["G1"] }
            ],
            "assignments": [
                { "task": "store", "worker": CLAUDE, "reviewers": [reviewer] },
                { "task": "api", "worker": CODEX, "reviewers": [AGY] }
            ]
        })
    };
    let answers = std::sync::Mutex::new(vec![output(CODEX), output(AGY)].into_iter());
    let orchestrator = FakeRunner::cli(CODEX, "openai").respond_with(move |_| {
        let value = answers
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .next()
            .unwrap_or_else(|| json!({}));
        FakeSession::structured(value).with_cli_session("codex-orchestrator")
    });
    let bench = Bench::new(
        vec![
            FakeRunner::cli(CLAUDE, "anthropic"),
            orchestrator,
            FakeRunner::cli(AGY, "google"),
        ],
        |settings| {
            let worker = |text: &str| text.parse::<ModelRef>().unwrap();
            settings.crew.main = worker(CODEX);
            settings.workflow = "orchestrator".to_owned();
            settings.drafting.by = PlanningBy::Orchestrator;
            settings.judging.method = JudgingMethod::Orchestrator;
            settings.routing.algorithm = RoutingAlgorithm::Orchestrator;
            settings.review.assign = ReviewAssignment::Orchestrator;
            settings.review.per_task = 1;
            settings.divide.contracts_first = false;
            settings.divide.predictions = false;
            let roles = &mut settings.roles;
            roles.entry(RoleName::Orchestrator).or_default().models = vec![worker(CODEX)];
            roles.entry(RoleName::Reviewer).or_default().models = vec![worker(AGY)];
            roles.entry(RoleName::Worker).or_default().models = vec![worker(CLAUDE), worker(CODEX)];
        },
        Vec::new(),
    )?;
    bench.step("draft", "orchestrator", json!({})).await?;

    assert_eq!(bench.specs_of(CODEX, StepName::Orchestrate).len(), 1);
    let input: OrchestrateInput =
        input_of(&bench.specs_of(CODEX, StepName::Orchestrate)[0]).expect("orchestrate input");
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
            bench.specs_of(other, StepName::Orchestrate).is_empty(),
            "{other}"
        );
    }
    let rejections: Vec<Option<String>> = bench
        .plan_events()
        .into_iter()
        .filter_map(|e| match e {
            PlanEvent::Orchestrated { rejection, .. } => Some(rejection),
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
             allow.\n`store`: Gemini 3 on agy reviews it, not GPT 5 on Codex, since \
             only Gemini 3 on agy may review tasks."
        ),
        "{moved:?}"
    );

    let (step, opened) = bench.plan_approval().await?;
    let graph = &plan_packet(&opened)?.graph;
    assert_eq!(graph.tasks.len(), 2);
    for task in &graph.tasks {
        let reviewers: Vec<String> = task.reviewers.iter().map(ToString::to_string).collect();
        assert_eq!(reviewers, [AGY], "{}", task.id);
        let worker = task
            .worker
            .as_ref()
            .map(ToString::to_string)
            .unwrap_or_default();
        assert!([CLAUDE, CODEX].contains(&worker.as_str()), "{worker}");
    }
    bench.answer(&opened, AnswerAction::Abandon, None)?;
    step.await??;
    Ok(())
}

/// The sample repo's plan with contracts first, as its workflow has it: the orchestrator wrote
/// providers where models go and rotated every agent through every role. Code moves each task
/// to a worker and a reviewer the rules allow, the reviewer writes each build task's tests, and
/// those tests are reviewed by a builder other than the tested task's worker, since the only
/// reviewer allowed wrote them. Supervised, the plan approval still asks you.
#[tokio::test]
async fn the_trial_plan_with_contracts_first_keeps_every_task_reviewed() -> anyhow::Result<()> {
    use argonauts_core::settings::protocol::{JudgingMethod, PlanningBy, ReviewAssignment};

    let task = |key: &str, kind: &str, path: &str| {
        json!({ "key": key, "title": key, "task": format!("Write {path}."), "kind": kind,
                "owned_paths": [path], "read_only_paths": [], "depends_on": [],
                "criteria": ["G1"] })
    };
    let output = json!({
        "plan": plan_json("CODEX", "Redis"),
        "tasks": [
            task("iface", "interface", "src/types.js"),
            task("impl", "build", "src/store.js"),
            task("cli", "build", "src/cli.js"),
        ],
        "assignments": [
            { "task": "iface", "worker": "codex/gpt", "reviewers": ["claude-code/claude"] },
            { "task": "impl", "worker": "claude-code/claude", "reviewers": ["antigravity-cli/gemini"] },
            { "task": "cli", "worker": "antigravity-cli/gemini", "reviewers": ["codex/gpt"] }
        ]
    });
    let orchestrator = FakeRunner::cli(CODEX, "openai").respond_with(move |_| {
        FakeSession::structured(output.clone()).with_cli_session("codex-orchestrator")
    });
    let bench = Bench::new(
        vec![
            FakeRunner::cli(CLAUDE, "anthropic"),
            orchestrator,
            FakeRunner::cli(AGY, "google"),
        ],
        |settings| {
            let worker = |text: &str| text.parse::<ModelRef>().unwrap();
            settings.crew.main = worker(CODEX);
            settings.workflow = "orchestrator".to_owned();
            settings.drafting.by = PlanningBy::Orchestrator;
            settings.judging.method = JudgingMethod::Orchestrator;
            settings.routing.algorithm = RoutingAlgorithm::Orchestrator;
            settings.review.assign = ReviewAssignment::Orchestrator;
            settings.review.per_task = 1;
            settings.divide.contracts_first = true;
            settings.divide.predictions = false;
            let roles = &mut settings.roles;
            roles.entry(RoleName::Orchestrator).or_default().models = vec![worker(CODEX)];
            roles.entry(RoleName::Reviewer).or_default().models = vec![worker(AGY)];
            roles.entry(RoleName::Worker).or_default().models = vec![worker(CLAUDE), worker(CODEX)];
        },
        Vec::new(),
    )?;
    bench.step("draft", "orchestrator", json!({})).await?;
    assert_eq!(bench.specs_of(CODEX, StepName::Orchestrate).len(), 1);

    let (step, opened) = bench.plan_approval().await?;
    let packet = plan_packet(&opened)?;
    let name = |model: Option<&ModelRef>| model.map(ToString::to_string).unwrap_or_default();
    let worker_of = |id: &argonauts_core::TaskId| {
        packet
            .graph
            .tasks
            .iter()
            .find(|task| &task.id == id)
            .and_then(|task| task.worker.clone())
    };
    assert_eq!(packet.graph.tasks.len(), 5, "{:#?}", packet.graph.tasks);
    for task in &packet.graph.tasks {
        let worker = name(task.worker.as_ref());
        let reviewers: Vec<String> = task.reviewers.iter().map(ToString::to_string).collect();
        match &task.kind {
            TaskKind::AcceptanceTests { for_task } => {
                assert_eq!(worker, AGY, "{}", task.id);
                let tested = name(worker_of(for_task).as_ref());
                assert_eq!(reviewers.len(), 1, "{}", task.id);
                assert!(
                    [CLAUDE, CODEX].contains(&reviewers[0].as_str()) && reviewers[0] != tested,
                    "{}: {reviewers:?}, tested {tested}",
                    task.id
                );
            }
            _ => {
                assert!(
                    [CLAUDE, CODEX].contains(&worker.as_str()),
                    "{}: {worker}",
                    task.id
                );
                assert_eq!(reviewers, [AGY], "{}", task.id);
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
async fn orchestrator_output_rejected_and_retried() -> anyhow::Result<()> {
    // Tasks that break the rules go back to the orchestrator, whoever they went to.
    let overlapping = || {
        let mut output = orchestrator_output(&[CLAUDE, AGY]);
        output["tasks"][1]["owned_paths"] = json!(["src/store/redis.rs"]);
        output
    };
    let dangling = || {
        let mut output = orchestrator_output(&[CLAUDE, AGY]);
        output["tasks"][1]["depends_on"] = json!(["ghost"]);
        output
    };
    // First answer: two tasks own one path. Second answer keeps the rules.
    let bench = orchestrated_bench(vec![overlapping(), orchestrator_output(&[CLAUDE, AGY])])?;
    bench
        .step("draft", "orchestrator", json!({ "models": [CLAUDE] }))
        .await?;
    let rejections: Vec<Option<String>> = bench
        .plan_events()
        .into_iter()
        .filter_map(|e| match e {
            PlanEvent::Orchestrated { rejection, .. } => Some(rejection),
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
    let specs = bench.specs_of(CLAUDE, StepName::Orchestrate);
    assert_eq!(specs.len(), 2);
    let retry: OrchestrateInput = input_of(&specs[1]).expect("orchestrate input");
    assert!(retry.rejection.unwrap_or_default().contains("both own"));
    assert!(
        specs[1].resume.is_some(),
        "the retry continues the orchestrator's session"
    );

    // Broken twice: the plan approval asks you, with nothing to approve.
    let bench = orchestrated_bench(vec![
        overlapping(),
        dangling(),
        orchestrator_output(&[CLAUDE, AGY]),
    ])?;
    let done = bench
        .step("draft", "orchestrator", json!({ "models": [CLAUDE] }))
        .await?;
    assert!(done.summary.unwrap_or_default().contains("asks you"));
    let state = bench.state();
    assert_eq!(state.plan.orchestrator_rejections().len(), 2);
    assert_eq!(state.plan.chosen(), None);

    let (step, opened) = bench.plan_approval().await?;
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

    // Sent back with a note, the orchestrator gets two new tries and reads the note.
    bench.answer(
        &opened,
        AnswerAction::SendBack,
        Some("Keep api out of gpt's hands."),
    )?;
    step.await??;
    let done = bench
        .step("draft", "orchestrator", json!({ "models": [CLAUDE] }))
        .await?;
    assert!(done.summary.unwrap_or_default().contains("3 tasks"));
    let specs = bench.specs_of(CLAUDE, StepName::Orchestrate);
    assert_eq!(specs.len(), 3);
    let again: OrchestrateInput = input_of(&specs[2]).expect("orchestrate input");
    assert!(
        again
            .rejection
            .unwrap_or_default()
            .contains("Keep api out of gpt's hands.")
    );
    assert!(bench.state().plan.chosen().is_some());
    Ok(())
}

/// Planning as a crew workflow runs it: drafts, a map, judging, and the plan approval, until
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
            role: draft
            blind: true
            input: goal
          - kind: map
            name: map
            role: map
            input: drafts
          - kind: judge
            name: scores
            role: judge
            blind: true
            input: drafts
          - kind: approval
            approval: plan
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

fn plan_approvals_of(bench: &Bench) -> Vec<ApprovalOpened> {
    bench
        .sink
        .events()
        .into_iter()
        .filter_map(|event| match event.kind {
            RunEventKind::Approval(ApprovalEvent::Opened(opened))
                if opened.kind == ApprovalKind::Plan =>
            {
                Some(*opened)
            }
            _ => None,
        })
        .collect()
}

/// Waits for the plan question numbered `n` (from 1), the pick question included, and answers it.
async fn answer_plan(
    bench: &Bench,
    running: &mut Running,
    n: usize,
    action: impl FnOnce(&ApprovalOpened) -> AnswerAction,
    text: Option<&str>,
) -> anyhow::Result<ApprovalOpened> {
    let opened = tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            if let Some(opened) = plan_approvals_of(bench).into_iter().nth(n - 1) {
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
    approvals::answer(
        &bench.host,
        ApprovalAnswer {
            id: opened.id.clone(),
            action: action(&opened),
            text: text.map(str::to_owned),
            evidence_sha256: opened.evidence_sha256.clone(),
            tasks: Vec::new(),
        },
    )
    .map_err(anyhow::Error::msg)?;
    Ok(opened)
}

fn packet(opened: &ApprovalOpened) -> anyhow::Result<&PlanPacket> {
    match &opened.packet {
        ApprovalPacket::Plan(packet) => Ok(packet),
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
            // Not the top plan, so the pick is yours and not the scores'.
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

    let approval = answer_plan(&bench, &mut running, 2, |_| AnswerAction::Approve, None).await?;
    assert_eq!(approval.title, "Approve this plan?");
    assert_eq!(running.await??, Flow::Continue);

    let state = bench.state();
    let chosen = state.plan.chosen().expect("your pick");
    assert_eq!(Some(format!("Plan {}", chosen.label)), picked);
    let judging = packet(&approval)?.judging.clone().expect("judged");
    assert_eq!(judging.winner, Some(chosen.label));
    assert!(judging.near_tie);
    assert!(
        bench
            .plan_events()
            .iter()
            .any(|event| matches!(event, PlanEvent::Decided(_)))
    );
    assert_eq!(state.tasks.by_id.len(), 3);
    Ok(())
}

const EVEN: &[(&str, f64)] = &[("CLAUDE", 7.0), ("CODEX", 7.2), ("AGY", 6.9)];

/// In auto the main agent answers the crew's questions, so a near tie goes to the plan the judges
/// scored highest, and only the plan approval after it remains.
#[tokio::test]
async fn in_auto_a_near_tie_goes_to_the_top_scored_plan() -> anyhow::Result<()> {
    let bench = Bench::new(three(true, EVEN), |_| {}, vec![approved_goal()?])?;
    let mut running = run_planning(&bench)?;
    let approval = answer_plan(&bench, &mut running, 1, |_| AnswerAction::Approve, None).await?;
    assert_eq!(approval.title, "Approve this plan?");
    assert_eq!(running.await??, Flow::Continue);

    let state = bench.state();
    let chosen = state.plan.chosen().expect("the top-scored plan");
    assert_eq!(chosen.worker, CODEX.parse()?);
    let judging = packet(&approval)?.judging.clone().expect("judged");
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
    use argonauts_core::policy::{AutoPreviewRecord, PolicyOutcome, Verdict};
    use argonauts_core::settings::approvals::DecideWith;

    let bench = Bench::new(three(true, CLAUDE_WINS), |_| {}, vec![approved_goal()?])?;
    bench.services.policy.decide_next(PolicyOutcome::AskUser {
        violations: Vec::new(),
        auto_preview: Some(AutoPreviewRecord {
            would: Verdict::Reject,
            by: DecideWith::RulesAndModel,
            reasons: vec!["two tasks edit the same file".to_owned()],
        }),
    });
    let mut running = run_planning(&bench)?;
    // The second round's plan is one nothing weighed, so that approval still waits.
    answer_plan(&bench, &mut running, 2, |_| AnswerAction::Approve, None).await?;
    assert_eq!(running.await??, Flow::Continue);

    let first = plan_approvals_of(&bench).remove(0);
    let decided = bench
        .sink
        .events()
        .into_iter()
        .find_map(|event| match event.kind {
            RunEventKind::Approval(ApprovalEvent::Decided(decided)) if decided.id == first.id => {
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

    // The second round's drafting agents, and the second division, get your note.
    let drafts = bench.specs_of(CODEX, StepName::Draft);
    assert_eq!(drafts.len(), 2);
    assert!(!prompt_text(&drafts[0]).contains(note));
    assert!(prompt_text(&drafts[1]).contains(note));
    let divided: Vec<SessionSpec> = [CLAUDE, CODEX, AGY]
        .iter()
        .flat_map(|worker| bench.specs_of(worker, StepName::PlanToTasks))
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
    assert_eq!(state.tasks.by_id.len(), 3);
    Ok(())
}
