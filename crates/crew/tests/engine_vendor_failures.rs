//! A vendor error never ends a run: the agent goes again once, then another agent
//! takes its role, and only when nobody can does the run park, saying why. In full auto a failed
//! approver is asked again, then another company's stands in, else your rules decide alone.
//! The coordinated run of 2026-10-04, whose lead design ended on a passing Codex
//! 401, is replayed from its log.

mod support;

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use anyhow::Context;
use serde_json::{Value, json};

use argonauts_core::ErrorCategory;
use argonauts_core::engine::EngineExit;
use argonauts_core::events::card::{CardEvent, ReviewEvent};
use argonauts_core::events::checkpoint::CheckpointEvent;
use argonauts_core::events::run::{ParkReason, Party, RunLifecycle, RunMode, RunOutcome};
use argonauts_core::events::session::{EndReason, SessionEnd, SessionEvent};
use argonauts_core::events::{RunEvent, RunEventKind};
use argonauts_core::limits::{Candidate, CostEstimate, HealthState};
use argonauts_core::settings::approvals::ApprovalMode;
use argonauts_core::settings::handoff::{CrewRole, FallbackEntry};
use argonauts_core::settings::protocol::GateCommand;
use argonauts_core::steps::StepName;
use argonauts_core::worker::{SessionSpec, WorkerEvent};
use argonauts_core::workflow::WorkflowRef;
use argonauts_crew::engine::shared::with_git_env;
use argonauts_testkit::{FakeSession, FakeWorker};

use support::{Crew, Outcome};

const CODEX: &str = "codex/gpt-5.6-luna";
const CLAUDE: &str = "claude-code/haiku";
const GEMINI: &str = "antigravity-cli/gemini-3.6-flash-low";
/// A fourth agent, from a fourth company.
const QWEN: &str = "opencode/qwen3-coder";

/// What Codex said and how lead:design-4 ended in the coordinated run, copied from its log.
const LEAD_DESIGN_401: &str = include_str!("fixtures/coordinated-401/lead-design-4.jsonl");

/// How an agent's lead design after Understand goes, by its turn among them.
#[derive(Clone, Copy)]
enum Design {
    Answers,
    /// A brief the rules turn down, so the lead is asked again.
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

/// One agent of a crew: its lead designs after Understand by turn, the steps whose sessions
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

/// Who the coordinator puts on the card.
#[derive(Clone)]
struct Assignment {
    owner: &'static str,
    reviewers: Vec<&'static str>,
}

impl Default for Assignment {
    fn default() -> Self {
        Self {
            owner: CODEX,
            reviewers: vec![GEMINI],
        }
    }
}

/// A crew of three, Codex leading the orchestrator workflow with lead designs on and a
/// design at each phase end. Each agent builds its card's file and answers every other step
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
/// the card assigned as `assignment` says.
fn crew_of(agents: &[(&str, Agent)], assignment: &Assignment) -> anyhow::Result<Crew> {
    let mut crew = Crew::new()?;
    crew.settings.crew.clear();
    for (worker, plan) in agents {
        let fake = FakeWorker::harness(worker, family_of(worker))
            .with_login(plan.logged_out.then_some(false))
            .respond_with(agent(plan.clone(), assignment.clone()));
        crew.services.workers.add(fake);
        crew.settings.crew.add(worker.parse()?);
    }
    crew.settings.main_agent = crew.settings.crew.members()[0].clone();
    crew.settings.phases = WorkflowRef::new("orchestrator");
    crew.settings.workflows.lead_designs = true.into();
    crew.settings.workflows.adapt_at_phase_ends = true;
    crew.settings.divide.contracts_first = false;
    crew.settings.checks.run = vec![GateCommand {
        name: "check".to_owned(),
        command: vec!["true".to_owned()],
    }];
    Ok(crew)
}

fn family_of(worker: &str) -> &'static str {
    match worker {
        CODEX => "gpt",
        CLAUDE => "claude",
        GEMINI => "gemini",
        _ => "qwen",
    }
}

/// One agent's answers. Its lead designs after Understand follow `plan`, then answer; the steps
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
        if spec.step == Some(StepName::ModelApprover) {
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
            Some(StepName::Coordinate | StepName::PlanToCards)
        ) {
            answer["cards"] = json!([{
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
        if spec.step == Some(StepName::Coordinate) {
            answer["assignments"] = json!([{
                "card": "due",
                "owner": assignment.owner,
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
    FakeSession::ended(EndReason::VendorError {
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
    FakeSession::ended(EndReason::VendorError {
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
    for line in LEAD_DESIGN_401.lines() {
        let event: RunEvent = serde_json::from_str(line).expect("a recorded event");
        match event.kind {
            RunEventKind::Agent(agent) => events.push(WorkerEvent::Agent(agent)),
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

/// Starts the run and answers every card it parks on with its recommended option, until it
/// finishes or parks for something no answer settles.
async fn drive(crew: &Crew) -> anyhow::Result<Outcome> {
    let outcome = crew.start(RunMode::Crew, "Add due dates").await?;
    answer_cards(crew, outcome).await
}

/// Answers each card the run parks on with its recommended option, until it stops for
/// anything else.
async fn answer_cards(crew: &Crew, mut outcome: Outcome) -> anyhow::Result<Outcome> {
    for _ in 0..20 {
        if !matches!(outcome.exit, EngineExit::Parked(ParkReason::Checkpoint)) {
            break;
        }
        let Some((command, _reply)) = support::recommended(&outcome.events) else {
            break;
        };
        outcome = crew.resume(outcome.events.clone(), vec![command]).await?;
    }
    Ok(outcome)
}

/// The lead designs after Understand that each agent was asked for, in order.
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
async fn an_overload_on_the_leads_design_is_retried_and_the_run_goes_on() -> anyhow::Result<()> {
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
    // One line says it goes again, and nothing says anybody took over.
    let said = notes(&outcome.events);
    let retries: Vec<_> = said
        .iter()
        .filter(|note| note.contains("· trying again"))
        .collect();
    assert_eq!(
        retries,
        [&"GPT 5.6 Luna on Codex couldn't be reached · trying again"],
        "{said:?}"
    );
    assert!(
        !said.iter().any(|note| note.contains("takes over")),
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
    let handed = said
        .iter()
        .find(|note| note.contains("takes over"))
        .context("one line says who took over")?;
    assert_eq!(
        handed.to_string(),
        "Claude Code takes over as the main agent · no second try, this error repeats every time"
    );
    Ok(())
}

#[tokio::test]
async fn an_overload_twice_hands_the_leads_role_to_another_agent() -> anyhow::Result<()> {
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
    // The retry is said, and then who took over, in names and not ids.
    let said = notes(&outcome.events);
    let retry = said
        .iter()
        .position(|note| note.contains("couldn't be reached · trying again"))
        .context("one line says Codex goes again")?;
    let handed = said
        .iter()
        .position(|note| note.contains("takes over"))
        .context("one line says who took the main agent's role")?;
    assert!(retry < handed, "{said:?}");
    assert_eq!(
        said[handed],
        "Still unreachable · Haiku on Claude Code takes over as the main agent"
    );
    Ok(())
}

#[tokio::test]
async fn when_every_agent_fails_the_run_parks_and_says_why() -> anyhow::Result<()> {
    // A sign-in error gets no second try, so each agent is asked once.
    let once = || vec![Design::SignIn];
    let crew = crew_logged_out(
        &[(CODEX, once()), (CLAUDE, once()), (GEMINI, once())],
        &[GEMINI],
    )?;
    let outcome = drive(&crew).await?;
    // Parked with its own reason, and a time it tries again by itself.
    match &outcome.exit {
        EngineExit::Parked(ParkReason::NoStandIn { detail, until }) => {
            assert_eq!(detail, "nobody could take over as the main agent");
            assert!(until.is_some(), "a wake is set");
        }
        other => panic!("{other:?}: {:?}", notes(&outcome.events)),
    }
    assert_eq!(
        finished(&outcome.events),
        None,
        "an agent error never ends the run"
    );
    // Antigravity's CLI says it's logged out, so it isn't asked again and gets the login line.
    assert_eq!(designs_after_understand(&crew, GEMINI), 1);
    let said = notes(&outcome.events);
    let parked = said
        .iter()
        .find(|note| note.starts_with("Nobody could take over as the main agent"))
        .with_context(|| format!("the park says why: {said:?}"))?;
    assert!(
        parked.contains("says it's logged out: log in again"),
        "{parked}"
    );
    assert!(
        !parked.contains("Codex says"),
        "Codex's CLI never said it was out: {parked}"
    );
    assert!(
        parked.ends_with("It also tries again on its own in 5 minutes."),
        "it says when it tries again: {parked}"
    );

    // Once the agents work again, /resume asks the lead's own agent first, on the same session.
    let resumed = crew.resume(outcome.events.clone(), Vec::new()).await?;
    let resumed = answer_cards(&crew, resumed).await?;
    let said = notes(&resumed.events);
    assert_eq!(
        resumed.exit,
        EngineExit::Finished(RunOutcome::Merged),
        "{said:?}"
    );
    // Codex, Claude Code and Antigravity once each, then Codex again after the resume.
    let sessions = design_sessions(&resumed.events);
    let again: Vec<_> = sessions.iter().filter(|(_, starts)| *starts > 1).collect();
    assert_eq!(again.len(), 1, "{sessions:?}");
    assert_eq!(again[0].1, 4);
    assert_eq!(designs_after_understand(&crew, CODEX), sessions.len() + 1);
    Ok(())
}

#[tokio::test]
async fn the_coordinated_runs_passing_401_no_longer_fails_it() -> anyhow::Result<()> {
    // As on 2026-10-04: the lead's first design after Understand is turned down, and the second
    // ends on Codex's 401. Now it's asked again and the run merges.
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

/// Full auto with the real policy: every approver session fails, so after asking each again
/// and finding no one else, your rules decide alone and nothing waits for you.
#[tokio::test]
async fn in_autopilot_a_failing_approver_never_leaves_a_card_to_you() -> anyhow::Result<()> {
    let crew = crew(&[])?;
    for worker in [CLAUDE, GEMINI, CODEX] {
        let fake = crew.services.workers.fake(worker).context("enlisted")?;
        let answers = agent(Agent::default(), Assignment::default());
        fake.respond(move |spec| {
            if spec.step == Some(StepName::ModelApprover) {
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
    // One line for each card full auto decided, each naming its card.
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
        assert!(!note.contains("Pick another approver"), "{note}");
    }
    assert!(!asked_you(&events));
    Ok(())
}

/// Starts the run in full auto with the real policy, and the conductor's own recovery when
/// `conductor_recovery`, and drives it until it finishes or parks.
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

/// Whether any checkpoint waited for your answer.
fn asked_you(events: &[RunEvent]) -> bool {
    events.iter().any(|event| {
        matches!(&event.kind, RunEventKind::Checkpoint(CheckpointEvent::Decided(decided))
            if decided.by == Party::User)
    })
}

/// The real policy, keeping its records in the repo's state folder.
fn real_policy(
    crew: &Crew,
    settings: &Arc<argonauts_core::settings::Settings>,
) -> anyhow::Result<Arc<dyn argonauts_core::policy::ApprovalPolicy>> {
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

/// The conductor's recovery table and handoff planner over the crew's fake boards, so a card
/// is handed on as a real run hands it.
fn conductor_limits(
    crew: &Crew,
    settings: &Arc<argonauts_core::settings::Settings>,
) -> Arc<dyn argonauts_core::limits::LimitsService> {
    let services = &crew.services;
    let planner = Arc::new(argonauts_conductor::handoff::Planner::new(
        settings.clone(),
        services.registry.clone(),
        services.quota.clone(),
        services.health.clone(),
        services.track_records.clone(),
    ));
    Arc::new(
        argonauts_conductor::recovery::Limits::new(
            settings.clone(),
            services.quota.clone(),
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
/// full auto, so the reviewer you asked for reviews and the card merges.
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

/// The first adjudication's overload is asked again, in supervised mode and in full auto, and the
/// ruling goes under the adjudicator that gave it.
#[tokio::test]
async fn a_first_adjudication_overload_is_asked_again() -> anyhow::Result<()> {
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
            (QWEN, Agent::overloaded(StepName::Adjudicate, 1)),
        ];
        let assignment = Assignment {
            owner: CODEX,
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
        let rulings = sessions_of(&outcome.events, QWEN, StepName::Adjudicate);
        assert_eq!(rulings.len(), 1, "{rulings:?}");
        assert_eq!(rulings[0].1, 2, "asked again on its own session");
        let ruled_by: Vec<String> = outcome
            .events
            .iter()
            .filter_map(|event| match &event.kind {
                RunEventKind::Review(ReviewEvent::Adjudicated { adjudicator, .. }) => {
                    Some(adjudicator.to_string())
                }
                _ => None,
            })
            .collect();
        assert_eq!(ruled_by, [QWEN]);
        assert!(
            !said.iter().any(|note| note.contains("adjudicator failed")),
            "{said:?}"
        );
    }
    Ok(())
}

/// A reviewer's seat goes only to an agent from another company than the owner that isn't
/// already reviewing; with none, the review goes on without that reviewer, and every review is
/// recorded under whoever wrote it.
#[tokio::test]
async fn a_reviewers_stand_in_never_doubles_up_or_comes_from_the_owners_company()
-> anyhow::Result<()> {
    let agents = [
        (CODEX, Agent::default()),
        (CLAUDE, Agent::overloaded(StepName::Review, 2)),
        (GEMINI, Agent::default()),
    ];
    let assignment = Assignment {
        owner: CODEX,
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
    // Gemini reviews once, as itself; Codex, the owner, never reviews.
    assert_eq!(reviewers(&outcome.events), [GEMINI]);
    assert_eq!(ran(&crew, GEMINI, StepName::Review), 1);
    assert_eq!(ran(&crew, CODEX, StepName::Review), 0);
    let dropped: Vec<_> = said
        .iter()
        .filter(|note| note.contains("The review goes on without that reviewer"))
        .collect();
    assert_eq!(dropped.len(), 1, "{said:?}");
    assert!(
        dropped[0].starts_with("Nobody could take over as a reviewer"),
        "{dropped:?}"
    );
    Ok(())
}

/// Two reviewers failing at once never share a stand-in: Qwen takes one seat, and the other
/// goes on without it.
#[tokio::test]
async fn two_failing_reviewers_never_share_a_stand_in() -> anyhow::Result<()> {
    let agents = [
        (CODEX, Agent::default()),
        (CLAUDE, Agent::overloaded(StepName::Review, 2)),
        (GEMINI, Agent::overloaded(StepName::Review, 2)),
        (QWEN, Agent::default()),
    ];
    let assignment = Assignment {
        owner: CODEX,
        reviewers: vec![CLAUDE, GEMINI],
    };
    let crew = crew_of(&agents, &assignment)?;
    // Two reviewers review blind, so a stand-in's CLI must keep your plugins out, as OpenCode's
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

/// `[fallback.reviewers] together = 2`: a reviewer that's out as its review starts has two
/// fallbacks review at once in its place, and the transcript says how their reviews count.
#[tokio::test]
async fn two_fallbacks_review_at_once_in_the_seat_of_a_reviewer_thats_out() -> anyhow::Result<()> {
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
        .entry(CrewRole::Reviewers)
        .or_default();
    reviewers_table.together = Some(2);
    let antigravity = argonauts_core::names::VendorId("antigravity-cli".to_owned());
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
                "in place of Gemini 3.6 Flash Low on Antigravity CLI · every finding from each \
                 is kept · /fallback changes this"
            ),
        "{said:?}"
    );
    Ok(())
}

/// A reviewer that is also a fallback for the reviewers, as a settings file may well name it,
/// never takes the seat of one that's out beside its own: it reviews once, and the fallback
/// that's left takes the seat alone.
#[tokio::test]
async fn a_reviewer_listed_as_a_fallback_never_reviews_twice() -> anyhow::Result<()> {
    let agents = [
        (CODEX, Agent::default()),
        (CLAUDE, Agent::default()),
        (GEMINI, Agent::default()),
        (QWEN, Agent::default()),
    ];
    let assignment = Assignment {
        owner: CODEX,
        reviewers: vec![GEMINI, CLAUDE],
    };
    let mut crew = crew_of(&agents, &assignment)?;
    let reviewers_table = crew
        .settings
        .fallback
        .roles
        .entry(CrewRole::Reviewers)
        .or_default();
    reviewers_table.together = Some(2);
    reviewers_table.order = [CLAUDE, QWEN]
        .into_iter()
        .map(|worker| Ok(FallbackEntry::Worker(worker.parse()?)))
        .collect::<anyhow::Result<_>>()?;
    // Two reviewers review blind, so whoever takes a seat must keep your plugins out.
    crew.services.workers.isolating("claude-code");
    crew.services.workers.isolating("opencode");
    let antigravity = argonauts_core::names::VendorId("antigravity-cli".to_owned());
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

/// A reviewer that stops midway hands its seat to one fallback as ever, and with `together = 2`
/// a second joins it, so two reviews stand for the seat.
#[tokio::test]
async fn a_second_fallback_joins_the_one_that_took_a_reviewers_seat_midway() -> anyhow::Result<()> {
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
        .entry(CrewRole::Reviewers)
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

/// A blind reviewer's seat never goes to an agent whose CLI can't keep your plugins out, so with
/// none that can, the review goes on without that reviewer and says why.
#[tokio::test]
async fn a_blind_reviewers_stand_in_must_keep_your_plugins_out() -> anyhow::Result<()> {
    let agents = [
        (CODEX, Agent::default()),
        (CLAUDE, Agent::default()),
        (GEMINI, Agent::overloaded(StepName::Review, 2)),
        (QWEN, Agent::default()),
    ];
    let assignment = Assignment {
        owner: CODEX,
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
/// seat goes to an agent of another CLI that keeps them out, and the handover says what loaded.
#[tokio::test]
async fn a_reviewer_that_loaded_your_plugins_hands_its_seat_to_one_that_keeps_them_out()
-> anyhow::Result<()> {
    let agents = [
        (CODEX, Agent::default()),
        (CLAUDE, Agent::default()),
        (GEMINI, Agent::default()),
        (QWEN, Agent::default()),
    ];
    let assignment = Assignment {
        owner: CODEX,
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
    // Claude Code's and OpenCode's CLIs keep your plugins out; Antigravity's can't.
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
            && note.contains("takes it over")),
        "{said:?}"
    );
    Ok(())
}

/// An overload on the owner's build goes again on its own session, in full auto, and the card
/// carries on with its owner.
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

/// An overload on the owner's build twice: in full auto the card
/// goes on to the next agent, nothing says to log in, and nothing waits.
#[tokio::test]
async fn an_overload_on_build_twice_hands_the_card_on_in_autopilot() -> anyhow::Result<()> {
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
            RunEventKind::Run(RunLifecycle::Parked {
                reason: ParkReason::WaitingForVendor { .. },
                ..
            })
        )
    });
    assert!(!waited, "{said:?}");
    assert!(!asked_you(&outcome.events));
    Ok(())
}

/// With `[fallback.workers] together = 2`, the worker's failure is said with its retry, and when
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
        .entry(CrewRole::Workers)
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
    let retry = at("couldn't be reached · trying again")
        .with_context(|| format!("no line on the retry: {said:?}"))?;
    let both = at("both take").with_context(|| format!("no line on who takes it: {said:?}"))?;
    let passed = at("passed first").with_context(|| format!("no line on who passed: {said:?}"))?;
    assert!(retry < both && both < passed, "{said:?}");
    assert!(
        said[both].starts_with("Still unreachable · ")
            && said[both].ends_with("· the first to pass the checks wins · /fallback changes this"),
        "{}",
        said[both]
    );
    assert!(
        said[passed].ends_with("'s attempt stopped"),
        "{}",
        said[passed]
    );
    Ok(())
}

/// With nobody to take the card, full auto never asks or parks for good: the card waits for its
/// agent with a time it tries again, and one line says why, with no login asked of you while
/// the CLI says it's logged in.
#[tokio::test]
async fn nobody_to_take_the_card_waits_and_tries_again_in_autopilot() -> anyhow::Result<()> {
    let agents = [(CODEX, Agent::overloaded(StepName::Build, 2))];
    let crew = crew_of(&agents, &Assignment::default())?;
    let outcome = autopilot_with(&crew, RunMode::Solo, true).await?;
    let said = notes(&outcome.events);
    match &outcome.exit {
        EngineExit::Parked(ParkReason::WaitingForVendor { vendor, until }) => {
            assert_eq!(vendor.0, "codex");
            assert!(until.is_some(), "a wake is set: {said:?}");
        }
        other => panic!("{other:?}: {said:?}"),
    }
    let waits = said
        .iter()
        .find(|note| note.starts_with("Nobody could take over"))
        .with_context(|| format!("one line says why: {said:?}"))?;
    assert!(
        waits.contains("from GPT 5.6 Luna on Codex: its session ended early (an agent error)"),
        "{waits}"
    );
    assert!(
        waits.ends_with("It tries again on its own in 5 minutes; /resume tries now."),
        "{waits}"
    );
    assert!(!said.iter().any(|note| note.contains("log in")), "{said:?}");
    assert!(
        !said
            .iter()
            .any(|note| note.contains("Wait until the reset")),
        "{said:?}"
    );
    assert!(!asked_you(&outcome.events));
    Ok(())
}

/// An agent whose CLI says it's logged out is out for the run: after its first failed session,
/// its seats go straight to a stand-in, and one line says to log in.
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
        owner: CLAUDE,
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
    let handovers: Vec<_> = said
        .iter()
        .filter(|note| note.starts_with("GPT 5.6 Luna on Codex"))
        .collect();
    assert_eq!(
        handovers,
        [
            &"GPT 5.6 Luna on Codex says it's logged out: log in again; Haiku on Claude Code takes \
           its roles meanwhile."
        ],
        "{said:?}"
    );
    Ok(())
}

/// An agent whose vendor is down is never started: its seats go straight to a stand-in.
#[tokio::test]
async fn an_agent_whose_vendor_is_down_is_never_started() -> anyhow::Result<()> {
    let assignment = Assignment {
        owner: CLAUDE,
        reviewers: vec![GEMINI],
    };
    let agents = [
        (CODEX, Agent::default()),
        (CLAUDE, Agent::default()),
        (GEMINI, Agent::default()),
    ];
    let crew = crew_of(&agents, &assignment)?;
    let codex = argonauts_core::names::VendorId("codex".to_owned());
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
    let handovers: Vec<_> = said
        .iter()
        .filter(|note| note.starts_with("GPT 5.6 Luna on Codex"))
        .collect();
    assert_eq!(
        handovers,
        [
            &"GPT 5.6 Luna on Codex is out (its agent is down), so Haiku on Claude Code takes its \
           roles meanwhile."
        ],
        "{said:?}"
    );
    Ok(())
}

/// A lead out of the run has its later asks in one step go to the same stand-in: they're one
/// seat, so nobody else is brought in.
#[tokio::test]
async fn a_cli_that_crashed_twice_is_not_started_again_for_later_seats() -> anyhow::Result<()> {
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
        owner: CLAUDE,
        reviewers: vec![GEMINI],
    };
    let crew = crew_of(&agents, &assignment)?;
    // In autopilot nothing waits for you, so the run never resumes and its memory holds.
    let outcome = autopilot(&crew, RunMode::Crew).await?;
    let said = notes(&outcome.events);
    assert!(!asked_you(&outcome.events), "{said:?}");
    // One try and its retry on the first seat; after that nothing waits on Codex again.
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

/// A CLI that crashed twice in a row never stands in for another agent later in the run.
#[tokio::test]
async fn a_cli_that_keeps_crashing_never_stands_in() -> anyhow::Result<()> {
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
        owner: CLAUDE,
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
        owner: CLAUDE,
        reviewers: vec![GEMINI],
    };
    let crew = crew_of(&agents, &assignment)?;
    let offered = |name: &str| -> anyhow::Result<Candidate> {
        Ok(Candidate {
            worker: name.parse()?,
            quota_left: None,
            quota_text: String::new(),
            track_record: None,
            cost: CostEstimate::Unknown,
            needs_confirm: false,
            may_run_out: false,
        })
    };
    crew.services
        .handoff
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
    let escalated = outcome.events.iter().any(|event| {
        matches!(
            &event.kind,
            RunEventKind::Card(CardEvent::State { reason, .. })
                if reason.contains("no agent of another provider can take it")
        )
    });
    assert!(escalated, "{said:?}");
    Ok(())
}

#[tokio::test]
async fn a_stand_in_keeps_the_seat_for_the_same_agents_later_asks() -> anyhow::Result<()> {
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
        owner: CLAUDE,
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
    // for after it in the same step included, and Antigravity CLI never does.
    assert!(designs_after_understand(&crew, CLAUDE) >= 2, "{said:?}");
    assert_eq!(designs_after_understand(&crew, GEMINI), 0, "{said:?}");
    Ok(())
}
