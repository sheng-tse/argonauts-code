//! What happens when a council's job can't start, in order: a model its CLI no longer lists
//! runs the nearest one it does, a refused model gives way to its CLI's other models, then
//! another CLI that can start takes the job, then a job the council can do without is left
//! out, then `fallback.same_step` may put another model of a CLI already in the step on it,
//! and only when nothing can does the run stop, fully, until you carry it on.

use crate::support;
use std::sync::{Arc, Mutex};

use serde_json::json;

use anyhow::{Context, bail};
use argonauts_core::agents::session_runner::SessionSpec;
use argonauts_core::base::names::ModelRef;
use argonauts_core::models::registry::ModelEntry;
use argonauts_core::runs::engine::EngineExit;
use argonauts_core::runs::events::approval::{
    AnswerAction, ApprovalEvent, ApprovalKind, ApprovalPacket,
};
use argonauts_core::runs::events::run::{RunMode, WaitReason};
use argonauts_core::runs::events::session::{EndReason, SessionEvent};
use argonauts_core::runs::events::workflow::{AgentLeftOut, WorkflowEvent};
use argonauts_core::runs::events::{RunEvent, RunEventKind};
use argonauts_core::settings::crew::{CrewSettings, Orchestrator};
use argonauts_core::settings::fallback::SameStep;
use argonauts_core::settings::workflows::AdaptWith;
use argonauts_core::{ErrorCategory, NoteLevel};
use argonauts_testkit::engine::runner::{FakeRunner, FakeSession};

use support::Crew;

const REQUEST: &str = "Add a per key rate limit";
const CLAUDE: &str = "claude-code/haiku";
const CODEX: &str = "codex/gpt-6-luna";
const AGY: &str = "antigravity-cli";

/// Three proposers side by side, each proposal that came in challenged by its maker, then the
/// judge picks one: a council's first steps.
const COUNCIL: &str = r#"export const meta = {
  name: 'council',
  description: 'Everyone proposes, the judge decides',
  phases: [{ name: 'Propose' }, { name: 'Challenge' }, { name: 'Decide' }],
}
const asked = [0, 1, 2].map((n) => () => agent('Propose how to do it: ' + args.request, {
  role: 'worker', access: 'read', model: n, label: 'propose:p' + (n + 1), phase: 'Propose',
}))
const answered = await parallel(asked)
const proposals = answered.filter(Boolean)
// Each proposal that came in is challenged by the model that made it.
const made = answered.map((proposal, n) => (proposal ? n : null)).filter((n) => n !== null)
await parallel(made.map((n) => () => agent('Argue against the other proposals.', {
  role: 'worker', access: 'read', model: n, label: 'challenge:p' + (n + 1), phase: 'Challenge',
})))
const picked = await agent('Pick the strongest of ' + proposals.length + ' proposals.', {
  role: 'judge', label: 'decide', phase: 'Decide',
})
return { proposals: proposals.length, picked }
"#;

/// A model agy lists today, by agy's own name for it.
fn listed(model: &str, name: &str) -> ModelEntry {
    let mut entry: ModelEntry = serde_json::from_value(json!({
        "id": format!("{AGY}/{model}"),
        "provider": "google",
        "kind": "cli",
        "access": "subscription",
        "cost": { "unit": "plan_usage" },
    }))
    .expect("a registry entry");
    entry.discovered = true;
    entry.name = Some(name.to_owned());
    entry
}

/// The models each session of `label` ran, in the order they started.
fn ran_as(events: &[RunEvent], label: &str) -> Vec<ModelRef> {
    events
        .iter()
        .filter_map(|event| match &event.kind {
            RunEventKind::Session(SessionEvent::Started(started))
                if started.label.as_deref() == Some(label) =>
            {
                Some(started.worker.clone())
            }
            _ => None,
        })
        .collect()
}

fn notes(events: &[RunEvent]) -> Vec<String> {
    events
        .iter()
        .filter_map(|event| match &event.kind {
            RunEventKind::Note(note) if note.level != NoteLevel::Quiet => Some(note.text.clone()),
            _ => None,
        })
        .collect()
}

fn agy(model: &str) -> ModelRef {
    ModelRef::new(AGY, Some(model.to_owned()))
}

/// A council of Claude Code, Codex and agy, agy's model pinned to `pinned`, and what agy lists
/// today. `agy` answers each session agy starts, by the model it runs.
fn council(
    pinned: &str,
    agy_answers: impl Fn(&SessionSpec) -> FakeSession + Send + Sync + 'static,
) -> anyhow::Result<Crew> {
    council_with(pinned, support::smallest_answer, agy_answers)
}

/// [`council`], with Codex answering as `codex_answers` says.
fn council_with(
    pinned: &str,
    codex_answers: impl Fn(&SessionSpec) -> FakeSession + Send + Sync + 'static,
    agy_answers: impl Fn(&SessionSpec) -> FakeSession + Send + Sync + 'static,
) -> anyhow::Result<Crew> {
    scripted(COUNCIL, pinned, codex_answers, agy_answers)
}

/// [`council_with`], running `script` as the council's workflow.
fn scripted(
    script: &'static str,
    pinned: &str,
    codex_answers: impl Fn(&SessionSpec) -> FakeSession + Send + Sync + 'static,
    agy_answers: impl Fn(&SessionSpec) -> FakeSession + Send + Sync + 'static,
) -> anyhow::Result<Crew> {
    let mut crew = Crew::new()?;
    crew.settings.evolution.propose_every = 0;
    let main = FakeRunner::cli(CLAUDE, "anthropic").respond_with(move |spec| {
        if spec.role == argonauts_core::base::names::RoleName::Orchestrator {
            return FakeSession::structured(json!({ "script": script }));
        }
        support::smallest_answer(spec)
    });
    crew.services.workers.add(main);
    crew.services
        .workers
        .add(FakeRunner::cli(CODEX, "openai").respond_with(codex_answers));
    crew.services
        .workers
        .add(FakeRunner::cli(AGY, "google").respond_with(agy_answers));
    for level in ["High", "Medium", "Low"] {
        let model = format!("gemini-3.8-flash-{}", level.to_lowercase());
        let name = format!("Gemini 3.8 Flash ({level})");
        crew.services.registry.add(listed(&model, &name));
    }
    crew.settings
        .agents
        .entry(AGY.to_owned())
        .or_default()
        .model = Some(pinned.to_owned());
    crew.settings.crew = CrewSettings {
        main: CLAUDE.parse()?,
        orchestrator: Orchestrator::Main,
        worker: vec![CLAUDE.parse()?, CODEX.parse()?, AGY.parse()?],
        reviewer: vec![CLAUDE.parse()?],
        judge: vec![CLAUDE.parse()?],
        worker_workflows: false,
        merger: Vec::new(),
        orchestrator_context: Default::default(),
        orchestrator_picks: Default::default(),
        ways: Default::default(),
    };
    crew.settings.workflow = "council".to_owned();
    crew.settings.workflows.adapt_with = AdaptWith::Script;
    crew.with_council_steps()?;
    Ok(crew)
}

/// agy no longer lists the model the settings pin it to, so its nearest listed model takes the
/// job, and one line says so before the job starts. It's the same job, with no fallback.
#[tokio::test]
async fn a_model_its_cli_no_longer_lists_runs_the_nearest_listed_one() -> anyhow::Result<()> {
    let crew = council("gemini-3.6-flash-low", support::smallest_answer)?;
    let _env = crew.repo.env_guard();

    let run = crew.start(RunMode::Crew, REQUEST).await?;

    assert_eq!(
        ran_as(&run.events, "propose:p3"),
        [agy("gemini-3.8-flash-low")]
    );
    let said = notes(&run.events);
    let line = "propose:p3 runs on Gemini 3.8 Flash (Low): agy no longer offers \
                Gemini 3.6 Flash (Low)";
    assert_eq!(
        said.iter().filter(|note| *note == line).count(),
        1,
        "{said:#?}"
    );
    assert!(
        !said.iter().any(|note| note.contains("fell back")),
        "the same job, so no fallback: {said:#?}"
    );
    Ok(())
}

/// agy refuses its model when the session starts, as one it dropped since its list was read:
/// its other models of the same line take the job, the nearest first, before any other CLI.
#[tokio::test]
async fn a_refused_model_gives_way_to_its_clis_other_models_first() -> anyhow::Result<()> {
    let asked: Arc<Mutex<Vec<String>>> = Arc::default();
    let seen = asked.clone();
    let crew = council("gemini-3.8-flash-low", move |spec| {
        let model = spec.model.model.clone().unwrap_or_default();
        seen.lock().unwrap().push(model.clone());
        if model == "gemini-3.8-flash-low" {
            return FakeSession::ended(EndReason::CliError {
                category: ErrorCategory::ModelUnavailable,
                message: format!("model {model} is not recognized"),
            });
        }
        support::smallest_answer(spec)
    })?;
    let _env = crew.repo.env_guard();

    let run = crew.start(RunMode::Crew, REQUEST).await?;

    assert_eq!(
        ran_as(&run.events, "propose:p3"),
        // The first runs what agy's settings give it, the refused model.
        [ModelRef::new(AGY, None), agy("gemini-3.8-flash-medium")],
        "{:#?}",
        notes(&run.events)
    );
    Ok(())
}

/// Claude Code refuses its model when the session starts, as for an account without access to
/// it. Claude Code lists no models itself, so its other models the registry knows take the job
/// before any other CLI does.
#[tokio::test]
async fn a_refused_claude_code_model_gives_way_to_claude_codes_other_models() -> anyhow::Result<()>
{
    let mut crew = council("gemini-3.8-flash-low", support::smallest_answer)?;
    let opus: ModelRef = "claude-code/opus".parse()?;
    crew.services.workers.add(
        FakeRunner::cli("claude-code/opus", "anthropic").respond_with(|_| {
            FakeSession::ended(EndReason::CliError {
                category: ErrorCategory::ModelUnavailable,
                message: "There's an issue with the selected model (opus). It may not exist or \
                          you may not have access to it."
                    .to_owned(),
            })
        }),
    );
    crew.services.workers.add(
        FakeRunner::cli("claude-code/sonnet", "anthropic").respond_with(support::smallest_answer),
    );
    crew.services
        .workers
        .add(FakeRunner::cli("opencode/big", "zen").respond_with(support::smallest_answer));
    for model in ["opus", "sonnet"] {
        let entry: ModelEntry = serde_json::from_value(json!({
            "id": format!("claude-code/{model}"),
            "provider": "anthropic",
            "kind": "cli",
            "access": "subscription",
            "cost": { "unit": "plan_usage" },
        }))?;
        crew.services.registry.add(entry);
    }
    crew.settings.crew.worker[0] = opus.clone();
    let _env = crew.repo.env_guard();

    let run = crew.start(RunMode::Crew, REQUEST).await?;

    let ran = ran_as(&run.events, "propose:p1");
    assert_eq!(ran.first(), Some(&opus), "{ran:?}");
    let next = ran.get(1).context("a second session took the job")?;
    assert_eq!(
        next.agent,
        "claude-code",
        "{ran:?} {:#?}",
        notes(&run.events)
    );
    assert_ne!(next, &opus, "{ran:?}");
    Ok(())
}

fn signed_out(_: &SessionSpec) -> FakeSession {
    FakeSession::ended(EndReason::Auth {
        message: "authentication required".to_owned(),
    })
}

/// Signed out while the council proposes, and back for what comes after.
fn out_for_the_proposals(spec: &SessionSpec) -> FakeSession {
    let proposes = spec
        .prompt
        .iter()
        .any(|part| part.text.contains("Propose how to do it"));
    if proposes {
        signed_out(spec)
    } else {
        support::smallest_answer(spec)
    }
}

/// agy is signed out, and another installed CLI can start, so it takes the job.
#[tokio::test]
async fn another_cli_that_can_start_takes_a_job_its_own_cli_cant() -> anyhow::Result<()> {
    let crew = council("gemini-3.8-flash-low", signed_out)?;
    crew.services
        .workers
        .add(FakeRunner::cli("opencode/big", "zen").respond_with(support::smallest_answer));
    let _env = crew.repo.env_guard();

    let run = crew.start(RunMode::Crew, REQUEST).await?;

    let ran = ran_as(&run.events, "propose:p3");
    assert_eq!(ran.last(), Some(&"opencode/big".parse()?), "{ran:?}");
    assert!(left_out(&run.events).is_empty());
    Ok(())
}

fn left_out(events: &[RunEvent]) -> Vec<AgentLeftOut> {
    events
        .iter()
        .filter_map(|event| match &event.kind {
            RunEventKind::Workflow(WorkflowEvent::LeftOut(left)) => Some(left.clone()),
            _ => None,
        })
        .collect()
}

/// agy is signed out and nothing else can take its proposal, so the job is left out with one
/// line, and the council goes on with the two proposals it has.
#[tokio::test]
async fn an_agent_the_council_can_do_without_is_left_out() -> anyhow::Result<()> {
    let crew = council("gemini-3.8-flash-low", signed_out)?;
    let _env = crew.repo.env_guard();

    let run = crew.start(RunMode::Crew, REQUEST).await?;

    let left = left_out(&run.events);
    assert_eq!(left.len(), 1, "{left:#?}");
    assert_eq!(left[0].label, "propose:p3");
    assert_eq!(left[0].why, "signed out");
    assert_eq!(left[0].said, "agy is signed out");
    assert_eq!(left[0].meant_for.agent, AGY);
    // Its agent view says what it was and what going on without it means.
    assert_eq!(left[0].acts_as, "proposer");
    assert_eq!(
        left[0].means,
        [
            "a council can do without one proposal, so it goes on with two",
            "sign in to agy and the next run asks it again",
        ]
    );
    let line = "propose:p3 is left out: agy is signed out and nothing else can take \
                it · the council goes on with 2";
    assert!(
        notes(&run.events).iter().any(|note| note == line),
        "{:#?}",
        notes(&run.events)
    );
    assert!(
        matches!(run.exit, EngineExit::Finished(_)),
        "the council went on to its end: {:?}",
        run.exit
    );
    Ok(())
}

/// A council that proposed, challenged, decided and started building, then asks a worker
/// that only reads to look at what was built: a job after the building started, which no
/// council can do without.
const READ_AFTER_BUILDING: &str = r#"export const meta = {
  name: 'council',
  description: 'Everyone proposes, the judge decides, one builds, another reads',
  phases: [{ name: 'Propose' }, { name: 'Challenge' }, { name: 'Decide' }, { name: 'Build' },
    { name: 'Read' }],
}
const asked = [0, 1, 2].map((n) => () => agent('Propose how to do it: ' + args.request, {
  role: 'worker', access: 'read', model: n, label: 'propose:p' + (n + 1), phase: 'Propose',
}))
const answered = await parallel(asked)
const proposals = answered.filter(Boolean)
// Each proposal that came in is challenged by the model that made it.
const made = answered.map((proposal, n) => (proposal ? n : null)).filter((n) => n !== null)
await parallel(made.map((n) => () => agent('Argue against the other proposals.', {
  role: 'worker', access: 'read', model: n, label: 'challenge:p' + (n + 1), phase: 'Challenge',
})))
await agent('Pick the strongest.', { role: 'judge', label: 'decide', phase: 'Decide' })
await agent('Build it.', { role: 'worker', model: 0, label: 'build:it', phase: 'Build', task: 'it' })
return await agent('Read what was built.', {
  role: 'worker', access: 'read', model: 2, label: 'read:it', phase: 'Read',
})
"#;

/// Only a council's proposals and challenges, the reading before the building, and the
/// reviewers beyond the first can be left out. A job that reads once the building has started
/// is one the run needs: agy is signed out and nothing else can take it, so the run stops
/// rather than going on without it.
#[tokio::test]
async fn a_read_only_job_after_the_building_started_isnt_left_out() -> anyhow::Result<()> {
    let mut crew = scripted(
        READ_AFTER_BUILDING,
        "gemini-3.8-flash-low",
        support::smallest_answer,
        signed_out,
    )?;
    // No other model of a CLI in the step may take it either.
    crew.settings.fallback.same_step = Some(SameStep::Never);
    let _env = crew.repo.env_guard();

    let run = crew.start(RunMode::Crew, REQUEST).await?;

    let labels: Vec<String> = left_out(&run.events)
        .into_iter()
        .map(|left| left.label)
        .collect();
    assert_eq!(labels, ["propose:p3"], "only the proposal is left out");
    assert!(
        matches!(run.exit, EngineExit::Waiting(WaitReason::NoFallback { .. })),
        "the run stopped: {:?}",
        run.exit
    );
    Ok(())
}

/// agy is signed out and Codex too, so one proposal is left where a council needs two: the run
/// stops fully, says why, what was tried and how to carry on, and sets no time to try again.
#[tokio::test]
async fn a_run_that_cant_go_on_stops_fully_with_why_and_how_to_carry_on() -> anyhow::Result<()> {
    let crew = council_with("gemini-3.8-flash-low", signed_out, signed_out)?;
    let _env = crew.repo.env_guard();

    let run = crew.start(RunMode::Crew, REQUEST).await?;

    let EngineExit::Waiting(WaitReason::NoFallback { until, stop, .. }) = &run.exit else {
        bail!("the run should stop fully: {:?}", run.exit);
    };
    assert_eq!(*until, None, "it never tries again on its own");
    let stop = stop.as_ref().context("the stop says why")?;
    assert_eq!(
        stop.why,
        "a council needs two proposals, and only propose:p1 can make one"
    );
    assert_eq!(
        stop.lines(),
        [
            "propose:p2 · Codex is signed out",
            "propose:p3 · agy is signed out",
            "Nothing else can take them: Claude Code already works in this step",
            "propose:p1's work so far is kept · once Codex or agy works, /resume \
             carries the run on",
        ]
    );
    Ok(())
}

/// Claude Code knows a second model, so it could make a second proposal beside its own.
fn with_sonnet(crew: &Crew) {
    let mut entry: ModelEntry = serde_json::from_value(json!({
        "id": "claude-code/sonnet",
        "provider": "anthropic",
        "kind": "cli",
        "access": "subscription",
        "cost": { "unit": "plan_usage" },
    }))
    .expect("a registry entry");
    entry.name = Some("Sonnet 5.5".to_owned());
    crew.services.registry.add(entry);
    crew.services.workers.add(
        FakeRunner::cli("claude-code/sonnet", "anthropic").respond_with(support::smallest_answer),
    );
}

/// Codex and agy sign out side by side, so either one may be left out first and the other be
/// the proposal the council can't do without. `line` says it for a job and its CLI.
fn either_proposal(line: impl Fn(&str, &str) -> String) -> [String; 2] {
    [line("propose:p2", "Codex"), line("propose:p3", "agy")]
}

fn second_questions(events: &[RunEvent]) -> Vec<String> {
    events
        .iter()
        .filter_map(|event| match &event.kind {
            RunEventKind::Approval(ApprovalEvent::Opened(opened))
                if matches!(opened.packet, ApprovalPacket::SecondAgent(_)) =>
            {
                Some(opened.title.clone())
            }
            _ => None,
        })
        .collect()
}

/// Codex and agy are signed out while the council proposes, and `fallback.same_step =
/// "allow"` lets Claude Code's other model make the second proposal the council needs, said in
/// one line, with nothing asked.
#[tokio::test]
async fn same_step_allow_puts_another_model_of_a_cli_in_the_step_on_the_job() -> anyhow::Result<()>
{
    let mut crew = council_with(
        "gemini-3.8-flash-low",
        out_for_the_proposals,
        out_for_the_proposals,
    )?;
    crew.settings.fallback.same_step = Some(SameStep::Allow);
    with_sonnet(&crew);
    let _env = crew.repo.env_guard();

    let run = crew.start(RunMode::Crew, REQUEST).await?;

    assert!(
        matches!(run.exit, EngineExit::Finished(_)),
        "the council went on with two proposals: {:?}",
        run.exit
    );
    assert!(second_questions(&run.events).is_empty(), "nothing is asked");
    let said = notes(&run.events);
    let lines = either_proposal(|job, cli| {
        format!(
            "{job} runs on Sonnet 5.5, a second proposal from Claude Code: {cli} is signed out \
             and nothing else can take it"
        )
    });
    assert!(said.iter().any(|note| lines.contains(note)), "{said:#?}");
    Ok(())
}

/// `fallback.same_step = "ask"`, the default, asks first; your no stops the run, and the stop
/// names your no among what was tried.
#[tokio::test]
async fn same_step_ask_stops_the_run_when_you_say_no() -> anyhow::Result<()> {
    let crew = council_with("gemini-3.8-flash-low", signed_out, signed_out)?;
    with_sonnet(&crew);
    let _env = crew.repo.env_guard();

    let asked = crew.start(RunMode::Crew, REQUEST).await?;
    let questions = second_questions(&asked.events);
    let titles = either_proposal(|job, _| format!("A second proposal from Claude Code for {job}?"));
    assert!(
        matches!(questions.as_slice(), [title] if titles.contains(title)),
        "{questions:?} {:?}",
        asked.exit
    );
    let no = AnswerAction::Choose {
        value: "no".to_owned(),
    };
    let (command, _reply) = support::answer(&asked.events, ApprovalKind::Question, no, None)?;
    let run = crew.resume(asked.events.clone(), vec![command]).await?;

    let EngineExit::Waiting(WaitReason::NoFallback { until, stop, .. }) = &run.exit else {
        bail!("the run should stop fully: {:?}", run.exit);
    };
    assert_eq!(*until, None);
    let stop = stop.as_ref().context("the stop says why")?;
    assert_eq!(
        stop.lines()[2],
        "Nothing else can take them: you said no to a second proposal from Claude Code"
    );
    Ok(())
}

/// `fallback.same_step = "never"` stops the run without asking.
#[tokio::test]
async fn same_step_never_stops_the_run_without_asking() -> anyhow::Result<()> {
    let mut crew = council_with("gemini-3.8-flash-low", signed_out, signed_out)?;
    crew.settings.fallback.same_step = Some(SameStep::Never);
    with_sonnet(&crew);
    let _env = crew.repo.env_guard();

    let run = crew.start(RunMode::Crew, REQUEST).await?;

    assert!(second_questions(&run.events).is_empty());
    let EngineExit::Waiting(WaitReason::NoFallback { until: None, .. }) = &run.exit else {
        bail!("the run should stop fully: {:?}", run.exit);
    };
    Ok(())
}

/// Codex crashes at every start, before it has a session: the second crash in a row with the
/// same message takes it out for the run, with one line, and its proposal is left out.
#[tokio::test]
async fn two_startup_crashes_in_a_row_take_a_cli_out_for_the_run() -> anyhow::Result<()> {
    let crashes = |_: &SessionSpec| {
        FakeSession::ended(EndReason::Crashed {
            message: "Failed to synchronize managed preferences".to_owned(),
        })
        .without_cli_session()
    };
    let crew = council_with("gemini-3.8-flash-low", crashes, support::smallest_answer)?;
    let _env = crew.repo.env_guard();

    let run = crew.start(RunMode::Crew, REQUEST).await?;

    let said = notes(&run.events);
    let out = "Codex is out for this run: it crashed twice as it started: \"Failed to \
               synchronize managed preferences\"";
    assert_eq!(
        said.iter().filter(|note| *note == out).count(),
        1,
        "{said:#?}"
    );
    assert_eq!(
        ran_as(&run.events, "propose:p2").len(),
        2,
        "it started twice"
    );
    let left = left_out(&run.events);
    assert_eq!(left.len(), 1, "{left:#?}");
    assert_eq!(left[0].label, "propose:p2");
    // Signing in wouldn't bring it back, so its view says only that the next run asks again.
    assert_eq!(
        left[0].means.last().map(String::as_str),
        Some("the next run asks it again")
    );
    Ok(())
}
