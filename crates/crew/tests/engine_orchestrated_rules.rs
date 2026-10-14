//! The plan's rules end to end, with the real approval policy over fake workers set up as the
//! orchestrated sample repo was: Codex orchestrates, Gemini (Antigravity CLI) alone reviews, as the
//! request said over the repo's pin to Codex, and Claude Code and Codex own. The orchestrator
//! writes each agent with its provider where a model goes and rotates them through every role, as
//! it did in that run, and code moves what breaks the rules instead of refusing the plan.

mod support;

use std::path::Path;
use std::sync::Arc;

use anyhow::Context;
use serde_json::{Value, json};

use argonauts_core::engine::EngineExit;
use argonauts_core::events::approval::{AnswerAction, ApprovalEvent, ApprovalKind, PolicyCheck};
use argonauts_core::events::plan::{DivideEvent, PlanEvent, TaskGraph};
use argonauts_core::events::run::{
    CheckedInstruction, InstructionOutcome, Party, RunLifecycle, RunMode, RunOutcome,
};
use argonauts_core::events::task::MergeEvent;
use argonauts_core::events::{RunEvent, RunEventKind};
use argonauts_core::names::{ModelRef, RoleName};
use argonauts_core::session_runner::SessionSpec;
use argonauts_core::settings::approvals::ApprovalMode;
use argonauts_core::settings::protocol::{
    CheckCommand, JudgingMethod, PlanningBy, ReviewAssignment, RoutingAlgorithm,
};
use argonauts_core::steps::StepName;
use argonauts_core::steps::intake::InstructionKind;
use argonauts_crew::engine::shared::with_git_env;
use argonauts_testkit::{FakeRunner, FakeSession};

use support::{Crew, Outcome};

const CLAUDE: &str = "claude-code";
const CODEX: &str = "codex";
const AGY: &str = "antigravity-cli";

const REQUEST: &str = "Add due dates. `tasks add <title> --due YYYY-MM-DD` stores a due date, \
                       `tasks list` shows it after the title, and a new `tasks overdue` command \
                       lists open tasks whose due date has passed. Add tests for each, and keep \
                       `node --test` passing. Have codex orchestrate and gemini review it.";

/// The files each task writes when it builds, by the task it became.
const FILES: [(&str, &str); 3] = [
    ("task-001", "src/store.js"),
    ("task-002", "src/cli.js"),
    ("task-003", "test/due.test.js"),
];

fn task(key: &str, path: &str) -> Value {
    json!({
        "key": key,
        "title": format!("Write {path}"),
        "task": format!("Write {path}."),
        "kind": "build",
        "owned_paths": [path],
        "read_only_paths": [],
        "depends_on": [],
        "criteria": [],
    })
}

/// The orchestrator's answer as it came in the sample run: providers where models go, and every
/// agent in every role once.
fn rotated_plan() -> Value {
    json!({
        "plan": {
            "summary": "Due dates in the store, the list, and an overdue command",
            "approach": "", "pieces": [], "choices": [], "risks": [],
            "test_strategy": "node --test", "new_dependencies": []
        },
        "tasks": [
            task("store", FILES[0].1),
            task("cli", FILES[1].1),
            task("tests", FILES[2].1),
        ],
        "assignments": [
            { "task": "store", "worker": "codex/gpt", "reviewers": ["claude-code/claude"] },
            { "task": "cli", "worker": "claude-code/claude", "reviewers": ["antigravity-cli/gemini"] },
            { "task": "tests", "worker": "antigravity-cli/gemini", "reviewers": ["codex/gpt"] }
        ]
    })
}

/// [`rotated_plan`] with two tasks owning one path, which no move of who works on them fixes.
fn overlapping_plan() -> Value {
    let mut plan = rotated_plan();
    plan["tasks"][1]["owned_paths"] = json!([FILES[0].1]);
    plan
}

/// An approval as the judge step, the orchestrator's `plan` for its step, the task's file
/// when it builds, else the smallest valid answer for the step.
fn worker(id: &str, provider: &str, plan: Option<Value>) -> FakeRunner {
    FakeRunner::cli(id, provider).respond_with(move |spec: &SessionSpec| {
        let mut written = None;
        match spec.step {
            Some(StepName::ModelJudge) => {
                return FakeSession::structured(json!({
                    "verdict": "approve",
                    "reasons": ["the evidence meets the rules"],
                    "rules_cited": ["R7"]
                }));
            }
            Some(StepName::Orchestrate) => {
                if let Some(plan) = &plan {
                    return FakeSession::structured(plan.clone())
                        .with_cli_session("codex-orchestrator");
                }
            }
            Some(StepName::Build) => {
                let task = spec.task.as_ref().map(|task| task.as_str());
                let file = FILES
                    .iter()
                    .find(|(id, _)| Some(*id) == task)
                    .map_or("captain.txt", |(_, file)| file);
                let path = spec.workdir.join(file);
                if let Some(dir) = path.parent() {
                    std::fs::create_dir_all(dir).expect("the task's folder");
                }
                std::fs::write(path, "due dates\n").expect("the build writes its file");
                written = Some(support::wrote(file));
            }
            _ => {}
        }
        let session = match &spec.json_schema {
            Some(schema) => FakeSession::structured(support::fill(schema)),
            None => FakeSession::completed("done"),
        };
        // The file is written with the agent's edit tool, as a CLI reports it.
        match written {
            Some(wrote) => session.with_event(wrote),
            None => session,
        }
    })
}

/// The sample repo's settings as the request layer left them, with Codex orchestrating `plan`.
fn trial_crew(plan: Value) -> anyhow::Result<Crew> {
    let mut crew = Crew::new()?;
    crew.services.workers.add(worker(CLAUDE, "anthropic", None));
    crew.services
        .workers
        .add(worker(CODEX, "openai", Some(plan)));
    crew.services.workers.add(worker(AGY, "google", None));
    let refs = |ids: &[&str]| -> Vec<ModelRef> {
        ids.iter().map(|id| id.parse().expect("a model")).collect()
    };
    let settings = &mut crew.settings;
    settings.crew = refs(&[CLAUDE, CODEX, AGY]).into();
    settings.crew.main = refs(&[CODEX])[0].clone();
    settings.workflow = "orchestrator".to_owned();
    settings.drafting.by = PlanningBy::Orchestrator;
    settings.judging.method = JudgingMethod::Orchestrator;
    settings.routing.algorithm = RoutingAlgorithm::Orchestrator;
    settings.review.assign = ReviewAssignment::Orchestrator;
    settings.review.per_task = 1;
    settings.captain.review = true;
    settings.limits.spend.spend_per_run = Some(2.0);
    settings.limits.spend.per_task_usd = Some(0.5);
    // The check takes the place of `node --test`, which stubs and acceptance tests would need.
    settings.divide.contracts_first = false;
    settings.divide.predictions = false;
    settings.checks.run = vec![CheckCommand {
        name: "tests".to_owned(),
        command: vec!["true".to_owned()],
    }];
    let roles = &mut settings.roles;
    roles.entry(RoleName::Orchestrator).or_default().models = refs(&[CODEX]);
    roles.entry(RoleName::Reviewer).or_default().models = refs(&[AGY]);
    roles.entry(RoleName::Worker).or_default().models = refs(&[CLAUDE, CODEX]);
    roles.entry(RoleName::TieBreaker).or_default().models = refs(&[CLAUDE]);
    roles.entry(RoleName::Judge).or_default().models = refs(&[CODEX]);
    settings.approvals.decides = refs(&[AGY])[0].clone().into();
    Ok(crew)
}

/// What the request said about how to run it, as code followed it.
fn followed(says: &str, kind: InstructionKind, said: &str) -> CheckedInstruction {
    CheckedInstruction {
        says: says.to_owned(),
        kind,
        outcome: InstructionOutcome::Followed {
            said: said.to_owned(),
            note: None,
        },
    }
}

/// Starts the request in `mode` with the real policy, and drives it until it waits or ends.
async fn run_in(crew: &Crew, mode: Option<ApprovalMode>) -> anyhow::Result<Outcome> {
    let mut started = crew.run_started(RunMode::Crew, REQUEST)?;
    if let RunEventKind::Run(RunLifecycle::Started(run)) = &mut started.kind {
        run.instructions = vec![
            followed(
                "Have codex orchestrate",
                InstructionKind::Orchestrate,
                "Codex orchestrates",
            ),
            followed(
                "gemini review it",
                InstructionKind::Review,
                "Antigravity CLI reviews",
            ),
        ];
    }
    let mut history = vec![started.clone()];
    if let Some(mode) = mode {
        history.push(RunEvent {
            seq: 1,
            kind: RunEventKind::Run(RunLifecycle::ApprovalModeChanged {
                mode,
                by: Party::User,
            }),
            ..started
        });
    }
    let (mut context, sink, _commands) = crew.context(history)?;
    context.services.policy = real_policy(crew, &context.settings)?;
    let engine = argonauts_crew::engine_for(&crew.settings, RunMode::Crew).run(context);
    let exit = tokio::time::timeout(
        std::time::Duration::from_secs(120),
        with_git_env(crew.git_env(), engine),
    )
    .await
    .context("the run never ended")?;
    Ok(Outcome {
        exit,
        events: sink.events(),
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

fn notes(events: &[RunEvent]) -> Vec<&str> {
    events
        .iter()
        .filter_map(|event| match &event.kind {
            RunEventKind::Note(note) => Some(note.text.as_str()),
            _ => None,
        })
        .collect()
}

/// The approvals still open, in the order they opened.
fn open_kinds(events: &[RunEvent]) -> Vec<ApprovalKind> {
    let mut open = Vec::new();
    for event in events {
        match &event.kind {
            RunEventKind::Approval(ApprovalEvent::Opened(opened)) => {
                open.push((opened.id.clone(), opened.kind));
            }
            RunEventKind::Approval(ApprovalEvent::Decided(decided)) => {
                open.retain(|(id, _)| *id != decided.id);
            }
            RunEventKind::Approval(ApprovalEvent::Withdrawn { id, .. }) => {
                open.retain(|(open, _)| open != id);
            }
            _ => {}
        }
    }
    open.into_iter().map(|(_, kind)| kind).collect()
}

/// Checks that left a question to you that autopilot didn't settle in your place.
fn waited_for_you(events: &[RunEvent]) -> Vec<&PolicyCheck> {
    let settled: Vec<_> = events
        .iter()
        .filter_map(|event| match &event.kind {
            RunEventKind::Approval(ApprovalEvent::Decided(decided))
                if decided.by == Party::Autopilot =>
            {
                Some(decided.id.clone())
            }
            _ => None,
        })
        .collect();
    events
        .iter()
        .filter_map(|event| match &event.kind {
            RunEventKind::Approval(ApprovalEvent::Checked(check)) => Some(check),
            _ => None,
        })
        .filter(|check| {
            !check.auto_preview && check.verdict.is_none() && !settled.contains(&check.id)
        })
        .collect()
}

fn decided_by_you(events: &[RunEvent]) -> bool {
    events.iter().any(|event| {
        matches!(&event.kind, RunEventKind::Approval(ApprovalEvent::Decided(decided))
            if decided.by == Party::User)
    })
}

fn graph(events: &[RunEvent]) -> Option<&TaskGraph> {
    events.iter().rev().find_map(|event| match &event.kind {
        RunEventKind::Divide(DivideEvent::Graph(graph)) => Some(graph),
        _ => None,
    })
}

fn merged(events: &[RunEvent]) -> bool {
    events.iter().any(|event| {
        matches!(
            event.kind,
            RunEventKind::Merge(MergeEvent::RunMerged { .. })
        )
    })
}

/// Every task goes to an agent the rules allow: Claude Code or Codex owns it, and Antigravity
/// CLI alone reviews it.
fn assert_within_the_rules(graph: &TaskGraph) {
    assert_eq!(graph.tasks.len(), 3, "{graph:#?}");
    for task in &graph.tasks {
        let worker = task.worker.as_ref().map(ToString::to_string);
        assert!(
            matches!(worker.as_deref(), Some(CLAUDE | CODEX)),
            "{}: {worker:?}",
            task.id
        );
        let reviewers: Vec<String> = task.reviewers.iter().map(ToString::to_string).collect();
        assert_eq!(reviewers, [AGY], "{}", task.id);
    }
}

/// The sample run's question, answered: in autopilot the orchestrator's rotated plan runs with
/// its assignments moved by code, one note says what moved, and the run reaches its merge with
/// nothing waiting for you.
#[tokio::test]
async fn autopilot_runs_the_trial_plan_to_a_merge_with_no_question() -> anyhow::Result<()> {
    let crew = trial_crew(rotated_plan())?;
    let outcome = run_in(&crew, Some(ApprovalMode::Autopilot)).await?;
    let events = &outcome.events;
    assert_eq!(
        outcome.exit,
        EngineExit::Finished(RunOutcome::Merged),
        "open {:?}, notes {:#?}",
        open_kinds(events),
        notes(events)
    );
    assert!(merged(events));
    for (_, file) in FILES {
        assert!(crew.repo.root().join(file).exists(), "{file} didn't land");
    }

    // The plan stood on the orchestrator's first answer, with its assignments moved.
    let orchestrated: Vec<Option<String>> = events
        .iter()
        .filter_map(|event| match &event.kind {
            RunEventKind::Plan(PlanEvent::Orchestrated { rejection, .. }) => {
                Some(rejection.clone())
            }
            _ => None,
        })
        .collect();
    assert_eq!(orchestrated, [None]);
    assert_within_the_rules(graph(events).expect("the work was divided"));
    let said = notes(events);
    let moved: Vec<&&str> = said
        .iter()
        .filter(|note| note.starts_with("Code moved who owns or reviews"))
        .collect();
    assert_eq!(moved.len(), 1, "{said:#?}");
    let mut lines = moved[0].lines();
    assert_eq!(
        lines.next(),
        Some(
            "Code moved who owns or reviews 2 of the orchestrator's tasks to agents your rules allow."
        )
    );
    assert_eq!(
        lines.next(),
        Some(
            "`store`: Antigravity CLI reviews it, not Claude Code, since only Antigravity CLI \
             may review tasks."
        )
    );

    // Nothing waited for you, and nothing was abandoned.
    assert!(
        waited_for_you(events).is_empty(),
        "{:#?}",
        waited_for_you(events)
    );
    assert!(!decided_by_you(events));
    assert!(open_kinds(events).is_empty());
    Ok(())
}

/// The orchestrator plans knowing what failed before the run changed anything, so its plan
/// gives that fix to a task instead of ruling it out.
#[tokio::test]
async fn the_orchestrator_hears_what_already_fails() -> anyhow::Result<()> {
    use argonauts_core::events::task::{CheckKind, CheckOutcome};

    let crew = trial_crew(rotated_plan())?;
    crew.services.checks.push_at_start(CheckOutcome {
        name: "tests".to_owned(),
        kind: CheckKind::Command,
        passed: false,
        command: Some(vec!["true".to_owned()]),
        exit_code: Some(1),
        duration_ms: 1,
        summary: "15 passed, 1 failed".to_owned(),
        output_tail: "not ok 3 - refills up to capacity".to_owned(),
        violations: Vec::new(),
        sandboxed: true,
        skipped: false,
    });
    let outcome = run_in(&crew, Some(ApprovalMode::Autopilot)).await?;
    assert_eq!(
        outcome.exit,
        EngineExit::Finished(RunOutcome::Merged),
        "{:#?}",
        notes(&outcome.events)
    );
    let orchestrator = crew.services.workers.fake(CODEX).expect("the orchestrator");
    let asked: Vec<String> = orchestrator
        .specs()
        .iter()
        .filter(|spec| spec.step == Some(StepName::Orchestrate))
        .map(|spec| {
            let parts: Vec<&str> = spec.prompt.iter().map(|part| part.text.as_str()).collect();
            parts.join("\n")
        })
        .collect();
    assert!(!asked.is_empty(), "the orchestrator never planned");
    assert!(
        asked[0].contains("Already failing on the code this run started from")
            && asked[0].contains("not ok 3 - refills up to capacity"),
        "{asked:#?}"
    );
    Ok(())
}

/// Supervised, the same run still asks you: the go-ahead waits for you, as questions do when you
/// choose to answer them.
#[tokio::test]
async fn supervised_still_asks() -> anyhow::Result<()> {
    let crew = trial_crew(rotated_plan())?;
    let outcome = run_in(&crew, Some(ApprovalMode::Supervised)).await?;
    assert!(
        matches!(outcome.exit, EngineExit::Waiting(_)),
        "{:?}",
        outcome.exit
    );
    assert_eq!(open_kinds(&outcome.events), [ApprovalKind::Understand]);
    Ok(())
}

/// Your settings let only Antigravity CLI own, and the request says it alone reviews: nobody
/// reviews their own work, so the rules can't all hold. The request's words beat settings, so
/// the workers' pin gives way, one line says so, and the run goes on to its merge.
#[tokio::test]
async fn rules_that_cant_all_hold_relax_the_lowest_with_a_note() -> anyhow::Result<()> {
    let mut crew = trial_crew(rotated_plan())?;
    crew.settings
        .roles
        .entry(RoleName::Worker)
        .or_default()
        .models = vec![AGY.parse()?];
    let outcome = run_in(&crew, Some(ApprovalMode::Autopilot)).await?;
    let events = &outcome.events;
    assert_eq!(
        outcome.exit,
        EngineExit::Finished(RunOutcome::Merged),
        "open {:?}, notes {:#?}",
        open_kinds(events),
        notes(events)
    );
    let said = notes(events);
    let relaxed: Vec<&&str> = said
        .iter()
        .filter(|note| note.starts_with("Your rules for who owns and reviews tasks"))
        .collect();
    assert_eq!(relaxed.len(), 1, "{said:#?}");
    assert_eq!(
        relaxed[0].lines().collect::<Vec<_>>(),
        [
            "Your rules for who owns and reviews tasks can't all hold, so the one from your \
             settings gives way: any of the crew may own a task, not only Antigravity CLI.",
            "Antigravity CLI is the only reviewer you allowed, so Antigravity CLI can't own a \
             task.",
        ]
    );
    assert_within_the_rules(graph(events).expect("the work was divided"));
    assert!(waited_for_you(events).is_empty());
    Ok(())
}

/// Tasks that still break the rules after every try leave nothing to approve: autopilot runs
/// the work as a checked Captain run, says so, and never abandons it or asks.
#[tokio::test]
async fn a_plan_that_cant_stand_runs_captain_in_autopilot() -> anyhow::Result<()> {
    let crew = trial_crew(overlapping_plan())?;
    let outcome = run_in(&crew, Some(ApprovalMode::Autopilot)).await?;
    let events = &outcome.events;
    assert!(
        matches!(outcome.exit, EngineExit::Finished(_)),
        "{:?}: open {:?}, notes {:#?}",
        outcome.exit,
        open_kinds(events),
        notes(events)
    );
    let decided: Vec<AnswerAction> = events
        .iter()
        .filter_map(|event| match &event.kind {
            RunEventKind::Approval(ApprovalEvent::Decided(decided)) => Some(decided.action.clone()),
            _ => None,
        })
        .collect();
    assert!(
        decided.contains(&AnswerAction::SwitchToCaptain),
        "{decided:?}"
    );
    assert!(!decided.contains(&AnswerAction::Abandon), "{decided:?}");
    let said = notes(events);
    assert!(
        said.iter().any(|note| note
            .starts_with("Autopilot answered \"Approve this plan?\" with \"Run it as Captain\"")),
        "{said:#?}"
    );
    assert!(waited_for_you(events).is_empty());
    assert!(!decided_by_you(events));
    Ok(())
}
