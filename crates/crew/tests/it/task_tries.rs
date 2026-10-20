//! A task's tries on its own session: a build that did the work and missed only its report is
//! resumed and asked for the report alone, never started over, and a try a restart finds before
//! its session began is told again what it was told.

use crate::support;
use std::sync::Arc;

use serde_json::{Value, json};

use argonauts_core::ErrorCategory;
use argonauts_core::events::run::RunMode;
use argonauts_core::events::session::{EndReason, SessionEvent};
use argonauts_core::events::task::{TaskEvent, TryCause};
use argonauts_core::events::{RunEvent, RunEventKind};
use argonauts_core::limits::LimitsService;
use argonauts_core::session_runner::{Access, SessionSpec};
use argonauts_core::settings::Settings;
use argonauts_crew::engine::shared::with_git_env;
use argonauts_crew::task::prompt::message_part;
use argonauts_testkit::runner::{FakeRunner, FakeSession};

use support::Crew;

const REQUEST: &str = "Add due dates to the tasks CLI";
const WORKER: &str = "claude-code/haiku";

fn report() -> Value {
    json!({
        "status": "done",
        "summary": "Due dates in the tasks CLI",
        "files_changed": [],
        "tests_run": ["node --test: 11 pass"],
        "concerns": []
    })
}

/// A Captain crew whose main agent is a fake Claude Code.
fn captain_crew() -> anyhow::Result<(Crew, Arc<FakeRunner>)> {
    let mut crew = Crew::new()?;
    crew.settings.crew.main = WORKER.parse()?;
    crew.settings.workflows.adapt_at_phase_ends = false;
    crew.settings.evolution.propose_every = 0;
    let worker = crew.worker(WORKER, "anthropic");
    Ok((crew, worker))
}

fn builds(worker: &FakeRunner) -> Vec<SessionSpec> {
    worker
        .specs()
        .into_iter()
        .filter(|spec| spec.access == Access::Build)
        .collect()
}

fn said(spec: &SessionSpec) -> String {
    spec.prompt
        .iter()
        .map(|part| part.text.as_str())
        .collect::<Vec<_>>()
        .join("\n")
}

/// Argonauts Code's recovery table and fallback planner over the crew's fake boards, so the
/// task recovers as a real run's does.
fn conductor_limits(crew: &Crew, settings: &Arc<Settings>) -> Arc<dyn LimitsService> {
    let services = &crew.services;
    let planner = Arc::new(argonauts_conductor::fallback::Planner::new(
        settings.clone(),
        services.registry.clone(),
        services.plan_usage.clone(),
        services.health.clone(),
        services.track_records.clone(),
    ));
    Arc::new(
        argonauts_conductor::recovery::Limits::new(
            settings.clone(),
            services.plan_usage.clone(),
            services.health.clone(),
            planner,
        )
        .with_registry(services.registry.clone()),
    )
}

/// Claude Code gave up on the format after doing the work. The engine leaves the session to
/// the task, whose recovery resumes it for the report alone, so the work isn't done twice.
#[tokio::test]
async fn a_build_that_missed_only_its_report_is_resumed_for_the_report() -> anyhow::Result<()> {
    let (crew, worker) = captain_crew()?;
    let mut missed = FakeSession::ended(EndReason::CliError {
        category: ErrorCategory::AnswerMissed,
        message: "Failed to provide valid structured output".to_owned(),
    })
    .with_cli_session("c-1");
    missed.end.tool_calls = 12;
    worker.push(missed);
    worker.push(FakeSession::structured(report()).with_cli_session("c-1"));
    let _env = crew.repo.env_guard();
    let started = crew.run_started(RunMode::Captain, REQUEST)?;
    let (mut context, _sink, _commands) = crew.context(vec![started])?;
    context.services.limits = conductor_limits(&crew, &context.settings);
    let engine = argonauts_crew::engine_for(&crew.settings, RunMode::Captain).run(context);
    with_git_env(crew.git_env(), engine).await;

    let builds = builds(&worker);
    assert!(builds.len() >= 2, "{} build sessions", builds.len());
    assert_eq!(
        builds[1].resume.as_deref(),
        Some("c-1"),
        "resumed, not restarted"
    );
    let told = said(&builds[1]);
    assert!(told.contains("send only the report now"), "{told}");
    Ok(())
}

/// A try on the task's own session, recorded with what it was told and stopped before its
/// session began, is told the same again when the run reopens, not just to continue.
#[tokio::test]
async fn a_try_reopened_before_its_session_began_is_told_again() -> anyhow::Result<()> {
    let (crew, worker) = captain_crew()?;
    worker.push(FakeSession::structured(report()).with_cli_session("c-1"));
    let _env = crew.repo.env_guard();
    let whole = crew.start(RunMode::Captain, REQUEST).await?;

    // The run as it stood once the first build ended, with a second try on the same session
    // recorded and nothing after it.
    let ended = whole
        .events
        .iter()
        .position(|event| {
            matches!(&event.kind, RunEventKind::Session(SessionEvent::Ended(_)))
                && event.task_id.is_some()
        })
        .expect("the build's end");
    let first_try = whole
        .events
        .iter()
        .find(|event| matches!(&event.kind, RunEventKind::Task(TaskEvent::Try { .. })))
        .expect("the first try")
        .clone();
    let mut history: Vec<RunEvent> = whole.events[..=ended].to_vec();
    let comments = "Review: the overdue list leaves out tasks due today.";
    let mut retry = first_try;
    retry.seq = history.last().map_or(0, |event| event.seq + 1);
    retry.session = None;
    retry.kind = RunEventKind::Task(TaskEvent::Try {
        try_number: 1,
        worker: WORKER.parse()?,
        cause: TryCause::ReviewChanges,
        told: vec![message_part("review", comments)],
    });
    history.push(retry);

    worker.push(FakeSession::structured(report()).with_cli_session("c-1"));
    let before = builds(&worker).len();
    crew.reopen(history).await?;
    let builds = builds(&worker);
    let resumed = builds
        .get(before)
        .expect("a build session after the reopen");
    assert_eq!(resumed.resume.as_deref(), Some("c-1"));
    let told = said(resumed);
    assert!(told.contains(comments), "{told}");
    assert!(
        !told.contains("Continue the task where you left off"),
        "{told}"
    );
    Ok(())
}
