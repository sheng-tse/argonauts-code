//! A CLI error never ends a run: the agent goes again once, then another agent
//! takes its role, and only when nobody can does the run wait, saying why. In autopilot a failed
//! judge is asked again, then another provider's judges in its place, else your rules decide alone.
//! The orchestrated run of 2026-10-04, whose workflow design ended on a passing Codex
//! 401, is replayed from its log.

use crate::support;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use anyhow::Context;
use serde_json::{Value, json};

use argonauts_core::agents::session_runner::{RunnerEvent, SessionSpec};
use argonauts_core::base::names::{RoleName, WorkflowId};
use argonauts_core::models::limits::{Candidate, CostEstimate, HealthState};
use argonauts_core::runs::engine::EngineExit;
use argonauts_core::runs::events::approval::ApprovalEvent;
use argonauts_core::runs::events::run::{Party, RunLifecycle, RunMode, RunOutcome, WaitReason};
use argonauts_core::runs::events::session::{EndReason, SessionEnd, SessionEvent};
use argonauts_core::runs::events::task::{ReviewEvent, TaskEvent};
use argonauts_core::runs::events::workflow::WorkflowEvent;
use argonauts_core::runs::events::{RunEvent, RunEventKind};
use argonauts_core::settings::Span;
use argonauts_core::settings::approvals::ApprovalMode;
use argonauts_core::settings::crew::CrewRole;
use argonauts_core::settings::fallback::{FallbackEntry, Retry};
use argonauts_core::settings::protocol::CheckCommand;
use argonauts_core::workflows::steps::StepName;
use argonauts_core::{ErrorCategory, LimitDimension};
use argonauts_crew::engine::shared::with_git_env;
use argonauts_crew::roles::display_name;
use argonauts_testkit::{FakeRunner, FakeSession};

use support::{Crew, Outcome};

const CODEX: &str = "codex/gpt-5.6-luna";
const CLAUDE: &str = "claude-code/haiku";
const GEMINI: &str = "antigravity-cli/gemini-3.6-flash-low";
/// A fourth agent, from a fourth provider.
const QWEN: &str = "opencode/qwen3-coder";

/// What Codex said and how `main:design` ended in the orchestrated run, copied from its log.
const DESIGN_UNAUTHORIZED: &str = include_str!("../fixtures/runs/design-unauthorized.jsonl");

/// How an agent's workflow design after Understand goes, by its turn among them.
#[derive(Clone, Copy)]
enum Design {
    Answers,
    /// A brief the rules turn down, so the main agent is asked again.
    Rejected,
    /// The service is overloaded, which may pass.
    Overloaded,
    /// Codex refuses the answer format, which every retry would meet again.
    RefusedFormat,
    /// A sign-in error, which its CLI's own login check confirms or contradicts.
    SignIn,
    /// The recorded session: Codex's reconnects, its errors, and its end.
    Recorded,
}

/// One agent of a crew: its workflow designs after Understand by turn, the steps whose sessions
/// end on an overloaded service and how many times, whether its reviews ask for changes,
/// and whether its CLI says it's logged out.
#[derive(Clone, Default)]
struct Agent {
    designs: Vec<Design>,
    overloaded: Vec<(StepName, usize)>,
    /// Every one of its sessions ends on an overloaded service.
    always_overloaded: bool,
    /// Every one of its sessions ends on a sign-in error.
    always_signed_out: bool,
    /// Every one of its sessions crashes as it starts.
    always_crashes: bool,
    /// Its CLI can't start a session at all.
    never_starts: bool,
    /// Its build sessions end with words but no report.
    builds_without_report: bool,
    requests_changes: bool,
    logged_out: bool,
}

impl Agent {
    fn designs(designs: Vec<Design>) -> Self {
        Self {
            designs,
            ..Self::default()
        }
    }

    /// Its first `times` sessions of `step` end on an overloaded service.
    fn overloaded(step: StepName, times: usize) -> Self {
        Self {
            overloaded: vec![(step, times)],
            ..Self::default()
        }
    }
}

/// Who the orchestrator puts on the task.
#[derive(Clone)]
struct Assignment {
    worker: &'static str,
    reviewers: Vec<&'static str>,
}

impl Default for Assignment {
    fn default() -> Self {
        Self {
            worker: CODEX,
            reviewers: vec![GEMINI],
        }
    }
}

/// A crew of three, Codex as the main agent of the orchestrator workflow, which it designs,
/// with a design at each phase end. Each agent builds its task's file and answers every other step
/// with the smallest valid answer.
fn crew(designs: &[(&str, Vec<Design>)]) -> anyhow::Result<Crew> {
    crew_logged_out(designs, &[])
}

/// [`crew`], where the CLIs of `logged_out` say they're logged out.
fn crew_logged_out(designs: &[(&str, Vec<Design>)], logged_out: &[&str]) -> anyhow::Result<Crew> {
    let agents: Vec<(&str, Agent)> = [CODEX, CLAUDE, GEMINI]
        .into_iter()
        .map(|worker| {
            let designs = designs
                .iter()
                .find(|(name, _)| *name == worker)
                .map(|(_, plan)| plan.clone())
                .unwrap_or_default();
            let agent = Agent {
                logged_out: logged_out.contains(&worker),
                ..Agent::designs(designs)
            };
            (worker, agent)
        })
        .collect();
    crew_of(&agents, &Assignment::default())
}

/// A crew of these agents in this order, the first leading the orchestrator workflow, and
/// the task assigned as `assignment` says.
fn crew_of(agents: &[(&str, Agent)], assignment: &Assignment) -> anyhow::Result<Crew> {
    let mut crew = Crew::new()?;
    crew.settings.crew.clear();
    for (worker, plan) in agents {
        let fake = FakeRunner::cli(worker, provider_of(worker))
            .with_login(plan.logged_out.then_some(false))
            .respond_with(agent(plan.clone(), assignment.clone()));
        crew.services.workers.add(fake);
        crew.settings.crew.add(worker.parse()?);
    }
    crew.settings.crew.main = crew.settings.crew.members()[0].clone();
    crew.settings.workflow = "orchestrator".to_owned();
    crew.settings.workflows.orchestrator_designs = true.into();
    crew.settings.workflows.adapt_at_phase_ends = true;
    crew.settings.divide.contracts_first = false;
    crew.settings.checks.run = vec![CheckCommand {
        name: "check".to_owned(),
        command: vec!["true".to_owned()],
    }];
    Ok(crew)
}

fn provider_of(worker: &str) -> &'static str {
    match worker {
        CODEX => "openai",
        CLAUDE => "anthropic",
        GEMINI => "google",
        _ => "alibaba",
    }
}

/// One agent's answers. Its workflow designs after Understand follow `plan`, then answer; the steps
/// `plan` says end on an overloaded service do, as many times as it says, then answer.
fn agent(
    plan: Agent,
    assignment: Assignment,
) -> impl Fn(&SessionSpec) -> FakeSession + Send + Sync + 'static {
    let turn = Arc::new(AtomicUsize::new(0));
    let failed: Arc<std::sync::Mutex<Vec<StepName>>> = Arc::default();
    move |spec: &SessionSpec| {
        if plan.always_overloaded {
            return overloaded();
        }
        if plan.always_signed_out {
            return sign_in_error();
        }
        if plan.always_crashes {
            return FakeSession::ended(EndReason::Crashed {
                message: "thread/start failed".to_owned(),
            });
        }
        if plan.never_starts {
            return FakeSession::ended(EndReason::StartFailed {
                message: "No such file or directory".to_owned(),
            });
        }
        if let Some(step) = spec.step
            && let Some((_, times)) = plan.overloaded.iter().find(|(at, _)| *at == step)
        {
            let mut failed = failed.lock().expect("the failure count");
            if failed.iter().filter(|done| **done == step).count() < *times {
                failed.push(step);
                return overloaded();
            }
        }
        if spec.step == Some(StepName::Build) {
            let _ = std::fs::write(spec.workdir.join(format!("{}.txt", spec.key)), "work\n");
            if plan.builds_without_report {
                return FakeSession::completed("Done, but the report didn't go through.");
            }
        }
        if spec.step == Some(StepName::DesignWorkflow) {
            let after_understand = prompt(spec).contains("\"current\": ");
            if after_understand {
                let n = turn.fetch_add(1, Ordering::SeqCst);
                match plan.designs.get(n).copied().unwrap_or(Design::Answers) {
                    Design::Answers => {}
                    Design::Rejected => {
                        return FakeSession::structured(support::brief(
                            Some("no-such-workflow"),
                            json!([]),
                            None,
                            "it fits",
                        ));
                    }
                    Design::Overloaded => return overloaded(),
                    Design::RefusedFormat => return refused_format(),
                    Design::SignIn => return sign_in_error(),
                    Design::Recorded => return recorded_401(),
                }
            }
            return FakeSession::structured(support::brief(
                Some("orchestrator"),
                json!([]),
                None,
                "it fits",
            ));
        }
        if spec.step == Some(StepName::ModelJudge) {
            return FakeSession::structured(json!({
                "verdict": "approve",
                "reasons": ["the evidence meets the rules"],
                "rules_cited": ["R7"]
            }));
        }
        let schema = spec.json_schema.clone().unwrap_or(Value::Null);
        let mut answer = support::fill(&schema);
        if matches!(
            spec.step,
            Some(StepName::Orchestrate | StepName::PlanToTasks)
        ) {
            answer["tasks"] = json!([{
                "key": "due",
                "title": "Due dates",
                "task": "Add due dates to tasks",
                "kind": "build",
                "owned_paths": ["src/due"],
                "read_only_paths": [],
                "depends_on": [],
                "criteria": []
            }]);
        }
        if spec.step == Some(StepName::Orchestrate) {
            answer["assignments"] = json!([{
                "task": "due",
                "worker": assignment.worker,
                "reviewers": assignment.reviewers
            }]);
        }
        if spec.step == Some(StepName::Review) && plan.requests_changes {
            answer["verdict"] = json!("request_changes");
            answer["summary"] = json!("The due date needs a test.");
        }
        if spec.step == Some(StepName::CloseRun) {
            answer = json!({ "answer": "Due dates are in, and the checks pass." });
        }
        FakeSession::structured(answer)
    }
}

/// An overloaded service, which may pass a moment later.
fn overloaded() -> FakeSession {
    FakeSession::ended(EndReason::CliError {
        category: ErrorCategory::Overloaded,
        message: "Overloaded (529)".to_owned(),
    })
}

/// A 401, as Codex gave one.
fn sign_in_error() -> FakeSession {
    FakeSession::ended(EndReason::Auth {
        message: "workspace routing discovery unauthorized (401)".to_owned(),
    })
}

/// Codex's refusal of an answer format, as it gave it.
fn refused_format() -> FakeSession {
    FakeSession::ended(EndReason::CliError {
        category: ErrorCategory::InvalidRequest,
        message: "Invalid schema for response_format 'codex_output_schema': 'schema.required' is \
                  required to include every key in properties. Missing 'summary'"
            .to_owned(),
    })
}

/// The recorded session as Codex played it: its reconnects, errors, and its end.
fn recorded_401() -> FakeSession {
    let mut events = Vec::new();
    let mut end = None;
    for line in DESIGN_UNAUTHORIZED.lines() {
        let event: RunEvent = serde_json::from_str(line).expect("a recorded event");
        match event.kind {
            RunEventKind::Agent(agent) => events.push(RunnerEvent::Agent(agent)),
            RunEventKind::Session(SessionEvent::Ended(ended)) => end = Some(*ended),
            _ => {}
        }
    }
    let end: SessionEnd = end.expect("the recorded end");
    let mut session = FakeSession::ended(end.reason.clone());
    session.events = events;
    session.end = end;
    session
}

fn prompt(spec: &SessionSpec) -> String {
    spec.prompt
        .iter()
        .map(|part| part.text.as_str())
        .collect::<Vec<_>>()
        .join("\n")
}

/// Starts the run and answers every question it waits on with its recommended option, until it
/// finishes or waits for something no answer settles.
async fn drive(crew: &Crew) -> anyhow::Result<Outcome> {
    let outcome = crew.start(RunMode::Crew, "Add due dates").await?;
    answer_tasks(crew, outcome).await
}

/// Answers each question the run waits on with its recommended option, until it stops for
/// anything else.
async fn answer_tasks(crew: &Crew, mut outcome: Outcome) -> anyhow::Result<Outcome> {
    for _ in 0..20 {
        if !matches!(outcome.exit, EngineExit::Waiting(WaitReason::Approval)) {
            break;
        }
        let Some((command, _reply)) = support::recommended(&outcome.events) else {
            break;
        };
        outcome = crew.resume(outcome.events.clone(), vec![command]).await?;
    }
    Ok(outcome)
}

/// The workflow designs after Understand that each agent was asked for, in order.
fn designs_after_understand(crew: &Crew, worker: &str) -> usize {
    crew.services
        .workers
        .fake(worker)
        .map(|fake| {
            fake.specs()
                .iter()
                .filter(|spec| {
                    spec.step == Some(StepName::DesignWorkflow)
                        && prompt(spec).contains("\"current\": ")
                })
                .count()
        })
        .unwrap_or(0)
}

/// The notes, then how the run finished and why, for a failure to show.
fn notes(events: &[RunEvent]) -> Vec<&str> {
    events
        .iter()
        .filter_map(|event| match &event.kind {
            RunEventKind::Note(note) => Some(note.text.as_str()),
            RunEventKind::Run(RunLifecycle::Finished {
                detail: Some(detail),
                ..
            }) => Some(detail.as_str()),
            _ => None,
        })
        .collect()
}

fn finished(events: &[RunEvent]) -> Option<RunOutcome> {
    events.iter().find_map(|event| match &event.kind {
        RunEventKind::Run(RunLifecycle::Finished { outcome, .. }) => Some(*outcome),
        _ => None,
    })
}

/// Each design session's key, and how many times it started.
fn design_sessions(events: &[RunEvent]) -> Vec<(String, usize)> {
    let mut sessions: Vec<(String, usize)> = Vec::new();
    for event in events {
        let RunEventKind::Session(SessionEvent::Started(started)) = &event.kind else {
            continue;
        };
        if started.step != Some(StepName::DesignWorkflow) || started.workflow_step.is_none() {
            continue;
        }
        let key = event
            .session
            .as_ref()
            .map(ToString::to_string)
            .unwrap_or_default();
        match sessions.iter_mut().find(|(seen, _)| *seen == key) {
            Some((_, starts)) => *starts += 1,
            None => sessions.push((key, 1)),
        }
    }
    sessions
}

#[tokio::test]
async fn an_overload_on_the_main_agents_design_is_retried_and_the_run_goes_on() -> anyhow::Result<()>
{
    let crew = crew(&[(CODEX, vec![Design::Overloaded])])?;
    let outcome = drive(&crew).await?;
    assert_eq!(
        outcome.exit,
        EngineExit::Finished(RunOutcome::Merged),
        "{:?}",
        notes(&outcome.events)
    );
    // One design session went again, so a replay finds its answer where the step asked.
    let sessions = design_sessions(&outcome.events);
    let again: Vec<_> = sessions.iter().filter(|(_, starts)| *starts > 1).collect();
    assert_eq!(again.len(), 1, "{sessions:?}");
    assert_eq!(again[0].1, 2);
    assert_eq!(designs_after_understand(&crew, CODEX), sessions.len() + 1);
    assert_eq!(designs_after_understand(&crew, CLAUDE), 0);
    // One line says it goes again, and nothing says it fell back to anybody.
    let said = notes(&outcome.events);
    let retries: Vec<_> = said
        .iter()
        .filter(|note| note.contains("· trying again"))
        .collect();
    assert_eq!(
        retries,
        [&"GPT 5.6 Luna on Codex is overloaded · trying again"],
        "{said:?}"
    );
    assert!(
        !said
            .iter()
            .any(|note| note.to_lowercase().contains("fell back to")),
        "{said:?}"
    );
    Ok(())
}

/// An error a retry can't change goes to the next agent at once: no wait and no second try,
/// and one line says so.
#[tokio::test]
async fn a_refused_answer_format_goes_straight_to_another_agent() -> anyhow::Result<()> {
    let crew = crew(&[(CODEX, vec![Design::RefusedFormat])])?;
    let outcome = drive(&crew).await?;
    let said = notes(&outcome.events);
    assert_eq!(
        outcome.exit,
        EngineExit::Finished(RunOutcome::Merged),
        "{said:?}"
    );
    assert!(
        !said.iter().any(|note| note.contains("trying again")),
        "{said:?}"
    );
    assert_eq!(
        designs_after_understand(&crew, CLAUDE),
        1,
        "the next on the crew"
    );
    let fell_back = said
        .iter()
        .find(|note| note.starts_with("Fell back to"))
        .context("one line says who it fell back to")?;
    assert_eq!(
        fell_back.to_string(),
        "Fell back to Claude Code as the main agent · no second try, this error repeats every time"
    );
    Ok(())
}

#[tokio::test]
async fn an_overload_twice_hands_the_main_agents_role_to_another_agent() -> anyhow::Result<()> {
    let crew = crew(&[(CODEX, vec![Design::Overloaded, Design::Overloaded])])?;
    let outcome = drive(&crew).await?;
    assert_eq!(
        outcome.exit,
        EngineExit::Finished(RunOutcome::Merged),
        "{:?}",
        notes(&outcome.events)
    );
    // Codex twice, then Claude Code, all on the one session the step asked for.
    let sessions = design_sessions(&outcome.events);
    let again: Vec<_> = sessions.iter().filter(|(_, starts)| *starts > 1).collect();
    assert_eq!(again.len(), 1, "{sessions:?}");
    assert_eq!(again[0].1, 3);
    assert_eq!(designs_after_understand(&crew, CODEX), sessions.len() + 1);
    assert_eq!(
        designs_after_understand(&crew, CLAUDE),
        1,
        "the next on the crew"
    );
    // The retry is said, and then who it fell back to, in names and not ids.
    let said = notes(&outcome.events);
    let retry = said
        .iter()
        .position(|note| note.contains("is overloaded · trying again"))
        .context("one line says Codex goes again")?;
    let fell_back = said
        .iter()
        .position(|note| note.contains("fell back to"))
        .context("one line says who took the main agent's role")?;
    assert!(retry < fell_back, "{said:?}");
    assert_eq!(
        said[fell_back],
        "Still overloaded · fell back to Haiku on Claude Code as the main agent"
    );
    Ok(())
}

#[tokio::test]
async fn when_every_agent_fails_the_run_waits_and_says_why() -> anyhow::Result<()> {
    // A sign-in error gets no second try, so each agent is asked once.
    let once = || vec![Design::SignIn];
    let crew = crew_logged_out(
        &[(CODEX, once()), (CLAUDE, once()), (GEMINI, once())],
        &[GEMINI],
    )?;
    let outcome = drive(&crew).await?;
    // It stops fully: waiting can't fix it, so it never tries again on its own.
    match &outcome.exit {
        EngineExit::Waiting(WaitReason::NoFallback {
            detail,
            until,
            stop,
        }) => {
            assert_eq!(detail, "nobody to fall back to as the main agent");
            assert!(until.is_none(), "no wake is set");
            assert!(stop.is_some(), "it says why it stopped");
        }
        other => panic!("{other:?}: {:?}", notes(&outcome.events)),
    }
    assert_eq!(
        finished(&outcome.events),
        None,
        "an agent error never ends the run"
    );
    // agy says it's logged out, so it isn't asked again and gets the login line.
    assert_eq!(designs_after_understand(&crew, GEMINI), 1);
    let stopped = stop_lines(&outcome.exit).with_context(|| "the stop says why")?;
    assert!(
        stopped.iter().any(|line| line.contains("is signed out")),
        "{stopped:?}"
    );
    assert!(
        !stopped
            .iter()
            .any(|line| line.contains("Codex is signed out")),
        "Codex's CLI never said it was out: {stopped:?}"
    );
    assert!(
        stopped.iter().all(|line| !line.contains("tries again")),
        "it never tries again on its own: {stopped:?}"
    );

    // Once the agents work again, /resume asks the main agent's own agent first, on the same
    // session.
    let resumed = crew.resume(outcome.events.clone(), Vec::new()).await?;
    let resumed = answer_tasks(&crew, resumed).await?;
    let said = notes(&resumed.events);
    assert_eq!(
        resumed.exit,
        EngineExit::Finished(RunOutcome::Merged),
        "{said:?}"
    );
    // Codex, Claude Code and agy once each, then Codex again after the resume.
    let sessions = design_sessions(&resumed.events);
    let again: Vec<_> = sessions.iter().filter(|(_, starts)| *starts > 1).collect();
    assert_eq!(again.len(), 1, "{sessions:?}");
    assert_eq!(again[0].1, 4);
    assert_eq!(designs_after_understand(&crew, CODEX), sessions.len() + 1);
    Ok(())
}

#[tokio::test]
async fn the_orchestrated_runs_passing_401_no_longer_fails_it() -> anyhow::Result<()> {
    // As on 2026-10-04: the main agent's first design after Understand is turned down, and the
    // second ends on Codex's 401. Now it's asked again and the run merges.
    let crew = crew(&[(CODEX, vec![Design::Rejected, Design::Recorded])])?;
    let outcome = drive(&crew).await?;
    assert_eq!(
        outcome.exit,
        EngineExit::Finished(RunOutcome::Merged),
        "{:?}",
        notes(&outcome.events)
    );
    // The turned-down brief goes back to its own session to be fixed, and the
    // 401's session is asked again, once.
    let sessions = design_sessions(&outcome.events);
    let again: Vec<_> = sessions.iter().filter(|(_, starts)| *starts > 1).collect();
    assert_eq!(again.len(), 1, "{sessions:?}");
    assert_eq!(again[0].1, 2, "{sessions:?}");
    let said = notes(&outcome.events);
    assert!(
        said.iter().any(|note| note.contains("it's fixing that")),
        "{said:?}"
    );
    let recorded = outcome.events.iter().any(|event| {
        matches!(&event.kind, RunEventKind::Session(SessionEvent::Ended(end))
            if matches!(&end.reason, EndReason::Auth { message }
                if message == "workspace routing discovery unauthorized (401)"))
    });
    assert!(recorded, "the recorded end played");
    assert_ne!(finished(&outcome.events), Some(RunOutcome::Failed));
    Ok(())
}

/// Autopilot with the real policy: every judge session fails, so after asking each again
/// and finding no one else, your rules decide alone and nothing waits for you.
#[tokio::test]
async fn in_autopilot_a_failing_judge_never_leaves_a_task_to_you() -> anyhow::Result<()> {
    let crew = crew(&[])?;
    for worker in [CLAUDE, GEMINI, CODEX] {
        let fake = crew.services.workers.fake(worker).context("enlisted")?;
        let answers = agent(Agent::default(), Assignment::default());
        fake.respond(move |spec| {
            if spec.step == Some(StepName::ModelJudge) {
                return overloaded();
            }
            answers(spec)
        });
    }
    let outcome = autopilot(&crew, RunMode::Crew).await?;
    let events = outcome.events;
    let said = notes(&events);
    assert_eq!(
        outcome.exit,
        EngineExit::Finished(RunOutcome::Merged),
        "{said:?}"
    );
    assert!(
        !said.iter().any(|note| note.contains("to you")),
        "nothing was left to you: {said:?}"
    );
    // One line for each question autopilot decided, each naming its question.
    let decided: Vec<&&str> = said
        .iter()
        .filter(|note| note.contains("your rules decided"))
        .collect();
    assert!(!decided.is_empty(), "{said:?}");
    assert!(
        decided
            .iter()
            .any(|note| note.starts_with("\"Run this workflow?\": ")),
        "{decided:?}"
    );
    let unique: std::collections::BTreeSet<_> = decided.iter().collect();
    assert_eq!(unique.len(), decided.len(), "no line twice: {decided:?}");
    for note in &decided {
        assert!(!note.contains("Pick another judge"), "{note}");
    }
    assert!(!asked_you(&events));
    Ok(())
}

/// Starts the run in autopilot with the real policy, and Argonauts Code's own recovery when
/// `conductor_recovery`, and drives it until it finishes or waits.
async fn autopilot_with(
    crew: &Crew,
    mode: RunMode,
    conductor_recovery: bool,
) -> anyhow::Result<Outcome> {
    let started = crew.run_started(mode, "Add due dates")?;
    let autopilot = RunEvent {
        seq: 1,
        kind: RunEventKind::Run(RunLifecycle::ApprovalModeChanged {
            mode: ApprovalMode::Autopilot,
            by: Party::User,
        }),
        ..started.clone()
    };
    let (mut context, sink, _commands) = crew.context(vec![started, autopilot])?;
    context.services.policy = real_policy(crew, &context.settings)?;
    if conductor_recovery {
        context.services.limits = conductor_limits(crew, &context.settings);
    }
    let engine = argonauts_crew::engine_for(&crew.settings, mode).run(context);
    let exit = with_git_env(crew.git_env(), engine).await;
    Ok(Outcome {
        exit,
        events: sink.events(),
    })
}

async fn autopilot(crew: &Crew, mode: RunMode) -> anyhow::Result<Outcome> {
    autopilot_with(crew, mode, false).await
}

/// Whether any approval waited for your answer.
fn asked_you(events: &[RunEvent]) -> bool {
    events.iter().any(|event| {
        matches!(&event.kind, RunEventKind::Approval(ApprovalEvent::Decided(decided))
            if decided.by == Party::User)
    })
}

/// The real policy, keeping its records in the repo's state folder.
fn real_policy(
    crew: &Crew,
    settings: &Arc<argonauts_core::settings::Settings>,
) -> anyhow::Result<Arc<dyn argonauts_core::safety::policy::ApprovalPolicy>> {
    let repo = crew.repo_context()?;
    let state = repo
        .run_dir
        .parent()
        .and_then(Path::parent)
        .context("the run folder is under the repo's state folder")?;
    std::fs::create_dir_all(state)?;
    let services = &crew.services;
    Ok(argonauts_policy::approval_policy_with_models(
        settings.clone(),
        Arc::new(|| None),
        state,
        services.workers.clone(),
        services.registry.clone(),
        services.steps.clone(),
    ))
}

/// Argonauts Code's recovery table and fallback planner over the crew's fake boards, so a task
/// is passed on as a real run hands it.
fn conductor_limits(
    crew: &Crew,
    settings: &Arc<argonauts_core::settings::Settings>,
) -> Arc<dyn argonauts_core::models::limits::LimitsService> {
    let services = &crew.services;
    let planner = Arc::new(argonauts_conductor::fallback::Planner::new(
        settings.clone(),
        services.registry.clone(),
        services.plan_usage.clone(),
        services.health.clone(),
        services.track_records.clone(),
    ));
    Arc::new(
        argonauts_conductor::limits::recovery::Limits::new(
            settings.clone(),
            services.plan_usage.clone(),
            services.health.clone(),
            planner,
        )
        .with_registry(services.registry.clone()),
    )
}

/// The sessions `worker` was asked for `step`, by key, with how many times each started.
fn sessions_of(events: &[RunEvent], worker: &str, step: StepName) -> Vec<(String, usize)> {
    let mut sessions: Vec<(String, usize)> = Vec::new();
    for event in events {
        let RunEventKind::Session(SessionEvent::Started(started)) = &event.kind else {
            continue;
        };
        let asked = started.asked.as_ref().unwrap_or(&started.worker);
        if started.step != Some(step) || asked.to_string() != worker {
            continue;
        }
        let key = event
            .session
            .as_ref()
            .map(ToString::to_string)
            .unwrap_or_default();
        match sessions.iter_mut().find(|(seen, _)| *seen == key) {
            Some((_, starts)) => *starts += 1,
            None => sessions.push((key, 1)),
        }
    }
    sessions
}

/// Who each recorded review is under, in order.
fn reviewers(events: &[RunEvent]) -> Vec<String> {
    events
        .iter()
        .filter_map(|event| match &event.kind {
            RunEventKind::Review(ReviewEvent::Reviewed { reviewer, .. }) => {
                Some(reviewer.to_string())
            }
            _ => None,
        })
        .collect()
}

/// How many sessions of `step` `worker` ran itself, whoever they were asked of.
fn ran(crew: &Crew, worker: &str, step: StepName) -> usize {
    crew.services.workers.fake(worker).map_or(0, |fake| {
        fake.specs()
            .iter()
            .filter(|spec| spec.step == Some(step))
            .count()
    })
}

/// The first review round's overload is asked again on its own session, in supervised mode and in
/// autopilot, so the reviewer you asked for reviews and the task merges.
#[tokio::test]
async fn a_first_round_review_overload_is_asked_again() -> anyhow::Result<()> {
    for in_autopilot in [false, true] {
        let gemini = Agent::overloaded(StepName::Review, 1);
        let agents = [
            (CODEX, Agent::default()),
            (CLAUDE, Agent::default()),
            (GEMINI, gemini),
        ];
        let crew = crew_of(&agents, &Assignment::default())?;
        let outcome = if in_autopilot {
            autopilot(&crew, RunMode::Crew).await?
        } else {
            drive(&crew).await?
        };
        let said = notes(&outcome.events);
        assert_eq!(
            outcome.exit,
            EngineExit::Finished(RunOutcome::Merged),
            "autopilot {in_autopilot}: {said:?}"
        );
        let reviews = sessions_of(&outcome.events, GEMINI, StepName::Review);
        assert_eq!(reviews.len(), 1, "{reviews:?}");
        assert_eq!(reviews[0].1, 2, "asked again on its own session");
        assert_eq!(reviewers(&outcome.events), [GEMINI]);
        assert!(
            !said.iter().any(|note| note.contains("review failed")),
            "{said:?}"
        );
    }
    Ok(())
}

/// The first settling's overload is asked again, in supervised mode and in autopilot, and the
/// ruling goes under the judge that gave it.
#[tokio::test]
async fn a_first_settling_overload_is_asked_again() -> anyhow::Result<()> {
    for in_autopilot in [false, true] {
        let agents = [
            (CODEX, Agent::default()),
            (CLAUDE, Agent::default()),
            (
                GEMINI,
                Agent {
                    requests_changes: true,
                    ..Agent::default()
                },
            ),
            (QWEN, Agent::overloaded(StepName::Settle, 1)),
        ];
        let assignment = Assignment {
            worker: CODEX,
            reviewers: vec![CLAUDE, GEMINI],
        };
        let crew = crew_of(&agents, &assignment)?;
        let outcome = if in_autopilot {
            autopilot(&crew, RunMode::Crew).await?
        } else {
            drive(&crew).await?
        };
        let said = notes(&outcome.events);
        assert_eq!(
            outcome.exit,
            EngineExit::Finished(RunOutcome::Merged),
            "autopilot {in_autopilot}: {said:?}"
        );
        let rulings = sessions_of(&outcome.events, QWEN, StepName::Settle);
        assert_eq!(rulings.len(), 1, "{rulings:?}");
        assert_eq!(rulings[0].1, 2, "asked again on its own session");
        let ruled_by: Vec<String> = outcome
            .events
            .iter()
            .filter_map(|event| match &event.kind {
                RunEventKind::Review(ReviewEvent::Settled { judge, .. }) => Some(judge.to_string()),
                _ => None,
            })
            .collect();
        assert_eq!(ruled_by, [QWEN]);
        assert!(
            !said.iter().any(|note| note.contains("judge failed")),
            "{said:?}"
        );
    }
    Ok(())
}

/// A reviewer's place goes only to an agent from another provider than the worker that isn't
/// already reviewing; with none, the review goes on without that reviewer, and every review is
/// recorded under whoever wrote it.
#[tokio::test]
async fn a_reviewers_fallback_never_doubles_up_or_comes_from_the_workers_provider()
-> anyhow::Result<()> {
    let agents = [
        (CODEX, Agent::default()),
        (CLAUDE, Agent::overloaded(StepName::Review, 2)),
        (GEMINI, Agent::default()),
    ];
    let assignment = Assignment {
        worker: CODEX,
        reviewers: vec![CLAUDE, GEMINI],
    };
    let crew = crew_of(&agents, &assignment)?;
    let outcome = drive(&crew).await?;
    let said = notes(&outcome.events);
    assert_eq!(
        outcome.exit,
        EngineExit::Finished(RunOutcome::Merged),
        "{said:?}"
    );
    // Gemini reviews once, as itself; Codex, the worker, never reviews.
    assert_eq!(reviewers(&outcome.events), [GEMINI]);
    assert_eq!(ran(&crew, GEMINI, StepName::Review), 1);
    assert_eq!(ran(&crew, CODEX, StepName::Review), 0);
    let dropped: Vec<_> = said
        .iter()
        .filter(|note| note.contains("The review goes on without that reviewer"))
        .collect();
    assert_eq!(dropped.len(), 1, "{said:?}");
    assert!(
        dropped[0].starts_with("Nobody to fall back to as a reviewer"),
        "{dropped:?}"
    );
    Ok(())
}

/// Two reviewers failing at once never share a fallback: Qwen takes one place, and the other
/// goes on without it.
#[tokio::test]
async fn two_failing_reviewers_never_share_a_fallback() -> anyhow::Result<()> {
    let agents = [
        (CODEX, Agent::default()),
        (CLAUDE, Agent::overloaded(StepName::Review, 2)),
        (GEMINI, Agent::overloaded(StepName::Review, 2)),
        (QWEN, Agent::default()),
    ];
    let assignment = Assignment {
        worker: CODEX,
        reviewers: vec![CLAUDE, GEMINI],
    };
    let crew = crew_of(&agents, &assignment)?;
    // Two reviewers review blind, so a fallback's CLI must keep your plugins out, as OpenCode's
    // does here.
    crew.services.workers.isolating("opencode");
    let outcome = drive(&crew).await?;
    let said = notes(&outcome.events);
    assert_eq!(
        outcome.exit,
        EngineExit::Finished(RunOutcome::Merged),
        "{said:?}"
    );
    assert_eq!(ran(&crew, QWEN, StepName::Review), 1, "{said:?}");
    assert_eq!(reviewers(&outcome.events), [QWEN]);
    let dropped = said
        .iter()
        .filter(|note| note.contains("The review goes on without that reviewer"))
        .count();
    assert_eq!(dropped, 1, "{said:?}");
    Ok(())
}

/// `[fallback.reviewer] together = 2`: a reviewer that's out as its review starts has two
/// fallbacks review at once in its place, and the transcript says how their reviews count.
#[tokio::test]
async fn two_fallbacks_review_at_once_in_the_place_of_a_reviewer_thats_out() -> anyhow::Result<()> {
    let agents = [
        (CODEX, Agent::default()),
        (CLAUDE, Agent::default()),
        (GEMINI, Agent::default()),
        (QWEN, Agent::default()),
    ];
    let mut crew = crew_of(&agents, &Assignment::default())?;
    let reviewers_table = crew
        .settings
        .fallback
        .roles
        .entry(CrewRole::Reviewer)
        .or_default();
    reviewers_table.together = Some(2);
    let antigravity = argonauts_core::base::names::AgentId("antigravity-cli".to_owned());
    crew.services
        .health
        .set(&antigravity, HealthState::Down, Some("server errors"));
    let outcome = drive(&crew).await?;
    let said = notes(&outcome.events);
    assert_eq!(
        outcome.exit,
        EngineExit::Finished(RunOutcome::Merged),
        "{said:?}"
    );
    assert_eq!(ran(&crew, GEMINI, StepName::Review), 0, "{said:?}");
    assert_eq!(reviewers(&outcome.events), [CLAUDE, QWEN], "{said:?}");
    let shared: Vec<_> = said
        .iter()
        .filter(|note| note.contains("both review"))
        .collect();
    assert_eq!(shared.len(), 1, "{said:?}");
    assert!(
        shared[0].starts_with("Haiku on Claude Code and Qwen3 Coder on OpenCode both review")
            && shared[0].ends_with(
                "in place of Gemini 3.6 Flash Low on agy · every finding from each \
                 is kept · /fallback changes this"
            ),
        "{said:?}"
    );
    Ok(())
}

/// A reviewer that is also a fallback for the reviewers, as a settings file may well name it,
/// never takes the place of one that's out beside its own: it reviews once, and the fallback
/// that's left takes the place alone.
#[tokio::test]
async fn a_reviewer_listed_as_a_fallback_never_reviews_twice() -> anyhow::Result<()> {
    let agents = [
        (CODEX, Agent::default()),
        (CLAUDE, Agent::default()),
        (GEMINI, Agent::default()),
        (QWEN, Agent::default()),
    ];
    let assignment = Assignment {
        worker: CODEX,
        reviewers: vec![GEMINI, CLAUDE],
    };
    let mut crew = crew_of(&agents, &assignment)?;
    let reviewers_table = crew
        .settings
        .fallback
        .roles
        .entry(CrewRole::Reviewer)
        .or_default();
    reviewers_table.together = Some(2);
    reviewers_table.order = [CLAUDE, QWEN]
        .into_iter()
        .map(|worker| Ok(FallbackEntry::Model(worker.parse()?)))
        .collect::<anyhow::Result<_>>()?;
    // Two reviewers review blind, so whoever takes a place must keep your plugins out.
    crew.services.workers.isolating("claude-code");
    crew.services.workers.isolating("opencode");
    let antigravity = argonauts_core::base::names::AgentId("antigravity-cli".to_owned());
    crew.services
        .health
        .set(&antigravity, HealthState::Down, Some("server errors"));
    let outcome = drive(&crew).await?;
    let said = notes(&outcome.events);
    assert_eq!(
        outcome.exit,
        EngineExit::Finished(RunOutcome::Merged),
        "{said:?}"
    );
    assert_eq!(ran(&crew, GEMINI, StepName::Review), 0, "{said:?}");
    assert_eq!(ran(&crew, CLAUDE, StepName::Review), 1, "{said:?}");
    assert_eq!(ran(&crew, QWEN, StepName::Review), 1, "{said:?}");
    let mut reviewed = reviewers(&outcome.events);
    reviewed.sort();
    assert_eq!(reviewed, [CLAUDE, QWEN], "{said:?}");
    Ok(())
}

/// A reviewer that stops midway hands its place to one fallback as ever, and with `together = 2`
/// a second joins it, so two reviews stand for the reviewer.
#[tokio::test]
async fn a_second_fallback_joins_the_one_that_took_a_reviewers_place_midway() -> anyhow::Result<()>
{
    let agents = [
        (CODEX, Agent::default()),
        (CLAUDE, Agent::default()),
        (GEMINI, Agent::overloaded(StepName::Review, 2)),
        (QWEN, Agent::default()),
    ];
    let mut crew = crew_of(&agents, &Assignment::default())?;
    let reviewers_table = crew
        .settings
        .fallback
        .roles
        .entry(CrewRole::Reviewer)
        .or_default();
    reviewers_table.together = Some(2);
    let outcome = drive(&crew).await?;
    let said = notes(&outcome.events);
    assert_eq!(
        outcome.exit,
        EngineExit::Finished(RunOutcome::Merged),
        "{said:?}"
    );
    assert_eq!(reviewers(&outcome.events), [CLAUDE, QWEN], "{said:?}");
    assert_eq!(
        said.iter()
            .filter(|note| note.contains("both review"))
            .count(),
        1,
        "{said:?}"
    );
    Ok(())
}

/// A blind reviewer's place never goes to an agent whose CLI can't keep your plugins out, so with
/// none that can, the review goes on without that reviewer and says why.
#[tokio::test]
async fn a_blind_reviewers_fallback_must_keep_your_plugins_out() -> anyhow::Result<()> {
    let agents = [
        (CODEX, Agent::default()),
        (CLAUDE, Agent::default()),
        (GEMINI, Agent::overloaded(StepName::Review, 2)),
        (QWEN, Agent::default()),
    ];
    let assignment = Assignment {
        worker: CODEX,
        reviewers: vec![CLAUDE, GEMINI],
    };
    let crew = crew_of(&agents, &assignment)?;
    let outcome = drive(&crew).await?;
    let said = notes(&outcome.events);
    assert_eq!(
        outcome.exit,
        EngineExit::Finished(RunOutcome::Merged),
        "{said:?}"
    );
    assert_eq!(ran(&crew, QWEN, StepName::Review), 0, "{said:?}");
    assert_eq!(reviewers(&outcome.events), [CLAUDE]);
    assert!(
        said.iter().any(|note| note.contains(
            "no other agent that keeps your plugins out can take the role. The review goes on \
             without that reviewer."
        )),
        "{said:?}"
    );
    Ok(())
}

/// A blind reviewer whose CLI loaded your plugins anyway is stopped and never asked again: its
/// place goes to an agent of another CLI that keeps them out, and the fallback line says what
/// loaded.
#[tokio::test]
async fn a_reviewer_that_loaded_your_plugins_hands_its_place_to_one_that_keeps_them_out()
-> anyhow::Result<()> {
    let agents = [
        (CODEX, Agent::default()),
        (CLAUDE, Agent::default()),
        (GEMINI, Agent::default()),
        (QWEN, Agent::default()),
    ];
    let assignment = Assignment {
        worker: CODEX,
        reviewers: vec![CLAUDE, GEMINI],
    };
    let crew = crew_of(&agents, &assignment)?;
    let claude = crew.services.workers.fake(CLAUDE).context("enlisted")?;
    let answers = agent(Agent::default(), assignment.clone());
    claude.respond(move |spec| {
        if spec.step == Some(StepName::Review) {
            return FakeSession::ended(EndReason::IsolationBroken {
                plugins: vec!["memory-keeper".to_owned()],
                skills: Vec::new(),
            });
        }
        answers(spec)
    });
    // Claude Code's and OpenCode's CLIs keep your plugins out; agy can't.
    crew.services.workers.isolating("claude-code");
    crew.services.workers.isolating("opencode");
    let outcome = drive(&crew).await?;
    let said = notes(&outcome.events);
    assert_eq!(
        outcome.exit,
        EngineExit::Finished(RunOutcome::Merged),
        "{said:?}"
    );
    assert_eq!(
        ran(&crew, CLAUDE, StepName::Review),
        1,
        "never asked again: {said:?}"
    );
    assert_eq!(ran(&crew, QWEN, StepName::Review), 1, "{said:?}");
    assert!(
        said.iter().any(|note| note
            .contains("(it loaded your plugin memory-keeper, which a blind role mustn't see), so")
            && note.contains("it fell back to Qwen3 Coder on OpenCode")),
        "{said:?}"
    );
    Ok(())
}

/// An overload on the worker's build goes again on its own session, in autopilot, and the task
/// carries on with its worker.
#[tokio::test]
async fn an_overload_on_build_goes_again_on_its_own_session() -> anyhow::Result<()> {
    let agents = [
        (CODEX, Agent::overloaded(StepName::Build, 1)),
        (CLAUDE, Agent::default()),
        (GEMINI, Agent::default()),
    ];
    let crew = crew_of(&agents, &Assignment::default())?;
    let outcome = autopilot(&crew, RunMode::Crew).await?;
    let said = notes(&outcome.events);
    assert_eq!(
        outcome.exit,
        EngineExit::Finished(RunOutcome::Merged),
        "{said:?}"
    );
    let builds = sessions_of(&outcome.events, CODEX, StepName::Build);
    assert_eq!(builds.len(), 1, "{builds:?}");
    assert_eq!(builds[0].1, 2, "asked again on its own session");
    assert_eq!(ran(&crew, CLAUDE, StepName::Build), 0);
    assert!(!said.iter().any(|note| note.contains("log in")), "{said:?}");
    assert!(!asked_you(&outcome.events));
    Ok(())
}

/// An overload on the worker's build twice: in autopilot the task
/// goes on to the next agent, nothing says to log in, and nothing waits.
#[tokio::test]
async fn an_overload_on_build_twice_hands_the_task_on_in_autopilot() -> anyhow::Result<()> {
    let agents = [
        (CODEX, Agent::overloaded(StepName::Build, 2)),
        (CLAUDE, Agent::default()),
        (GEMINI, Agent::default()),
    ];
    let crew = crew_of(&agents, &Assignment::default())?;
    let outcome = autopilot_with(&crew, RunMode::Crew, true).await?;
    let said = notes(&outcome.events);
    assert_eq!(
        outcome.exit,
        EngineExit::Finished(RunOutcome::Merged),
        "{said:?}"
    );
    assert_eq!(ran(&crew, CODEX, StepName::Build), 2);
    assert!(ran(&crew, CLAUDE, StepName::Build) >= 1, "{said:?}");
    assert!(!said.iter().any(|note| note.contains("log in")), "{said:?}");
    let waited = outcome.events.iter().any(|event| {
        matches!(
            &event.kind,
            RunEventKind::Run(RunLifecycle::Waiting {
                reason: WaitReason::WaitingForAgent { .. },
                ..
            })
        )
    });
    assert!(!waited, "{said:?}");
    assert!(!asked_you(&outcome.events));
    Ok(())
}

/// With `[fallback.worker] together = 2`, the worker's failure is said with its retry, and when
/// it stops again two fallbacks take the task together: the line that says so opens with
/// what stopped it still, and the one after says who passed first.
#[tokio::test]
async fn two_fallbacks_take_a_task_together_after_the_retry_thats_said() -> anyhow::Result<()> {
    let agents = [
        (CODEX, Agent::overloaded(StepName::Build, 2)),
        (CLAUDE, Agent::default()),
        (GEMINI, Agent::default()),
        (QWEN, Agent::default()),
    ];
    let mut crew = crew_of(&agents, &Assignment::default())?;
    let workers_table = crew
        .settings
        .fallback
        .roles
        .entry(CrewRole::Worker)
        .or_default();
    workers_table.together = Some(2);
    let outcome = autopilot_with(&crew, RunMode::Crew, true).await?;
    let said = notes(&outcome.events);
    assert_eq!(
        outcome.exit,
        EngineExit::Finished(RunOutcome::Merged),
        "{said:?}"
    );
    let at = |words: &str| said.iter().position(|note| note.contains(words));
    let retry = at("is overloaded · trying again")
        .with_context(|| format!("no line on the retry: {said:?}"))?;
    let both = at("both take").with_context(|| format!("no line on who takes it: {said:?}"))?;
    let passed = at("passed first").with_context(|| format!("no line on who passed: {said:?}"))?;
    assert!(retry < both && both < passed, "{said:?}");
    assert!(
        said[both].starts_with("Still overloaded · ")
            && said[both].ends_with("· the first to pass the checks wins · /fallback changes this"),
        "{}",
        said[both]
    );
    assert!(said[passed].ends_with("'s try stopped"), "{}", said[passed]);
    Ok(())
}

/// With nobody to take the task, autopilot never asks and never waits on a timer: the run stops
/// fully, says why, and only `/resume` carries it on, with no login asked of you while the CLI
/// says it's logged in.
#[tokio::test]
async fn nobody_to_take_the_task_stops_the_run_in_autopilot() -> anyhow::Result<()> {
    let agents = [(CODEX, Agent::overloaded(StepName::Build, 2))];
    let crew = crew_of(&agents, &Assignment::default())?;
    let outcome = autopilot_with(&crew, RunMode::Captain, true).await?;
    let said = notes(&outcome.events);
    let EngineExit::Waiting(WaitReason::NoFallback { until, stop, .. }) = &outcome.exit else {
        panic!("{:?}: {said:?}", outcome.exit);
    };
    assert_eq!(*until, None, "it never tries again on its own: {said:?}");
    let stop = stop.as_ref().context("the stop says why")?;
    assert_eq!(stop.why, "nothing else can take \"Add due dates\"");
    assert_eq!(
        stop.agents,
        ["\"Add due dates\" · Codex's session ended early (an agent error)"]
    );
    assert_eq!(stop.waits_for, ["Codex"]);
    assert!(!said.iter().any(|note| note.contains("log in")), "{said:?}");
    assert!(
        !said
            .iter()
            .any(|note| note.contains("tries again on its own")),
        "{said:?}"
    );
    assert!(!asked_you(&outcome.events));
    Ok(())
}

/// An agent whose CLI says it's logged out is out for the run: after its first failed session,
/// its jobs go straight to a fallback, and one line says to log in.
#[tokio::test]
async fn an_agent_whose_cli_says_its_logged_out_is_out_for_the_run() -> anyhow::Result<()> {
    let codex = Agent {
        always_signed_out: true,
        logged_out: true,
        ..Agent::default()
    };
    let agents = [
        (CODEX, codex),
        (CLAUDE, Agent::default()),
        (GEMINI, Agent::default()),
    ];
    let assignment = Assignment {
        worker: CLAUDE,
        reviewers: vec![GEMINI],
    };
    let crew = crew_of(&agents, &assignment)?;
    let outcome = drive(&crew).await?;
    let said = notes(&outcome.events);
    assert_eq!(
        outcome.exit,
        EngineExit::Finished(RunOutcome::Merged),
        "{said:?}"
    );
    let codex_steps: Vec<_> = crew
        .services
        .workers
        .fake(CODEX)
        .map(|fake| {
            fake.specs()
                .iter()
                .map(|spec| (spec.step, spec.role))
                .collect()
        })
        .unwrap_or_default();
    assert!(codex_steps.len() <= 2, "{codex_steps:?}: {said:?}");
    let fallbacks: Vec<_> = said
        .iter()
        .filter(|note| note.starts_with("GPT 5.6 Luna on Codex"))
        .collect();
    assert_eq!(
        fallbacks,
        [
            &"GPT 5.6 Luna on Codex says it's logged out: log in again; Haiku on Claude Code takes \
           its roles meanwhile."
        ],
        "{said:?}"
    );
    Ok(())
}

/// A model whose agent is down is never started: its jobs go straight to a fallback.
#[tokio::test]
async fn an_agent_whose_agent_is_down_is_never_started() -> anyhow::Result<()> {
    let assignment = Assignment {
        worker: CLAUDE,
        reviewers: vec![GEMINI],
    };
    let agents = [
        (CODEX, Agent::default()),
        (CLAUDE, Agent::default()),
        (GEMINI, Agent::default()),
    ];
    let crew = crew_of(&agents, &assignment)?;
    let codex = argonauts_core::base::names::AgentId("codex".to_owned());
    crew.services
        .health
        .set(&codex, HealthState::Down, Some("server errors"));
    let outcome = drive(&crew).await?;
    let said = notes(&outcome.events);
    assert_eq!(
        outcome.exit,
        EngineExit::Finished(RunOutcome::Merged),
        "{said:?}"
    );
    let codex_sessions = crew
        .services
        .workers
        .fake(CODEX)
        .map_or(0, |fake| fake.specs().len());
    assert_eq!(codex_sessions, 0, "{said:?}");
    let fallbacks: Vec<_> = said
        .iter()
        .filter(|note| note.starts_with("GPT 5.6 Luna on Codex"))
        .collect();
    assert_eq!(
        fallbacks,
        [
            &"GPT 5.6 Luna on Codex is out (its agent is down), so Haiku on Claude Code takes its \
           roles meanwhile."
        ],
        "{said:?}"
    );
    Ok(())
}

/// A main agent out of the run has its later asks in one step go to the same fallback: they're one
/// job, so nobody else is brought in.
#[tokio::test]
async fn a_cli_that_crashed_twice_is_not_started_again_for_later_jobs() -> anyhow::Result<()> {
    let codex = Agent {
        always_crashes: true,
        ..Agent::default()
    };
    let agents = [
        (CODEX, codex),
        (CLAUDE, Agent::default()),
        (GEMINI, Agent::default()),
    ];
    let assignment = Assignment {
        worker: CLAUDE,
        reviewers: vec![GEMINI],
    };
    let crew = crew_of(&agents, &assignment)?;
    // In autopilot nothing waits for you, so the run never resumes and its memory holds.
    let outcome = autopilot(&crew, RunMode::Crew).await?;
    let said = notes(&outcome.events);
    assert!(!asked_you(&outcome.events), "{said:?}");
    // One try and its retry on the first job; after that nothing waits on Codex again.
    let retries = said
        .iter()
        .filter(|note| note.contains("Codex crashed · trying again"))
        .count();
    assert_eq!(retries, 1, "{said:?}");
    assert_eq!(
        finished(&outcome.events),
        Some(RunOutcome::Merged),
        "{said:?}"
    );
    Ok(())
}

/// A CLI that crashed twice in a row is never another agent's fallback later in the run.
#[tokio::test]
async fn a_cli_that_keeps_crashing_is_never_a_fallback() -> anyhow::Result<()> {
    let codex = Agent {
        always_crashes: true,
        ..Agent::default()
    };
    let agents = [
        (CODEX, codex),
        (CLAUDE, Agent::default()),
        (GEMINI, Agent::overloaded(StepName::Review, 2)),
    ];
    let assignment = Assignment {
        worker: CLAUDE,
        reviewers: vec![GEMINI],
    };
    let crew = crew_of(&agents, &assignment)?;
    let outcome = autopilot(&crew, RunMode::Crew).await?;
    let said = notes(&outcome.events);
    let codex_reviews = crew.services.workers.fake(CODEX).map_or(0, |fake| {
        fake.specs()
            .iter()
            .filter(|spec| spec.step == Some(StepName::Review))
            .count()
    });
    assert_eq!(codex_reviews, 0, "{said:?}");
    Ok(())
}

/// A task whose agent ended without a report is retried on another provider, but never on a
/// CLI that crashed twice in a row earlier in the run.
#[tokio::test]
async fn a_retry_on_another_provider_never_starts_a_cli_that_keeps_crashing() -> anyhow::Result<()>
{
    let codex = Agent {
        always_crashes: true,
        ..Agent::default()
    };
    let claude = Agent {
        builds_without_report: true,
        ..Agent::default()
    };
    let agents = [(CODEX, codex), (CLAUDE, claude), (GEMINI, Agent::default())];
    let assignment = Assignment {
        worker: CLAUDE,
        reviewers: vec![GEMINI],
    };
    let crew = crew_of(&agents, &assignment)?;
    let offered = |name: &str| -> anyhow::Result<Candidate> {
        Ok(Candidate {
            worker: name.parse()?,
            plan_usage_left: None,
            plan_usage_text: String::new(),
            track_record: None,
            cost: CostEstimate::Unknown,
            needs_confirm: false,
            may_run_out: false,
        })
    };
    crew.services
        .fallback
        .set_candidates(vec![offered(CODEX)?, offered(GEMINI)?]);
    let outcome = autopilot(&crew, RunMode::Crew).await?;
    let said = notes(&outcome.events);
    let codex_builds = crew.services.workers.fake(CODEX).map_or(0, |fake| {
        fake.specs()
            .iter()
            .filter(|spec| spec.step == Some(StepName::Build))
            .count()
    });
    assert_eq!(codex_builds, 0, "{said:?}");
    let needs_you = outcome.events.iter().any(|event| {
        matches!(
            &event.kind,
            RunEventKind::Task(TaskEvent::State { reason, .. })
                if reason.contains("no agent of another provider can take it")
        )
    });
    assert!(needs_you, "{said:?}");
    Ok(())
}

#[tokio::test]
async fn a_fallback_keeps_the_job_for_the_same_agents_later_asks() -> anyhow::Result<()> {
    let codex = Agent {
        always_signed_out: true,
        logged_out: true,
        ..Agent::default()
    };
    let agents = [
        (CODEX, codex),
        (CLAUDE, Agent::designs(vec![Design::Rejected])),
        (GEMINI, Agent::default()),
    ];
    let assignment = Assignment {
        worker: CLAUDE,
        reviewers: vec![GEMINI],
    };
    let crew = crew_of(&agents, &assignment)?;
    let outcome = drive(&crew).await?;
    let said = notes(&outcome.events);
    assert_eq!(
        outcome.exit,
        EngineExit::Finished(RunOutcome::Merged),
        "{said:?}"
    );
    // Claude Code designs in Codex's place each time, the brief turned down and the one asked
    // for after it in the same step included, and agy never does.
    assert!(designs_after_understand(&crew, CLAUDE) >= 2, "{said:?}");
    assert_eq!(designs_after_understand(&crew, GEMINI), 0, "{said:?}");
    Ok(())
}

/// Another model on Claude Code, beside the crew's.
const SONNET: &str = "claude-code/sonnet";

/// A crew whose one reviewer, Claude Code's Haiku, meets an overloaded service on its first
/// `overloads` reviews, with a second Claude Code model listed last.
fn crew_with_a_second_claude_model(overloads: usize) -> anyhow::Result<Crew> {
    let agents = [
        (CODEX, Agent::default()),
        (CLAUDE, Agent::overloaded(StepName::Review, overloads)),
        (GEMINI, Agent::default()),
        (SONNET, Agent::default()),
    ];
    let assignment = Assignment {
        worker: CODEX,
        reviewers: vec![CLAUDE],
    };
    crew_of(&agents, &assignment)
}

/// Claude Code's Haiku built the task and Codex, its one reviewer, stays overloaded. Only
/// Sonnet, from the builder's own provider, is left. On autopilot Sonnet reviews, one line says
/// so, and the merge is kept for you instead of going ahead.
#[tokio::test]
async fn on_autopilot_the_builders_provider_reviews_and_the_merge_waits_for_you()
-> anyhow::Result<()> {
    let agents = [
        (CLAUDE, Agent::default()),
        (CODEX, Agent::overloaded(StepName::Review, 9)),
        (SONNET, Agent::default()),
    ];
    let assignment = Assignment {
        worker: CLAUDE,
        reviewers: vec![CODEX],
    };
    let crew = crew_of(&agents, &assignment)?;
    let outcome = autopilot(&crew, RunMode::Crew).await?;
    let said = notes(&outcome.events);
    assert_eq!(reviewers(&outcome.events), [SONNET], "{said:?}");
    let stood_in: Vec<_> = said
        .iter()
        .filter(|note| note.contains("nobody outside the builder's provider is left"))
        .collect();
    assert_eq!(stood_in.len(), 1, "{said:?}");
    assert!(
        stood_in[0].ends_with(&format!(
            "so {} takes a reviewer on autopilot · its work stays on its branch for you",
            display_name(&SONNET.parse()?)
        )),
        "{stood_in:?}"
    );
    let merges: Vec<_> = outcome
        .events
        .iter()
        .filter_map(|event| match &event.kind {
            RunEventKind::Approval(ApprovalEvent::Decided(decided)) => Some(decided),
            _ => None,
        })
        .filter(|decided| decided.by == Party::Autopilot)
        .collect();
    assert!(
        merges.iter().all(|decided| !matches!(
            decided.action,
            argonauts_core::runs::events::approval::AnswerAction::Approve
        )),
        "{merges:?}"
    );
    // Autopilot's line names the fallback and the task it reviewed.
    let named = format!(
        "{}, a fallback from its builder's own provider, reviewed ",
        display_name(&SONNET.parse()?)
    );
    assert!(
        said.iter()
            .any(|note| note.starts_with("Autopilot answered ") && note.contains(&named)),
        "{said:?}"
    );
    assert_ne!(
        outcome.exit,
        EngineExit::Finished(RunOutcome::Merged),
        "{said:?}"
    );
    Ok(())
}

/// Another Claude Code model, for a list of agents to pick from.
const OPUS: &str = "claude-code/opus";

/// Claude Code's Haiku builds the task and Codex, its one reviewer, stays overloaded, with
/// only Claude Code's `others` left to review in its place.
fn crew_left_to_the_builders_provider(others: &[&str]) -> anyhow::Result<Crew> {
    let mut agents = vec![
        (CLAUDE, Agent::default()),
        (CODEX, Agent::overloaded(StepName::Review, 9)),
    ];
    agents.extend(others.iter().map(|model| (*model, Agent::default())));
    let assignment = Assignment {
        worker: CLAUDE,
        reviewers: vec![CODEX],
    };
    crew_of(&agents, &assignment)
}

/// The questions about a job that only its builder's provider could take, oldest first.
fn builders_provider_questions(
    events: &[RunEvent],
) -> Vec<&argonauts_core::runs::events::approval::ApprovalOpened> {
    use argonauts_core::runs::events::approval::ApprovalPacket;
    events
        .iter()
        .filter_map(|event| match &event.kind {
            RunEventKind::Approval(ApprovalEvent::Opened(opened)) => Some(&**opened),
            _ => None,
        })
        .filter(|opened| {
            matches!(&opened.packet, ApprovalPacket::AgentDown(down) if down.kept_by.is_none())
        })
        .collect()
}

/// Outside autopilot nobody from the builder's own provider reviews without your say. The
/// review waits for Codex, one line says why, and the question offers to wait, recommended,
/// or to let Sonnet review with the merge left to you. Letting it, the run that wakes gives
/// Sonnet the review and goes on.
#[tokio::test]
async fn when_only_the_builders_provider_is_left_the_review_waits_and_asks() -> anyhow::Result<()> {
    use argonauts_core::runs::events::approval::{AnswerAction, ApprovalKind, ApprovalPacket};

    let crew = crew_left_to_the_builders_provider(&[SONNET])?;
    let waiting = drive(&crew).await?;
    let said = notes(&waiting.events);
    // It waits for Codex, not stopped: Codex coming back, or its time, tries it again.
    assert!(
        matches!(
            &waiting.exit,
            EngineExit::Waiting(WaitReason::WaitingForAgent { agent_id, until })
                if agent_id.0 == "codex" && until.is_some()
        ),
        "{:?} {said:?}",
        waiting.exit
    );
    assert_eq!(ran(&crew, SONNET, StepName::Review), 0, "{said:?}");
    let codex = display_name(&CODEX.parse()?);
    let waits = said
        .iter()
        .find(|note| note.starts_with(&format!("A reviewer waits for {codex}: ")))
        .context("one line says why the review waits")?;
    let haiku = argonauts_crew::roles::short_name(&CLAUDE.parse()?);
    assert!(
        waits.contains(&format!(
            ", and nobody outside the builder's provider is left to review {haiku}'s work."
        )),
        "{waits}"
    );

    let asked = builders_provider_questions(&waiting.events);
    let [question] = asked.as_slice() else {
        anyhow::bail!("one question, not {asked:?}");
    };
    assert_eq!(question.kind, ApprovalKind::AgentDown);
    // Titled by what stopped the review, which may be no outage at all.
    assert!(
        question.title.contains(" on Codex")
            && question.title.ends_with(" · Due dates waits for it"),
        "{}",
        question.title
    );
    let ApprovalPacket::AgentDown(down) = &question.packet else {
        anyhow::bail!("{:?}", question.packet);
    };
    assert_eq!(down.fits_all, Some(SONNET.parse()?));
    assert!(
        down.evidence.ends_with(&format!(
            ", and nobody outside the builder's provider is left to review {haiku}'s work"
        )),
        "{}",
        down.evidence
    );
    let offered: Vec<(&str, Option<&str>, bool)> = question
        .options
        .iter()
        .map(|option| {
            (
                option.label.as_str(),
                option.detail.as_deref(),
                option.recommended,
            )
        })
        .collect();
    let sonnet = display_name(&SONNET.parse()?);
    let let_sonnet = format!("Let {sonnet} review");
    assert_eq!(
        offered,
        [
            ("Wait for Codex", None, true),
            (let_sonnet.as_str(), Some("the merge is left to you"), false),
        ],
        "one agent left, so nothing else to pick from"
    );

    let (command, _reply) = support::answer(
        &waiting.events,
        ApprovalKind::AgentDown,
        AnswerAction::Candidate {
            worker: SONNET.parse()?,
        },
        None,
    )?;
    let woke = crew.resume(waiting.events.clone(), vec![command]).await?;
    let outcome = answer_tasks(&crew, woke).await?;
    let said = notes(&outcome.events);
    assert_eq!(reviewers(&outcome.events), [SONNET], "{said:?}");
    assert!(
        matches!(outcome.exit, EngineExit::Finished(_)),
        "{:?} {said:?}",
        outcome.exit
    );
    let merged_by: Vec<&Party> = outcome
        .events
        .iter()
        .filter_map(|event| match &event.kind {
            RunEventKind::Approval(ApprovalEvent::Decided(decided)) => Some(decided),
            _ => None,
        })
        .filter(|decided| decided.action == AnswerAction::Merge)
        .map(|decided| &decided.by)
        .collect();
    assert_eq!(merged_by, [&Party::User], "the merge is yours: {said:?}");
    Ok(())
}

/// Picking an agent lists every one that may review, each of the builder's provider saying
/// the merge is then yours, beside waiting. The one you pick reviews.
#[tokio::test]
async fn picking_an_agent_lists_every_one_that_may_review() -> anyhow::Result<()> {
    use argonauts_core::runs::events::approval::{AnswerAction, ApprovalKind};

    let crew = crew_left_to_the_builders_provider(&[SONNET, OPUS])?;
    let waiting = drive(&crew).await?;
    let said = notes(&waiting.events);
    let asked = builders_provider_questions(&waiting.events);
    let [first] = asked.as_slice() else {
        anyhow::bail!("one question, not {asked:?} {said:?}");
    };
    let labels: Vec<&str> = first
        .options
        .iter()
        .map(|option| option.label.as_str())
        .collect();
    assert_eq!(labels.len(), 3, "{labels:?}");
    assert_eq!(labels[2], "Pick an agent", "{labels:?}");

    let (command, _reply) = support::answer(
        &waiting.events,
        ApprovalKind::AgentDown,
        AnswerAction::OtherCandidates,
        None,
    )?;
    let woke = crew.resume(waiting.events.clone(), vec![command]).await?;
    let said = notes(&woke.events);
    let asked = builders_provider_questions(&woke.events);
    let pick = asked
        .last()
        .filter(|pick| pick.id != first.id)
        .with_context(|| format!("the list of agents is asked next: {said:?}"))?;
    assert!(
        pick.title.starts_with("Pick an agent for "),
        "{}",
        pick.title
    );
    let mut offered: Vec<(String, Option<&str>)> = pick
        .options
        .iter()
        .map(|option| (option.label.clone(), option.detail.as_deref()))
        .collect();
    let mut want = vec![("Wait for Codex".to_owned(), None)];
    for model in [SONNET, OPUS] {
        want.push((
            display_name(&model.parse()?),
            Some("the merge is left to you"),
        ));
    }
    offered[1..].sort();
    want[1..].sort();
    assert_eq!(offered, want);

    let (command, _reply) = support::answer(
        &woke.events,
        ApprovalKind::AgentDown,
        AnswerAction::Candidate {
            worker: OPUS.parse()?,
        },
        None,
    )?;
    let woke = crew.resume(woke.events.clone(), vec![command]).await?;
    let outcome = answer_tasks(&crew, woke).await?;
    let said = notes(&outcome.events);
    assert_eq!(reviewers(&outcome.events), [OPUS], "{said:?}");
    assert_eq!(ran(&crew, SONNET, StepName::Review), 0, "{said:?}");
    Ok(())
}

/// Waiting for Codex is your answer while nothing changes: the run that wakes on it tries
/// Codex again and asks nothing more.
#[tokio::test]
async fn waiting_for_the_asked_cli_asks_nothing_more() -> anyhow::Result<()> {
    use argonauts_core::runs::events::approval::{AnswerAction, ApprovalKind};

    let crew = crew_left_to_the_builders_provider(&[SONNET])?;
    let waiting = drive(&crew).await?;
    assert_eq!(builders_provider_questions(&waiting.events).len(), 1);
    let (command, _reply) = support::answer(
        &waiting.events,
        ApprovalKind::AgentDown,
        AnswerAction::Wait,
        None,
    )?;
    let woke = crew.resume(waiting.events.clone(), vec![command]).await?;
    let said = notes(&woke.events);
    assert_eq!(
        builders_provider_questions(&woke.events).len(),
        1,
        "{said:?}"
    );
    assert_eq!(ran(&crew, SONNET, StepName::Review), 0, "{said:?}");
    assert!(
        matches!(
            &woke.exit,
            EngineExit::Waiting(WaitReason::WaitingForAgent { agent_id, .. })
                if agent_id.0 == "codex"
        ),
        "{:?} {said:?}",
        woke.exit
    );
    Ok(())
}

/// What the run said about a workflow of its own, each with the workflow it is about. The
/// workflow's view shows the latest under its header.
fn workflow_notes(events: &[RunEvent]) -> Vec<(Option<&WorkflowId>, &str)> {
    events
        .iter()
        .filter_map(|event| match &event.kind {
            RunEventKind::Workflow(WorkflowEvent::Note(note)) => {
                Some((note.workflow.as_ref(), note.text.as_str()))
            }
            _ => None,
        })
        .collect()
}

/// The workflow whose step the session asked of `asked`, and run by another model, works for.
fn fallbacks_workflow<'e>(events: &'e [RunEvent], asked: &str) -> Option<&'e WorkflowId> {
    events.iter().find_map(|event| match &event.kind {
        RunEventKind::Session(SessionEvent::Started(started))
            if started
                .asked
                .as_ref()
                .is_some_and(|model| model.to_string() == asked) =>
        {
            started.workflow_step.as_ref().map(|step| &step.workflow)
        }
        _ => None,
    })
}

/// An overload is about the one model, so the reviewer's place may go to any other agent: the
/// next of the crew, which here is another CLI. With `same_cli` for reviewers it stays on
/// Claude Code, on its other model, and the line says why no other CLI was asked. The review
/// step's workflow says the same in one line, for its view: who stopped, who it fell back to,
/// and that `same_cli` asked for it only where the setting kept another CLI out.
#[tokio::test]
async fn a_reviewers_fallback_stays_on_its_cli_when_the_role_says_so() -> anyhow::Result<()> {
    let name = |model: &str| display_name(&model.parse().expect("a model"));
    let free = crew_with_a_second_claude_model(2)?;
    let outcome = drive(&free).await?;
    let said = notes(&outcome.events);
    assert_eq!(
        outcome.exit,
        EngineExit::Finished(RunOutcome::Merged),
        "{said:?}"
    );
    assert_eq!(reviewers(&outcome.events), [GEMINI], "{said:?}");
    let elsewhere = format!(
        "{} is overloaded · fell back to {}",
        name(CLAUDE),
        name(GEMINI)
    );
    assert_eq!(
        workflow_notes(&outcome.events),
        [(
            fallbacks_workflow(&outcome.events, CLAUDE),
            elsewhere.as_str()
        )],
        "{said:?}"
    );

    let mut staying = crew_with_a_second_claude_model(2)?;
    let reviewers_table = staying
        .settings
        .fallback
        .roles
        .entry(CrewRole::Reviewer)
        .or_default();
    reviewers_table.same_cli = Some(true);
    let outcome = drive(&staying).await?;
    let said = notes(&outcome.events);
    assert_eq!(
        outcome.exit,
        EngineExit::Finished(RunOutcome::Merged),
        "{said:?}"
    );
    assert_eq!(reviewers(&outcome.events), [SONNET], "{said:?}");
    assert_eq!(ran(&staying, GEMINI, StepName::Review), 0);
    let fell_back = said
        .iter()
        .find(|note| note.starts_with("Still overloaded · fell back to"))
        .context("one line says who took the review")?;
    assert!(
        fell_back.ends_with("\nsame_cli is on, so agy isn't asked"),
        "{fell_back}"
    );
    let stayed = format!(
        "{} is overloaded · fell back to {}, as same_cli asks",
        name(CLAUDE),
        name(SONNET)
    );
    let about = fallbacks_workflow(&outcome.events, CLAUDE);
    assert!(
        about.is_some(),
        "the review is a step of the run's workflow"
    );
    assert_eq!(
        workflow_notes(&outcome.events),
        [(about, stayed.as_str())],
        "{said:?}"
    );
    Ok(())
}

/// With `same_cli` and no other model on the CLI, nobody takes the reviewer's place: the run
/// waits for Claude Code rather than asking another CLI, and says what keeps it there.
#[tokio::test]
async fn with_same_cli_and_no_model_left_the_review_waits_for_its_cli() -> anyhow::Result<()> {
    let agents = [
        (CODEX, Agent::default()),
        (CLAUDE, Agent::overloaded(StepName::Review, 2)),
        (GEMINI, Agent::default()),
    ];
    let assignment = Assignment {
        worker: CODEX,
        reviewers: vec![CLAUDE],
    };
    let mut crew = crew_of(&agents, &assignment)?;
    crew.settings.fallback.same_cli = Some(true);
    let outcome = drive(&crew).await?;
    let said = notes(&outcome.events);
    assert!(
        !matches!(outcome.exit, EngineExit::Finished(RunOutcome::Merged)),
        "{said:?}"
    );
    assert_eq!(ran(&crew, GEMINI, StepName::Review), 0, "{said:?}");
    let stopped = stop_lines(&outcome.exit).context("the stop says why the review waits")?;
    assert!(
        stopped
            .iter()
            .any(|line| line.contains("fallback.same_cli keeps it on Claude Code")),
        "{stopped:?}"
    );
    Ok(())
}

/// Claude Code is down, and `same_cli` keeps its review there, so the run waits. The question
/// about Claude Code lists the review as a paused task, a reviewer's, and names the agent
/// that could take it. Answered either way that moves it, the run that wakes gives the review
/// to that agent and goes on to merge.
#[tokio::test]
async fn a_review_kept_on_a_cli_that_is_down_moves_when_you_say_so() -> anyhow::Result<()> {
    use argonauts_core::base::names::{AgentId, RoleName};
    use argonauts_core::runs::events::agent_status::FallbackEvent;
    use argonauts_core::runs::events::approval::{AnswerAction, ApprovalKind, ApprovalPacket};

    for answer in [AnswerAction::OneForAll, AnswerAction::PerTask] {
        let agents = [
            (CODEX, Agent::default()),
            (CLAUDE, Agent::default()),
            (GEMINI, Agent::default()),
        ];
        let assignment = Assignment {
            worker: CODEX,
            reviewers: vec![CLAUDE],
        };
        let mut crew = crew_of(&agents, &assignment)?;
        crew.settings.fallback.same_cli = Some(true);
        let claude = AgentId("claude-code".to_owned());
        let why = Some("two of its models are out");
        crew.services.health.set(&claude, HealthState::Down, why);

        let waiting = drive(&crew).await?;
        let said = notes(&waiting.events);
        assert!(
            matches!(
                waiting.exit,
                EngineExit::Waiting(WaitReason::NoFallback { .. })
            ),
            "{:?} {said:?}",
            waiting.exit
        );
        let asked = waiting
            .events
            .iter()
            .rev()
            .find_map(|event| match &event.kind {
                RunEventKind::Approval(ApprovalEvent::Opened(opened)) => match &opened.packet {
                    ApprovalPacket::AgentDown(down) => Some((opened.title.clone(), down.clone())),
                    _ => None,
                },
                _ => None,
            });
        let (title, down) = asked.context("the run asks about Claude Code")?;
        assert_eq!(
            title, "Claude Code is down · 1 task waits for it",
            "{said:?}"
        );
        assert_eq!(down.kept_by.as_deref(), Some("same_cli"));
        let roles: Vec<RoleName> = down.paused.iter().map(|paused| paused.role).collect();
        assert_eq!(roles, [RoleName::Reviewer], "{:?}", down.paused);
        assert_eq!(down.fits_all, Some(GEMINI.parse()?), "{said:?}");
        assert_eq!(ran(&crew, GEMINI, StepName::Review), 0, "{said:?}");

        let (command, _reply) = support::answer(
            &waiting.events,
            ApprovalKind::AgentDown,
            answer.clone(),
            None,
        )?;
        let woke = crew.resume(waiting.events.clone(), vec![command]).await?;
        let outcome = answer_tasks(&crew, woke).await?;
        let said = notes(&outcome.events);
        assert_eq!(
            outcome.exit,
            EngineExit::Finished(RunOutcome::Merged),
            "{answer:?} {said:?}"
        );
        assert_eq!(reviewers(&outcome.events), [GEMINI], "{answer:?} {said:?}");
        assert_eq!(ran(&crew, CLAUDE, StepName::Review), 0, "{answer:?}");
        let to: Vec<_> = outcome
            .events
            .iter()
            .filter_map(|event| match &event.kind {
                RunEventKind::Fallback(FallbackEvent::JobFreed { to, .. }) => Some(to.clone()),
                _ => None,
            })
            .collect();
        let chosen = (answer == AnswerAction::OneForAll)
            .then(|| GEMINI.parse())
            .transpose()?;
        assert_eq!(to, [chosen], "{answer:?}");
    }
    Ok(())
}

/// The run review's proposal is work the run can do without, and the run's end waits on it.
/// An agent that fails it isn't tried again and nobody takes its place: the review is noted as
/// failed after its one session, and the result stands.
#[tokio::test]
async fn a_failing_run_review_gets_one_try_and_no_fallback() -> anyhow::Result<()> {
    let agents = [
        (CODEX, Agent::overloaded(StepName::RunReview, 9)),
        (CLAUDE, Agent::default()),
        (GEMINI, Agent::default()),
    ];
    let mut crew = crew_of(&agents, &Assignment::default())?;
    crew.settings.evolution.propose_every = 1;
    crew.with_a_learned_workflow()?;
    let outcome = drive(&crew).await?;
    let said = notes(&outcome.events);
    assert_eq!(
        outcome.exit,
        EngineExit::Finished(RunOutcome::Merged),
        "{said:?}"
    );
    let tried = [CODEX, CLAUDE, GEMINI].map(|worker| ran(&crew, worker, StepName::RunReview));
    assert_eq!(tried, [1, 0, 0], "{said:?}");
    assert!(
        said.contains(&argonauts_core::base::log::RUN_REVIEW_FAILED),
        "{said:?}"
    );
    assert!(
        !said.iter().any(|note| note.contains("trying again")),
        "{said:?}"
    );
    Ok(())
}

/// With its agent already out, the run review asks nobody for a proposal and says so, where
/// it would otherwise have gone through every fallback before the run could end.
#[tokio::test]
async fn a_run_review_whose_agent_is_out_proposes_nothing() -> anyhow::Result<()> {
    let agents = [
        (CODEX, Agent::default()),
        (CLAUDE, Agent::default()),
        (GEMINI, Agent::default()),
    ];
    let mut crew = crew_of(&agents, &Assignment::default())?;
    crew.settings.evolution.propose_every = 1;
    crew.with_a_learned_workflow()?;
    let run_review = argonauts_core::base::names::RoleName::RunReview;
    crew.settings.roles.entry(run_review).or_default().models = vec![CLAUDE.parse()?];
    let claude = argonauts_core::base::names::AgentId("claude-code".to_owned());
    let down = Some("network errors on 2 sessions in 2m");
    crew.services.health.set(&claude, HealthState::Down, down);
    let outcome = drive(&crew).await?;
    let said = notes(&outcome.events);
    assert_eq!(
        outcome.exit,
        EngineExit::Finished(RunOutcome::Merged),
        "{said:?}"
    );
    let tried = [CODEX, CLAUDE, GEMINI].map(|worker| ran(&crew, worker, StepName::RunReview));
    assert_eq!(tried, [0, 0, 0], "{said:?}");
    assert!(
        said.contains(
            &"The run review proposed no change this time, since Haiku on Claude Code is out."
        ),
        "{said:?}"
    );
    Ok(())
}

/// The run's end waits on the run review's proposal, so its session may run only as long as
/// `evolution.propose_timeout` says. One the meter stops there leaves no proposal and one line
/// that says why: the run review still finishes, and nobody is asked in its place.
#[tokio::test]
async fn a_slow_run_review_is_stopped_and_proposes_nothing() -> anyhow::Result<()> {
    let agents = [
        (CODEX, Agent::default()),
        (CLAUDE, Agent::default()),
        (GEMINI, Agent::default()),
    ];
    let mut crew = crew_of(&agents, &Assignment::default())?;
    crew.settings.evolution.propose_every = 1;
    crew.with_a_learned_workflow()?;
    crew.settings.evolution.propose_timeout = Span::new("3m");
    let usual = agent(Agent::default(), Assignment::default());
    let codex = crew.services.workers.fake(CODEX).context("Codex is in")?;
    codex.respond(move |spec| match spec.step {
        Some(StepName::RunReview) => FakeSession::ended(EndReason::LimitHit {
            dimension: LimitDimension::Minutes,
        }),
        _ => usual(spec),
    });
    let outcome = drive(&crew).await?;
    let said = notes(&outcome.events);
    assert_eq!(
        outcome.exit,
        EngineExit::Finished(RunOutcome::Merged),
        "{said:?}"
    );
    let limits: Vec<Option<f64>> = codex
        .specs()
        .iter()
        .filter(|spec| spec.step == Some(StepName::RunReview))
        .map(|spec| spec.limits.max_minutes)
        .collect();
    assert_eq!(limits, [Some(3.0)], "one session, capped at the setting");
    let tried = [CLAUDE, GEMINI].map(|worker| ran(&crew, worker, StepName::RunReview));
    assert_eq!(tried, [0, 0], "{said:?}");
    assert!(
        said.contains(
            &"The run review proposed no change this time, since GPT 5.6 Luna on Codex took \
              longer than 3m."
        ),
        "{said:?}"
    );
    assert!(
        !said.contains(&argonauts_core::base::log::RUN_REVIEW_FAILED),
        "{said:?}"
    );
    Ok(())
}

/// `evolution.propose_timeout = "none"` leaves the proposal's session as long as it takes.
#[tokio::test]
async fn a_run_review_with_no_timeout_runs_uncapped() -> anyhow::Result<()> {
    let agents = [
        (CODEX, Agent::default()),
        (CLAUDE, Agent::default()),
        (GEMINI, Agent::default()),
    ];
    let mut crew = crew_of(&agents, &Assignment::default())?;
    crew.settings.evolution.propose_every = 1;
    crew.with_a_learned_workflow()?;
    crew.settings.evolution.propose_timeout = Span::new("none");
    let outcome = drive(&crew).await?;
    assert_eq!(outcome.exit, EngineExit::Finished(RunOutcome::Merged));
    let codex = crew.services.workers.fake(CODEX).context("Codex is in")?;
    let limits: Vec<Option<f64>> = codex
        .specs()
        .iter()
        .filter(|spec| spec.step == Some(StepName::RunReview))
        .map(|spec| spec.limits.max_minutes)
        .collect();
    assert_eq!(limits, [None]);
    Ok(())
}

/// `[fallback.retry] tries` is how many times the same model goes again: twice here, each said,
/// and the model answers on its third go, so nothing falls back.
#[tokio::test]
async fn the_same_model_tries_again_as_many_times_as_you_set() -> anyhow::Result<()> {
    let mut crew = crew(&[(CODEX, vec![Design::Overloaded, Design::Overloaded])])?;
    crew.settings.fallback.retry = Some(Retry {
        tries: 2,
        ..Retry::without_wait()
    });
    let outcome = drive(&crew).await?;
    let said = notes(&outcome.events);
    assert_eq!(
        outcome.exit,
        EngineExit::Finished(RunOutcome::Merged),
        "{said:?}"
    );
    let retries = said
        .iter()
        .filter(|note| note.contains("is overloaded · trying again"))
        .count();
    assert_eq!(retries, 2, "{said:?}");
    assert!(
        !said.iter().any(|note| note.contains("fell back to")),
        "{said:?}"
    );
    assert_eq!(designs_after_understand(&crew, CLAUDE), 0);
    Ok(())
}

/// With no retry set, the first failure goes to the next agent at once and no line says it
/// tries again.
#[tokio::test]
async fn with_no_retry_set_the_first_failure_falls_back_at_once() -> anyhow::Result<()> {
    let mut crew = crew(&[(CODEX, vec![Design::Overloaded])])?;
    crew.settings.fallback.retry = Some(Retry {
        tries: 0,
        ..Retry::without_wait()
    });
    let outcome = drive(&crew).await?;
    let said = notes(&outcome.events);
    assert_eq!(
        outcome.exit,
        EngineExit::Finished(RunOutcome::Merged),
        "{said:?}"
    );
    assert!(
        !said.iter().any(|note| note.contains("trying again")),
        "{said:?}"
    );
    assert_eq!(designs_after_understand(&crew, CLAUDE), 1, "{said:?}");
    Ok(())
}

/// A CLI that can't start is remembered while the run goes on: its first job finds out, and
/// every later job asked of it that somebody else may take goes straight to a fallback, so the
/// main agent's CLI is started once for all its workflow designs and the run finishes. A blind
/// role's job has nobody else to go to here, since no other agent of this crew keeps your plugins
/// out, so those still try the CLI they were asked of. A resume starts the count over, as it
/// does for a CLI that kept crashing, so the run goes through without a stop.
#[tokio::test]
async fn a_cli_that_wont_start_is_started_once_in_a_run() -> anyhow::Result<()> {
    let wont_start = Agent {
        never_starts: true,
        ..Agent::default()
    };
    let agents = [
        (CLAUDE, wont_start),
        (CODEX, Agent::default()),
        (GEMINI, Agent::default()),
    ];
    let assignment = Assignment {
        worker: CODEX,
        reviewers: vec![GEMINI],
    };
    let crew = crew_of(&agents, &assignment)?;
    let outcome = autopilot(&crew, RunMode::Crew).await?;
    let said = notes(&outcome.events);
    assert_eq!(
        outcome.exit,
        EngineExit::Finished(RunOutcome::Merged),
        "{said:?}"
    );
    let asked = crew.services.workers.fake(CLAUDE);
    let specs = asked.map(|fake| fake.specs()).unwrap_or_default();
    let steps: Vec<_> = specs
        .iter()
        .map(|spec| (spec.step, spec.role, spec.isolated))
        .collect();
    assert_eq!(ran(&crew, CLAUDE, StepName::DesignWorkflow), 1, "{steps:?}");
    // Every design after the first was asked of the same CLI, and went to Codex unstarted.
    assert!(ran(&crew, CODEX, StepName::DesignWorkflow) >= 2, "{said:?}");
    Ok(())
}

/// A Captain run works like the CLI you chose, so its review goes to nobody you didn't list.
/// On autopilot, with the reviewer's CLI unable to start, the built work is kept and the run
/// waits for that reviewer, trying again by itself: nothing is asked of you, and the third CLI,
/// though it could review, is never started.
#[tokio::test]
async fn on_autopilot_a_captain_run_waits_for_a_reviewer_that_wont_start() -> anyhow::Result<()> {
    let wont_start = Agent {
        never_starts: true,
        ..Agent::default()
    };
    let agents = [
        (CLAUDE, Agent::default()),
        (CODEX, wont_start),
        (GEMINI, Agent::default()),
    ];
    let mut crew = crew_of(&agents, &Assignment::default())?;
    crew.settings.captain.review = true;
    let outcome = autopilot_with(&crew, RunMode::Captain, true).await?;
    let said = notes(&outcome.events);
    match &outcome.exit {
        EngineExit::Waiting(WaitReason::NoFallback { detail, until, .. }) => {
            assert_eq!(detail, "nobody to fall back to as a reviewer");
            assert!(until.is_none(), "it stopped, with no wake set: {said:?}");
        }
        other => panic!("{other:?}: {said:?}"),
    }
    let stopped = stop_lines(&outcome.exit).unwrap_or_default();
    assert_eq!(
        stopped[0], "nothing else can take a reviewer",
        "{stopped:?}"
    );
    assert!(
        stopped[1].starts_with("a reviewer · Codex couldn't start"),
        "{stopped:?}"
    );
    assert_eq!(ran(&crew, CLAUDE, StepName::Build), 1, "{said:?}");
    assert_eq!(ran(&crew, GEMINI, StepName::Review), 0, "{said:?}");
    assert!(!asked_you(&outcome.events));
    let left_open = outcome.events.iter().any(|event| {
        matches!(
            &event.kind,
            RunEventKind::Approval(ApprovalEvent::Opened(_))
        )
    });
    assert!(!left_open, "no question waits for an answer: {said:?}");
    Ok(())
}

/// The same Captain run with its reviewer's CLI starting: on autopilot the reviewer of another
/// provider approves, and the run merges with nothing asked of you.
#[tokio::test]
async fn on_autopilot_a_captain_run_reviewed_on_another_cli_merges() -> anyhow::Result<()> {
    let agents = [
        (CLAUDE, Agent::default()),
        (CODEX, Agent::default()),
        (GEMINI, Agent::default()),
    ];
    let mut crew = crew_of(&agents, &Assignment::default())?;
    crew.settings.captain.review = true;
    let outcome = autopilot_with(&crew, RunMode::Captain, true).await?;
    let said = notes(&outcome.events);
    assert_eq!(
        outcome.exit,
        EngineExit::Finished(RunOutcome::Merged),
        "{said:?}"
    );
    assert_eq!(ran(&crew, CLAUDE, StepName::Build), 1, "{said:?}");
    assert_eq!(ran(&crew, CODEX, StepName::Review), 1, "{said:?}");
    assert_eq!(ran(&crew, CLAUDE, StepName::Review), 0, "{said:?}");
    assert!(!asked_you(&outcome.events));
    Ok(())
}

/// On autopilot, with a worker whose build keeps ending without a report and no other CLI able
/// to start, the task gets one more try by the same agent, told what stopped it, and is then
/// put off with its work kept. The run ends saying nothing merged and why, each line naming the
/// task by its title, and no question is left for you.
#[tokio::test]
async fn on_autopilot_a_task_nobody_else_can_take_is_tried_again_then_deferred()
-> anyhow::Result<()> {
    let wont_start = || Agent {
        never_starts: true,
        ..Agent::default()
    };
    let fails = Agent {
        builds_without_report: true,
        ..Agent::default()
    };
    let agents = [
        (CLAUDE, fails),
        (CODEX, wont_start()),
        (GEMINI, wont_start()),
    ];
    let assignment = Assignment {
        worker: CLAUDE,
        reviewers: vec![CODEX],
    };
    let crew = crew_of(&agents, &assignment)?;
    let outcome = autopilot_with(&crew, RunMode::Crew, true).await?;
    let said = notes(&outcome.events);
    assert_eq!(
        outcome.exit,
        EngineExit::Finished(RunOutcome::Failed),
        "{said:?}"
    );
    let about_the_task: Vec<&&str> = said
        .iter()
        .filter(|note| note.contains("Due dates"))
        .collect();
    assert_eq!(
        about_the_task,
        [
            &"Autopilot answered \"Due dates needs you\" with \"Give it another try\" instead of \
              asking you, since no fallback from a provider that hasn't tried the task is left.",
            &"Autopilot answered \"Due dates needs you\" with \"Defer it\" instead of asking \
              you, since no fallback from a provider that hasn't tried the task is left.",
            &"Nothing was merged, since no task passed: \"Due dates\" was deferred (autopilot \
              deferred it).",
            // The run's end carries the same reason.
            &"Nothing was merged, since no task passed: \"Due dates\" was deferred (autopilot \
              deferred it).",
        ],
        "{said:#?}"
    );
    assert_eq!(ran(&crew, CLAUDE, StepName::Build), 2, "{said:?}");
    assert_eq!(ran(&crew, CODEX, StepName::Build), 0, "{said:?}");
    assert!(!asked_you(&outcome.events));
    Ok(())
}

/// A council run on autopilot where only one CLI can start: that agent takes the task the
/// others couldn't build, and then nobody is left to review it, since a task is never reviewed
/// by its own author. The run keeps the work and waits, trying the reviewers again by itself.
#[tokio::test]
async fn on_autopilot_a_council_run_waits_for_a_reviewer_who_isnt_the_author() -> anyhow::Result<()>
{
    let wont_start = || Agent {
        never_starts: true,
        ..Agent::default()
    };
    let agents = [
        (CLAUDE, Agent::default()),
        (CODEX, wont_start()),
        (GEMINI, wont_start()),
    ];
    let mut crew = crew_of(&agents, &Assignment::default())?;
    crew.settings.workflow = "council".to_owned();
    crew.settings.workflows.orchestrator_designs = false.into();
    let outcome = autopilot_with(&crew, RunMode::Crew, true).await?;
    let said = notes(&outcome.events);
    match &outcome.exit {
        EngineExit::Waiting(WaitReason::NoFallback { detail, until, .. }) => {
            assert_eq!(detail, "nobody to fall back to as a reviewer");
            assert!(until.is_none(), "it stopped, with no wake set: {said:?}");
        }
        other => panic!("{other:?}: {said:?}"),
    }
    assert_eq!(ran(&crew, CLAUDE, StepName::Build), 1, "{said:?}");
    assert_eq!(ran(&crew, CLAUDE, StepName::Review), 0, "{said:?}");
    let stopped = stop_lines(&outcome.exit).unwrap_or_default();
    assert_eq!(
        stopped.first().map(String::as_str),
        Some("nothing else can take a reviewer"),
        "{said:?}"
    );
    assert!(!asked_you(&outcome.events));
    Ok(())
}

/// What OpenCode's start says with no model set, known before anything runs.
const NO_MODEL: &str = "OpenCode has no model set, and its own default runs on a hosted service, \
                        so the code would leave this machine";
const OPENCODE: &str = "opencode";
const OPENCODE_ACP: &str = "opencode-acp";
/// A CLI outside the crew that starts, from a fifth provider.
const SPARE: &str = "kimi/kimi-k2";

/// A council run of Claude Code, Codex, and agy, whose CLI can't start, with OpenCode
/// and OpenCode over ACP installed but no model set for either, and `spare` installed too. Each
/// of those keeps your plugins out, so it may take a blind job such as a draft.
fn council_with_opencode_unset(spare: Option<&str>) -> anyhow::Result<Crew> {
    let wont_start = Agent {
        never_starts: true,
        ..Agent::default()
    };
    let agents = [
        (CLAUDE, Agent::default()),
        (CODEX, Agent::default()),
        (GEMINI, wont_start),
    ];
    let mut crew = crew_of(&agents, &Assignment::default())?;
    let answers = || agent(Agent::default(), Assignment::default());
    for opencode in [OPENCODE, OPENCODE_ACP] {
        let refusing = FakeRunner::cli(opencode, "opencode")
            .refusing_to_start(NO_MODEL)
            .respond_with(answers());
        crew.services.workers.add(refusing);
        crew.services.workers.isolating(opencode);
    }
    if let Some(spare) = spare {
        crew.services
            .workers
            .add(FakeRunner::cli(spare, "moonshot").respond_with(answers()));
        crew.services.workers.isolating("kimi");
    }
    crew.settings.workflow = "base".to_owned();
    crew.settings.workflows.orchestrator_designs = false.into();
    crew.settings.workflows.adapt_at_phase_ends = false;
    Ok(crew)
}

/// Each session `asked` was asked for that another agent ran: its role and who ran it.
fn taken_from(events: &[RunEvent], asked: &str) -> Vec<(RoleName, String)> {
    let mut taken: Vec<(RoleName, String)> = events
        .iter()
        .filter_map(|event| match &event.kind {
            RunEventKind::Session(SessionEvent::Started(started))
                if started
                    .asked
                    .as_ref()
                    .is_some_and(|by| by.to_string() == asked)
                    && started.worker.to_string() != asked =>
            {
                Some((started.role, started.worker.to_string()))
            }
            _ => None,
        })
        .collect();
    taken.dedup();
    taken
}

fn started_sessions(crew: &Crew, worker: &str) -> usize {
    crew.services
        .workers
        .fake(worker)
        .map_or(0, |fake| fake.specs().len())
}

/// A proposer's CLI can't start, and OpenCode would refuse to with no model set, so neither
/// OpenCode nor OpenCode over ACP is ever started: the next CLI that can start takes the
/// proposer's work.
#[tokio::test]
async fn a_proposers_fallback_skips_clis_that_would_refuse_to_start() -> anyhow::Result<()> {
    let crew = council_with_opencode_unset(Some(SPARE))?;
    let outcome = autopilot_with(&crew, RunMode::Crew, true).await?;
    let said = notes(&outcome.events);
    for opencode in [OPENCODE, OPENCODE_ACP] {
        assert_eq!(started_sessions(&crew, opencode), 0, "{opencode}: {said:?}");
    }
    let taken = taken_from(&outcome.events, GEMINI);
    assert!(
        taken
            .iter()
            .any(|(role, by)| *role == RoleName::Draft && by == SPARE),
        "{taken:?}: {said:?}"
    );
    // The CLI that couldn't start is remembered, so none of its later jobs start it again.
    assert_eq!(started_sessions(&crew, GEMINI), 1, "{said:?}");
    assert!(
        !said.iter().any(|note| note.contains("OpenCode")),
        "{said:?}"
    );
    Ok(())
}

/// With only CLIs left that would refuse to start, a proposer's job says at once that nobody is
/// left to take it, and why its own CLI stopped, without starting OpenCode to hear it refuse.
#[tokio::test]
async fn a_proposer_with_only_clis_that_would_refuse_left_has_nobody_to_fall_back_to()
-> anyhow::Result<()> {
    let crew = council_with_opencode_unset(None)?;
    let outcome = autopilot_with(&crew, RunMode::Crew, true).await?;
    let said = notes(&outcome.events);
    for opencode in [OPENCODE, OPENCODE_ACP] {
        assert_eq!(started_sessions(&crew, opencode), 0, "{opencode}: {said:?}");
    }
    assert!(
        said.iter().any(|note| {
            note.contains("draft failed and is left out: Nobody to fall back to as ")
                && note.contains("couldn't start")
        }),
        "{said:?}"
    );
    assert!(
        !said.iter().any(|note| note.contains("OpenCode")),
        "{said:?}"
    );
    Ok(())
}

/// A run that stopped fully: why, then its item's lines.
fn stop_lines(exit: &EngineExit) -> Option<Vec<String>> {
    let EngineExit::Waiting(WaitReason::NoFallback {
        stop: Some(stop), ..
    }) = exit
    else {
        return None;
    };
    Some(
        std::iter::once(stop.why.clone())
            .chain(stop.lines())
            .collect(),
    )
}

/// A CLI that crashed as it started because the internet was gone did nothing wrong: once the
/// internet is back, one more crash with the same words is its first, so the CLI isn't taken
/// out for the run.
#[tokio::test]
async fn startup_crashes_while_offline_dont_take_a_cli_out() -> anyhow::Result<()> {
    use crate::engine_fallback::{Internet, said};

    let mut crew = crate::support::Crew::new()?;
    let codex = crew.answering_worker(CODEX, "openai");
    crew.answering_worker(CLAUDE, "anthropic");
    let internet = Internet::over(&crew);
    let started = Arc::new(AtomicUsize::new(0));
    let lost = internet.clone();
    codex.respond(move |spec| {
        let crash = || {
            FakeSession::ended(EndReason::Crashed {
                message: "fetch failed".to_owned(),
            })
            .without_cli_session()
        };
        match started.fetch_add(1, Ordering::SeqCst) {
            0 => {
                lost.drop_for(std::time::Duration::from_millis(300));
                crash()
            }
            1 => crash(),
            _ => crate::support::smallest_answer(spec),
        }
    });
    let outcome = internet
        .start(&crew, RunMode::Crew, "add a --verbose flag")
        .await?;
    let notes = said(&outcome.events);
    assert!(
        notes
            .iter()
            .all(|(_, text)| !text.contains("as it started")),
        "{notes:?}"
    );
    assert!(codex.specs().len() >= 3, "{notes:?}");
    Ok(())
}
